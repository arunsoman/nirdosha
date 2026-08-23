//! Tests for `http_get`/`http_post` (`ast::BUILTIN_NAMES`'s doc comment)
//! — plain HTTP, client-only, built on the same real `TcpStream`
//! `connect` already uses. Deliberately does *not* depend on any
//! external service being present — every test here speaks raw HTTP
//! bytes over a `TcpListener` spun up in the test harness itself, the
//! same self-contained discipline `tests/tcp.rs` already established.

use nirdosha::ast::Ty;
use nirdosha::interpreter::Value;
use nirdosha::parser::Parser;
use nirdosha::run;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};
use std::io::{Read, Write};
use std::net::TcpListener;

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

fn first_type_error(src: &str) -> TypeErrorKind {
    let program = parse_ok(src);
    match typecheck(&program) {
        Ok(()) => panic!("expected a type error, but the program type-checked cleanly"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

/// Accepts exactly one connection, reads the whole request (until the
/// client half-closes — `http_get`/`http_post` always send `Connection:
/// close` and shut down their write side), writes `raw_response` back,
/// and returns the request bytes read so a test can assert on them (the
/// request line, a header, a POSTed body, ...).
fn serve_one(listener: TcpListener, raw_response: &'static [u8]) -> std::thread::JoinHandle<Vec<u8>> {
    std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut request = Vec::new();
        stream.read_to_end(&mut request).unwrap();
        stream.write_all(raw_response).unwrap();
        request
    })
}

// ---- basic get/post, against a real socket ---------------------------------

#[test]
fn http_get_returns_the_status_and_body() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = serve_one(listener, b"HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\n\r\nhello from server");

    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            return match http_get("127.0.0.1", {port}, "/greet") {{
                Ok(resp) => Text(resp.body),
                Err(e) => Text(e),
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "hello from server"),
            other => panic!("expected Text(Str(\"hello from server\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"hello from server\")), got {other:?}"),
    }
    let request = server.join().unwrap();
    let request = String::from_utf8(request).unwrap();
    assert!(request.starts_with("GET /greet HTTP/1.1\r\n"), "got: {request:?}");
    assert!(request.contains("Connection: close"), "got: {request:?}");
}

#[test]
fn http_get_reports_a_non_200_status_code() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = serve_one(listener, b"HTTP/1.1 404 Not Found\r\n\r\nnope");

    let src = format!(
        r#"
        fn main() -> i64 {{
            return match http_get("127.0.0.1", {port}, "/missing") {{
                Ok(resp) => resp.status,
                Err(e) => -1,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Int(404)) => {}
        other => panic!("expected Ok(Int(404)), got {other:?}"),
    }
    server.join().unwrap();
}

#[test]
fn http_post_sends_the_body_with_a_matching_content_length() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = serve_one(listener, b"HTTP/1.1 201 Created\r\n\r\nok");

    let src = format!(
        r#"
        fn main() -> i64 {{
            return match http_post("127.0.0.1", {port}, "/items", "hello world") {{
                Ok(resp) => resp.status,
                Err(e) => -1,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Int(201)) => {}
        other => panic!("expected Ok(Int(201)), got {other:?}"),
    }
    let request = String::from_utf8(server.join().unwrap()).unwrap();
    assert!(request.starts_with("POST /items HTTP/1.1\r\n"), "got: {request:?}");
    assert!(request.contains("Content-Length: 11"), "got: {request:?}");
    assert!(request.ends_with("hello world"), "got: {request:?}");
}

#[test]
fn a_response_body_composes_directly_with_json_get_str() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = serve_one(
        listener,
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"message\": \"pong\"}",
    );

    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            return match http_get("127.0.0.1", {port}, "/ping") {{
                Ok(resp) => match json_parse(resp.body) {{
                    Ok(doc) => match json_get_str(doc, "message") {{
                        Ok(msg) => Text(msg),
                        Err(e) => Text(e),
                    }},
                    Err(e) => Text(e),
                }},
                Err(e) => Text(e),
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "pong"),
            other => panic!("expected Text(Str(\"pong\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"pong\")), got {other:?}"),
    }
    server.join().unwrap();
}

// ---- recoverable failures ----------------------------------------------

#[test]
fn connecting_to_a_closed_port_is_a_recoverable_err_not_a_trap() {
    let port = free_port(); // bound-and-dropped above, guaranteed closed
    let src = format!(
        r#"
        fn main() -> bool {{
            return match http_get("127.0.0.1", {port}, "/") {{
                Ok(resp) => false,
                Err(e) => true,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn a_response_with_no_header_body_separator_is_a_recoverable_err() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = serve_one(listener, b"not a real HTTP response at all");

    let src = format!(
        r#"
        fn main() -> bool {{
            return match http_get("127.0.0.1", {port}, "/") {{
                Ok(resp) => false,
                Err(e) => true,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
    server.join().unwrap();
}

// ---- static rejections ---------------------------------------------------

#[test]
fn http_get_requires_a_str_host() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(HttpResponse, str) = http_get(1, 80, "/")
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn http_get_requires_an_i64_port() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(HttpResponse, str) = http_get("localhost", "eighty", "/")
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Str });
}

#[test]
fn http_post_requires_a_str_body() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(HttpResponse, str) = http_post("localhost", 80, "/", 42)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn redeclaring_http_response_is_rejected_as_a_duplicate_type() {
    let kind = first_type_error(
        r#"
        struct HttpResponse { code: i64 }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateType("HttpResponse".to_string()));
}

#[test]
fn http_is_interpreter_only_rejected_by_codegen() {
    let src = r#"
        fn main() -> i64 {
            let r: Result(HttpResponse, str) = http_get("localhost", 80, "/")
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(nirdosha::codegen::check_supported(&program).is_err());
}

// ---- worked example -------------------------------------------------------

#[test]
fn example_http_runs_to_completion() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = serve_one(
        listener,
        b"HTTP/1.1 200 OK\r\nContent-Type: application/json\r\n\r\n{\"message\": \"pong\"}",
    );
    let src = include_str!("../../examples/http.nir").replace("8977", &port.to_string());
    let program = parse_ok(&src);
    typecheck(&program).expect("should typecheck cleanly");
    match run(&src) {
        Ok(Value::Unit) => {}
        other => panic!("expected Ok(Unit), got {other:?}"),
    }
    server.join().unwrap();
}
