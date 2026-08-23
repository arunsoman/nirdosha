//! Tests for Row 11 layer 6 (generics on `struct`/`enum` declarations)
//! and layer 7 (the `Option(T)`/`Result(T, E)` prelude —
//! `nirdosha_row11_amendment.md` §3.6). Type identity is
//! structural-per-instantiation, not erased (`Duo(i64, str)` and
//! `Duo(f64, bool)` are simply different, unrelated `Ty`s), and
//! construction/match resolve type arguments either from an expected
//! type at the call site or, failing that, structurally from the
//! arguments themselves.

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

// ---- generic structs ---------------------------------------------------

#[test]
fn generic_struct_construction_and_field_access_round_trip() {
    let src = r#"
        struct Duo(A, B) {
            first: A,
            second: B,
        }
        fn main() -> i64 {
            let p: Duo(i64, str) = Duo(1, "one")
            return p.first
        }
    "#;
    match run(src) {
        Ok(Value::Int(1)) => {}
        other => panic!("expected Ok(Int(1)), got {other:?}"),
    }
}

#[test]
fn two_different_instantiations_are_structurally_distinct_types() {
    let kind = first_type_error(
        r#"
        struct Duo(A, B) {
            first: A,
            second: B,
        }
        fn takes_int_unit(p: Duo(i64, unit)) -> i64 {
            return p.first
        }
        fn main() -> i64 {
            let p: Duo(f64, bool) = Duo(1.0, true)
            return takes_int_unit(p)
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::TypeMismatch {
            expected: Ty::Named("Duo".to_string(), vec![Ty::I64, Ty::Unit]),
            found: Ty::Named("Duo".to_string(), vec![Ty::F64, Ty::Bool]),
        }
    );
}

#[test]
fn generic_construction_infers_type_args_structurally_with_no_expected_type() {
    // `print` doesn't pin an expected type -- the type arguments have to
    // come from the constructor's own arguments instead.
    let src = r#"
        struct Duo(A, B) {
            first: A,
            second: B,
        }
        fn main() -> unit {
            print(Duo(1, "one"))
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("structural inference should resolve A=i64, B=str with no annotation");
}

#[test]
fn generic_construction_via_a_function_argument_uses_the_declared_param_type() {
    let src = r#"
        struct Duo(A, B) {
            first: A,
            second: B,
        }
        fn sum_first_two(p: Duo(i64, i64)) -> i64 {
            return p.first + p.second
        }
        fn main() -> i64 {
            return sum_first_two(Duo(2, 3))
        }
    "#;
    match run(src) {
        Ok(Value::Int(5)) => {}
        other => panic!("expected Ok(Int(5)), got {other:?}"),
    }
}

#[test]
fn wrong_type_argument_count_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Duo(A, B) {
            first: A,
            second: B,
        }
        fn main() -> i64 {
            let p: Duo(i64) = Duo(1, 2)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::WrongTypeArity { name: "Duo".to_string(), want: 2, got: 1 });
}

#[test]
fn a_non_generic_struct_given_type_arguments_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Point { x: f64 }
        fn main() -> i64 {
            let p: Point(i64) = Point(1.0)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::WrongTypeArity { name: "Point".to_string(), want: 0, got: 1 });
}

#[test]
fn duplicate_type_parameter_names_are_rejected_statically() {
    let kind = first_type_error(
        r#"
        struct Bad(A, A) {
            x: A,
        }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateTypeParam("A".to_string()));
}

#[test]
fn nested_generic_field_access_resolves_the_correct_concrete_type() {
    let src = r#"
        struct Wrapper(T) {
            inner: T,
        }
        fn main() -> f64 {
            let w: Wrapper(f64) = Wrapper(3.5)
            return w.inner
        }
    "#;
    match run(src) {
        Ok(Value::Float(n)) => assert_eq!(n, 3.5),
        other => panic!("expected Ok(Float(3.5)), got {other:?}"),
    }
}

// ---- generic enums (user-defined, not the prelude) --------------------

#[test]
fn generic_enum_construction_and_match_round_trip() {
    let src = r#"
        struct Text {
            value: str,
        }
        enum Either(A, B) {
            Left(A),
            Right(B),
        }
        fn describe(e: Either(i64, Text)) -> Text {
            return match e {
                Left(n) => Text("left"),
                Right(s) => s,
            }
        }
        fn main() -> Text {
            let e: Either(i64, Text) = Right(Text("hi"))
            return describe(e)
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "hi"),
            other => panic!("expected Text(Str(\"hi\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"hi\")), got {other:?}"),
    }
}

// ---- affinity propagation through generic instantiations ---------------

