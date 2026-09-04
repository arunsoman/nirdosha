//! Tests for `sandbox` + `chan`/`send`/`recv` together (docs/SANDBOXING.md's
//! "layer 2": a real cross-process transport for the same `chan T`
//! primitive `tests/channels.rs` already covers in-process). Same
//! `Ty::Channel`, same `send`/`recv` syntax on both sides of the process
//! boundary -- what's different is purely the transport underneath (a
//! real Unix domain socket instead of an in-memory queue), invisible
//! from Nirdosha source. See `interpreter.rs`'s `ChannelInner`/
//! `TransportState` for where that split actually lives.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{ErrorKind, Interpreter, Value};
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
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

/// Same "point sandboxed children at the real binary, not the test
/// harness" fix `tests/sandbox.rs` needed -- see its own `interp` helper
/// and docs/PHASE0.md's "Thirteenth update" for the bug this avoids repeating.
fn interp(program: nirdosha::ast::Program, src: &str) -> Interpreter {
    Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src))
        .with_sandbox_exe(std::path::PathBuf::from(env!("CARGO_BIN_EXE_nirdosha")))
}

// ---- the example, run end to end ------------------------------------

#[test]
fn example_sandbox_channels_runs_to_completion() {
    let src = include_str!("../../../examples/sandbox_channels.nir");
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Ok(v) => assert_eq!(v, Value::Unit),
        Err(e) => panic!("expected Ok(Unit), got Err({e})"),
    }
}

// ---- bidirectional communication over a real process boundary ---------

#[test]
fn a_value_sent_to_a_sandboxed_process_and_doubled_comes_back() {
    let src = r#"
        fn worker(c: chan i64) -> unit {
            let v: i64 = recv(c)
            send(c, v * 2)
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let s: sandbox = sandbox worker(c)
            send(c, 21)
            let result: i64 = recv(c)
            stop s
            return result
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Ok(v) => assert_eq!(v, Value::Int(42)),
        Err(e) => panic!("expected Ok(Int(42)), got Err({e})"),
    }
}

#[test]
fn multiple_messages_flow_correctly_in_both_directions() {
    let src = r#"
        fn worker(c: chan i64) -> unit {
            let a: i64 = recv(c)
            let b: i64 = recv(c)
            send(c, a + b)
            send(c, a * b)
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let s: sandbox = sandbox worker(c)
            send(c, 6)
            send(c, 7)
            let sum: i64 = recv(c)
            let prod: i64 = recv(c)
            stop s
            return sum + prod
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Ok(v) => assert_eq!(v, Value::Int(13 + 42)),
        Err(e) => panic!("expected Ok(Int(55)), got Err({e})"),
    }
}

#[test]
fn a_bool_payload_round_trips_correctly() {
    let src = r#"
        fn worker(c: chan bool) -> unit {
            let v: bool = recv(c)
            send(c, !v)
            return
        }
        fn main() -> bool {
            let c: chan bool = chan
            let s: sandbox = sandbox worker(c)
            send(c, true)
            let r: bool = recv(c)
            stop s
            return r
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Ok(v) => assert_eq!(v, Value::Bool(false)),
        Err(e) => panic!("expected Ok(Bool(false)), got Err({e})"),
    }
}

// ---- honest failure modes, not silent hangs or wrong answers ----------

#[test]
fn a_sandboxed_process_that_exits_without_sending_produces_a_clear_channel_error() {
    // The worker never touches `c` at all and just returns -- the
    // parent's `recv` must fail with a real, structured error, not hang
    // forever or panic. Also pins the improved message (see
    // `read_value`'s `UnexpectedEof` mapping): Rust's own "failed to fill
    // whole buffer" would be useless to a user.
    let src = r#"
        fn worker(c: chan i64) -> unit {
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let s: sandbox = sandbox worker(c)
            let v: i64 = recv(c)
            stop s
            return v
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Err(e) => match &e.kind {
            ErrorKind::ChannelIoError { message } => {
                assert!(
                    message.contains("closed this channel"),
                    "expected the improved EOF message, got {message:?}"
                );
            }
            other => panic!("expected ChannelIoError, got {other:?}"),
        },
        Ok(v) => panic!("expected a ChannelIoError, got Ok({v:?})"),
    }
}

#[test]
fn a_channel_with_already_queued_messages_cannot_be_passed_to_sandbox() {
    // Deliberate layer-2 scope limit (see `ChannelInner`'s doc comment):
    // replaying already-in-memory-queued messages onto a fresh socket
    // isn't attempted. Must be a clear error, not a silent message drop.
    let src = r#"
        fn worker(c: chan i64) -> unit { return }
        fn main() -> i64 {
            let c: chan i64 = chan
            send(c, 1)
            let s: sandbox = sandbox worker(c)
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Err(e) => match &e.kind {
            ErrorKind::SandboxSpawnFailed { message } => {
                assert!(
                    message.contains("already-queued"),
                    "expected the pre-queued-messages message, got {message:?}"
                );
            }
            other => panic!("expected SandboxSpawnFailed, got {other:?}"),
        },
        Ok(v) => panic!("expected a SandboxSpawnFailed error, got Ok({v:?})"),
    }
}

#[test]
fn the_same_channel_cannot_be_passed_to_two_sandboxes() {
    let src = r#"
        fn worker(c: chan i64) -> unit { return }
        fn main() -> i64 {
            let c: chan i64 = chan
            let s1: sandbox = sandbox worker(c)
            let s2: sandbox = sandbox worker(c)
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Err(e) => match &e.kind {
            ErrorKind::SandboxSpawnFailed { message } => {
                assert!(
                    message.contains("already passed to a `sandbox`"),
                    "expected the already-crossed-once message, got {message:?}"
                );
            }
            other => panic!("expected SandboxSpawnFailed, got {other:?}"),
        },
        Ok(v) => panic!("expected a SandboxSpawnFailed error, got Ok({v:?})"),
    }
}

// ---- static rejections -------------------------------------------------

#[test]
fn a_channel_of_a_non_scalar_type_cannot_be_a_sandbox_argument() {
    let kind = first_type_error(
        r#"
        fn worker(c: chan box i64) -> unit { return }
        fn main() -> i64 {
            let c: chan box i64 = chan
            let s: sandbox = sandbox worker(c)
            return 0
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::SandboxArgMustBeScalar { found: Ty::Channel(Box::new(Ty::Box(Box::new(Ty::I64)))) }
    );
}
