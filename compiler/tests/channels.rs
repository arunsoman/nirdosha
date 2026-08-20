//! Tests for `chan`/`send`/`recv` (goal.md row 3's "channels first"
//! answer, per the design in goal.md's own row-3 entry: shared-memory
//! locks are meant to stay opt-in and gated, async messages are the
//! default primitive). Unlike `thread T` (see `tests/concurrency.rs`), a
//! `chan T` handle is **not** affine -- it's meant to be held by more than
//! one concurrent computation at once -- so the ownership story here is
//! about the *payload* crossing a channel via `send`, not the handle
//! itself. See `Ty::Channel`'s doc comment in `src/ast.rs` for why `recv`
//! being a genuine blocking wait keeps row 3's claim narrower than full
//! Pony-style proof-by-construction for now.

use nirdosha::ast::Ty;
use nirdosha::interpreter::Value;
use nirdosha::ownership::{check_ownership, OwnershipErrorKind};
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};
use nirdosha::run;

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

fn first_ownership_error(src: &str) -> OwnershipErrorKind {
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly before ownership-checking it");
    match check_ownership(&program) {
        Ok(()) => panic!("expected an ownership error, but none was found"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

// ---- the example, run end to end ----------------------------------------

#[test]
fn example_channels_runs_to_completion() {
    let src = include_str!("../examples/channels.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- basic send/recv round trip ------------------------------------------

#[test]
fn sent_value_round_trips_through_recv() {
    let src = r#"
        fn producer(c: chan i64) -> unit {
            send(c, 41)
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let h: thread unit = spawn producer(c)
            let v: i64 = recv(c)
            join h
            return v + 1
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(42)));
}

#[test]
fn multiple_sends_are_received_in_fifo_order() {
    let src = r#"
        fn producer(c: chan i64) -> unit {
            send(c, 1)
            send(c, 2)
            send(c, 3)
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let h: thread unit = spawn producer(c)
            let a: i64 = recv(c)
            let b: i64 = recv(c)
            let d: i64 = recv(c)
            join h
            // order-sensitive: only 1,2,3 (in that order) gives 123
            return a * 100 + b * 10 + d
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(123)));
}

#[test]
fn a_boxed_payload_survives_the_round_trip() {
    let src = r#"
        fn producer(c: chan box i64) -> unit {
            send(c, box 99)
            return
        }
        fn main() -> i64 {
            let c: chan box i64 = chan
            let h: thread unit = spawn producer(c)
            let b: box i64 = recv(c)
            join h
            return *b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(99)));
}

#[test]
fn the_channel_handle_itself_is_freely_reusable() {
    // Not affine (unlike `thread T`) -- both the spawning side and the
    // spawned side keep using the same `c` after `spawn`, and the
    // spawning side sends on it too. None of this should be an ownership
    // error; only a *payload* passed to `send` is ever consumed.
    let src = r#"
        fn producer(c: chan i64) -> unit {
            send(c, 1)
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let h: thread unit = spawn producer(c)
            send(c, 2)
            let a: i64 = recv(c)
            let b: i64 = recv(c)
            join h
            return a + b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(3)));
}

// ---- race-freedom: send's payload is a real move -------------------------

#[test]
fn a_boxed_value_cannot_be_reused_after_being_sent() {
    let kind = first_ownership_error(
        r#"
        fn main() -> i64 {
            let c: chan box i64 = chan
            let b: box i64 = box 5
            send(c, b)
            let bad: i64 = *b
            return bad
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "b".to_string() });
}

// ---- static rejections ---------------------------------------------------

#[test]
fn bare_chan_with_no_type_hint_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            chan
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ChannelNeedsExplicitType);
}

#[test]
fn chan_against_a_non_channel_annotation_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let x: i64 = chan
            return 0
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Channel(Box::new(Ty::Error)) }
    );
}

#[test]
fn recv_on_a_non_channel_value_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let x: i64 = 5
            recv(x)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedChannelType { found: Ty::I64 });
}

#[test]
fn sending_the_wrong_payload_type_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let c: chan i64 = chan
            send(c, true)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Bool });
}