#[test]
fn a_generic_struct_instantiated_with_an_affine_arg_is_affine() {
    let src = r#"
        struct Wrapper(T) {
            v: T,
        }
        fn main() -> i64 {
            let w: Wrapper(box i64) = Wrapper(box 5)
            let w2: Wrapper(box i64) = w
            let w3: Wrapper(box i64) = w
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(
        check_ownership(&program).is_err(),
        "Wrapper(box i64) is affine -- using `w` twice must be a static use-after-move error"
    );
}

#[test]
fn the_same_generic_struct_instantiated_with_a_scalar_is_not_affine() {
    let src = r#"
        struct Wrapper(T) {
            v: T,
        }
        fn main() -> i64 {
            let w: Wrapper(i64) = Wrapper(5)
            let w2: Wrapper(i64) = w
            let w3: Wrapper(i64) = w
            return w2.v + w3.v
        }
    "#;
    match run(src) {
        Ok(Value::Int(10)) => {}
        other => panic!("expected Ok(Int(10)), got {other:?}"),
    }
}

#[test]
fn a_generic_enum_payload_that_is_affine_makes_the_whole_value_affine() {
    let src = r#"
        enum Boxed(T) {
            Has(T),
        }
        fn main() -> i64 {
            let b: Boxed(box i64) = Has(box 5)
            let b2: Boxed(box i64) = b
            let b3: Boxed(box i64) = b
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(check_ownership(&program).is_err(), "moving `b` twice must be a static use-after-move error");
}

// As of Phase 4a, `struct`/`enum`/`match` over non-affine payloads are
// *accepted* by codegen (see `codegen.rs`/`LANGUAGE.md` §10) -- this was
// the gate the old rejection test below guarded, now flipped to a
// positive check that a prelude-variant construction passes
// `check_supported` (the structural pre-pass) cleanly, mirroring the way
// every other now-compiled construct's generics test reads. Full
// compile-and-run parity for `Option`/`Result` lives in `tests/codegen.rs`
// (the `structs_enums_*` tests), not here.
#[test]
fn constructing_a_prelude_variant_is_accepted_by_codegen() {
    let src = r#"
        fn main() -> i64 {
            let o: Option(i64) = Some(5)
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(
        nirdosha::codegen::check_supported(&program).is_ok(),
        "Phase 4a: a non-affine struct/enum construction should be accepted by check_supported, not rejected"
    );
}

// ---- the Option(T)/Result(T, E) prelude itself (layer 7) ---------------

#[test]
fn option_some_and_none_round_trip_through_match() {
    let src = r#"
        fn unwrap_or(o: Option(i64), default: i64) -> i64 {
            return match o {
                Some(n) => n,
                None => default,
            }
        }
        fn main() -> i64 {
            let a: Option(i64) = Some(42)
            let b: Option(i64) = None()
            return unwrap_or(a, -1) + unwrap_or(b, -1)
        }
    "#;
    match run(src) {
        Ok(Value::Int(41)) => {} // 42 + (-1)
        other => panic!("expected Ok(Int(41)), got {other:?}"),
    }
}

#[test]
fn option_match_is_exhaustive_and_rejects_a_missing_arm() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let o: Option(i64) = Some(1)
            return match o {
                Some(n) => n,
            }
        }
    "#,
    );
    assert_eq!(
        kind,
        TypeErrorKind::NonExhaustiveMatch { enum_name: "Option".to_string(), missing: vec!["None".to_string()] }
    );
}

#[test]
fn result_ok_and_err_round_trip_through_match() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn describe(r: Result(i64, Text)) -> Text {
            return match r {
                Ok(n) => Text("ok"),
                Err(msg) => msg,
            }
        }
        fn main() -> Text {
            let bad: Result(i64, Text) = Err(Text("boom"))
            return describe(bad)
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "boom"),
            other => panic!("expected Text(Str(\"boom\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"boom\")), got {other:?}"),
    }
}

#[test]
fn none_with_no_expected_type_needs_an_explicit_annotation() {
    // `None` never mentions `Option`'s own type parameter in its payload
    // (it has none) -- there is nothing to structurally infer `T` from,
    // and no expected type is available in a bare `print(...)` argument
    // position, so this is exactly `GenericConstructorNeedsExplicitType`.
    let kind = first_type_error(
        r#"
        fn main() -> unit {
            print(None())
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::GenericConstructorNeedsExplicitType { name: "Option".to_string() });
}

#[test]
fn redeclaring_option_is_rejected_as_a_duplicate_type() {
    let kind = first_type_error(
        r#"
        enum Option(X) {
            Some(X),
            None,
        }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateType("Option".to_string()));
}

#[test]
fn reusing_some_as_a_function_name_is_rejected_as_a_duplicate_constructor() {
    let kind = first_type_error(
        r#"
        fn Some() -> i64 { return 0 }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateConstructor("Some".to_string()));
}

#[test]
fn option_of_option_nests_correctly() {
    let src = r#"
        fn main() -> i64 {
            let inner: Option(i64) = Some(7)
            let outer: Option(Option(i64)) = Some(inner)
            return match outer {
                Some(o) => match o {
                    Some(n) => n,
                    None => -1,
                },
                None => -2,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Int(7)) => {}
        other => panic!("expected Ok(Int(7)), got {other:?}"),
    }
}

// ---- worked example -----------------------------------------------------

#[test]
fn example_generics_runs_to_completion() {
    let program = parse_ok(include_str!("../../examples/generics.nir"));
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should pass ownership checking");
    match run(include_str!("../../examples/generics.nir")) {
        Ok(Value::Int(n)) => assert_eq!(n, 8),
        other => panic!("expected Ok(Int(8)), got {other:?}"),
    }
}
