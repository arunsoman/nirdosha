//! Tests for `f64` -- literals, arithmetic, comparisons, negation, and
//! function parameter/return positions. See `Ty::F64`'s doc comment in
//! ast.rs: one float width, no literal-widening story the way integers
//! have one, no int<->float conversion (no cast operator exists yet).

use nirdosha::ast::Ty;
use nirdosha::interpreter::Value;
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

// ---- the example, run end to end ----------------------------------------

#[test]
fn example_floats_runs_to_completion() {
    let src = include_str!("../examples/floats.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- arithmetic -----------------------------------------------------------

#[test]
fn float_arithmetic_matches_ieee_754() {
    let src = r#"
        fn main() -> f64 {
            let a: f64 = 7.0
            let b: f64 = 2.0
            return a / b
        }
    "#;
    match run(src) {
        Ok(Value::Float(f)) => assert_eq!(f, 3.5),
        other => panic!("expected Ok(Float(3.5)), got {other:?}"),
    }
}

#[test]
fn dividing_a_float_by_zero_saturates_instead_of_trapping() {
    // Unlike `Value::Int`'s `DivByZero` runtime error, float division by
    // zero is not an error at all -- it's IEEE 754 `inf`, the semantics
    // `Ty::F64`'s doc comment commits to.
    let src = r#"
        fn main() -> f64 {
            let a: f64 = 1.0
            let b: f64 = 0.0
            return a / b
        }
    "#;
    match run(src) {
        Ok(Value::Float(f)) => assert!(f.is_infinite() && f > 0.0),
        other => panic!("expected Ok(Float(inf)), got {other:?}"),
    }
}

#[test]
fn negating_a_float_works() {
    let src = r#"
        fn main() -> f64 {
            let a: f64 = 2.5
            return -a
        }
    "#;
    match run(src) {
        Ok(Value::Float(f)) => assert_eq!(f, -2.5),
        other => panic!("expected Ok(Float(-2.5)), got {other:?}"),
    }
}

#[test]
fn float_comparisons_work() {
    let src = r#"
        fn main() -> bool {
            let a: f64 = 1.5
            let b: f64 = 2.5
            return a < b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

// ---- passing floats through functions --------------------------------------

#[test]
fn floats_pass_through_function_parameters_and_returns_unchanged() {
    let src = r#"
        fn pass_through(x: f64) -> f64 {
            return x
        }
        fn main() -> f64 {
            return pass_through(9.25)
        }
    "#;
    match run(src) {
        Ok(Value::Float(f)) => assert_eq!(f, 9.25),
        other => panic!("expected Ok(Float(9.25)), got {other:?}"),
    }
}

// ---- static rejections ----------------------------------------------------

#[test]
fn mixing_an_int_literal_with_a_float_operand_is_rejected_statically() {
    // No implicit int-literal-to-float widening (see `Ty::F64`'s doc
    // comment): `1 + 2.0` is a type mismatch, not an automatic promotion,
    // the same "no implicit conversions" rule int-vs-int widths already
    // gets. `2.0`'s type has to be `F64` for this test to be checking
    // what it claims -- pinned by the `TypeMismatch` shape below, not
    // just "some error happened".
    let kind = first_type_error(
        r#"
        fn main() -> f64 {
            let a: f64 = 2.0
            return 1 + a
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::F64, found: Ty::I64 });
}

#[test]
fn assigning_an_int_literal_to_a_declared_float_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let a: f64 = 1
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::F64, found: Ty::I64 });
}

#[test]
fn indexing_is_rejected_statically_no_matter_the_base_type() {
    // Grammar exists (`v[i]`), but no indexable type does yet -- see
    // `TypeErrorKind::NotIndexable`'s doc comment. Pinned against a
    // float-typed base specifically since this file is where `f64`
    // itself is exercised, not because indexing is float-specific.
    let kind = first_type_error(
        r#"
        fn main() {
            let a: f64 = 1.0
            let b: f64 = a[0]
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NotIndexable { found: Ty::F64 });
}
