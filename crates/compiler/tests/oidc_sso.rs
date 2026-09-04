//! Production mode's real OIDC Authorization Code + PKCE redirect flow
//! (`serve.rs`'s `GET /auth/login`/`GET /auth/callback`), exercised
//! end-to-end against a **minimal mock identity provider** spun up
//! inside this test itself (`tiny_http`, already a real dependency of
//! this crate — not a new one for this test). There's no real external
//! IdP available in this environment, so this proves the *mechanism* —
//! the full redirect round-trip, PKCE `code_verifier`/`code_challenge`
//! matching, one-shot `state` consumption, and the token verification
//! landing through the exact same `validate_oidc_token` every other
//! request already goes through — not interop with any specific real
//! provider's own quirks.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use nirdosha::ast::Program;
use nirdosha::interpreter::Value;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::serve::{AuthConfig, OidcSsoConfig};
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

const JWKS: &str = r#"{"keys":[{"kid":"key1","kty":"oct","k":"bXktc2VjcmV0LWtleQ"}]}"#;
const ISSUER: &str = "https://mock-idp.local";
const AUDIENCE: &str = "sso-test-app";

const SRC: &str = r#"
    fn admin_only(identity: VerifiedIdentity, x: i64) -> i64 requires(role: "admin") {
        return x * 2
    }
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

/// A raw HTTP/1.1 request over a plain `TcpStream`, capturing status,
/// the `Location` header (if any), and the body — same "no HTTP client
/// dependency needed" posture `tests/serve.rs::http_request` already
/// takes, extended to also read `Location` since this test needs to
/// manually follow each redirect hop itself.
fn raw_request(host: &str, port: u16, method: &str, path: &str, body: &str, content_type: Option<&str>) -> (u16, Option<String>, String) {
    let mut stream = TcpStream::connect((host, port)).expect("connect");
    let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\n");
    if let Some(ct) = content_type {
        req.push_str(&format!("Content-Type: {ct}\r\nContent-Length: {}\r\n", body.len()));
    }
    req.push_str("\r\n");
    req.push_str(body);
    stream.write_all(req.as_bytes()).expect("write request");
    let mut resp = Vec::new();
    stream.read_to_end(&mut resp).expect("read response");
    let resp = String::from_utf8_lossy(&resp).into_owned();
    let mut parts = resp.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let status = head.lines().next().unwrap_or("").split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let location = head.lines().find_map(|l| l.strip_prefix("Location: ").or_else(|| l.strip_prefix("location: ")).map(str::to_string));
    (status, location, body)
}

/// Splits a `scheme://host:port/path?query` URL (this test only ever
/// produces loopback `http://127.0.0.1:PORT/...` URLs, so this is
/// deliberately narrow, not general URL parsing).
fn split_url(url: &str) -> (u16, String) {
    let rest = url.strip_prefix("http://").expect("test URLs are always http://127.0.0.1:PORT/...");
    let (authority, path) = rest.split_once('/').map(|(a, p)| (a, format!("/{p}"))).unwrap_or((rest, "/".to_string()));
    let port: u16 = authority.split(':').nth(1).expect("host:port").parse().expect("port");
    (port, path)
}

/// Percent-decodes one query/form value -- the mock IdP below receives
/// `redirect_uri`/`code_verifier` etc. exactly as `serve.rs`'s own
/// `url_encode_component` encoded them (private to that module, so this
/// test double needs its own minimal decoder, same small fixed
/// unreserved-set logic).
fn url_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            if let Ok(byte) = u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                out.push(byte);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn parse_query(qs: &str) -> std::collections::HashMap<String, String> {
    qs.split('&')
        .filter(|p| !p.is_empty())
        .filter_map(|p| p.split_once('=').map(|(k, v)| (url_decode(k), url_decode(v))))
        .collect()
}

