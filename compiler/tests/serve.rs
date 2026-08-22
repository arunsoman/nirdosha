//! Tests for `nirdosha serve` (`src/serve.rs`) — a real `tiny_http`
//! server dispatching `POST /api/<fn>` through `Interpreter::call_named`,
//! plus the authz gate `serve.rs` adds on top of it (`call_named` alone
//! does not enforce `requires(role: ...)` — see that module's doc
//! comment). Each test spins up its own server on a fresh loopback port
//! (`free_port`, same pattern `tests/http.rs` already uses) and talks to
//! it with a hand-rolled HTTP/1.1 request over a raw `TcpStream` — no
//! HTTP client dependency needed for the test itself.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use nirdosha::ast::Program;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::serve::AuthConfig;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

const JWKS: &str = r#"{"keys":[{"kid":"key1","kty":"oct","k":"bXktc2VjcmV0LWtleQ"}]}"#;
const ISSUER: &str = "https://mock-idp.local";
const AUDIENCE: &str = "store-app";

const SRC: &str = r#"
    struct Widget {
        id: i64,
        name: str,
    }

    fn list_widget() -> str {
        return "[]"
    }

    fn admin_only(identity: VerifiedIdentity, x: i64) -> i64 requires(role: "admin") {
        return x * 2
    }

    // typeck.rs requires a `main` to exist even though these tests only
    // ever reach the other functions via `Interpreter::call_named`.
    fn main() {}
"#;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

fn build_program(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
    program
}

/// Starts `nirdosha serve` for `SRC` on a fresh port, in the background,
/// and waits until it's actually accepting connections before returning.
fn start_server(auth: Option<AuthConfig>) -> u16 {
    let port = free_port();
    let program = Arc::new(build_program(SRC));
    std::thread::spawn(move || {
        nirdosha::serve::run(program, port, auth, None).expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("server on port {port} never came up");
}

/// A minimal hand-rolled HTTP/1.1 client -- `Connection: close` so the
/// server (correctly) closes after responding, letting a plain
/// `read_to_string` read the whole response without hanging on
/// keep-alive. Returns `(status, body)`.
fn http_request(port: u16, method: &str, path: &str, body: &str, auth_header: Option<&str>) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let mut req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n",
        body.len()
    );
    if let Some(h) = auth_header {
        req.push_str(&format!("Authorization: {h}\r\n"));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).expect("write request");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let mut parts = resp.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let status = head.lines().next().unwrap_or("").split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, body)
}

// ---- unauthenticated server -------------------------------------------

#[test]
fn get_root_serves_the_derived_ui() {
    let port = start_server(None);
    let (status, body) = http_request(port, "GET", "/", "", None);
    assert_eq!(status, 200);
    assert!(body.contains("\"name\":\"Widget\""), "generated UI should mention the Widget screen");
}

#[test]
fn post_api_calls_a_plain_function_and_encodes_its_result() {
    let port = start_server(None);
    let (status, body) = http_request(port, "POST", "/api/list_widget", "{}", None);
    assert_eq!(status, 200);
    assert_eq!(body, "\"[]\"", "a str-returning fn should encode as a plain JSON string");
}

#[test]
fn post_api_unknown_function_is_404() {
    let port = start_server(None);
    let (status, body) = http_request(port, "POST", "/api/no_such_fn", "{}", None);
    assert_eq!(status, 404);
    assert!(body.contains("no such function"));
}

#[test]
fn requires_gated_fn_without_any_token_is_401() {
    let port = start_server(None);
    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, None);
    assert_eq!(status, 401);
    assert!(body.contains("sign in required"));
}

#[test]
fn bearer_token_with_no_server_side_auth_config_is_a_clear_500_not_silently_accepted() {
    let port = start_server(None); // no --jwks-file/--issuer/--audience
    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, Some("Bearer whatever"));
    assert_eq!(status, 500);
    assert!(body.contains("no --jwks-file"));
}

// ---- authenticated server (the real authz-gate path) ------------------

