//! Tests for JSON (`Ty::Json`) — concrete, per-target-type accessors
//! (`json_get_str`/`_i64`/`_f64`/`_bool`) over an already-parsed
//! `serde_json::Value` tree, every fallible one returning a real
//! `Result(_, str)` (Row 11 layer 7's prelude) rather than trapping.

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

#[test]
fn parsing_and_reading_a_string_field_round_trips() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return match json_parse("{\"name\": \"nirdosha\"}") {
                Ok(doc) => match json_get_str(doc, "name") {
                    Ok(s) => Text(s),
                    Err(e) => Text(e),
                },
                Err(e) => Text(e),
            }
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "nirdosha"),
            other => panic!("expected Text(Str(\"nirdosha\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"nirdosha\")), got {other:?}"),
    }
}

#[test]
fn integer_float_and_bool_fields_all_round_trip() {
    let src = r#"
        fn get_age(doc: json) -> i64 {
            return match json_get_i64(doc, "age") {
                Ok(n) => n,
                Err(e) => -1,
            }
        }
        fn get_active(doc: json) -> bool {
            return match json_get_bool(doc, "active") {
                Ok(b) => b,
                Err(e) => false,
            }
        }
        fn main() -> i64 {
            return match json_parse("{\"age\": 3, \"score\": 9.5, \"active\": true}") {
                Ok(doc) => if get_active(doc) { get_age(doc) } else { 0 },
                Err(e) => -1,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Int(3)) => {}
        other => panic!("expected Ok(Int(3)), got {other:?}"),
    }
}

#[test]
fn a_float_field_round_trips() {
    let src = r#"
        fn main() -> f64 {
            return match json_parse("{\"score\": 9.5}") {
                Ok(doc) => match json_get_f64(doc, "score") {
                    Ok(n) => n,
                    Err(e) => -1.0,
                },
                Err(e) => -1.0,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Float(n)) => assert_eq!(n, 9.5),
        other => panic!("expected Ok(Float(9.5)), got {other:?}"),
    }
}

#[test]
fn a_missing_field_is_a_recoverable_err_not_a_trap() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn describe(doc: json) -> Text {
            return match json_get_str(doc, "missing") {
                Ok(s) => Text(s),
                Err(e) => Text("missing!"),
            }
        }
        fn main() -> Text {
            return match json_parse("{\"name\": \"x\"}") {
                Ok(doc) => describe(doc),
                Err(e) => Text("parse failed"),
            }
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "missing!"),
            other => panic!("expected Text(Str(\"missing!\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"missing!\")), got {other:?}"),
    }
}

#[test]
fn malformed_json_is_a_recoverable_err_not_a_trap() {
    let src = r#"
        fn main() -> bool {
            return match json_parse("{not valid json") {
                Ok(doc) => true,
                Err(msg) => false,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(false)) => {}
        other => panic!("expected Ok(Bool(false)), got {other:?}"),
    }
}

#[test]
fn arrays_report_their_length() {
    let src = r#"
        fn main() -> i64 {
            return match json_parse("[10, 20, 30]") {
                Ok(arr) => match json_array_len(arr) {
                    Ok(n) => n,
                    Err(e) => -1,
                },
                Err(e) => -1,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Int(3)) => {}
        other => panic!("expected Ok(Int(3)), got {other:?}"),
    }
}

#[test]
fn array_elements_are_themselves_navigable_json() {
    let src = r#"
        fn main() -> i64 {
            return match json_parse("[{\"n\": 10}, {\"n\": 20}, {\"n\": 30}]") {
                Ok(arr) => match json_array_get(arr, 1) {
                    Ok(el) => match json_get_i64(el, "n") {
                        Ok(n) => n,
                        Err(e) => -1,
                    },
                    Err(e) => -1,
                },
                Err(e) => -1,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Int(20)) => {}
        other => panic!("expected Ok(Int(20)), got {other:?}"),
    }
}

#[test]
fn an_out_of_bounds_array_index_is_a_recoverable_err() {
    let src = r#"
        fn main() -> bool {
            return match json_parse("[1, 2]") {
                Ok(arr) => match json_array_get(arr, 5) {
                    Ok(el) => false,
                    Err(e) => true,
                },
                Err(e) => false,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn nested_objects_navigate_with_plain_json_get() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return match json_parse("{\"user\": {\"name\": \"ada\"}}") {
                Ok(doc) => match json_get(doc, "user") {
                    Ok(user) => match json_get_str(user, "name") {
                        Ok(s) => Text(s),
                        Err(e) => Text(e),
                    },
                    Err(e) => Text(e),
                },
                Err(e) => Text(e),
            }
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "ada"),
            other => panic!("expected Text(Str(\"ada\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"ada\")), got {other:?}"),
    }
}