/// A minimal stand-in OIDC provider: `GET /authorize` skips any real
/// login UI (it's a test double) and immediately redirects back with a
/// fixed-shape code; `POST /token` re-derives the PKCE `code_challenge`
/// from the presented `code_verifier` and rejects a mismatch, exactly
/// like a real IdP would, before minting a token via `mock_issue_token`
/// (through `nirdosha::run`, the same public entry point `examples/
/// identity_mock.nir`'s own pattern uses — `mock_issue_token` itself is
/// `pub(crate)`, not reachable directly from an external test crate).
fn start_mock_idp() -> u16 {
    let port = free_port();
    let challenges: Arc<Mutex<std::collections::HashMap<String, String>>> = Arc::new(Mutex::new(std::collections::HashMap::new()));
    std::thread::spawn(move || {
        let server = tiny_http::Server::http(("127.0.0.1", port)).expect("mock IdP should bind");
        for request in server.incoming_requests() {
            let url = request.url().to_string();
            if url.starts_with("/authorize") {
                let query = url.splitn(2, '?').nth(1).unwrap_or("");
                let params = parse_query(query);
                let redirect_uri = params.get("redirect_uri").cloned().unwrap_or_default();
                let state = params.get("state").cloned().unwrap_or_default();
                let code_challenge = params.get("code_challenge").cloned().unwrap_or_default();
                challenges.lock().unwrap().insert(state.clone(), code_challenge);
                let code = format!("code-{state}");
                let location = format!("{redirect_uri}?code={code}&state={state}");
                let resp = tiny_http::Response::from_string("").with_status_code(302).with_header(
                    tiny_http::Header::from_bytes(&b"Location"[..], location.as_bytes()).unwrap(),
                );
                let _ = request.respond(resp);
                continue;
            }
            if url == "/token" {
                let mut request = request;
                let mut body = String::new();
                request.as_reader().read_to_string(&mut body).unwrap_or(0);
                let params = parse_query(&body);
                let code = params.get("code").cloned().unwrap_or_default();
                let code_verifier = params.get("code_verifier").cloned().unwrap_or_default();
                let state = code.strip_prefix("code-").unwrap_or("").to_string();
                let expected_challenge = challenges.lock().unwrap().get(&state).cloned();
                use base64::Engine;
                use sha2::{Digest, Sha256};
                let actual_challenge = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(Sha256::digest(code_verifier.as_bytes()));
                if expected_challenge.as_deref() != Some(actual_challenge.as_str()) {
                    let resp = tiny_http::Response::from_string(r#"{"error":"invalid_grant"}"#).with_status_code(400);
                    let _ = request.respond(resp);
                    continue;
                }
                // A real `issued_at` (not a fixed past constant like
                // `tests/mock_issue_token.rs` uses for a pure round-trip
                // check) -- `serve.rs::resolve_identity` checks `exp`
                // against the real wall clock, so a stale `issued_at`
                // here would make every minted token arrive pre-expired.
                let issued_at =
                    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
                let src = format!(
                    r#"
                    struct Text {{ value: str }}
                    fn main() -> Text {{
                        let jwks: str = "{{\"keys\":[{{\"kid\":\"key1\",\"kty\":\"oct\",\"k\":\"bXktc2VjcmV0LWtleQ\"}}]}}"
                        return match mock_issue_token("alice", "{ISSUER}", "{AUDIENCE}", {issued_at}, 3600, "{{\"roles\":[\"admin\"]}}", jwks) {{
                            Ok(token) => Text(token),
                            Err(e) => Text(e),
                        }}
                    }}
                    "#
                );
                let token = match nirdosha::run(&src) {
                    Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
                        Value::Str(s) => s.to_string(),
                        _ => panic!("expected Text(str)"),
                    },
                    other => panic!("mock_issue_token via nirdosha::run failed: {other:?}"),
                };
                let payload = serde_json::json!({"id_token": token}).to_string();
                let resp = tiny_http::Response::from_string(payload)
                    .with_status_code(200)
                    .with_header(tiny_http::Header::from_bytes(&b"Content-Type"[..], &b"application/json"[..]).unwrap());
                let _ = request.respond(resp);
                continue;
            }
            let _ = request.respond(tiny_http::Response::from_string("not found").with_status_code(404));
        }
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("mock IdP on port {port} never came up");
}

fn start_production_server(mock_idp_port: u16) -> u16 {
    let port = free_port();
    let program = Arc::new(build_program(SRC));
    let transact_log = std::env::temp_dir().join(format!("nirdosha-test-sso-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-test-sso-workflow-{port}.db"));
    let auth = AuthConfig { jwks_json: JWKS.to_string(), issuer: ISSUER.to_string(), audience: AUDIENCE.to_string() };
    let sso = OidcSsoConfig {
        client_id: "test-client".to_string(),
        client_secret: None,
        redirect_uri: format!("http://127.0.0.1:{port}/auth/callback"),
        authorize_endpoint: format!("http://127.0.0.1:{mock_idp_port}/authorize"),
        token_endpoint: format!("http://127.0.0.1:{mock_idp_port}/token"),
    };
    std::thread::spawn(move || {
        nirdosha::serve::run(
            program,
            "127.0.0.1",
            port,
            Some(auth),
            None,
            transact_log,
            workflow_log,
            None,
            None,
            None,
            None,
            None,
            None,
            false,
            Some(sso),
        )
        .expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("production server on port {port} never came up");
}

#[test]
fn full_authorization_code_pkce_round_trip_lands_a_verified_identity() {
    let mock_idp_port = start_mock_idp();
    let port = start_production_server(mock_idp_port);

    // Step 1: `GET /auth/login` -- our server generates PKCE state and
    // redirects to the mock IdP's `/authorize`.
    let (status, location, _) = raw_request("127.0.0.1", port, "GET", "/auth/login", "", None);
    assert_eq!(status, 302, "auth/login should redirect to the authorize endpoint");
    let authorize_url = location.expect("Location header on the /auth/login redirect");
    assert!(authorize_url.contains(&format!(":{mock_idp_port}/authorize")), "should redirect to the mock IdP: {authorize_url}");
    assert!(authorize_url.contains("code_challenge="), "should carry a PKCE code_challenge: {authorize_url}");
    assert!(authorize_url.contains("state="), "should carry a state: {authorize_url}");

    // Step 2: follow the redirect to the mock IdP's `/authorize` --
    // it immediately redirects back to our own `/auth/callback`.
    let (idp_port, idp_path) = split_url(&authorize_url);
    let (status, location, _) = raw_request("127.0.0.1", idp_port, "GET", &idp_path, "", None);
    assert_eq!(status, 302, "mock IdP's /authorize should redirect back with a code");
    let callback_url = location.expect("Location header from the mock IdP");
    assert!(callback_url.contains("/auth/callback"), "should redirect back to our own callback route: {callback_url}");

    // Step 3: follow that redirect to our own `/auth/callback` -- this
    // is where the real work happens: consume the one-shot PKCE state,
    // exchange the code for a token (a real HTTP POST to the mock IdP's
    // `/token`, which itself re-derives and checks the PKCE challenge),
    // and verify the resulting id_token through the exact same
    // `validate_oidc_token` every other request already goes through.
    let (_, callback_path) = split_url(&callback_url);
    let (status, location, body) = raw_request("127.0.0.1", port, "GET", &callback_path, "", None);
    assert_eq!(status, 302, "a successful callback should redirect the browser back into the SPA: {body}");
    let final_location = location.expect("Location header on a successful callback");
    assert!(final_location.starts_with("/#/auth/callback?token="), "should hand the token to the SPA via a # fragment: {final_location}");
    let token = final_location.strip_prefix("/#/auth/callback?token=").expect("token in the fragment");
    assert_eq!(token.matches('.').count(), 2, "should be a real 3-part JWT: {token}");

    // Step 4: that token should actually satisfy a `requires(role:
    // "admin")` gate against our own server -- proving this isn't just
    // a redirect dance, the identity it produces is real and verified.
    // `raw_request` above has no `Authorization` header parameter (it
    // didn't need one for the redirect steps), so this one request is
    // built by hand instead.
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let req_body = r#"{"x":21}"#;
    let req = format!(
        "POST /api/admin_only HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\nAuthorization: Bearer {token}\r\n\r\n{req_body}",
        req_body.len()
    );
    stream.write_all(req.as_bytes()).expect("write request");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let mut parts = resp.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let final_status: u16 = head.lines().next().unwrap_or("").split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    let final_body = parts.next().unwrap_or("").to_string();
    assert_eq!(final_status, 200, "the SSO-issued token should satisfy requires(role: \"admin\"): {final_body}");
    assert_eq!(final_body, "42");
}

#[test]
fn replaying_the_same_state_after_first_use_fails() {
    let mock_idp_port = start_mock_idp();
    let port = start_production_server(mock_idp_port);

    let (_, location, _) = raw_request("127.0.0.1", port, "GET", "/auth/login", "", None);
    let authorize_url = location.expect("Location header");
    let (idp_port, idp_path) = split_url(&authorize_url);
    let (_, location, _) = raw_request("127.0.0.1", idp_port, "GET", &idp_path, "", None);
    let callback_url = location.expect("Location header");
    let (_, callback_path) = split_url(&callback_url);

    let (status, _, _) = raw_request("127.0.0.1", port, "GET", &callback_path, "", None);
    assert_eq!(status, 302, "first use of the callback should succeed");

    let (status, _, body) = raw_request("127.0.0.1", port, "GET", &callback_path, "", None);
    assert_eq!(status, 400, "replaying the same state/code must not succeed twice: {body}");
}

#[test]
fn demo_login_route_does_not_exist_on_a_production_sso_server() {
    let mock_idp_port = start_mock_idp();
    let port = start_production_server(mock_idp_port);
    let (status, _, body) = raw_request("127.0.0.1", port, "POST", "/api/_demo_login", "{}", Some("application/json"));
    assert_eq!(status, 404, "a production server must never expose the self-declared-identity demo route: {body}");
}
