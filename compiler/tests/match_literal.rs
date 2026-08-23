//! Tests for literal-pattern `match` (`str`/`i64`/`bool` scrutinees,
//! matched against literal-value arms plus a mandatory trailing `_`) —
//! the post-v1 extension to Row 11's original enum-only `match`
//! (`nirdosha_row11_amendment.md` §3.4, which explicitly named "no
//! wildcard/binding-only catch-all patterns" as a disclosed v1 gap).

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
fn str_match_replaces_a_nested_if_else_chain() {
    let src = r#"
        struct Text {
            value: str,
        }
        struct Role {
            value: str,
        }
        fn role_to_claims(role: Role) -> Text {
            return match role.value {
                "admin" => Text("{\"roles\":[\"admin\"]}"),
                "analyst" => Text("{\"roles\":[\"analyst\"]}"),
                _ => Text("{\"roles\":[\"customer\"]}"),
            }
        }
        fn main() -> Text {
            return role_to_claims(Role("analyst"))
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "{\"roles\":[\"analyst\"]}"),
            other => panic!("expected Text(Str(..)), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(..)), got {other:?}"),
    }
}

#[test]
fn str_match_falls_through_to_wildcard_for_an_unlisted_value() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return match "nobody" {
                "admin" => Text("a"),
                "analyst" => Text("b"),
                _ => Text("customer"),
            }
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "customer"),
            other => panic!("expected Text(Str(\"customer\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"customer\")), got {other:?}"),
    }
}

#[test]
fn int_match_dispatches_on_the_right_literal() {
    let src = r#"
        fn main() -> i64 {
            return match 2 {
                1 => 100,
                2 => 200,
                3 => 300,
                _ => -1,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Int(n)) => assert_eq!(n, 200),
        other => panic!("expected Ok(Int(200)), got {other:?}"),
    }
}

#[test]
fn bool_match_dispatches_on_the_right_literal() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return match true {
                true => Text("yes"),
                false => Text("no"),
                _ => Text("unreachable"),
            }
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "yes"),
            other => panic!("expected Text(Str(\"yes\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"yes\")), got {other:?}"),
    }
}

#[test]
fn literal_match_without_a_trailing_wildcard_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            return match 1 {
                1 => 100,
                2 => 200,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NonExhaustiveLiteralMatch { found: Ty::I64 });
}

#[test]
fn wildcard_before_the_last_arm_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            return match 1 {
                _ => 0,
                1 => 100,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::WildcardArmNotLast);
}

#[test]
fn a_pattern_of_the_wrong_literal_type_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            return match "x" {
                1 => 100,
                _ => 0,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::LiteralPatternTypeMismatch { scrutinee_ty: Ty::Str, pattern_ty: Ty::I64 });
}

#[test]
fn duplicate_literal_arms_are_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            return match 1 {
                1 => 100,
                1 => 200,
                _ => 0,
            }
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateMatchArm { variant: "1".to_string() });
}

#[test]
fn literal_match_arms_disagreeing_in_type_is_rejected_in_value_position() {
    let src = r#"
        fn main() -> i64 {
            let x: i64 = match "a" {
                "a" => 1,
                _ => "not an int",
            }
            return x
        }
    "#;
    let program = parse_ok(src);
    assert!(typecheck(&program).is_err());
}

#[test]
fn literal_match_used_as_a_bare_statement_does_not_need_arm_types_to_agree() {
    let src = r#"
        fn main() -> i64 {
            match "a" {
                "a" => 1,
                _ => "different type, fine as a statement",
            }
            return 0
        }
    "#;
    let program = parse_ok(src);
    assert!(typecheck(&program).is_ok());
}

#[test]
fn enum_scrutinee_with_a_literal_style_arm_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        enum E {
            A,
            B,
        }
        fn main() -> i64 {
            let e: E = A()
            return match e {
                "a" => 1,
                _ => 0,
            }
        }
    "#,
    );
    match kind {
        TypeErrorKind::MatchArmMustBeVariant { .. } => {}
        other => panic!("expected MatchArmMustBeVariant, got {other:?}"),
    }
}