/// `roles_json` must already be pre-escaped for embedding inside a
/// Nirdosha string literal (`\"admin\"`, not `"admin"`) -- same reason
/// `JWKS` below is pre-escaped (see `tests/mock_issue_token.rs`'s
/// identical fixture and its doc comment on why).
fn mint_token(roles_json: &str) -> String {
    // Dogfoods the language's own builtins from Rust, via `run()` --
    // the same round-trip `tests/mock_issue_token.rs` proves works.
    // `issued_at` is a *real* current-time read, not the fixed
    // `1700000000` literal an earlier version of this helper used --
    // `serve.rs::dispatch` now rejects an expired bearer token against
    // a real clock (a fixed red-team finding), so a token anchored to a
    // fixed point in the past would drift into "expired" the moment
    // more than `ttl_secs` of real wall-clock time has passed since
    // that literal was written, which is already true today. This is
    // Rust test-harness code, not a `.nir` program, so reading the real
    // clock here doesn't touch the language's own determinism contract.
    let issued_at = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is before the Unix epoch")
        .as_secs() as i64;
    let src = format!(
        r#"
        fn main() -> str {{
            return match mock_issue_token("alice", "{ISSUER}", "{AUDIENCE}", {issued_at}, 3600, "{{\"roles\":{roles_json}}}", "{jwks}") {{
                Ok(token) => token,
                Err(e) => e,
            }}
        }}
    "#,
        jwks = JWKS.replace('"', "\\\"")
    );
    match nirdosha::run(&src) {
        Ok(nirdosha::interpreter::Value::Str(s)) => s.to_string(),
        other => panic!("expected a minted token, got {other:?}"),
    }
}

fn auth_config() -> AuthConfig {
    AuthConfig { jwks_json: JWKS.to_string(), issuer: ISSUER.to_string(), audience: AUDIENCE.to_string() }
}

#[test]
fn requires_gated_fn_with_a_valid_role_token_succeeds() {
    let port = start_server(Some(auth_config()));
    let token = mint_token(r#"[\"admin\"]"#);
    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, Some(&format!("Bearer {token}")));
    assert_eq!(status, 200, "body was: {body}");
    assert_eq!(body, "42");
}

#[test]
fn requires_gated_fn_with_an_expired_token_is_401_not_accepted_forever() {
    // A fixed red-team finding: `exp` used to be parsed and carried onto
    // `VerifiedIdentity` but never checked anywhere in this HTTP path —
    // a token stayed a valid, fully privileged credential forever, no
    // matter how old. `issued_at` here is a real 2020 timestamp with a
    // short ttl, so it's genuinely expired against any real clock this
    // test could ever run under, unlike `mint_token`'s real-`now`-anchored
    // tokens used elsewhere in this file.
    let src = format!(
        r#"
        fn main() -> str {{
            return match mock_issue_token("alice", "{ISSUER}", "{AUDIENCE}", 1600000000, 3600, "{{\"roles\":[\"admin\"]}}", "{jwks}") {{
                Ok(token) => token,
                Err(e) => e,
            }}
        }}
    "#,
        jwks = JWKS.replace('"', "\\\"")
    );
    let token = match nirdosha::run(&src) {
        Ok(nirdosha::interpreter::Value::Str(s)) => s.to_string(),
        other => panic!("expected a minted token, got {other:?}"),
    };
    let port = start_server(Some(auth_config()));
    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, Some(&format!("Bearer {token}")));
    assert_eq!(status, 401, "body was: {body}");
    assert!(body.contains("token has expired"), "body was: {body}");
}

#[test]
fn requires_gated_fn_with_a_token_missing_the_role_is_403() {
    let port = start_server(Some(auth_config()));
    let token = mint_token(r#"[\"customer\"]"#);
    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, Some(&format!("Bearer {token}")));
    assert_eq!(status, 403, "body was: {body}");
    assert!(body.contains("insufficient privilege"));
}

#[test]
fn requires_gated_fn_with_a_garbage_token_is_401_not_a_500() {
    let port = start_server(Some(auth_config()));
    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, Some("Bearer not-a-real-jwt"));
    assert_eq!(status, 401, "body was: {body}");
    assert!(body.contains("invalid token"));
}
