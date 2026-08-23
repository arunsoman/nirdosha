//! Tests for `struct`/`enum`/`match` — Row 11
//! (`nirdosha_row11_amendment.md`), layer 1: fixed concrete fields, no
//! generics yet (deferred to a later layer). Construction reuses
//! `Expr::Call`, field access is the one genuinely new expression form,
//! and `match` is exhaustive over a closed variant set with no wildcard
//! in v1.

use nirdosha::ast::Ty;
use nirdosha::interpreter::Value;
use nirdosha::ownership::check_ownership;
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

// ---- structs: construction + field access ----------------------------------

#[test]
fn struct_construction_and_field_access_round_trip() {
    let src = r#"
        struct Point {
            x: f64,
            y: f64,
        }
        fn main() -> f64 {
            let p: Point = Point(3.0, 4.0)
            return p.x + p.y
        }
    "#;
    match run(src) {
        Ok(Value::Float(n)) => assert_eq!(n, 7.0),
        other => panic!("expected Ok(Float(7.0)), got {other:?}"),
    }
}

#[test]
fn nested_field_access_through_two_structs() {
    let src = r#"
        struct Point {
            x: f64,
            y: f64,
        }
        struct Line {
            from: Point,
            to: Point,
        }
        fn main() -> f64 {
            let l: Line = Line(Point(0.0, 0.0), Point(3.0, 4.0))
            return l.to.x + l.to.y
        }
    "#;
    match run(src) {
        Ok(Value::Float(n)) => assert_eq!(n, 7.0),
        other => panic!("expected Ok(Float(7.0)), got {other:?}"),
    }
}

#[test]
fn struct_field_bounds_are_checked_like_any_other_boundary() {
    let kind = first_type_error(
        r#"
        struct Small {
            n: i8,
        }
        fn main() -> i64 {
            let s: Small = Small(999)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::LiteralOutOfRange { ty: Ty::I8, value: 999 });
}

#[test]
fn wrong_constructor_arity_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Point {
            x: f64,
            y: f64,
        }
        fn main() -> i64 {
            let p: Point = Point(1.0)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ConstructorArityMismatch { name: "Point".to_string(), want: 2, got: 1 });
}

#[test]
fn accessing_an_unknown_field_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Point {
            x: f64,
            y: f64,
        }
        fn main() -> f64 {
            let p: Point = Point(1.0, 2.0)
            return p.z
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NoSuchField { struct_name: "Point".to_string(), field: "z".to_string() });
}

#[test]
fn field_access_on_a_non_struct_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let n: i64 = 1
            return n.x
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NotAStruct { found: Ty::I64 });
}

// ---- enums: construction + match --------------------------------------------

#[test]
fn match_dispatches_to_the_right_arm_and_binds_the_payload() {
    let src = r#"
        enum Shape {
            Circle(f64),
            Rectangle(f64, f64),
        }
        fn area(s: Shape) -> f64 {
            return match s {
                Circle(r) => 3.0 * r * r,
                Rectangle(w, h) => w * h,
            }
        }
        fn main() -> f64 {
            return area(Rectangle(3.0, 4.0))
        }
    "#;
    match run(src) {
        Ok(Value::Float(n)) => assert_eq!(n, 12.0),
        other => panic!("expected Ok(Float(12.0)), got {other:?}"),
    }
}

#[test]
fn a_zero_payload_variant_still_uses_call_syntax() {
    let src = r#"
        enum Signal {
            Go,
            Stop,
        }
        fn code(s: Signal) -> i64 {
            return match s {
                Go() => 1,
                Stop() => 0,
            }
        }
        fn main() -> i64 {
            return code(Go())
        }
    "#;
    match run(src) {
        Ok(Value::Int(1)) => {}
        other => panic!("expected Ok(Int(1)), got {other:?}"),
    }
}

