//! Tests for Phase 2's dense linear algebra builtins. One correctness
//! test per builtin (textbook-checkable values), plus the shape failures
//! the unified plan's §4.2.1 calls for (`NotSquare`) and the fragment-
//! validation-adjacent naming-collision check (§4.0.2's builtin
//! registry now has real names a user function could accidentally
//! reuse).

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

#[test]
fn example_linalg_runs_to_completion() {
    let src = include_str!("../../examples/linalg.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

#[test]
fn transpose_swaps_rows_and_columns() {
    let src = r#"
        fn main() -> Matrix(f64, 3, 2) {
            let m: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            return transpose(m)
        }
    "#;
    assert_eq!(run(src), Ok(mat_f64(&[1.0, 4.0, 2.0, 5.0, 3.0, 6.0], 3, 2)));
}

#[test]
fn dot_computes_the_inner_product() {
    let src = r#"
        fn main() -> f64 {
            let a: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let b: Vector(f64, 3) = [4.0, 5.0, 6.0]
            return dot(a, b)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(32.0)));
}

#[test]
fn cross_computes_the_cross_product() {
    let src = r#"
        fn main() -> Vector(f64, 3) {
            let a: Vector(f64, 3) = [1.0, 0.0, 0.0]
            let b: Vector(f64, 3) = [0.0, 1.0, 0.0]
            return cross(a, b)
        }
    "#;
    assert_eq!(run(src), Ok(vec_f64(&[0.0, 0.0, 1.0])));
}

#[test]
fn cross_requires_three_element_vectors() {
    let kind = first_type_error(
        r#"
        fn main() {
            let a: Vector(f64, 2) = [1.0, 2.0]
            let b: Vector(f64, 2) = [3.0, 4.0]
            let c: Vector(f64, 2) = cross(a, b)
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::WrongBuiltinArgType {
            builtin: "cross".to_string(),
            expected: "a Vector(_, 3)".to_string(),
            found: Ty::Vector(Box::new(Ty::F64), 2),
        }
    );
}

#[test]
fn zeros_ones_identity_produce_the_right_shapes() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 3) {
            let z: Vector(f64, 3) = zeros(3)
            let o: Matrix(f64, 2, 2) = ones(2, 2)
            let id: Matrix(f64, 3, 3) = identity(3)
            return zeros(2, 3)
        }
    "#;
    assert_eq!(run(src), Ok(mat_f64(&[0.0; 6], 2, 3)));
}

#[test]
fn zeros_requires_a_literal_dimension() {
    let kind = first_type_error(
        r#"
        fn main() {
            let n: i64 = 3
            let z: Vector(f64, 3) = zeros(n)
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedLiteralDimension { builtin: "zeros".to_string() });
}

#[test]
fn sum_and_len_and_norm() {
    let src = r#"
        fn main() -> f64 {
            let v: Vector(f64, 3) = [3.0, 4.0, 0.0]
            let n: i64 = len(v)
            let s: f64 = sum(v)
            return norm(v)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(5.0)));
}

#[test]
fn norm1_and_norm_inf() {
    let src = r#"
        fn main() -> f64 {
            let v: Vector(f64, 3) = [1.0, -2.0, 3.0]
            let a: f64 = norm1(v)
            return norm_inf(v)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(3.0)));
}

#[test]
fn frobenius_norm_of_a_matrix() {
    let src = r#"
        fn main() -> f64 {
            let m: Matrix(f64, 2, 2) = [[3.0, 0.0], [0.0, 4.0]]
            return frobenius_norm(m)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(5.0)));
}

#[test]
fn trace_sums_the_diagonal() {
    let src = r#"
        fn main() -> f64 {
            let m: Matrix(f64, 3, 3) = [[1.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 3.0]]
            return trace(m)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(6.0)));
}

#[test]
fn trace_requires_a_square_matrix() {
    let kind = first_type_error(
        r#"
        fn main() {
            let m: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            let t: f64 = trace(m)
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NotSquare { found: Ty::Matrix(Box::new(Ty::F64), 2, 3) });
}

#[test]
fn det_of_a_known_matrix() {
    let src = r#"
        fn main() -> f64 {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            return det(m)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(-2.0)));
}

