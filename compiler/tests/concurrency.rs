//! Tests for `spawn`/`join`/`thread <T>` (goal.md rows 2-3) -- the
//! language-level API, at real-OS-thread-backed correctness. Since when
//! `spawn` started running on `thread_pool.rs`'s reused worker pool
//! instead of one fresh `std::thread::spawn` per call (see that module's
//! own doc comment for the full "why not literal Java-style virtual
//! threads" reasoning), the resource-usage/reuse properties specifically
//! have their own dedicated coverage in `tests/thread_pool_reuse.rs` and
//! `thread_pool.rs`'s own unit tests -- this file stays about the
//! *language-level contract* (a real thread runs the computation, `join`
//! blocks and consumes the handle exactly once, a panic surfaces as
//! `ThreadPanicked`), unaffected by how many physical OS threads end up
//! backing it. Race-freedom isn't implemented in this file's subject at
//! all -- it's `ownership.rs` reusing its existing move-checker on
//! `spawn`'s arguments and on `join`'s handle (see `src/ownership.rs`'s
//! `Expr::Spawn`/`Expr::Join` arms) -- so several tests here exist to pin
//! that *that* is where the guarantee comes from, not anything thread-
//! specific.

use nirdosha::interpreter::{ErrorKind, Interpreter, Value};
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
fn example_threads_runs_to_completion() {
    let src = include_str!("../../examples/threads.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- basic spawn+join round trip -----------------------------------------

#[test]
fn spawned_function_result_round_trips_through_join() {
    let src = r#"
        fn compute(a: i64, b: i64) -> i64 { return a * b + 1 }
        fn main() -> i64 {
            let h: thread i64 = spawn compute(6, 7)
            return join h
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(43)));
}

#[test]
fn two_independent_spawns_both_join_correctly() {
    let src = r#"
        fn double(n: i64) -> i64 { return n * 2 }
        fn main() -> i64 {
            let h1: thread i64 = spawn double(10)
            let h2: thread i64 = spawn double(20)
            let r1: i64 = join h1
            let r2: i64 = join h2
            return r1 + r2
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(60)));
}

// ---- race-freedom comes from ownership.rs's existing move-checker -------

#[test]
fn boxed_argument_cannot_be_reused_after_being_spawned_away() {
    let kind = first_ownership_error(
        r#"
        fn sink(b: box i64) -> i64 { return *b }
        fn main() -> i64 {
            let x: box i64 = box 5
            let h: thread i64 = spawn sink(x)
            let bad: i64 = *x
            let r: i64 = join h
            return r + bad
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "x".to_string() });
}

#[test]
fn joining_the_same_handle_twice_is_a_static_use_after_move() {
    let kind = first_ownership_error(
        r#"
        fn f() -> i64 { return 1 }
        fn main() -> i64 {
            let h: thread i64 = spawn f()
            let a: i64 = join h
            let b: i64 = join h
            return a + b
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "h".to_string() });
}

// ---- dynamic backstops (defense in depth, not the real gate) ------------

#[test]
fn joining_an_already_joined_handle_is_rejected_at_runtime_if_ownership_is_bypassed() {
    // `ownership.rs` already proves this statically (see the test above);
    // this drives the interpreter directly, skipping `check_ownership`, to
    // confirm the runtime backstop (`ErrorKind::AlreadyJoined`) fires on
    // its own -- the same "checker is the real gate, this is the
    // backstop" shape as every other runtime check in interpreter.rs.
    let program = parse_ok(
        r#"
        fn f() -> i64 { return 1 }
        fn main() -> i64 {
            let h: thread i64 = spawn f()
            let a: i64 = join h
            let b: i64 = join h
            return a + b
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    let result = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from("")).run_main();
    match result {
        Err(e) => assert_eq!(e.kind, ErrorKind::AlreadyJoined),
        Ok(v) => panic!("expected AlreadyJoined, got Ok({v:?})"),
    }
}

#[test]
fn a_panic_inside_a_spawned_function_surfaces_as_thread_panicked() {
    // i64 has no dynamic `OutOfRange` guard (unlike the narrower integer
    // types) -- see PHASE0.md's note on the refine.rs i128 fix -- so a
    // genuine i64-vs-i64 overflow reaches Rust's own overflow check and
    // panics. `join` must convert that into a structured runtime error,
    // not let the panic escape past the thread boundary.
    let src = r#"
        fn boom() -> i64 {
            let a: i64 = 9223372036854775807
            let b: i64 = a + 1
            return b
        }
        fn main() -> i64 {
            let h: thread i64 = spawn boom()
            return join h
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    let result = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src)).run_main();
    match result {
        Err(e) => assert!(
            matches!(e.kind, ErrorKind::ThreadPanicked { .. }),
            "expected ThreadPanicked, got {:?}",
            e.kind
        ),
        Ok(v) => panic!("expected ThreadPanicked, got Ok({v:?})"),
    }
}

// ---- static rejections -----------------------------------------------

#[test]
fn spawning_a_builtin_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let h: thread unit = spawn print(1)
            let r: unit = join h
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::CannotSpawnBuiltin { name: "print".to_string() });
}

#[test]
fn joining_a_non_thread_value_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            return join 5
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedThreadType { found: nirdosha::ast::Ty::I64 });
}