#[test]
fn match_used_as_a_bare_statement_does_not_need_arm_types_to_agree() {
    let src = r#"
        enum E {
            A,
            B,
        }
        fn main() -> i64 {
            let e: E = A()
            match e {
                A() => print(1),
                B() => 2,
            }
            return 0
        }
    "#;
    match run(src) {
        Ok(Value::Int(0)) => {}
        other => panic!("expected Ok(Int(0)), got {other:?}"),
    }
}

#[test]
fn non_exhaustive_match_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        enum Shape {
            Circle(f64),
            Rectangle(f64, f64),
        }
        fn main() -> f64 {
            let s: Shape = Circle(1.0)
            return match s {
                Circle(r) => r,
            }
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::NonExhaustiveMatch { enum_name: "Shape".to_string(), missing: vec!["Rectangle".to_string()] }
    );
}

#[test]
fn matching_a_variant_from_a_different_enum_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        enum Shape {
            Circle(f64),
        }
        enum Signal {
            Go,
        }
        fn main() -> f64 {
            let s: Shape = Circle(1.0)
            return match s {
                Circle(r) => r,
                Go() => 0.0,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::UnknownVariant { enum_name: "Shape".to_string(), variant: "Go".to_string() });
}

#[test]
fn the_same_variant_appearing_twice_in_one_match_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        enum Shape {
            Circle(f64),
            Rectangle(f64, f64),
        }
        fn main() -> f64 {
            let s: Shape = Circle(1.0)
            return match s {
                Circle(r) => r,
                Circle(r2) => r2,
                Rectangle(w, h) => w * h,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateMatchArm { variant: "Circle".to_string() });
}

#[test]
fn wrong_variant_binding_arity_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        enum Shape {
            Circle(f64),
        }
        fn main() -> f64 {
            let s: Shape = Circle(1.0)
            return match s {
                Circle(r, extra) => r,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::WrongVariantArity { variant: "Circle".to_string(), want: 1, got: 2 });
}

#[test]
fn matching_a_non_enum_is_rejected_statically() {
    // `f64` deliberately has no literal-match form (floating-point
    // pattern equality is a footgun the `str`/`i64`/`bool` literal-match
    // arms don't need to inherit — see `ast::LiteralPattern`'s doc
    // comment) — still the one scrutinee type this rejects outright.
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            return match 5.0 {
                Foo() => 1,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NotAnEnum { found: Ty::F64 });
}

#[test]
fn matching_an_int_with_an_enum_style_arm_is_rejected_statically() {
    // `i64` (like `str`/`bool`) *is* a valid `match` scrutinee now — via
    // the literal-pattern form, not the enum-variant form, so a bare
    // `Foo() => ..` arm against it is a different, more specific error
    // than "not a valid scrutinee at all".
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            return match 5 {
                Foo() => 1,
                _ => 0,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::MatchArmMustBeLiteral { scrutinee_ty: Ty::I64 });
}

#[test]
fn match_arms_disagreeing_in_type_is_rejected_in_value_position() {
    let src = r#"
        enum E {
            A,
            B,
        }
        fn main() -> i64 {
            let e: E = A()
            let x: bool = match e {
                A() => true,
                B() => 1,
            }
            return 0
        }
    "#;
    // The `B` arm infers to `i64` against a `bool`-expected `let` --
    // `check`'s ordinary "no implicit conversions" path catches it,
    // reported as a plain `TypeMismatch` the same way any other
    // wrongly-typed `let` initializer would be.
    let kind = first_type_error(src);
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Bool, found: Ty::I64 });
}

// ---- declaration-time collisions --------------------------------------------

#[test]
fn two_structs_with_the_same_name_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Point { x: f64 }
        struct Point { y: f64 }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateType("Point".to_string()));
}

#[test]
fn a_struct_name_colliding_with_a_function_name_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Point { x: f64 }
        fn Point() -> i64 { return 0 }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateConstructor("Point".to_string()));
}

#[test]
fn two_variants_across_different_enums_sharing_a_name_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        enum A { X }
        enum B { X }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateConstructor("X".to_string()));
}

