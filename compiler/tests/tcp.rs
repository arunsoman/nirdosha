//! Tests for `tcp`/`connect` (`send`/`recv`/`stop` are reused from
//! `chan`/`sandbox`, not new keywords). Deliberately does *not* depend on
//! any external service (Docker, Neo4j, a real HTTP server) being
//! present -- every test here spins up its own minimal `TcpListener` in
//! the test harness itself, the same "self-contained, not environment-
//! fragile" discipline every other test file in this project follows.
//! `examples/tcp_client.nir` is the illustrative, real-world-facing demo
//! (documented as needing an external service); this file is what
//! actually has to pass in CI.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{ErrorKind, Value};
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

/// Binds to an OS-assigned free port and returns it -- avoids any fixed
/// port number that could collide with something else already listening
/// on the machine running these tests.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

// ---- basic connect/send/recv/stop, against a real socket -----------------

#[test]
fn a_connected_client_can_send_and_receive_real_bytes() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
        stream.write_all(b"pong").unwrap();
    });

    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            let conn: tcp = connect("127.0.0.1", {port})
            send(conn, "ping")
            let reply: str = recv(conn)
            stop conn
            return Text(reply)
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

#[test]
fn connecting_to_a_closed_port_produces_a_channel_io_error() {
    // A port nothing is listening on -- bind-and-immediately-drop to get
    // a real, guaranteed-closed port rather than guessing at one that
    // might coincidentally be in use.
    let port = free_port(); // dropped immediately below, so it's free again but was just bound
    let src = format!(
        r#"
        fn main() -> i64 {{
            let conn: tcp = connect("127.0.0.1", {port})
            return 0
        }}
    "#
    );
    let program = parse_ok(&src);
    typecheck(&program).expect("should typecheck cleanly");
    let result = nirdosha::interpreter::Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src.as_str())).run_main();
    match result {
        Err(e) => assert!(
            matches!(e.kind, ErrorKind::ChannelIoError { .. }),
            "expected ChannelIoError, got {:?}",
            e.kind
        ),
        Ok(v) => panic!("expected a connection error, got Ok({v:?})"),
    }
}

// ---- static rejections -------------------------------------------------

#[test]
fn connect_requires_a_str_host() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let conn: tcp = connect(1, 80)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn connect_requires_an_i64_port() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let conn: tcp = connect("localhost", "eighty")
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Str });
}

#[test]
fn send_on_a_tcp_connection_requires_a_str_payload() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let conn: tcp = connect("localhost", 80)
            send(conn, 42)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn stopping_a_tcp_connection_twice_is_a_static_use_after_move() {
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            let conn: tcp = connect("localhost", 80)
            stop conn
            stop conn
            return 0
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let result = nirdosha::ownership::check_ownership(&program);
    assert!(result.is_err(), "using a tcp connection after `stop`ping it must be a static ownership error");
}
