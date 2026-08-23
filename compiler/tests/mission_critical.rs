//! Tests for Phase 3 (Mission-Critical Runtime): deterministic RNG,
//! WGS84 geometry, the linear Kalman filter, the TCP server primitive
//! (`listen`/`accept`), and the `audited` Tier-3 escape hatch.

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

// ---- examples, run end to end -----------------------------------------

#[test]
fn example_sensor_fusion_runs_to_completion() {
    let src = include_str!("../../examples/sensor_fusion.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

#[test]
fn example_wargame_agents_runs_to_completion() {
    let src = include_str!("../../examples/wargame_agents.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- deterministic RNG --------------------------------------------------

#[test]
fn same_seed_produces_the_same_sequence() {
    let src = r#"
        fn main() -> f64 {
            rand_seed(1234)
            let a: f64 = rand_f64()
            rand_seed(1234)
            let b: f64 = rand_f64()
            return a - b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(0.0)));
}

#[test]
fn different_seeds_produce_different_values() {
    let src = r#"
        fn main() -> bool {
            rand_seed(1)
            let a: f64 = rand_f64()
            rand_seed(2)
            let b: f64 = rand_f64()
            return a != b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn rand_f64_is_in_zero_one_range() {
    let src = r#"
        fn main() -> bool {
            rand_seed(99)
            let a: f64 = rand_f64()
            return a >= 0.0
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn rand_before_seed_is_a_runtime_error() {
    let src = r#"
        fn main() -> f64 {
            return rand_f64()
        }
    "#;
    match run(src) {
        Err(msg) => assert!(msg.contains("rand_seed"), "expected an RngNotSeeded error, got: {msg}"),
        other => panic!("expected a runtime error, got {other:?}"),
    }
}

// ---- geometry -------------------------------------------------------------

#[test]
fn lla_to_ecef_at_the_origin() {
    let src = r#"
        fn main() -> Vector(f64, 3) {
            let lla: Vector(f64, 3) = [0.0, 0.0, 0.0]
            return lla_to_ecef(lla)
        }
    "#;
    match run(src) {
        Ok(Value::Vector(v)) => {
            let x = match &v[0] { Value::Float(f) => *f, other => panic!("{other:?}") };
            let y = match &v[1] { Value::Float(f) => *f, other => panic!("{other:?}") };
            let z = match &v[2] { Value::Float(f) => *f, other => panic!("{other:?}") };
            assert!((x - 6_378_137.0).abs() < 1e-6);
            assert!(y.abs() < 1e-6);
            assert!(z.abs() < 1e-6);
        }
        other => panic!("expected a Vector, got {other:?}"),
    }
}

#[test]
fn ecef_to_lla_round_trips_through_lla_to_ecef() {
    let src = r#"
        fn main() -> Vector(f64, 3) {
            let lla: Vector(f64, 3) = [37.7749, -122.4194, 10.0]
            return ecef_to_lla(lla_to_ecef(lla))
        }
    "#;
    match run(src) {
        Ok(Value::Vector(v)) => {
            let lat = match &v[0] { Value::Float(f) => *f, other => panic!("{other:?}") };
            let lon = match &v[1] { Value::Float(f) => *f, other => panic!("{other:?}") };
            let alt = match &v[2] { Value::Float(f) => *f, other => panic!("{other:?}") };
            assert!((lat - 37.7749).abs() < 1e-6);
            assert!((lon - (-122.4194)).abs() < 1e-6);
            assert!((alt - 10.0).abs() < 1e-3);
        }
        other => panic!("expected a Vector, got {other:?}"),
    }
}

#[test]
fn enu_round_trips_through_ecef() {
    let src = r#"
        fn main() -> bool {
            let station: Vector(f64, 3) = [37.7749, -122.4194, 0.0]
            let target: Vector(f64, 3) = [37.8044, -122.2712, 100.0]
            let target_ecef: Vector(f64, 3) = lla_to_ecef(target)
            let enu: Vector(f64, 3) = ecef_to_enu(target_ecef, station)
            let back: Vector(f64, 3) = enu_to_ecef(enu, station)
            return distance(back, target_ecef) < 0.001
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn distance_between_identical_points_is_zero() {
    let src = r#"
        fn main() -> f64 {
            let a: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let b: Vector(f64, 3) = [1.0, 2.0, 3.0]
            return distance(a, b)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Float(0.0)));
}

#[test]
fn bearing_due_north_is_zero() {
    // From the equator directly north along the same meridian.
    let src = r#"
        fn main() -> f64 {
            let from: Vector(f64, 3) = [0.0, 0.0, 0.0]
            let to: Vector(f64, 3) = [10.0, 0.0, 0.0]
            return bearing(from, to)
        }
    "#;
    match run(src) {
        Ok(Value::Float(f)) => assert!(f.abs() < 1e-9, "expected ~0 degrees, got {f}"),
        other => panic!("expected Ok(Float), got {other:?}"),
    }
}

// ---- linear Kalman filter --------------------------------------------------

#[test]
fn kf_predict_with_identity_transition_and_zero_process_noise_is_unchanged() {
    let src = r#"
        fn main() -> Vector(f64, 1) {
            let x: Vector(f64, 1) = [5.0]
            let p: Matrix(f64, 1, 1) = [[2.0]]
            let f: Matrix(f64, 1, 1) = [[1.0]]
            let q: Matrix(f64, 1, 1) = [[0.0]]
            return kf_predict_state(x, p, f, q)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Vector(vec![Value::Float(5.0)].into())));
}

/// `kf_predict_cov` had no direct interpreter test of its own before this
/// (only `kf_predict_state`, above, and indirect coverage via
/// `examples/sensor_fusion.nir`) -- a real, found-not-assumed gap
/// surfaced while adding compiled Vector/Matrix codegen (Phase 4), whose
/// own `kf_predict_state_and_cov_match_interpreter` test
/// (`tests/codegen.rs`) cross-checks the compiled backend against
/// whatever the interpreter does here, so this needs to be pinned
/// directly, not just transitively.
#[test]
fn kf_predict_cov_grows_with_process_noise_and_transition() {
    let src = r#"
        fn main() -> Matrix(f64, 1, 1) {
            let x: Vector(f64, 1) = [5.0]
            let p: Matrix(f64, 1, 1) = [[2.0]]
            let f: Matrix(f64, 1, 1) = [[1.0]]
            let q: Matrix(f64, 1, 1) = [[0.5]]
            return kf_predict_cov(x, p, f, q)
        }
    "#;
    // P' = F P F^T + Q = 1*2*1 + 0.5 = 2.5, for this 1x1 identity-
    // transition case -- simple enough to hand-check exactly, unlike the
    // 4x4 case `tests/codegen.rs`'s own test already cross-checks against
    // the interpreter instead of an independently hand-computed value.
    assert_eq!(run(src), Ok(Value::Matrix(vec![Value::Float(2.5)].into(), 1, 1)));
}

#[test]
fn kf_update_moves_estimate_toward_the_measurement() {
    let src = r#"
        fn main() -> bool {
            let x: Vector(f64, 1) = [0.0]
            let p: Matrix(f64, 1, 1) = [[1.0]]
            let z: Vector(f64, 1) = [10.0]
            let h: Matrix(f64, 1, 1) = [[1.0]]
            let r: Matrix(f64, 1, 1) = [[1.0]]
            let x2: Vector(f64, 1) = kf_update_state(x, p, z, h, r)
            return x2[0] > 0.0
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn kf_dimension_mismatch_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            let x: Vector(f64, 2) = [0.0, 0.0]
            let p: Matrix(f64, 1, 1) = [[1.0]]
            let f: Matrix(f64, 1, 1) = [[1.0]]
            let q: Matrix(f64, 1, 1) = [[1.0]]
            let out: Vector(f64, 2) = kf_predict_state(x, p, f, q)
        }
    "#,
    );
    assert!(matches!(kind, TypeErrorKind::WrongBuiltinArgType { .. }), "expected WrongBuiltinArgType, got {kind:?}");
}

// ---- TCP server (listen/accept) --------------------------------------------

#[test]
fn a_client_can_talk_to_a_listening_server() {
    let src = r#"
        fn server(port: i64, ready: chan i64) {
            let l: tcp_listener = listen(port)
            send(ready, 1)
            let conn: tcp = accept(l)
            let msg: str = recv(conn)
            send(conn, msg)
            stop(conn)
            stop(l)
        }

        struct Text {
            value: str,
        }
        fn main() -> Text {
            let ready: chan i64 = chan
            let t: thread unit = spawn server(19213, ready)
            let go: i64 = recv(ready)
            let c: tcp = connect("127.0.0.1", 19213)
            send(c, "hello over tcp")
            let reply: str = recv(c)
            stop(c)
            join(t)
            return Text(reply)
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "hello over tcp"),
            other => panic!("expected Text(Str(\"hello over tcp\")), got Text({other:?})"),
        },
        other => panic!("expected the echoed string, got {other:?}"),
    }
}

#[test]
fn stopping_a_listener_twice_is_a_static_use_after_move() {
    let src = r#"
        fn main() {
            let l: tcp_listener = listen(19214)
            stop(l)
            stop(l)
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck");
    let err = nirdosha::ownership::check_ownership(&program);
    assert!(err.is_err(), "expected a use-after-move ownership error");
}

#[test]
fn accept_requires_a_tcp_listener() {
    let kind = first_type_error(
        r#"
        fn main() {
            let c: tcp = connect("127.0.0.1", 1)
            let x: tcp = accept(c)
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedTcpListenerType { found: Ty::Tcp });
}

// ---- audited escape hatch ---------------------------------------------------

#[test]
fn audited_block_runs_normally_in_the_interpreter() {
    let src = r#"
        fn main() -> i64 {
            audited "trusted range, proven by an external tool" {
                return 1 + 2
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(3)));
}

#[test]
fn empty_audited_justification_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            audited "" {
                print(1)
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::EmptyAuditedJustification);
}

#[test]
fn whitespace_only_audited_justification_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() {
            audited "   " {
                print(1)
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::EmptyAuditedJustification);
}

#[test]
fn audited_still_typechecks_its_body_normally() {
    let kind = first_type_error(
        r#"
        fn main() {
            audited "justification" {
                let x: i64 = true
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Bool });
}

#[test]
fn a_function_returning_only_from_inside_audited_satisfies_definite_return() {
    // Regression: `audited`'s body is straight-line code that always
    // executes once, unlike `while` -- `definitely_returns` has to see
    // into it or this legitimate function is rejected.
    let src = r#"
        fn f() -> i64 {
            audited "trusted" {
                return 42
            }
        }
        fn main() -> i64 {
            return f()
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(42)));
}

/// Isolates just `@f`'s own generated function body from the full IR --
/// `main`'s unrelated `return f(...)` statement (never wrapped in
/// `audited`) legitimately gets its own guard, so asserting over the
/// *whole* program would conflate the two.
fn function_body_ir<'a>(ir: &'a str, fn_name: &str) -> &'a str {
    let start = ir.find(&format!("@{fn_name}(")).unwrap_or_else(|| panic!("no `@{fn_name}` in IR:\n{ir}"));
    let end = ir[start..].find("\n}\n").map(|i| start + i).unwrap_or(ir.len());
    &ir[start..end]
}

#[test]
fn audited_suppresses_the_codegen_bounds_guard() {
    let src = r#"
        fn f(a: i8, b: i8) -> i8 {
            audited "caller guarantees no overflow" {
                return a + b
            }
        }
        fn main() -> i8 {
            return f(1, 2)
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck");
    nirdosha::ownership::check_ownership(&program).expect("should pass ownership checking");
    let smt_report = nirdosha::smt::analyze(&program);
    let ir = nirdosha::codegen::emit_llvm_ir(&program, &smt_report).expect("should codegen");
    let f_body = function_body_ir(&ir, "f");
    assert!(!f_body.contains("range_trap"), "expected no bounds-check trap inside `f`'s audited body, got:\n{f_body}");
}

#[test]
fn without_audited_the_same_arithmetic_still_gets_a_guard() {
    let src = r#"
        fn f(a: i8, b: i8) -> i8 {
            return a + b
        }
        fn main() -> i8 {
            return f(1, 2)
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck");
    nirdosha::ownership::check_ownership(&program).expect("should pass ownership checking");
    let smt_report = nirdosha::smt::analyze(&program);
    let ir = nirdosha::codegen::emit_llvm_ir(&program, &smt_report).expect("should codegen");
    let f_body = function_body_ir(&ir, "f");
    assert!(f_body.contains("range_trap"), "expected a bounds-check trap inside `f` without `audited`, got:\n{f_body}");
}
