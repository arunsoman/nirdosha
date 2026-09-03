//! Integration tests for the identity role-mapping cache (`ROADMAP.md`
//! Track A item A6) — an admin-editable `RoleMapping { app_role, idp_role
//! }` table that translates the app's canonical role vocabulary into
//! whatever the connected IdP actually puts in a token's `roles` claim.
//! Real `tiny_http` server + real `--db` tempfile (the cache is loaded
//! from, and the fixture's `requires(role: ...)` fns are gated by, real
//! SQLite state) — same `start_server`/`http_request`/`mint_token` shape
//! as `tests/field_rbac.rs`. Every test overrides the cache's TTL via
//! `NIRDOSHA_TEST_ROLE_MAPPING_TTL_MS` (`serve.rs::role_mapping_ttl`'s
//! own documented testing seam) so the TTL boundary can be proven with a
//! real, short wait instead of eating a 30-second tax per test or faking
//! the clock.

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
const TEST_TTL_MS: u64 = 150;

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

fn fixture_src(db_path: &str) -> String {
    format!(
        r#"
        struct Text {{
            value: str,
        }}
        struct RoleMapping {{
            id: i64,
            app_role: str,
            idp_role: str,
        }}

        fn list_role_mapping() -> Result(json, Text) requires(role: "admin") {{
            return match db_connect("{db_path}") {{
                Ok(conn) => match db_query(conn, "SELECT id, app_role, idp_role FROM role_mapping ORDER BY id") {{
                    Ok(rows) => Ok(rows),
                    Err(e) => Err(Text(e)),
                }},
                Err(e) => Err(Text(e)),
            }}
        }}

        fn create_role_mapping(m: RoleMapping) -> Result(i64, Text) requires(role: "admin") {{
            return match db_connect("{db_path}") {{
                Ok(conn) => match db_execute(conn, "INSERT INTO role_mapping (app_role, idp_role) VALUES (?, ?)", m.app_role, m.idp_role) {{
                    Ok(n) => Ok(n),
                    Err(e) => Err(Text(e)),
                }},
                Err(e) => Err(Text(e)),
            }}
        }}

        fn compliance_only_action() -> Result(i64, Text) requires(role: "compliance_officer") {{
            return Ok(1)
        }}

        fn main() {{}}
    "#
    )
}

