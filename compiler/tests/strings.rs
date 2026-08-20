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
    let src = include_str!("../examples/strings.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- literals and escapes ------------------------------------------------

#[test]
fn a_plain_string_literal_round_trips() {
    let src = r#"
        fn main() -> str {
            return "hello"
        }
    "#;
    match run(src) {
        Ok(Value::Str(s)) => assert_eq!(&*s, "hello"),
        other => panic!("expected Ok(Str(\"hello\")), got {other:?}"),
    }
}

#[test]
fn escape_sequences_are_interpreted_correctly() {
    let src = r#"
        fn main() -> str {
            return "a\nb\tc\\d\"e\rf"
        }
    "#;
    match run(src) {
        Ok(Value::Str(s)) => assert_eq!(&*s, "a\nb\tc\\d\"e\rf"),
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

#[test]
fn strings_pass_through_function_parameters_and_returns_unchanged() {
    let src = r#"
        fn identity(s: str) -> str {
            return s
        }
        fn main() -> str {
            return identity("passed through")
        }
    "#;
    match run(src) {
        Ok(Value::Str(s)) => assert_eq!(&*s, "passed through"),
        other => panic!("expected the passed-through string, got {other:?}"),
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
        fn main() -> str {
            let a: str = "a"
            let b: str = "b"
            return a + b
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedNumeric { found: Ty::Str });
}
