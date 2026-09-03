//! Tests for `nirdosha serve` (`src/serve.rs`) — a real `tiny_http`
//! server dispatching `POST /api/<fn>` through `Interpreter::call_named`,
//! plus the authz gate `serve.rs` adds on top of it (`call_named` alone
//! does not enforce `requires(role: ...)` — see that module's doc
//! comment). Each test spins up its own server on a fresh loopback port
//! (`free_port`, same pattern `tests/http.rs` already uses) and talks to
//! it with a hand-rolled HTTP/1.1 request over a raw `TcpStream` — no
//! HTTP client dependency needed for the test itself.

use std::io::{BufRead, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

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

    fn list_widget() -> i64 {
        return 0
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
    let transact_log = std::env::temp_dir().join(format!("nirdosha-test-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-test-workflow-{port}.db"));
    std::thread::spawn(move || {
        nirdosha::serve::run(program, "127.0.0.1", port, auth, None, transact_log, workflow_log, None, None, None, None, None, None, false, None)
            .expect("serve::run should not fail to bind");
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
    assert_eq!(body, "0", "a plain scalar-returning fn should encode directly, not wrapped in {{\"ok\":...}}");
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

// ---- demo mode (self-declared identity, actually functional) ---------
// `main.rs::cmd_serve` synthesizes `AuthConfig::demo()` and passes
// `demo_mode: true` when no real `--jwks-file`/`--issuer`/`--audience`
// is given -- unlike `start_server(None)` above (a bare `auth: None`,
// `demo_mode: false`, exercising `resolve_identity`'s defensive-code
// arm directly), this exercises the actual demo-mode path a real
// `nirdosha serve` invocation takes.

fn start_demo_server() -> u16 {
    let port = free_port();
    let program = Arc::new(build_program(SRC));
    let transact_log = std::env::temp_dir().join(format!("nirdosha-test-demo-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-test-demo-workflow-{port}.db"));
    std::thread::spawn(move || {
        nirdosha::serve::run(
            program,
            "127.0.0.1",
            port,
            Some(AuthConfig::demo()),
            None,
            transact_log,
            workflow_log,
            None,
            None,
            None,
            None,
            None,
            None,
            true,
            None,
        )
        .expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("demo server on port {port} never came up");
}

#[test]
fn demo_login_mints_a_real_token_that_satisfies_a_role_gate() {
    let port = start_demo_server();
    let (login_status, login_body) =
        http_request(port, "POST", "/api/_demo_login", r#"{"subject":"alice","roles":["admin"],"claims":{}}"#, None);
    assert_eq!(login_status, 200, "demo login should succeed: {login_body}");
    let token = login_body
        .split("\"token\":\"")
        .nth(1)
        .and_then(|s| s.split('"').next())
        .unwrap_or_else(|| panic!("no token in demo login response: {login_body}"));
    assert_eq!(token.matches('.').count(), 2, "a minted token should be a real 3-part JWT, got: {token}");

    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, Some(&format!("Bearer {token}")));
    assert_eq!(status, 200, "a self-picked admin role should actually satisfy requires(role: \"admin\"): {body}");
    assert_eq!(body, "42");
}

#[test]
fn demo_login_with_the_wrong_role_still_403s() {
    let port = start_demo_server();
    let (_, login_body) = http_request(port, "POST", "/api/_demo_login", r#"{"subject":"bob","roles":["nobody"],"claims":{}}"#, None);
    let token = login_body.split("\"token\":\"").nth(1).and_then(|s| s.split('"').next()).expect("token");

    let (status, body) = http_request(port, "POST", "/api/admin_only", r#"{"x":21}"#, Some(&format!("Bearer {token}")));
    assert_eq!(status, 403, "a role the demo login never granted must not satisfy the gate: {body}");
}

#[test]
fn demo_login_route_does_not_exist_on_a_real_auth_server() {
    let port = start_server(Some(auth_config()));
    let (status, body) = http_request(port, "POST", "/api/_demo_login", "{}", None);
    assert_eq!(status, 404, "a production server must never expose the self-declared-identity demo route: {body}");
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
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            return match mock_issue_token("alice", "{ISSUER}", "{AUDIENCE}", {issued_at}, 3600, "{{\"roles\":{roles_json}}}", "{jwks}") {{
                Ok(token) => Text(token),
                Err(e) => Text(e),
            }}
        }}
    "#,
        jwks = JWKS.replace('"', "\\\"")
    );
    match nirdosha::run(&src) {
        Ok(nirdosha::interpreter::Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            nirdosha::interpreter::Value::Str(s) => s.to_string(),
            other => panic!("expected Text(Str(_)), got Text({other:?})"),
        },
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
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            return match mock_issue_token("alice", "{ISSUER}", "{AUDIENCE}", 1600000000, 3600, "{{\"roles\":[\"admin\"]}}", "{jwks}") {{
                Ok(token) => Text(token),
                Err(e) => Text(e),
            }}
        }}
    "#,
        jwks = JWKS.replace('"', "\\\"")
    );
    let token = match nirdosha::run(&src) {
        Ok(nirdosha::interpreter::Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            nirdosha::interpreter::Value::Str(s) => s.to_string(),
            other => panic!("expected Text(Str(_)), got Text({other:?})"),
        },
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

// ---- observability layer 2a: `--otel-port`/`--otel-token`, end to end -----
//
// `tests/observability_layer2a.rs` covers the mechanism itself
// (enable/disable/debounce/fanout) directly against `observability::
// Tracer`. These two tests instead prove the whole stack this module
// actually wires together: a real `nirdosha serve` app port plus its
// APM port, live, at the same time.

/// Same shape as `start_server`, plus a second, loopback-only APM port
/// dynamically gated by `otel_token`.
fn start_server_with_otel(otel_token: &str) -> (u16, u16) {
    let port = free_port();
    let otel_port = free_port();
    let program = Arc::new(build_program(SRC));
    let transact_log = std::env::temp_dir().join(format!("nirdosha-test-transact-otel-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-test-workflow-otel-{port}.db"));
    let token = otel_token.to_string();
    std::thread::spawn(move || {
        nirdosha::serve::run(
            program,
            "127.0.0.1",
            port,
            None,
            None,
            transact_log,
            workflow_log,
            None,
            None,
            None,
            None,
            Some(otel_port),
            Some(token),
            false,
            None,
        )
        .expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() && TcpStream::connect(("127.0.0.1", otel_port)).is_ok() {
            return (port, otel_port);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("server on port {port} (otel port {otel_port}) never came up");
}

#[test]
fn api_call_is_traced_live_to_a_connected_otel_client_and_not_before_it_connects() {
    let (port, otel_port) = start_server_with_otel("apm-secret");

    // Before any APM client connects: an ordinary call still works
    // (dormant tracing must never break the app itself).
    let (status, body) = http_request(port, "POST", "/api/list_widget", "{}", None);
    assert_eq!(status, 200, "body was: {body}");

    // Connect and authenticate the APM client.
    let mut stream = TcpStream::connect(("127.0.0.1", otel_port)).expect("connect to the APM port");
    stream.write_all(b"Bearer apm-secret\n").expect("write handshake");
    let mut reader = std::io::BufReader::new(stream.try_clone().expect("clone stream"));
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("read handshake reply");
    assert_eq!(reply, "ok\n");

    // Give the accept loop a moment to register the client and flip
    // `enabled()` before issuing the call that should now be traced.
    std::thread::sleep(Duration::from_millis(200));
    let (status, body) = http_request(port, "POST", "/api/list_widget", "{}", None);
    assert_eq!(status, 200, "body was: {body}");

    let deadline = Instant::now() + Duration::from_secs(5);
    let mut saw_list_widget_call = false;
    while Instant::now() < deadline {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let parsed: serde_json::Value = match serde_json::from_str(line.trim_end()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if parsed["type"] == "span" && parsed["name"] == "call" && parsed["fn_name"] == "list_widget" {
            saw_list_widget_call = true;
            break;
        }
    }
    assert!(saw_list_widget_call, "expected a `call` span for `list_widget` to reach the connected APM client");
}

#[test]
fn otel_port_rejects_a_wrong_token() {
    let (_port, otel_port) = start_server_with_otel("right-token");
    let mut stream = TcpStream::connect(("127.0.0.1", otel_port)).expect("connect to the APM port");
    stream.write_all(b"Bearer wrong-token\n").expect("write handshake");
    let mut reader = std::io::BufReader::new(stream);
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("read handshake reply");
    assert!(reply.starts_with("err "), "expected a rejection, got: {reply:?}");
}

#[test]
fn otel_port_without_a_token_is_rejected_at_cli_startup() {
    let exe = env!("CARGO_BIN_EXE_nirdosha");
    let mut src_file = std::env::temp_dir();
    src_file.push(format!("nirdosha_test_otel_no_token_{}.nir", std::process::id()));
    std::fs::write(&src_file, SRC).expect("write temp source file");
    let port = free_port();
    let otel_port = free_port();

    let output = std::process::Command::new(exe)
        .arg("serve")
        .arg(&src_file)
        .arg("--port")
        .arg(port.to_string())
        .arg("--otel-port")
        .arg(otel_port.to_string())
        .output()
        .expect("nirdosha serve should launch");
    let _ = std::fs::remove_file(&src_file);

    assert!(!output.status.success(), "`serve --otel-port` with no `--otel-token` must fail to start");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("--otel-token"), "expected the error to mention --otel-token, got: {stderr}");
}

// ---- Kubernetes compliance: /healthz, /readyz, /metrics, SIGTERM, ------
// ---- token files, and the postgres --db fail-fast guard ---------------
// (KUBERNETES.md's P0/P1/P2 rows)

#[test]
fn healthz_returns_200_immediately() {
    let port = start_server(None);
    let (status, body) = http_request(port, "GET", "/healthz", "", None);
    assert_eq!(status, 200);
    assert!(body.contains("\"status\":\"ok\""), "body was: {body}");
}

#[test]
fn readyz_returns_200_with_no_db_configured() {
    let port = start_server(None);
    let (status, body) = http_request(port, "GET", "/readyz", "", None);
    assert_eq!(status, 200, "body was: {body}");
    assert!(body.contains("\"status\":\"ready\""), "body was: {body}");
    assert!(body.contains("not configured"), "body was: {body}");
}

#[test]
fn readyz_reports_real_db_connectivity_when_db_is_configured() {
    let port = free_port();
    let program = Arc::new(build_program(SRC));
    let db_path = std::env::temp_dir().join(format!("nirdosha-test-readyz-db-{port}.db"));
    let transact_log = std::env::temp_dir().join(format!("nirdosha-test-readyz-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-test-readyz-workflow-{port}.db"));
    let db_path_str = db_path.to_string_lossy().into_owned();
    std::thread::spawn(move || {
        nirdosha::serve::run(
            program,
            "127.0.0.1",
            port,
            None,
            None,
            transact_log,
            workflow_log,
            None,
            Some(db_path_str),
            None,
            None,
            None,
            None,
            false,
            None,
        )
        .expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    let (status, body) = http_request(port, "GET", "/readyz", "", None);
    assert_eq!(status, 200, "body was: {body}");
    assert!(body.contains("db: ok"), "expected a real `SELECT 1` check against the configured --db, body was: {body}");
    let _ = std::fs::remove_file(&db_path);
}

#[test]
fn metrics_endpoint_reports_prometheus_text_format_with_real_counts() {
    let port = start_server(None);
    let _ = http_request(port, "GET", "/", "", None); // 200
    let _ = http_request(port, "POST", "/api/no_such_fn", "{}", None); // 404
    let (status, body) = http_request(port, "GET", "/metrics", "", None);
    assert_eq!(status, 200);
    assert!(body.contains("# TYPE nirdosha_requests_total counter"), "body was: {body}");
    assert!(body.contains("nirdosha_responses_total{class=\"2xx\"}"), "body was: {body}");
    assert!(body.contains("nirdosha_responses_total{class=\"4xx\"}"), "body was: {body}");
    let total_line = body
        .lines()
        .find(|l| l.starts_with("nirdosha_requests_total "))
        .unwrap_or_else(|| panic!("total counter line missing from: {body}"));
    let total: u64 = total_line.trim_start_matches("nirdosha_requests_total ").trim().parse().expect("counter value must parse as an integer");
    assert!(total >= 2, "expected at least the 2 prior requests counted (not counting this /metrics call), got {total}");
}

#[test]
fn db_flag_pointed_at_postgres_is_rejected_with_a_clear_error_not_silently_misused() {
    let port = free_port();
    let program = Arc::new(build_program(SRC));
    let transact_log = std::env::temp_dir().join(format!("nirdosha-test-pgreject-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-test-pgreject-workflow-{port}.db"));
    let result = nirdosha::serve::run(
        program,
        "127.0.0.1",
        port,
        None,
        None,
        transact_log.clone(),
        workflow_log.clone(),
        None,
        Some("postgres://user:pass@localhost/db".to_string()),
        None,
        None,
        None,
        None,
        false,
        None,
    );
    let err = result.expect_err("a postgres:// --db value must be rejected outright, not passed to rusqlite::Connection::open");
    assert!(err.contains("--db"), "error should name the flag, got: {err}");
    assert!(err.contains("Postgres") || err.contains("postgres"), "error should explain the gap, got: {err}");
    for p in [
        transact_log,
        workflow_log,
        std::path::PathBuf::from(format!("{}.lock", std::env::temp_dir().join(format!("nirdosha-test-pgreject-transact-{port}.db")).display())),
    ] {
        let _ = std::fs::remove_file(p);
        let _ = std::fs::remove_file(format!("{}.lock", std::env::temp_dir().join(format!("nirdosha-test-pgreject-workflow-{port}.db")).display()));
    }
}

/// The actual `KUBERNETES.md` "politeness gap" fix: a real subprocess,
/// sent a real `SIGTERM`, must exit promptly and cleanly (exit code 0)
/// instead of requiring the orchestrator to wait out a full
/// `terminationGracePeriodSeconds` and escalate to `SIGKILL`. Durability
/// under a hard kill is already proven elsewhere
/// (`tests/transact_process_kill.rs`) — this test is specifically about
/// the *polite* path this change adds.
#[test]
fn sigterm_causes_prompt_graceful_shutdown() {
    let exe = env!("CARGO_BIN_EXE_nirdosha");
    let mut src_file = std::env::temp_dir();
    src_file.push(format!("nirdosha_test_sigterm_{}.nir", std::process::id()));
    std::fs::write(&src_file, SRC).expect("write temp source file");
    let port = free_port();

    let mut child = std::process::Command::new(exe)
        .arg("serve")
        .arg(&src_file)
        .arg("--port")
        .arg(port.to_string())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("nirdosha serve should launch");

    let mut up = false;
    for _ in 0..150 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(up, "server never came up");

    let pid = child.id();
    let kill_status = std::process::Command::new("kill").arg("-TERM").arg(pid.to_string()).status();
    assert!(kill_status.map(|s| s.success()).unwrap_or(false), "sending SIGTERM to the child should succeed");

    let start = Instant::now();
    let exit_status = loop {
        if let Some(status) = child.try_wait().expect("try_wait should not error") {
            break status;
        }
        if start.elapsed() > Duration::from_secs(5) {
            let _ = child.kill();
            let _ = child.wait();
            panic!("process did not exit within 5s of SIGTERM -- the shutdown-flag poll loop should notice within ~200ms");
        }
        std::thread::sleep(Duration::from_millis(50));
    };
    assert!(exit_status.success(), "a clean SIGTERM shutdown should exit 0, got {exit_status:?}");

    let _ = std::fs::remove_file(&src_file);
    for suffix in [".transact.db", ".transact.db.lock", ".workflow.db", ".workflow.db.lock"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", src_file.display()));
    }
}

#[test]
fn presence_token_and_presence_token_file_are_mutually_exclusive() {
    let exe = env!("CARGO_BIN_EXE_nirdosha");
    let mut src_file = std::env::temp_dir();
    src_file.push(format!("nirdosha_test_presence_conflict_{}.nir", std::process::id()));
    std::fs::write(&src_file, SRC).expect("write temp source file");
    let mut token_file = std::env::temp_dir();
    token_file.push(format!("nirdosha_test_presence_token_{}.txt", std::process::id()));
    std::fs::write(&token_file, "secret\n").expect("write token file");

    let output = std::process::Command::new(exe)
        .arg("serve")
        .arg(&src_file)
        .arg("--port")
        .arg(free_port().to_string())
        .arg("--presence-token")
        .arg("raw-secret")
        .arg("--presence-token-file")
        .arg(&token_file)
        .output()
        .expect("nirdosha serve should launch");
    let _ = std::fs::remove_file(&src_file);
    let _ = std::fs::remove_file(&token_file);

    assert!(!output.status.success(), "passing both --presence-token and --presence-token-file must be rejected");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("mutually exclusive"), "expected a clear conflict error, got: {stderr}");
}

/// End-to-end: `--presence-token-file` must authenticate a real request
/// exactly the way `--presence-token`'s raw value already does — proving
/// the file-based flag isn't just accepted at parse time but actually
/// wired through to `handle_presence`'s constant-time comparison.
#[test]
fn presence_token_file_authenticates_a_real_presence_request() {
    let exe = env!("CARGO_BIN_EXE_nirdosha");
    let mut src_file = std::env::temp_dir();
    src_file.push(format!("nirdosha_test_presence_file_{}.nir", std::process::id()));
    std::fs::write(&src_file, SRC).expect("write temp source file");
    let mut token_file = std::env::temp_dir();
    token_file.push(format!("nirdosha_test_presence_token_ok_{}.txt", std::process::id()));
    // Trailing newline on purpose -- exercises `read_token_file`'s
    // exactly-one-newline trim (the common `echo secret > file`/
    // Kubernetes Secret shape).
    std::fs::write(&token_file, "presence-secret\n").expect("write token file");
    let port = free_port();

    let mut child = std::process::Command::new(exe)
        .arg("serve")
        .arg(&src_file)
        .arg("--port")
        .arg(port.to_string())
        .arg("--presence-token-file")
        .arg(&token_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("nirdosha serve should launch");

    let mut up = false;
    for _ in 0..150 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(up, "server never came up");

    let (wrong_status, _) = http_request(port, "POST", "/api/_presence_connect", r#"{"subject":"alice"}"#, Some("Bearer not-the-secret"));
    assert_eq!(wrong_status, 401, "a token that doesn't match the file's content must be rejected");

    let (right_status, right_body) =
        http_request(port, "POST", "/api/_presence_connect", r#"{"subject":"alice"}"#, Some("Bearer presence-secret"));
    assert_eq!(right_status, 200, "the trimmed file content must authenticate successfully, body was: {right_body}");

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&src_file);
    let _ = std::fs::remove_file(&token_file);
    for suffix in [".transact.db", ".transact.db.lock", ".workflow.db", ".workflow.db.lock"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", src_file.display()));
    }
}

#[test]
fn otel_token_file_satisfies_the_otel_port_all_or_nothing_check() {
    let exe = env!("CARGO_BIN_EXE_nirdosha");
    let mut src_file = std::env::temp_dir();
    src_file.push(format!("nirdosha_test_otel_token_file_{}.nir", std::process::id()));
    std::fs::write(&src_file, SRC).expect("write temp source file");
    let mut token_file = std::env::temp_dir();
    token_file.push(format!("nirdosha_test_otel_token_{}.txt", std::process::id()));
    std::fs::write(&token_file, "otel-secret").expect("write token file"); // no trailing newline this time
    let port = free_port();
    let otel_port = free_port();

    let mut child = std::process::Command::new(exe)
        .arg("serve")
        .arg(&src_file)
        .arg("--port")
        .arg(port.to_string())
        .arg("--otel-port")
        .arg(otel_port.to_string())
        .arg("--otel-token-file")
        .arg(&token_file)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
        .expect("nirdosha serve should launch");

    let mut up = false;
    for _ in 0..150 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            up = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert!(up, "`--otel-port` with `--otel-token-file` (no raw --otel-token) must be accepted, not rejected as 'no token'");

    let _ = child.kill();
    let _ = child.wait();
    let _ = std::fs::remove_file(&src_file);
    let _ = std::fs::remove_file(&token_file);
    for suffix in [".transact.db", ".transact.db.lock", ".workflow.db", ".workflow.db.lock"] {
        let _ = std::fs::remove_file(format!("{}{suffix}", src_file.display()));
    }
}
