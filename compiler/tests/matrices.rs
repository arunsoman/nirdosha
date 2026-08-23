//! Tests for `Vector`/`Matrix` -- literals, runtime representation,
//! indexing, elementwise `+`/`-`, Hadamard `.*`/`./`, linear-algebra `*`
//! (scalar×matrix, matrix×vector, matrix×matrix), and the static
//! rejections the unified plan's §4.1.6 calls for: shape mismatch,
//! `Vector * Vector`, out-of-bounds index.

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

fn vec_f64(vals: &[f64]) -> Value {
    Value::Vector(vals.iter().map(|v| Value::Float(*v)).collect())
}

fn mat_f64(vals: &[f64], rows: usize, cols: usize) -> Value {
    Value::Matrix(vals.iter().map(|v| Value::Float(*v)).collect(), rows, cols)
}

// ---- the example, run end to end ----------------------------------------

#[test]
fn example_matrices_runs_to_completion() {
    let src = include_str!("../../examples/matrices.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- literals and runtime representation -----------------------------------

#[test]
fn a_vector_literal_produces_the_right_values() {
    let src = r#"
        fn main() -> Vector(f64, 3) {
            return [1.0, 2.0, 3.0]
        }
    "#;
    assert_eq!(run(src), Ok(vec_f64(&[1.0, 2.0, 3.0])));
}

#[test]
fn a_matrix_literal_flattens_row_major() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 2) {
            return [[1.0, 2.0], [3.0, 4.0]]
        }
    "#;
    assert_eq!(run(src), Ok(mat_f64(&[1.0, 2.0, 3.0, 4.0], 2, 2)));
}

#[test]
fn different_lengths_are_different_types() {
    // "Sized by Default" (unified plan §2): Vector(f64, 2) and
    // Vector(f64, 3) are different types, so assigning one to the other
    // is a plain TypeMismatch, not a runtime shape check.
    let kind = first_type_error(
        r#"
        fn main() {
            let a: Vector(f64, 2) = [1.0, 2.0]
            let b: Vector(f64, 3) = a
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::TypeMismatch {
            expected: Ty::Vector(Box::new(Ty::F64), 3),
            found: Ty::Vector(Box::new(Ty::F64), 2),
        }
    );
}

// ---- indexing ---------------------------------------------------------------

#[test]
fn vector_indexing_returns_the_right_element() {
    let src = r#"
        fn main() -> f64 {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            return v[1]
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(2.0)));
}

#[test]
fn matrix_indexing_returns_the_right_element() {
    let src = r#"
        fn main() -> f64 {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            return m[1, 0]
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(3.0)));
}

#[test]
fn out_of_bounds_vector_index_is_a_runtime_error() {
    let src = r#"
        fn main() -> f64 {
            let v: Vector(f64, 2) = [1.0, 2.0]
            return v[5]
        }
    "#;
    match run(src) {
        Err(msg) => assert!(msg.contains("index"), "expected an index-out-of-bounds error, got: {msg}"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn negative_vector_index_is_a_runtime_error() {
    let src = r#"
        fn main() -> f64 {
            let v: Vector(f64, 2) = [1.0, 2.0]
            let i: i64 = 0
            return v[i - 1]
        }
    "#;
    match run(src) {
        Err(msg) => assert!(msg.contains("index"), "expected an index-out-of-bounds error, got: {msg}"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn vector_indexed_with_two_indices_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let v: Vector(f64, 2) = [1.0, 2.0]
            let x: f64 = v[0, 1]
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::WrongIndexArity { expected: 1, found: 2 });
}

#[test]
fn matrix_indexed_with_one_index_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            let x: f64 = m[0]
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::WrongIndexArity { expected: 2, found: 1 });
}

#[test]
fn indexing_a_scalar_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let a: f64 = 1.0
            let x: f64 = a[0]
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NotIndexable { found: Ty::F64 });
}

// ---- elementwise +/- and Hadamard .*/./ --------------------------------------

#[test]
fn vector_addition_is_elementwise() {
    let src = r#"
        fn main() -> Vector(f64, 3) {
            let a: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let b: Vector(f64, 3) = [10.0, 20.0, 30.0]
            return a + b
        }
    "#;
    assert_eq!(run(src), Ok(vec_f64(&[11.0, 22.0, 33.0])));
}

#[test]
fn matrix_subtraction_is_elementwise() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 2) {
            let a: Matrix(f64, 2, 2) = [[5.0, 6.0], [7.0, 8.0]]
            let b: Matrix(f64, 2, 2) = [[1.0, 1.0], [1.0, 1.0]]
            return a - b
        }
    "#;
    assert_eq!(run(src), Ok(mat_f64(&[4.0, 5.0, 6.0, 7.0], 2, 2)));
}

#[test]
fn hadamard_multiply_is_elementwise() {
    let src = r#"
        fn main() -> Vector(f64, 3) {
            let a: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let b: Vector(f64, 3) = [10.0, 20.0, 30.0]
            return a .* b
        }
    "#;
    assert_eq!(run(src), Ok(vec_f64(&[10.0, 40.0, 90.0])));
}

