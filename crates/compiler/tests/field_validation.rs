//! Integration tests for field-level format validation (`screen <Struct>
//! { field <name> { pattern/format/min/max: ... } }`) — the real
//! server-side enforcement in `serve.rs` (`check_field_validations`),
//! not just the parsing/typechecking that `tests/screen_dsl.rs` already
//! covers. Real `tiny_http` server, no `--db`/auth needed (unlike
//! `tests/field_rbac.rs`'s edit-gate tests, a format constraint is
//! checked purely against the submitted value, never against a stored
//! row or an identity) — same `start_server`/`http_request` shape as
//! `tests/field_rbac.rs`/`tests/serve.rs`, trimmed to what this feature
//! actually needs.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use nirdosha::ast::Program;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

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

const CONTACT_SRC: &str = r#"
    struct Contact {
        id: i64,
        name: str,
        email: str,
        age: i64,
    }
    struct Text {
        value: str,
    }

    screen Contact {
        field name {
            pattern: "^[A-Za-z ]+$"
        }
        field email {
            format: "email"
        }
        field age {
            min: 0
            max: 130
        }
    }

    fn create_contact(c: Contact) -> Result(i64, Text) {
        return Ok(c.age)
    }

    fn update_contact(c: Contact) -> Result(i64, Text) {
        return Ok(c.id)
    }

    fn main() {}
"#;

fn start_server() -> u16 {
    let port = free_port();
    let program = Arc::new(build_program(CONTACT_SRC));
    let transact_log = std::env::temp_dir().join(format!("nirdosha-field-validation-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-field-validation-workflow-{port}.db"));
    std::thread::spawn(move || {
        nirdosha::serve::run(program, "127.0.0.1", port, None, None, transact_log, workflow_log, None, None, None, None, None, None, false, None)
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

fn http_request(port: u16, method: &str, path: &str, body: &str) -> (u16, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    let req = format!(
        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Type: application/json\r\nContent-Length: {}\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(req.as_bytes()).expect("write request");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    let mut parts = resp.splitn(2, "\r\n\r\n");
    let head = parts.next().unwrap_or("");
    let body = parts.next().unwrap_or("").to_string();
    let status = head.lines().next().unwrap_or("").split_whitespace().nth(1).and_then(|s| s.parse().ok()).unwrap_or(0);
    (status, body)
}

#[test]
fn pattern_violation_is_rejected_with_400() {
    let port = start_server();
    let (status, body) = http_request(
        port,
        "POST",
        "/api/create_contact",
        r#"{"c":{"id":0,"name":"Grace 123","email":"grace@example.com","age":30}}"#,
    );
    assert_eq!(status, 400, "body was: {body}");
    assert!(body.contains("name"), "error should name the offending field; body was: {body}");
    assert!(!body.contains("[object"), "error body must be a plain string, not a nested object: {body}");
}

#[test]
fn format_email_violation_is_rejected_with_400() {
    let port = start_server();
    let (status, body) =
        http_request(port, "POST", "/api/create_contact", r#"{"c":{"id":0,"name":"Grace","email":"not-an-email","age":30}}"#);
    assert_eq!(status, 400, "body was: {body}");
    assert!(body.contains("email"), "error should name the offending field; body was: {body}");
}

#[test]
fn min_violation_is_rejected_with_400() {
    let port = start_server();
    let (status, body) =
        http_request(port, "POST", "/api/create_contact", r#"{"c":{"id":0,"name":"Grace","email":"grace@example.com","age":-1}}"#);
    assert_eq!(status, 400, "body was: {body}");
    assert!(body.contains("age"), "error should name the offending field; body was: {body}");
}

#[test]
fn max_violation_is_rejected_with_400() {
    let port = start_server();
    let (status, body) =
        http_request(port, "POST", "/api/create_contact", r#"{"c":{"id":0,"name":"Grace","email":"grace@example.com","age":200}}"#);
    assert_eq!(status, 400, "body was: {body}");
    assert!(body.contains("age"), "error should name the offending field; body was: {body}");
}

#[test]
fn valid_values_are_accepted() {
    let port = start_server();
    let (status, body) =
        http_request(port, "POST", "/api/create_contact", r#"{"c":{"id":0,"name":"Grace Hopper","email":"grace@example.com","age":85}}"#);
    assert_eq!(status, 200, "body was: {body}");
    assert!(body.contains(r#""ok":85"#), "body was: {body}");
}

#[test]
fn same_constraints_apply_to_update_not_just_create() {
    let port = start_server();
    let (status, body) =
        http_request(port, "POST", "/api/update_contact", r#"{"c":{"id":1,"name":"Grace","email":"still-not-an-email","age":30}}"#);
    assert_eq!(status, 400, "update_ should be constrained the same way create_ is; body was: {body}");
    assert!(body.contains("email"), "body was: {body}");
}