#[test]
fn a_struct_with_two_fields_of_the_same_name_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Bad {
            x: f64,
            x: i64,
        }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateField { struct_name: "Bad".to_string(), field: "x".to_string() });
}

#[test]
fn an_undeclared_type_name_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let x: Bogus = 1
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::UnknownType("Bogus".to_string()));
}

// ---- ownership: affinity propagates through struct/enum fields -------------

#[test]
fn a_struct_holding_a_box_field_is_affine_and_moves_as_a_whole() {
    let src = r#"
        struct Handle {
            inner: box i64,
        }
        fn main() -> i64 {
            let h: Handle = Handle(box 5)
            let h2: Handle = h
            let h3: Handle = h
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(
        check_ownership(&program).is_err(),
        "using `h` again after it moved into `h2` must be a static use-after-move error"
    );
}

#[test]
fn a_struct_with_no_affine_fields_is_freely_copyable() {
    let src = r#"
        struct Point {
            x: f64,
            y: f64,
        }
        fn main() -> f64 {
            let p: Point = Point(1.0, 2.0)
            let p2: Point = p
            let p3: Point = p
            return p2.x + p3.x
        }
    "#;
    match run(src) {
        Ok(Value::Float(n)) => assert_eq!(n, 2.0),
        other => panic!("expected Ok(Float(2.0)), got {other:?}"),
    }
}

#[test]
fn extracting_an_affine_field_moves_the_whole_struct() {
    let src = r#"
        struct Handle {
            inner: box i64,
        }
        fn main() -> i64 {
            let h: Handle = Handle(box 5)
            let taken: box i64 = h.inner
            let taken2: box i64 = h.inner
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(
        check_ownership(&program).is_err(),
        "extracting `h.inner` twice must be a static use-after-move error"
    );
}

#[test]
fn an_enum_payload_that_is_affine_makes_the_whole_enum_affine() {
    let src = r#"
        enum Wrapper {
            Has(box i64),
        }
        fn main() -> i64 {
            let w: Wrapper = Has(box 5)
            let w2: Wrapper = w
            let w3: Wrapper = w
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(check_ownership(&program).is_err(), "moving `w` twice must be a static use-after-move error");
}

// ---- codegen: Phase 4a — non-affine struct/enum/match now COMPILE ----
// (This was the "interpreter-only, rejected not mis-compiled" guard for
// the whole of Row 11 before Phase 4a; the affine-containing case is the
// real remaining gap and gets its own rejection test in
// `tests/codegen.rs` (`an_affine_field_struct_is_rejected_*`). A bare
// non-affine struct with `f64` fields and a `.field` access is exactly the
// 4a common case — flip the old blanket rejection to a positive
// `check_supported` + `emit_llvm_ir` both-succeed check here; the full
// compile-and-run parity for `structs_enums.nir` lives in
// `tests/codegen.rs`.)

#[test]
fn non_affine_struct_program_is_accepted_by_codegen() {
    let src = r#"
        struct Point {
            x: f64,
            y: f64,
        }
        fn main() -> f64 {
            let p: Point = Point(1.0, 2.0)
            return p.x
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(
        nirdosha::codegen::check_supported(&program).is_ok(),
        "Phase 4a: a non-affine struct program should pass check_supported"
    );
    let report = nirdosha::smt::analyze(&program);
    assert!(
        nirdosha::codegen::emit_llvm_ir(&program, &report).is_ok(),
        "Phase 4a: a non-affine struct program should emit LLVM IR cleanly"
    );
}

// ---- worked example ----------------------------------------------------------

#[test]
fn example_structs_enums_runs_to_completion() {
    let program = parse_ok(include_str!("../../examples/structs_enums.nir"));
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should pass ownership checking");
    match run(include_str!("../../examples/structs_enums.nir")) {
        Ok(Value::Float(n)) => assert!((n - 29.56636).abs() < 1e-9, "got {n}"),
        other => panic!("expected Ok(Float(~29.56636)), got {other:?}"),
    }
}