#[test]
fn reading_a_string_field_as_the_wrong_type_is_a_recoverable_err() {
    let src = r#"
        fn main() -> bool {
            return match json_parse("{\"name\": \"x\"}") {
                Ok(doc) => match json_get_i64(doc, "name") {
                    Ok(n) => false,
                    Err(msg) => true,
                },
                Err(e) => false,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn matching_directly_on_a_builtin_call_result_resolves_generic_args_precisely() {
    // `doc` is bound from `match json_parse(..) { Ok(doc) => .. }` --
    // the scrutinee is a builtin *call*, not a bound identifier, so this
    // exercises `ownership.rs`'s `builtin_return_ty` resolution
    // specifically: `doc` must be recognized as non-affine (`Ty::Json`,
    // substituted from `Result(json, str)`) and usable twice, not
    // conservatively treated as affine.
    let src = r#"
        fn main() -> bool {
            return match json_parse("{\"a\": 1}") {
                Ok(doc) => match json_get_i64(doc, "a") {
                    Ok(n) => n == 1,
                    Err(e) => match json_get_str(doc, "missing") {
                        Ok(s) => false,
                        Err(e2) => false,
                    },
                },
                Err(e) => false,
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    nirdosha::ownership::check_ownership(&program)
        .expect("`doc` is non-affine (json); using it twice must not be a use-after-move error");
}

#[test]
fn matching_directly_on_a_user_function_call_result_resolves_generic_args_precisely() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn find(doc: json) -> Result(Text, Text) {
            return match json_get_str(doc, "name") {
                Ok(s) => Ok(Text(s)),
                Err(e) => Err(Text(e)),
            }
        }
        fn main() -> bool {
            return match json_parse("{\"name\": \"x\"}") {
                Ok(doc) => match find(doc) {
                    Ok(s) => s.value == "x",
                    Err(e) => match find(doc) {
                        Ok(s2) => false,
                        Err(e2) => false,
                    },
                },
                Err(e) => false,
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    nirdosha::ownership::check_ownership(&program)
        .expect("`doc` (json, non-affine) must be usable twice via `find`, resolved through `fn_rets`");
}

#[test]
fn json_get_str_requires_a_json_first_argument() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(str, str) = json_get_str(1, "x")
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Json, found: Ty::I64 });
}

#[test]
fn json_is_interpreter_only_rejected_by_codegen() {
    let src = r#"
        fn main() -> i64 {
            let r: Result(json, str) = json_parse("{}")
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(nirdosha::codegen::check_supported(&program).is_err());
}

#[test]
fn example_json_runs_to_completion() {
    let program = parse_ok(include_str!("../../examples/json.nir"));
    typecheck(&program).expect("should typecheck cleanly");
    assert_eq!(run(include_str!("../../examples/json.nir")), Ok(Value::Unit));
}

// ---- json_set_str (json_get_str's inverse, WORKFLOW.md) --------------

#[test]
fn json_set_str_adds_a_field_to_an_empty_object() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return match json_parse("{}") {
                Ok(doc) => match json_set_str(doc, "link_token", "abc123") {
                    Ok(updated) => match json_get_str(updated, "link_token") {
                        Ok(s) => Text(s),
                        Err(e) => Text(e),
                    },
                    Err(e) => Text(e),
                },
                Err(e) => Text(e),
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert_eq!(
        run(src),
        Ok(Value::Struct(std::sync::Arc::from("Text"), std::sync::Arc::from(vec![Value::Str(std::sync::Arc::from("abc123"))])))
    );
}

#[test]
fn json_set_str_preserves_existing_fields() {
    let src = r#"
        fn main() -> i64 {
            return match json_parse("{\"a\": 1}") {
                Ok(doc) => match json_set_str(doc, "b", "two") {
                    Ok(updated) => match json_get_i64(updated, "a") {
                        Ok(n) => n,
                        Err(e) => -1,
                    },
                    Err(e) => -1,
                },
                Err(e) => -1,
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert_eq!(run(src), Ok(Value::Int(1)));
}

#[test]
fn json_set_str_on_a_non_object_is_a_clean_err_not_a_trap() {
    let src = r#"
        fn main() -> bool {
            return match json_parse("[1, 2]") {
                Ok(doc) => match json_set_str(doc, "k", "v") {
                    Ok(updated) => false,
                    Err(e) => true,
                },
                Err(e) => false,
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert_eq!(run(src), Ok(Value::Bool(true)));
}
