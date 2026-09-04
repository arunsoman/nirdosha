//! Integration tests for field-level `view`/`edit` RBAC (`screen
//! <Struct> { field <name> { view: role(...), edit: role(...) } }`) —
//! the real server-side enforcement in `serve.rs` (`redact_gated_fields`,
//! `check_edit_gates`, and the view-gated pass-through-on-omit step),
//! not just the parsing/typechecking that already existed. Real
//! `--db` tempfile (needed for both the redaction path's response shape
//! and the write-enforcement path's "before" row read), a real `tiny_http`
//! server (same pattern `tests/serve.rs` already uses — `start_server`/
//! `http_request`/`mint_token`), since the behavior under test spans the
//! full request/response cycle, not a single pure function.

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

fn widget_src(db_path: &str) -> String {
    format!(
        r#"
        struct Widget {{
            id: i64,
            name: str,
            secret: str,
        }}
        struct Text {{
            value: str,
        }}

        screen Widget {{
            field secret {{
                view: role("admin")
                edit: role("admin")
            }}
        }}

        fn create_widget(w: Widget) -> Result(i64, Text) {{
            return match db_connect("{db_path}") {{
                Ok(conn) => match db_execute(conn, "CREATE TABLE IF NOT EXISTS widget (id INTEGER PRIMARY KEY AUTOINCREMENT, name TEXT, secret TEXT)") {{
                    Ok(n) => match db_execute(conn, "INSERT INTO widget (name, secret) VALUES (?, ?)", w.name, w.secret) {{
                        Ok(m) => Ok(m),
                        Err(e) => Err(Text(e)),
                    }},
                    Err(e) => Err(Text(e)),
                }},
                Err(e) => Err(Text(e)),
            }}
        }}

        fn list_widget() -> Result(json, Text) {{
            return match db_connect("{db_path}") {{
                Ok(conn) => match db_query(conn, "SELECT id, name, secret FROM widget ORDER BY id") {{
                    Ok(rows) => Ok(rows),
                    Err(e) => Err(Text(e)),
                }},
                Err(e) => Err(Text(e)),
            }}
        }}

        fn update_widget(w: Widget) -> Result(i64, Text) {{
            return match db_connect("{db_path}") {{
                Ok(conn) => match db_execute(conn, "UPDATE widget SET name = ?, secret = ? WHERE id = ?", w.name, w.secret, w.id) {{
                    Ok(n) => Ok(n),
                    Err(e) => Err(Text(e)),
                }},
                Err(e) => Err(Text(e)),
            }}
        }}

        fn main() {{}}
    "#
    )
}

fn start_server(db_path: &str) -> u16 {
    let port = free_port();
    let program = Arc::new(build_program(&widget_src(db_path)));
    let auth = AuthConfig { jwks_json: JWKS.to_string(), issuer: ISSUER.to_string(), audience: AUDIENCE.to_string() };
    let transact_log = std::env::temp_dir().join(format!("nirdosha-field-rbac-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-field-rbac-workflow-{port}.db"));
    let db_path = db_path.to_string();
    std::thread::spawn(move || {
        nirdosha::serve::run(program, "127.0.0.1", port, Some(auth), None, transact_log, workflow_log, None, Some(db_path), None, None, None, None, false, None)
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

/// Same technique `tests/serve.rs::mint_token` already uses -- mints a
/// real signed token via the language's own `mock_issue_token` builtin,
/// round-tripping through the same `oidc_validate_token` the server
/// itself uses, rather than hand-constructing a JWT.
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
    std::env::temp_dir().join(format!("nirdosha-field-rbac-{name}-{}.db", std::process::id())).to_string_lossy().to_string()
}

#[test]
fn view_gated_field_is_redacted_for_an_unauthorized_reader() {
    let db = tempdb("redact");
    let port = start_server(&db);
    let admin = mint_token(r#"[\"admin\"]"#);
    let outsider = mint_token(r#"[\"customer\"]"#);

    let (status, _) =
        http_request(port, "POST", "/api/create_widget", r#"{"w":{"id":0,"name":"gadget","secret":"shh"}}"#, Some(&format!("Bearer {admin}")));
    assert_eq!(status, 200);

    let (status, body) = http_request(port, "POST", "/api/list_widget", "{}", Some(&format!("Bearer {outsider}")));
    assert_eq!(status, 200, "body was: {body}");
    assert!(body.contains(r#""secret":null"#), "expected secret redacted, got: {body}");

    let (status, body) = http_request(port, "POST", "/api/list_widget", "{}", Some(&format!("Bearer {admin}")));
    assert_eq!(status, 200, "body was: {body}");
    assert!(body.contains(r#""secret":"shh""#), "expected secret visible to admin, got: {body}");
}

#[test]
fn table_route_redacts_the_same_field() {
    let db = tempdb("table-redact");
    let port = start_server(&db);
    let admin = mint_token(r#"[\"admin\"]"#);
    let outsider = mint_token(r#"[\"customer\"]"#);

    http_request(port, "POST", "/api/create_widget", r#"{"w":{"id":0,"name":"gadget","secret":"shh"}}"#, Some(&format!("Bearer {admin}")));

    let (status, body) = http_request(port, "POST", "/_nirdosha/table/widget", "{}", Some(&format!("Bearer {outsider}")));
    assert_eq!(status, 200, "body was: {body}");
    assert!(body.contains(r#""secret":null"#), "expected secret redacted via the table route too, got: {body}");
}

#[test]
fn edit_gate_blocks_only_a_real_change_to_the_gated_field() {
    let db = tempdb("edit-gate");
    let port = start_server(&db);
    let admin = mint_token(r#"[\"admin\"]"#);
    let outsider = mint_token(r#"[\"customer\"]"#);

    let (_, create_body) =
        http_request(port, "POST", "/api/create_widget", r#"{"w":{"id":0,"name":"gadget","secret":"shh"}}"#, Some(&format!("Bearer {admin}")));
    assert!(create_body.contains(r#""ok":1"#), "body was: {create_body}");

    // The outsider never saw the real secret (it's redacted to `null` in
    // every read) -- submitting that same `null` back while changing an
    // unrelated field must still succeed: the pass-through-on-omit step
    // should recognize "no real change" and not block the rest of the
    // update.
    let (status, body) = http_request(
        port,
        "POST",
        "/api/update_widget",
        r#"{"w":{"id":1,"name":"renamed","secret":null}}"#,
        Some(&format!("Bearer {outsider}")),
    );
    assert_eq!(status, 200, "unrelated-field update should succeed even though `secret` is redacted; body was: {body}");

    // Now the outsider actually tries to change the gated field.
    let (status, body) = http_request(
        port,
        "POST",
        "/api/update_widget",
        r#"{"w":{"id":1,"name":"renamed","secret":"leaked"}}"#,
        Some(&format!("Bearer {outsider}")),
    );
    assert_eq!(status, 403, "body was: {body}");
    assert!(body.contains("cannot change"), "body was: {body}");

    // An authorized identity changing the same field succeeds.
    let (status, body) = http_request(
        port,
        "POST",
        "/api/update_widget",
        r#"{"w":{"id":1,"name":"renamed","secret":"new-secret"}}"#,
        Some(&format!("Bearer {admin}")),
    );
    assert_eq!(status, 200, "body was: {body}");

    let (_, body) = http_request(port, "POST", "/api/list_widget", "{}", Some(&format!("Bearer {admin}")));
    assert!(body.contains(r#""name":"renamed""#), "name should have been updated by the outsider's earlier call; body was: {body}");
    assert!(body.contains(r#""secret":"new-secret""#), "secret should reflect the admin's change; body was: {body}");
}
