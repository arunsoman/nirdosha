//! Tests for `file`/`open` (`send`/`recv`/`stop` are reused from
//! `tcp`/`sandbox`, not new keywords — see `PROTOLANG_PORT.md`'s file I/O
//! design). Every test here operates on a real temp file on disk; no
//! external service is needed, so this is what actually has to pass in CI.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{ErrorKind, Value};
use nirdosha::parser::Parser;
use nirdosha::run;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};

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

/// A fresh path under the OS temp dir, unique per test run — avoids any
/// collision between tests (or between concurrent `cargo test` runs)
/// touching the same file.
fn temp_path(name: &str) -> String {
    let mut p = std::env::temp_dir();
    p.push(format!("nirdosha_file_io_test_{}_{}", std::process::id(), name));
    p.to_str().unwrap().to_string()
}

// ---- basic open/send/recv/stop, against a real file -----------------------

#[test]
fn writing_then_reading_a_real_file_round_trips() {
    let path = temp_path("roundtrip.txt");
    let src = format!(
        r#"
        fn main() -> str {{
            let out: file = open("{path}", "w")
            send(out, "hello from nirdosha")
            stop out

            let inp: file = open("{path}", "r")
            let content: str = recv(inp)
            stop inp
            return content
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Str(s)) => assert_eq!(&*s, "hello from nirdosha"),
        other => panic!("expected Ok(Str(\"hello from nirdosha\")), got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn appending_to_an_existing_file_does_not_truncate_it() {
    let path = temp_path("append.txt");
    let src = format!(
        r#"
        fn main() -> str {{
            let first: file = open("{path}", "w")
            send(first, "one-")
            stop first

            let second: file = open("{path}", "a")
            send(second, "two")
            stop second

            let inp: file = open("{path}", "r")
            let content: str = recv(inp)
            stop inp
            return content
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Str(s)) => assert_eq!(&*s, "one-two"),
        other => panic!("expected Ok(Str(\"one-two\")), got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn reading_past_eof_returns_an_empty_string_not_an_error() {
    // Unlike a live `tcp` peer closing (a genuine error), a file simply
    // running out of bytes is normal — `recv` should return `""`, not
    // trap. See `interpreter.rs::read_file`'s doc comment.
    let path = temp_path("eof.txt");
    let src = format!(
        r#"
        fn main() -> str {{
            let out: file = open("{path}", "w")
            send(out, "x")
            stop out

            let inp: file = open("{path}", "r")
            let first: str = recv(inp)
            let second: str = recv(inp)
            stop inp
            return second
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Str(s)) => assert_eq!(&*s, ""),
        other => panic!("expected Ok(Str(\"\")), got {other:?}"),
    }
    let _ = std::fs::remove_file(&path);
}

#[test]
fn opening_a_missing_file_for_read_produces_a_channel_io_error() {
    let path = temp_path("does_not_exist.txt");
    let _ = std::fs::remove_file(&path); // guarantee it's really absent
    let src = format!(
        r#"
        fn main() -> i64 {{
            let f: file = open("{path}", "r")
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
        Ok(v) => panic!("expected a file-not-found error, got Ok({v:?})"),
    }
}

#[test]
fn opening_with_an_invalid_mode_produces_a_channel_io_error() {
    let path = temp_path("bad_mode.txt");
    let src = format!(
        r#"
        fn main() -> i64 {{
            let f: file = open("{path}", "rw+")
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
        Ok(v) => panic!("expected an invalid-mode error, got Ok({v:?})"),
    }
}

// ---- static rejections -----------------------------------------------------

#[test]
fn open_requires_a_str_path() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let f: file = open(1, "r")
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn open_requires_a_str_mode() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let f: file = open("/tmp/x", 1)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn send_on_a_file_requires_a_str_payload() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let f: file = open("/tmp/x", "w")
            send(f, 42)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn stopping_a_file_twice_is_a_static_use_after_move() {
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            let f: file = open("/tmp/x", "w")
            stop f
            stop f
            return 0
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let result = nirdosha::ownership::check_ownership(&program);
    assert!(result.is_err(), "using a file after `stop`ping it must be a static ownership error");
}