#[test]
fn det_requires_a_square_matrix() {
    let kind = first_type_error(
        r#"
        fn main() {
            let m: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            let d: f64 = det(m)
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NotSquare { found: Ty::Matrix(Box::new(Ty::F64), 2, 3) });
}

#[test]
fn inv_of_a_known_matrix_round_trips_via_multiplication() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 2) {
            let m: Matrix(f64, 2, 2) = [[4.0, 7.0], [2.0, 6.0]]
            return m * inv(m)
        }
    "#;
    match run(src) {
        Ok(Value::Matrix(elems, 2, 2)) => {
            let vals: Vec<f64> = elems.iter().map(|v| match v {
                Value::Float(f) => *f,
                other => panic!("expected Float, got {other:?}"),
            }).collect();
            // m * inv(m) should be the identity, up to floating-point error.
            let expected = [1.0, 0.0, 0.0, 1.0];
            for (v, e) in vals.iter().zip(expected.iter()) {
                assert!((v - e).abs() < 1e-9, "expected {e}, got {v}");
            }
        }
        other => panic!("expected a 2x2 Matrix, got {other:?}"),
    }
}

#[test]
fn inv_of_a_singular_matrix_is_a_runtime_error() {
    let src = r#"
        fn main() -> Matrix(f64, 2, 2) {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [2.0, 4.0]]
            return inv(m)
        }
    "#;
    match run(src) {
        Err(msg) => assert!(msg.contains("singular"), "expected a singular-matrix error, got: {msg}"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn solve_computes_the_right_answer() {
    let src = r#"
        fn main() -> Vector(f64, 2) {
            let a: Matrix(f64, 2, 2) = [[2.0, 0.0], [0.0, 3.0]]
            let b: Vector(f64, 2) = [4.0, 9.0]
            return solve(a, b)
        }
    "#;
    assert_eq!(run(src), Ok(vec_f64(&[2.0, 3.0])));
}

#[test]
fn solve_of_a_singular_matrix_is_a_runtime_error() {
    let src = r#"
        fn main() -> Vector(f64, 2) {
            let a: Matrix(f64, 2, 2) = [[1.0, 2.0], [2.0, 4.0]]
            let b: Vector(f64, 2) = [1.0, 2.0]
            return solve(a, b)
        }
    "#;
    match run(src) {
        Err(msg) => assert!(msg.contains("singular"), "expected a singular-matrix error, got: {msg}"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

#[test]
fn rank_of_a_rank_deficient_matrix() {
    let src = r#"
        fn main() -> i64 {
            let m: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [2.0, 4.0, 6.0]]
            return rank(m)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(1)));
}

#[test]
fn is_square_is_shape_only() {
    let src = r#"
        fn main() -> bool {
            let m: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            return is_square(m)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(false)));
}

#[test]
fn is_symmetric_checks_values() {
    let src = r#"
        fn main() -> bool {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 1.0]]
            return is_symmetric(m)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(false)));
}

#[test]
fn is_diag_checks_off_diagonal_zeros() {
    let src = r#"
        fn main() -> bool {
            let m: Matrix(f64, 2, 2) = [[5.0, 0.0], [0.0, 9.0]]
            return is_diag(m)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

// ---- naming collisions (real bug found and fixed while building this) -----

#[test]
fn a_user_function_cannot_shadow_a_builtin_name() {
    let kind = first_type_error(
        r#"
        fn identity(x: f64) -> f64 {
            return x
        }
        fn main() -> f64 {
            return 1.0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::FnNameShadowsBuiltin("identity".to_string()));
}

#[test]
fn spawning_a_linalg_builtin_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let v: Vector(f64, 2) = [1.0, 2.0]
            let w: Vector(f64, 2) = [3.0, 4.0]
            let t: thread f64 = spawn dot(v, w)
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::CannotSpawnBuiltin { name: "dot".to_string() });
}

// Every dense-linalg builtin, including the once-data-dependent-control-
// flow ones (`det`/`inv`/`solve`/`rank`/`kf_update_state`/
// `kf_update_cov` — Phase 5's linked-runtime-call codegen), is now
// codegen-supported: Phase 4 flipped every shape-driven builtin (`sum`,
// `dot`, `transpose`, ...) to a positive compile-and-run assertion in
// `compiler/tests/codegen.rs`, and Phase 5 did the same there for the
// remaining six. Nothing in the dense-linalg builtin surface is still
// rejected by codegen, so there is no rejection test left to keep here.