fn start_server(db_path: &str) -> u16 {
    // Every test in this file wants the same short TTL -- setting the
    // same value from multiple parallel tests is a no-op race, not a
    // correctness issue (unlike setting *different* values would be).
    // SAFETY: every test in this file sets the same value, so a
    // concurrent write from another test thread is a same-value race,
    // not a correctness issue (see this fn's own doc comment above).
    unsafe { std::env::set_var("NIRDOSHA_TEST_ROLE_MAPPING_TTL_MS", TEST_TTL_MS.to_string()) };
    let port = free_port();
    let program = Arc::new(build_program(&fixture_src(db_path)));
    let auth = AuthConfig { jwks_json: JWKS.to_string(), issuer: ISSUER.to_string(), audience: AUDIENCE.to_string() };
    let transact_log = std::env::temp_dir().join(format!("nirdosha-role-mapping-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-role-mapping-workflow-{port}.db"));
    let db_path = db_path.to_string();
    std::thread::spawn(move || {
        nirdosha::serve::run(program, "127.0.0.1", port, Some(auth), None, transact_log, workflow_log, None, Some(db_path), None, None, None, None)
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

/// Same technique `tests/field_rbac.rs::mint_token` uses.
fn mint_token(roles_json: &str) -> String {
    let issued_at =
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).expect("system clock is before the Unix epoch").as_secs() as i64;
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

fn tempdb(name: &str) -> String {
    std::env::temp_dir().join(format!("nirdosha-role-mapping-{name}-{}.db", std::process::id())).to_string_lossy().to_string()
}

#[test]
fn literal_role_still_works_with_no_mapping_configured() {
    let db = tempdb("literal");
    let port = start_server(&db);
    let admin = mint_token(r#"[\"compliance_officer\"]"#);

    let (status, body) = http_request(port, "POST", "/api/compliance_only_action", "{}", Some(&format!("Bearer {admin}")));
    assert_eq!(status, 200, "a literal role match must keep working even with the role_mapping table empty; body was: {body}");
}

#[test]
fn unmapped_idp_role_is_rejected_before_any_mapping_exists() {
    let db = tempdb("unmapped");
    let port = start_server(&db);
    let raw_idp = mint_token(r#"[\"IDP_ComplianceGroup123\"]"#);

    let (status, body) = http_request(port, "POST", "/api/compliance_only_action", "{}", Some(&format!("Bearer {raw_idp}")));
    assert_eq!(status, 403, "an unmapped raw IdP role name must not satisfy the app role; body was: {body}");
}

#[test]
fn mapped_idp_role_is_accepted_only_after_the_ttl_refreshes() {
    let db = tempdb("mapped");
    let port = start_server(&db);
    let admin = mint_token(r#"[\"admin\"]"#);
    let raw_idp = mint_token(r#"[\"IDP_ComplianceGroup123\"]"#);

    // Before any mapping exists: rejected.
    let (status, _) = http_request(port, "POST", "/api/compliance_only_action", "{}", Some(&format!("Bearer {raw_idp}")));
    assert_eq!(status, 403);

    // Admin creates the mapping row.
    let (status, body) = http_request(
        port,
        "POST",
        "/api/create_role_mapping",
        r#"{"m":{"id":0,"app_role":"compliance_officer","idp_role":"IDP_ComplianceGroup123"}}"#,
        Some(&format!("Bearer {admin}")),
    );
    assert_eq!(status, 200, "body was: {body}");

    // Immediately after, still within the TTL window from server
    // startup's eager load -- bounded staleness, not instant.
    let (status, body) = http_request(port, "POST", "/api/compliance_only_action", "{}", Some(&format!("Bearer {raw_idp}")));
    assert_eq!(status, 403, "the mapping must not take effect before the TTL refreshes; body was: {body}");

    // Past the (test-shortened) TTL, the cache refreshes and the raw IdP
    // role now satisfies the app role through the mapping.
    std::thread::sleep(Duration::from_millis(TEST_TTL_MS + 100));
    let (status, body) = http_request(port, "POST", "/api/compliance_only_action", "{}", Some(&format!("Bearer {raw_idp}")));
    assert_eq!(status, 200, "the mapped IdP role should satisfy the app role once the cache refreshes; body was: {body}");
}

#[test]
fn mapping_already_in_the_db_at_startup_is_loaded_eagerly_not_after_one_ttl_window() {
    // A mapping created via a first server instance, then a SECOND
    // server instance started against the same DB, must see it
    // immediately -- proving the eager startup load, not just the
    // per-request TTL refresh path the test above already covers.
    let db = tempdb("eager-startup");
    let port1 = start_server(&db);
    let admin = mint_token(r#"[\"admin\"]"#);
    let (status, body) = http_request(
        port1,
        "POST",
        "/api/create_role_mapping",
        r#"{"m":{"id":0,"app_role":"compliance_officer","idp_role":"IDP_ComplianceGroup123"}}"#,
        Some(&format!("Bearer {admin}")),
    );
    assert_eq!(status, 200, "body was: {body}");

    let port2 = start_server(&db);
    let raw_idp = mint_token(r#"[\"IDP_ComplianceGroup123\"]"#);
    let (status, body) = http_request(port2, "POST", "/api/compliance_only_action", "{}", Some(&format!("Bearer {raw_idp}")));
    assert_eq!(status, 200, "a mapping already in the DB at startup must be live immediately, not after one TTL window; body was: {body}");
}