#[test]
fn mismatched_vector_shapes_are_rejected_statically_for_addition() {
    let kind = first_type_error(
        r#"
        fn main() {
            let a: Vector(f64, 1) = [1.0]
            let b: Vector(f64, 2) = [1.0, 2.0]
            let c: Vector(f64, 1) = a + b
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::TypeMismatch {
            expected: Ty::Vector(Box::new(Ty::F64), 1),
            found: Ty::Vector(Box::new(Ty::F64), 2),
        }
    );
}

// ---- linear-algebra `*` ------------------------------------------------------

#[test]
fn scalar_times_matrix_scales_every_element() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 2) {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            return 2.0 * m
        }
    "#;
    assert_eq!(run(src), Ok(mat_f64(&[2.0, 4.0, 6.0, 8.0], 2, 2)));
}

#[test]
fn matrix_times_scalar_scales_every_element() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 2) {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            return m * 2.0
        }
    "#;
    assert_eq!(run(src), Ok(mat_f64(&[2.0, 4.0, 6.0, 8.0], 2, 2)));
}

#[test]
fn matrix_times_vector_computes_the_right_result() {
    let src = r#"
        fn main() -> Vector(f64, 2) {
            let m: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            return m * v
        }
    "#;
    // [1*1+2*2+3*3, 4*1+5*2+6*3] = [14, 32]
    assert_eq!(run(src), Ok(vec_f64(&[14.0, 32.0])));
}

#[test]
fn matrix_times_matrix_computes_the_right_result() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 2) {
            let a: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            let b: Matrix(f64, 3, 2) = [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0]]
            return a * b
        }
    "#;
    assert_eq!(run(src), Ok(mat_f64(&[22.0, 28.0, 49.0, 64.0], 2, 2)));
}

#[test]
fn mismatched_inner_dimensions_are_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let a: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            let b: Matrix(f64, 4, 2) = [[1.0, 2.0], [3.0, 4.0], [5.0, 6.0], [7.0, 8.0]]
            let c: Matrix(f64, 2, 2) = a * b
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::ShapeMismatch {
            left: Ty::Matrix(Box::new(Ty::F64), 2, 3),
            right: Ty::Matrix(Box::new(Ty::F64), 4, 2),
        }
    );
}

#[test]
fn vector_times_vector_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let a: Vector(f64, 2) = [1.0, 2.0]
            let b: Vector(f64, 2) = [3.0, 4.0]
            let c: f64 = a * b
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::VectorTimesVectorNotSupported);
}

// ---- array-literal shape validation ------------------------------------------

#[test]
fn ragged_matrix_rows_are_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let bad: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0]]
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::TypeMismatch {
            expected: Ty::Vector(Box::new(Ty::F64), 2),
            found: Ty::Vector(Box::new(Ty::F64), 1),
        }
    );
}

#[test]
fn heterogeneous_vector_elements_are_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let bad: Vector(f64, 2) = [1.0, true]
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::F64, found: Ty::Bool });
}

#[test]
fn three_levels_of_nesting_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let bad: Vector(f64, 1) = [[1.0]]
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Vector(Box::new(Ty::F64), 1), found: Ty::Matrix(Box::new(Ty::F64), 1, 1) });
}

// ---- equality -----------------------------------------------------------------

#[test]
fn equal_vectors_compare_equal() {
    let src = r#"
        fn main() -> bool {
            let a: Vector(f64, 2) = [1.0, 2.0]
            let b: Vector(f64, 2) = [1.0, 2.0]
            return a == b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

// ---- integer element type (generic over Ty, not f64-only) --------------------

#[test]
fn integer_element_vectors_work_too() {
    let src = r#"
        fn main() -> Vector(i64, 3) {
            let a: Vector(i64, 3) = [1, 2, 3]
            let b: Vector(i64, 3) = [10, 20, 30]
            return a + b
        }
    "#;
    match run(src) {
        Ok(Value::Vector(v)) => {
            let ints: Vec<i64> = v.iter().map(|x| match x {
                Value::Int(n) => *n,
                other => panic!("expected Int, got {other:?}"),
            }).collect();
            assert_eq!(ints, vec![11, 22, 33]);
        }
        other => panic!("expected a Vector, got {other:?}"),
    }
}

#[test]
fn elementwise_integer_division_by_zero_is_a_runtime_error() {
    let src = r#"
        fn main() -> i64 {
            let a: Vector(i64, 2) = [1, 2]
            let b: Vector(i64, 2) = [0, 1]
            let c: Vector(i64, 2) = a ./ b
            return c[0]
        }
    "#;
    match run(src) {
        Err(msg) => assert!(msg.contains("division by zero"), "expected a division-by-zero error, got: {msg}"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

// ---- codegen still honestly rejects Vector/Matrix ----------------------------

#[test]
fn codegen_rejects_main_returning_vector_directly() {
    // Vector/Matrix codegen landed (Vector/Matrix codegen plan, Phase
    // 0+1 -- see `compiler/tests/codegen.rs`'s `vector_*`/`matrix_*`
    // tests for the positive coverage), so `check_supported` alone no
    // longer rejects this program -- the remaining, deliberate boundary
    // is `main` itself returning an aggregate directly: there's no exit
    // code to derive from a Vector/Matrix the way there is from an
    // integer/f64 result, so `codegen::emit_llvm_ir`'s `emit_c_main`
    // step is what rejects it now, not the earlier structural check.
    let src = r#"
        fn main() -> Vector(f64, 2) {
            return [1.0, 2.0]
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck");
    nirdosha::ownership::check_ownership(&program).expect("should pass ownership checking");
    assert!(nirdosha::codegen::check_supported(&program).is_ok());
    let report = nirdosha::smt::analyze(&program);
    assert!(nirdosha::codegen::emit_llvm_ir(&program, &report).is_err());
}
