//! Tests for `refine.rs` — goal.md row 4's Tier-1 proof pass (interval
//! analysis, not SMT — see the module doc for why). Each test asserts
//! either that a specific site *is* proven, or — just as important, and
//! tested just as deliberately — that a genuinely unprovable site is
//! *not* falsely claimed as proven. A pass that only ever demonstrates
//! successful proofs hasn't demonstrated it's sound.

use nirdosha::ast::{Program, Stmt};
use nirdosha::parser::Parser;
use nirdosha::refine::analyze;
use nirdosha::token::Lexer;

fn parse(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

/// The `Span` of the `n`th top-level `let` statement in `main`'s body
/// (0-indexed) — extracted from the real parsed AST rather than
/// hand-computed from source text, so these tests don't silently rot if
/// a comment or blank line shifts a line number.
fn nth_let_span(program: &Program, n: usize) -> nirdosha::token::Span {
    let main = program.fns.iter().find(|f| f.name == "main").expect("no main");
    main.body
        .stmts
        .iter()
        .filter_map(|s| match s {
            Stmt::Let { span, .. } => Some(*span),
            _ => None,
        })
        .nth(n)
        .expect("not enough let statements")
}

/// The `Span` of the `n`th `/` (division) operation found anywhere in
/// `main`'s body, in the order the parser encounters them.
fn nth_div_span(program: &Program, n: usize) -> nirdosha::token::Span {
    use nirdosha::ast::{BinOp, Expr};
    fn walk(e: &Expr, out: &mut Vec<nirdosha::token::Span>) {
        if let Expr::Binary(op, l, r, span) = e {
            walk(l, out);
            walk(r, out);
            if *op == BinOp::Div {
                out.push(*span);
            }
        }
    }
    let main = program.fns.iter().find(|f| f.name == "main").expect("no main");
    let mut spans = Vec::new();
    for s in &main.body.stmts {
        if let Stmt::Let { value, .. } = s {
            walk(value, &mut spans);
        }
    }
    spans[n]
}

// ---- proofs that should succeed ----------------------------------------

#[test]
fn straight_line_arithmetic_within_range_is_proven() {
    let program = parse(
        r#"
        fn main() -> i64 {
            let x: i32 = 100 + 50
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    assert!(report.proven_in_range.contains(&nth_let_span(&program, 0)));
}

#[test]
fn a_tracked_variable_carries_its_precise_interval_forward() {
    // `n` isn't just "some i64" here -- it's known to be exactly 5, so
    // `n + 3` is known to be exactly 8, well within i8's range.
    let program = parse(
        r#"
        fn main() -> i64 {
            let n: i64 = 5
            let x: i8 = n + 3
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    assert!(report.proven_in_range.contains(&nth_let_span(&program, 1)));
}

#[test]
fn literal_nonzero_divisor_is_proven_nonzero() {
    let program = parse(
        r#"
        fn main() -> i64 {
            let x: i64 = 10 / 2
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    assert!(report.proven_nonzero_divisor.contains(&nth_div_span(&program, 0)));
}

#[test]
fn condition_excluded_zero_divisor_is_proven_nonzero() {
    // `d`'s interval is exactly [1, 1] after the `let`, which excludes
    // zero -- this is proven from tracked-interval precision, not from
    // reading the source text.
    let program = parse(
        r#"
        fn main() -> i64 {
            let d: i64 = 1
            let x: i64 = 100 / d
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    assert!(report.proven_nonzero_divisor.contains(&nth_div_span(&program, 0)));
}

// ---- proofs that should NOT succeed (the honesty half) -----------------

#[test]
fn two_full_range_i8_params_summed_is_not_proven_in_range() {
    // i8 + i8 can reach 254 (127 + 127), which overflows i8's range --
    // this must NOT be claimed as proven, since it genuinely isn't safe
    // for all possible inputs.
    let program = parse(
        r#"
        fn add(a: i8, b: i8) -> i8 {
            let x: i8 = a + b
            return x
        }
        fn main() -> i64 {
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    let add_fn = program.fns.iter().find(|f| f.name == "add").unwrap();
    let let_span = match &add_fn.body.stmts[0] {
        Stmt::Let { span, .. } => *span,
        _ => panic!("expected a let"),
    };
    assert!(
        !report.proven_in_range.contains(&let_span),
        "i8 + i8 can overflow i8 -- must not be claimed as proven safe"
    );
}

#[test]
fn division_by_an_unconstrained_parameter_is_not_proven_nonzero() {
    let program = parse(
        r#"
        fn divide(a: i64, b: i64) -> i64 {
            let x: i64 = a / b
            return x
        }
        fn main() -> i64 {
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    let divide_fn = program.fns.iter().find(|f| f.name == "divide").unwrap();
    let div_span = {
        use nirdosha::ast::{BinOp, Expr};
        match &divide_fn.body.stmts[0] {
            Stmt::Let { value: Expr::Binary(BinOp::Div, _, _, span), .. } => *span,
            _ => panic!("expected a division"),
        }
    };
    assert!(
        !report.proven_nonzero_divisor.contains(&div_span),
        "b could be zero -- must not be claimed as proven nonzero"
    );
}

#[test]
fn factorial_multiplication_is_not_proven_in_range() {
    // The realistic case: factorial's recursive multiplication genuinely
    // can overflow for large enough n, and this pass has no
    // interprocedural summary for the recursive call's result -- that
    // specific `return` must stay honestly unproven, not falsely claimed
    // safe. NOTE: this is deliberately *not* "nothing in factorial is
    // proven" -- an earlier version of this test asserted exactly that,
    // and it broke, correctly, the moment `refine.rs` gained the ability
    // to prove `return` sites at all: `return 1` in the `n <= 1` branch
    // is genuinely, trivially safe (1 always fits `i64`), and now
    // correctly gets proven. That's a real improvement surfacing a test
    // that was accidentally over-broad, not a regression to work around.
    let program = parse(include_str!("../examples/factorial.nir"));
    let report = analyze(&program);
    let factorial_fn = program.fns.iter().find(|f| f.name == "factorial").unwrap();
    // The `n * factorial(n - 1)` return lives inside the `else` branch.
    let mul_return_span = {
        use nirdosha::ast::Expr;
        fn find_mul_return(stmts: &[Stmt]) -> Option<nirdosha::token::Span> {
            stmts.iter().find_map(|s| match s {
                Stmt::Return { value: Some(Expr::Binary(..)), span } => Some(*span),
                Stmt::Expr(Expr::If { then_block, else_block, .. }) => {
                    find_mul_return(&then_block.stmts).or_else(|| match else_block.as_deref() {
                        Some(nirdosha::ast::ElseBranch::Block(b)) => find_mul_return(&b.stmts),
                        _ => None,
                    })
                }
                _ => None,
            })
        }
        find_mul_return(&factorial_fn.body.stmts).expect("test setup: expected to find the multiplying return")
    };
    assert!(
        !report.proven_in_range.contains(&mul_return_span),
        "factorial's multiplication genuinely can overflow -- this specific return must not be \
         claimed as proven safe"
    );
}

// ---- loops: the documented precision cost -------------------------------

#[test]
fn arithmetic_before_a_loop_is_still_proven() {
    let program = parse(
        r#"
        fn main() -> i64 {
            let x: i8 = 10 + 5
            let n: i64 = 0
            while n < 3 {
                n = n + 1
            }
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    assert!(report.proven_in_range.contains(&nth_let_span(&program, 0)));
}

#[test]
fn arithmetic_on_a_loop_reassigned_variable_after_the_loop_is_not_proven() {
    // `n` is widened to "unknown" on loop entry (module doc) because the
    // body reassigns it -- so even after the loop, further arithmetic on
    // `n` can't be proven to fit a narrow type. This is the real,
    // documented precision cost of not doing full fixed-point iteration.
    let program = parse(
        r#"
        fn main() -> i64 {
            let n: i64 = 0
            while n < 3 {
                n = n + 1
            }
            let y: i8 = n
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    assert!(
        !report.proven_in_range.contains(&nth_let_span(&program, 1)),
        "n was widened to unknown by the loop -- assigning it to an i8 must not be proven safe"
    );
}

// ---- the i64 vacuous-truth bug, pinned directly ------------------------

#[test]
fn two_unconstrained_i64_params_multiplied_is_not_proven_in_range() {
    // The direct regression test for the bug `Stmt::Return` support
    // exposed (see `Interval`'s struct doc in refine.rs for the full
    // story): with `i64`-backed bounds, `Interval::unknown()` was
    // *exactly* `i64`'s own legal range, so checking "is this in range
    // for i64" was vacuously true for every interval -- this pass could
    // never actually catch a genuine i64-level overflow, only ever
    // (wrongly) prove one safe. Two unconstrained i64 parameters
    // multiplied together can very obviously overflow i64 in reality;
    // this must not be claimed as proven, and before the i128 fix, it
    // was.
    let program = parse(
        r#"
        fn multiply(a: i64, b: i64) -> i64 {
            let c: i64 = a * b
            return c
        }
        fn main() -> i64 {
            return 0
        }
    "#,
    );
    let report = analyze(&program);
    let multiply_fn = program.fns.iter().find(|f| f.name == "multiply").unwrap();
    let let_span = match &multiply_fn.body.stmts[0] {
        Stmt::Let { span, .. } => *span,
        _ => panic!("expected a let"),
    };
    assert!(
        !report.proven_in_range.contains(&let_span),
        "two unconstrained i64 params multiplied can genuinely overflow i64 -- must not be proven safe"
    );
}

// ---- doesn't panic on any real example program --------------------------

#[test]
fn analyze_does_not_panic_on_any_example() {
    for src in [
        include_str!("../examples/hello.nir"),
        include_str!("../examples/factorial.nir"),
        include_str!("../examples/loop.nir"),
        include_str!("../examples/ownership.nir"),
        include_str!("../examples/borrow.nir"),
        include_str!("../examples/threads.nir"),
        include_str!("../examples/channels.nir"),
        include_str!("../examples/sandbox.nir"),
        include_str!("../examples/sandbox_channels.nir"),
    ] {
        let program = parse(src);
        let _ = analyze(&program); // just must not panic
    }
}
