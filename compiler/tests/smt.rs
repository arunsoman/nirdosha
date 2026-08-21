//! Tests for `smt.rs` — the real Z3-backed Tier-1 pass (goal.md row 4).
//! Mirrors `tests/refine.rs`'s shape (same honesty principle: several
//! tests confirm the pass *doesn't* over-claim, not just that it
//! succeeds), plus one flagship test that demonstrates the actual reason
//! this pass exists alongside `refine.rs` rather than replacing it in
//! spirit: condition-based narrowing that interval analysis structurally
//! cannot do, checked here by running the *same* program through both
//! passes and confirming they disagree in exactly the expected direction.

use nirdosha::ast::{Program, Stmt};
use nirdosha::parser::Parser;
use nirdosha::smt::analyze;
use nirdosha::token::Lexer;

fn parse(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

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
fn a_tracked_variable_carries_its_precise_value_forward() {
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

// ---- the flagship test: what SMT buys over interval analysis -----------

#[test]
fn condition_narrowing_proves_what_interval_analysis_cannot() {
    // `n` is a plain `i64` parameter -- its own declared range spans far
    // beyond `i8`. Only *inside* the branch where `n >= 0 && n <= 100`
    // holds is `let x: i8 = n` actually safe, and proving that requires
    // asserting the branch condition into the solver -- exactly what
    // `smt.rs` does and `refine.rs` structurally cannot (see both
    // modules' doc comments). Same source, both passes, opposite
    // verdicts -- that's the real, checked claim, not just an assertion
    // in a comment.
    let src = r#"
        fn classify(n: i64) -> i64 {
            if n >= 0 && n <= 100 {
                let x: i8 = n
                return 0
            }
            return -1
        }
        fn main() -> i64 {
            return classify(5)
        }
    "#;
    let program = parse(src);

    let smt_report = nirdosha::smt::analyze(&program);
    let refine_report = nirdosha::refine::analyze(&program);

    let classify_fn = program.fns.iter().find(|f| f.name == "classify").unwrap();
    let let_span = {
        use nirdosha::ast::Expr;
        fn find_let_in_if(stmts: &[Stmt]) -> Option<nirdosha::token::Span> {
            for s in stmts {
                if let Stmt::Expr(Expr::If { then_block, .. }) = s {
                    for inner in &then_block.stmts {
                        if let Stmt::Let { span, .. } = inner {
                            return Some(*span);
                        }
                    }
                }
            }
            None
        }
        find_let_in_if(&classify_fn.body.stmts).expect("test setup: expected a let inside the if")
    };

    assert!(
        smt_report.proven_in_range.contains(&let_span),
        "smt.rs should prove `x: i8 = n` safe using the branch condition `n >= 0 && n <= 100`"
    );
    assert!(
        !refine_report.proven_in_range.contains(&let_span),
        "refine.rs has no condition-narrowing -- it should NOT be able to prove this \
         (if it can, either it grew narrowing, or this test's premise is stale -- \
         either way, worth an explicit look, not a silent pass)"
    );
}

// ---- proofs that should NOT succeed (the honesty half) -----------------

#[test]
fn two_full_range_i8_params_summed_is_not_proven_in_range() {
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
    // NOTE: deliberately checks the *specific* multiplying `return`, not
    // "nothing in factorial is proven" -- an earlier, broader version of
    // this assertion broke, correctly, the moment `smt.rs` gained the
    // ability to prove `return` sites at all: `return 1` in the `n <= 1`
    // branch is genuinely, trivially safe (1 always fits `i64`) and now
    // correctly gets proven. A real improvement surfacing an
    // accidentally-over-broad test, not a regression.
    let program = parse(include_str!("../examples/factorial.nir"));
    let report = analyze(&program);
    let factorial_fn = program.fns.iter().find(|f| f.name == "factorial").unwrap();
    let mul_return_span = {
        use nirdosha::ast::{Expr, Stmt};
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

// ---- loops: same documented precision cost as refine.rs -----------------

#[test]
fn arithmetic_on_a_loop_reassigned_variable_after_the_loop_is_not_proven() {
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
        "n was widened to unconstrained by the loop -- assigning it to an i8 must not be proven safe"
    );
}

// ---- doesn't panic/hang on any real example program ----------------------

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
        include_str!("../examples/strings.nir"),
        include_str!("../examples/tcp_client.nir"),
        include_str!("../examples/floats.nir"),
        include_str!("../examples/matrices.nir"),
        include_str!("../examples/linalg.nir"),
        include_str!("../examples/sensor_fusion.nir"),
        include_str!("../examples/wargame_agents.nir"),
    ] {
        let program = parse(src);
        let _ = analyze(&program); // just must not panic or hang
    }
}

// ---- Phase 5: SMT-proven index bounds (unified plan §4.5.1) ---------------

fn nth_index_span(program: &Program, n: usize) -> nirdosha::token::Span {
    use nirdosha::ast::Expr;
    fn walk(e: &Expr, out: &mut Vec<nirdosha::token::Span>) {
        if let Expr::Index(base, indices, span) = e {
            walk(base, out);
            for i in indices {
                walk(i, out);
            }
            out.push(*span);
            return;
        }
        match e {
            Expr::Binary(_, l, r, _) => {
                walk(l, out);
                walk(r, out);
            }
            Expr::Unary(_, inner, _) | Expr::Assign(_, inner, _) => walk(inner, out),
            Expr::ArrayLit(elements, _) => {
                for el in elements {
                    walk(el, out);
                }
            }
            _ => {}
        }
    }
    let main = program.fns.iter().find(|f| f.name == "main").expect("no main");
    let mut out = Vec::new();
    for s in &main.body.stmts {
        if let Stmt::Let { value, .. } = s {
            walk(value, &mut out);
        }
    }
    out.into_iter().nth(n).expect("not enough index sites")
}

#[test]
fn a_literal_index_within_bounds_is_proven() {
    let program = parse(
        r#"
        fn main() {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let x: f64 = v[1]
        }
    "#,
    );
    let report = analyze(&program);
    assert!(report.proven_index_bounds.contains(&nth_index_span(&program, 0)));
}

#[test]
fn a_literal_index_out_of_bounds_is_not_proven() {
    let program = parse(
        r#"
        fn main() {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let x: f64 = v[5]
        }
    "#,
    );
    let report = analyze(&program);
    assert!(!report.proven_index_bounds.contains(&nth_index_span(&program, 0)));
}

#[test]
fn an_unconstrained_index_is_not_proven() {
    let program = parse(
        r#"
        fn main() {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let n: i64 = 10
            let x: f64 = v[n]
        }
    "#,
    );
    let report = analyze(&program);
    assert!(!report.proven_index_bounds.contains(&nth_index_span(&program, 0)));
}

/// Z3 can discharge a proof interval analysis (`refine.rs`) genuinely
/// can't: an index narrowed by an `if` condition, not just a literal or
/// a straight-line computation. This is the concrete case that actually
/// justifies having both passes rather than just the cheaper one.
#[test]
fn a_condition_narrowed_index_is_proven_by_smt_specifically() {
    let program = parse(
        r#"
        fn main() {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let n: i64 = 10
            if n < 3 {
                let x: f64 = v[n]
            }
        }
    "#,
    );
    let report = analyze(&program);
    use nirdosha::ast::{Expr, Stmt};
    let main = program.fns.iter().find(|f| f.name == "main").unwrap();
    let Stmt::Expr(Expr::If { then_block, .. }) = &main.body.stmts[2] else {
        panic!("expected the if-statement");
    };
    let Stmt::Let { value: Expr::Index(_, _, span), .. } = &then_block.stmts[0] else {
        panic!("expected the index let");
    };
    assert!(report.proven_index_bounds.contains(span));
}
