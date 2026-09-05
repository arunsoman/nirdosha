//! Tests for `str` -- literals, escapes, function parameter/return
//! positions, and equality. Deliberately minimal (see `Ty::Str`'s doc
//! comment in ast.rs): no concatenation, no indexing. Existing purely to
//! name things (a hostname, an image tag) that couldn't be spelled any
//! other way -- see `tests/tcp.rs` for what that's actually for.

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
fn example_strings_runs_to_completion() {
    let src = include_str!("fixtures/strings.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- literals and escapes ------------------------------------------------

#[test]
fn a_plain_string_literal_round_trips() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return Text("hello")
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "hello"),
            other => panic!("expected Text(Str(\"hello\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"hello\")), got {other:?}"),
    }
}

#[test]
fn escape_sequences_are_interpreted_correctly() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return Text("a\nb\tc\\d\"e\rf")
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "a\nb\tc\\d\"e\rf"),
            other => panic!("expected Text(Str(escaped)), got Text({other:?})"),
        },
        other => panic!("expected the escaped string, got {other:?}"),
    }
}

#[test]
fn an_unknown_escape_is_a_lex_error() {
    let toks = Lexer::new(r#"fn main() { let s: str = "\q" }"#).tokenize();
    assert!(toks.is_err(), "an unrecognized escape must be rejected, not silently kept literal");
}

#[test]
fn an_unterminated_string_is_a_lex_error() {
    let toks = Lexer::new("fn main() { let s: str = \"never closed").tokenize();
    assert!(toks.is_err(), "a string with no closing quote must be a lex error, not a hang");
}

// ---- passing strings through functions ------------------------------------

// A bare `str` can no longer be a function's parameter or return type at
// all (the "enum favoring" rule — `TypeErrorKind::StrInFnSignature`,
// `typeck.rs::check_fn`), so this no longer tests passing a bare `str`
// through a function boundary; it tests the sanctioned replacement —
// wrapping free text in a carrier struct (`Text`) — still passes through
// a function boundary with its value unchanged, not silently corrupted
// or truncated in transit.
#[test]
fn text_passes_through_function_parameters_and_returns_unchanged() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn pass_through(s: Text) -> Text {
            return s
        }
        fn main() -> Text {
            return pass_through(Text("passed through"))
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "passed through"),
            other => panic!("expected Text(Str(\"passed through\")), got Text({other:?})"),
        },
        other => panic!("expected the passed-through Text, got {other:?}"),
    }
}

// ---- equality (found missing at runtime once, fixed, pinned here) --------

#[test]
fn equal_strings_compare_equal() {
    let src = r#"
        fn main() -> bool {
            let a: str = "same"
            let b: str = "same"
            return a == b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn different_strings_compare_unequal() {
    let src = r#"
        fn main() -> bool {
            let a: str = "one"
            let b: str = "two"
            return a != b
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

// ---- static rejections (a real gap found and fixed, pinned here) --------

#[test]
fn ordering_strings_is_rejected_statically_not_at_runtime() {
    // typeck.rs used to only reject `Bool` for `<`/`>`/etc, letting
    // `str < str` typecheck and fail at runtime with a generic
    // TypeMismatch instead. Fixed to reject any non-numeric type
    // uniformly -- this pins the fix for `str` specifically.
    let kind = first_type_error(
        r#"
        fn main() -> bool {
            let a: str = "a"
            let b: str = "b"
            return a < b
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedNumeric { found: Ty::Str });
}

#[test]
fn arithmetic_on_strings_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> bool {
            let a: str = "a"
            let b: str = "b"
            let c: i64 = a + b
            return true
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedNumeric { found: Ty::Str });
}
