//! Tests for `ownership.rs` — goal.md row 1's static move-checker. Each
//! test name states the property being proved; see `ownership.rs`'s module
//! doc for why the branch-merge and loop-double-pass cases specifically
//! need their own coverage (both were real bugs caught while writing this
//! module, not hypothetical edge cases added for completeness).

use nirdosha::interpreter::Value;
use nirdosha::ownership::{check_ownership, OwnershipErrorKind};
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;
use nirdosha::run;

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("should typecheck cleanly before ownership-checking it");
    program
}

fn first_ownership_error(src: &str) -> OwnershipErrorKind {
    let program = parse_ok(src);
    match check_ownership(&program) {
        Ok(()) => panic!("expected an ownership error, but none was found"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

// ---- the basics: box, deref, move -----------------------------------

#[test]
fn box_and_deref_run_end_to_end() {
    let src = r#"
        fn main() -> i64 {
            let b: box i64 = box 42
            return *b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(42)));
}

#[test]
fn deref_does_not_move_the_box() {
    // Reading `*b` twice must not be a use-after-move — a deref-read is
    // exempt from move-checking (module doc). If this regresses, it means
    // `touch_expr`'s `Expr::Deref` special case broke.
    let src = r#"
        fn main() -> i64 {
            let b: box i64 = box 10
            return *b + *b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(20)));
}

#[test]
fn moving_a_box_into_a_new_binding_works() {
    let src = r#"
        fn main() -> i64 {
            let b: box i64 = box 7
            let c: box i64 = b
            return *c
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(7)));
}

#[test]
fn use_after_move_via_let_is_rejected() {
    let kind = first_ownership_error(
        r#"
        fn main() -> i64 {
            let b: box i64 = box 7
            let c: box i64 = b
            return *b
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "b".to_string() });
}

#[test]
fn use_after_move_via_function_call_is_rejected() {
    // Passing an affine value to a function by name moves it, same as a
    // `let` — this test is the "call argument" moving-position, distinct
    // from the "let initializer" one above.
    let kind = first_ownership_error(
        r#"
        fn consume(b: box i64) -> i64 {
            return *b
        }
        fn main() -> i64 {
            let b: box i64 = box 7
            let first: i64 = consume(b)
            let second: i64 = consume(b)
            return first + second
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "b".to_string() });
}

#[test]
fn moving_out_via_return_is_the_last_use_and_is_fine() {
    let src = r#"
        fn make_box(n: i64) -> box i64 {
            let b: box i64 = box n
            return b
        }
        fn main() -> i64 {
            let b: box i64 = make_box(9)
            return *b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(9)));
}

#[test]
fn reassignment_clears_moved_status() {
    // `b`'s original box is moved into `c`, but `b` is then given a fresh
    // value — using `b` again afterward must be fine, since it no longer
    // refers to the moved-away box at all.
    let src = r#"
        fn main() -> i64 {
            let b: box i64 = box 1
            let c: box i64 = b
            b = box 2
            return *b + *c
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(3)));
}

// ---- nested boxes: a real gap found while testing, fixed and pinned ---

#[test]
fn dereferencing_a_nested_box_once_works() {
    let src = r#"
        fn main() -> i64 {
            let bb: box box i64 = box box 5
            let inner: box i64 = *bb
            return *inner
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(5)));
}

#[test]
fn dereferencing_a_nested_box_twice_is_use_after_move() {
    // `*bb` for `bb: box box i64` hands out the *inner* `box i64` by
    // value — itself affine — so unlike `*b` for a scalar box, this has
    // to consume `bb`. A first draft of `ownership.rs` exempted every
    // deref unconditionally and would have accepted this; see the
    // `Expr::Deref` arm of `touch_expr` for the fix.
    let kind = first_ownership_error(
        r#"
        fn main() -> i64 {
            let bb: box box i64 = box box 5
            let a: box i64 = *bb
            let b: box i64 = *bb
            return *a + *b
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "bb".to_string() });
}

// ---- branch-merge: goal.md-relevant regression coverage --------------

#[test]
fn moving_in_only_one_if_branch_still_poisons_later_use() {
    // `b` is moved only inside the `then` branch. The checker can't know
    // at compile time whether `cond` is true or false, so it has to
    // assume the worse case for anything after the `if` — this is what
    // `merge_moved` exists for.
    let kind = first_ownership_error(
        r#"
        fn sink(b: box i64) -> i64 { return *b }
        fn main() -> i64 {
            let b: box i64 = box 1
            let cond: bool = true
            if cond {
                let used: i64 = sink(b)
            }
            return *b
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "b".to_string() });
}

#[test]
fn moving_in_both_if_branches_then_reassigning_is_fine() {
    // Both branches move `b` away, but `b` is unconditionally reassigned
    // afterward — same "reassignment clears moved status" rule as above,
    // just reached through a branch-merge first.
    let src = r#"
        fn sink(b: box i64) -> i64 { return *b }
        fn main() -> i64 {
            let b: box i64 = box 1
            let cond: bool = false
            if cond {
                let x: i64 = sink(b)
            } else {
                let y: i64 = sink(b)
            }
            b = box 99
            return *b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(99)));
}

// ---- the loop double-pass: catches a second-iteration-only move -------

#[test]
fn moving_a_pre_loop_variable_inside_the_body_is_rejected() {
    // `b` is declared before the loop and moved on the *first* iteration
    // of the body. A checker that only examined the body once, from the
    // pre-loop state, would accept this (iteration 1 looks fine in
    // isolation) — it's only wrong because the body would run again. This
    // is exactly the bug `ownership.rs`'s module doc describes catching
    // during development; this test is what would have caught it.
    let kind = first_ownership_error(
        r#"
        fn sink(b: box i64) -> i64 { return *b }
        fn main() -> i64 {
            let b: box i64 = box 1
            let n: i64 = 0
            while n < 3 {
                let used: i64 = sink(b)
                n = n + 1
            }
            return n
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "b".to_string() });
}

#[test]
fn moving_a_variable_declared_fresh_inside_the_loop_body_each_time_is_fine() {
    // Contrast with the test above: here the box is created *inside* the
    // loop body on every iteration, so there's a fresh binding each time —
    // nothing pre-loop ever gets moved, so this must type- and
    // ownership-check cleanly.
    let src = r#"
        fn sink(b: box i64) -> i64 { return *b }
        fn main() -> i64 {
            let n: i64 = 0
            let total: i64 = 0
            while n < 3 {
                let b: box i64 = box n
                total = total + sink(b)
                n = n + 1
            }
            return total
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(3))); // sink(0) + sink(1) + sink(2)
}

#[test]
fn all_examples_pass_ownership_checking() {
    // The three Phase 0 examples don't use `box` at all, so for those this
    // is really a "the checker doesn't false-positive on ordinary scalar
    // code" test; `ownership.nir` is the one that actually exercises moves.
    for src in [
        include_str!("../examples/hello.nir"),
        include_str!("../examples/factorial.nir"),
        include_str!("../examples/loop.nir"),
        include_str!("../examples/ownership.nir"),
    ] {
        let program = parse_ok(src);
        assert_eq!(check_ownership(&program), Ok(()));
    }
}

#[test]
fn example_ownership_runs_to_completion() {
    let src = include_str!("../examples/ownership.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}
