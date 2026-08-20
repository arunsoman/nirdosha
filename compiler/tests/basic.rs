//! Integration tests over the public `nirdosha::run` entry point, plus a
//! few tests that drive the parser/interpreter directly to check
//! *structured* error data (goal.md row 9) rather than just a message
//! string.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{ErrorKind, Interpreter, Value};
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};
use nirdosha::run;

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

/// Type-checks `src` and returns the first error's kind, or panics if it
/// type-checked cleanly. Used by the "this must be caught statically"
/// tests below — the point of typeck.rs is that these never reach the
/// interpreter at all.
fn first_type_error(src: &str) -> TypeErrorKind {
    let program = parse_ok(src);
    match typecheck(&program) {
        Ok(()) => panic!("expected a type error, but the program type-checked cleanly"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

// ---- the three example programs, run end to end -----------------------

// All three examples' `main()` has no `-> type`, so it returns `Unit` — the
// interesting output (8, 3628800, the loop's prints) goes to stdout via
// `print()`, not through `main`'s return value. These tests only confirm
// each example runs to completion without a structured error; the actual
// arithmetic is covered by the isolated-feature tests below, which use an
// explicit `-> i64` on `main` so the computed value round-trips through
// `run`'s `Ok(...)` instead of stdout.

#[test]
fn example_hello_runs_to_completion() {
    let src = include_str!("../examples/hello.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

#[test]
fn example_factorial_runs_to_completion() {
    let src = include_str!("../examples/factorial.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

#[test]
fn example_loop_runs_to_completion() {
    let src = include_str!("../examples/loop.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- language features, in isolation -----------------------------------

#[test]
fn if_else_as_expression_with_return_in_both_branches() {
    let src = r#"
        fn abs(n: i64) -> i64 {
            if n < 0 {
                return -n
            } else {
                return n
            }
        }
        fn main() -> i64 {
            return abs(-7)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(7)));
}

#[test]
fn return_inside_nested_if_unwinds_through_expression_position() {
    // Regression test for the Signal-propagation fix: a `return` nested
    // two `if`-expressions deep has to unwind all the way to `call()`,
    // not just out of the innermost block.
    let src = r#"
        fn classify(n: i64) -> i64 {
            if n > 0 {
                if n > 100 {
                    return 2
                }
                return 1
            }
            return 0
        }
        fn main() -> i64 {
            return classify(200)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(2)));
}

#[test]
fn while_loop_with_assignment_terminates() {
    let src = r#"
        fn main() -> i64 {
            let n: i64 = 0
            let total: i64 = 0
            while n < 5 {
                total = total + n
                n = n + 1
            }
            return total
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(10))); // 0+1+2+3+4
}

#[test]
fn short_circuit_and_does_not_evaluate_rhs() {
    // `divzero()` would error if called; `&&` must short-circuit before
    // reaching it once the left side is `false`.
    let src = r#"
        fn divzero() -> bool {
            let x: i64 = 1 / 0
            return true
        }
        fn main() -> bool {
            return false && divzero()
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(false)));
}

#[test]
fn short_circuit_or_does_not_evaluate_rhs() {
    let src = r#"
        fn divzero() -> bool {
            let x: i64 = 1 / 0
            return true
        }
        fn main() -> bool {
            return true || divzero()
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

// ---- structured errors (row 9): match on `ErrorKind`, not message text --

#[test]
fn out_of_range_i8_is_a_structured_error() {
    let program = parse_ok(
        r#"
        fn main() -> i8 {
            let x: i8 = 200
            return x
        }
    "#,
    );
    let result = Interpreter::new(std::sync::Arc::new(program.clone())).run_main();
    match result {
        Err(e) => assert_eq!(
            e.kind,
            ErrorKind::OutOfRange { ty: Ty::I8.name(), value: 200 }
        ),
        Ok(_) => panic!("expected an OutOfRange error"),
    }
}

#[test]
fn division_by_zero_is_a_structured_error() {
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            return 1 / 0
        }
    "#,
    );
    let result = Interpreter::new(std::sync::Arc::new(program.clone())).run_main();
    match result {
        Err(e) => assert_eq!(e.kind, ErrorKind::DivByZero),
        Ok(_) => panic!("expected a DivByZero error"),
    }
}

#[test]
fn unknown_variable_is_a_structured_error() {
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            return does_not_exist
        }
    "#,
    );
    let result = Interpreter::new(std::sync::Arc::new(program.clone())).run_main();
    match result {
        Err(e) => assert_eq!(e.kind, ErrorKind::UnknownVar("does_not_exist".to_string())),
        Ok(_) => panic!("expected an UnknownVar error"),
    }
}

#[test]
fn arity_mismatch_is_a_structured_error() {
    let program = parse_ok(
        r#"
        fn add(a: i64, b: i64) -> i64 {
            return a + b
        }
        fn main() -> i64 {
            return add(1)
        }
    "#,
    );
    let result = Interpreter::new(std::sync::Arc::new(program.clone())).run_main();
    match result {
        Err(e) => assert_eq!(
            e.kind,
            ErrorKind::ArityMismatch { fn_name: "add".to_string(), want: 2, got: 1 }
        ),
        Ok(_) => panic!("expected an ArityMismatch error"),
    }
}

// ---- parser rejects what the grammar says it should ---------------------

#[test]
fn assignment_to_non_identifier_is_a_parse_error() {
    let toks = Lexer::new("fn main() { 1 + 1 = 2 }").tokenize().unwrap();
    let result = Parser::new(toks).parse_program();
    assert!(result.is_err(), "assigning to a non-identifier must be rejected");
}

#[test]
fn chained_call_is_a_parse_error() {
    // GRAMMAR.md's `call` production once (wrongly) claimed zero-or-more
    // repeated calls (`f()()`, currying-style) via a `*`. The real
    // grammar only ever allows one: there's no function-value concept
    // for a second call to mean anything against (`Expr::Call` names its
    // callee by a plain identifier, resolved by lookup). Found and fixed
    // during a grammar-document review, pinned here so it can't silently
    // regress either direction (accepted when it shouldn't be, or
    // rejected for the wrong reason).
    let toks = Lexer::new("fn f() -> i64 { return 5 } fn main() -> i64 { return f()() }")
        .tokenize()
        .unwrap();
    let result = Parser::new(toks).parse_program();
    assert!(result.is_err(), "f()() must be rejected -- no first-class function values exist");
}

#[test]
fn trailing_comma_in_params_is_a_parse_error() {
    let toks = Lexer::new("fn f(a: i64,) -> i64 { return a }").tokenize().unwrap();
    let result = Parser::new(toks).parse_program();
    assert!(result.is_err(), "trailing comma in a parameter list must be rejected");
}

#[test]
fn trailing_comma_in_args_is_a_parse_error() {
    let toks = Lexer::new("fn f(a: i64) -> i64 { return a } fn main() -> i64 { return f(1,) }")
        .tokenize()
        .unwrap();
    let result = Parser::new(toks).parse_program();
    assert!(result.is_err(), "trailing comma in a call's argument list must be rejected");
}

#[test]
fn statement_separator_disambiguation_always_extends_the_expression() {
    // GRAMMAR.md's documented rule, in its simplest form: no separator
    // between statements means `let x = 1` immediately followed by `-2`
    // on the next line is genuinely ambiguous as a plain CFG (confirmed
    // by the grammar_check/ LALR(1) cross-check) -- the parser always
    // resolves it the same way, by extending the expression rather than
    // ending the statement.
    let src = r#"
        fn main() -> i64 {
            let x: i64 = 1
            -2
            return x
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(-1))); // x = (1 - 2), not two statements
}


#[test]
fn missing_return_on_non_unit_function_is_a_structured_error() {
    // This test drives the interpreter directly, bypassing typeck.rs, to
    // confirm the interpreter's own dynamic MissingReturn check still
    // exists as defense-in-depth. `not_all_paths_return_is_caught_statically`
    // below is the version that matters going forward: the same mistake,
    // caught before the program ever runs.
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            let x: i64 = 5
        }
    "#,
    );
    let result = Interpreter::new(std::sync::Arc::new(program.clone())).run_main();
    match result {
        Err(e) => assert_eq!(e.kind, ErrorKind::MissingReturn { fn_name: "main".to_string() }),
        Ok(_) => panic!("expected a MissingReturn error"),
    }
}

// ---- typeck.rs: caught statically, never reaching the interpreter -------

#[test]
fn bool_where_int_expected_is_caught_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let x: i64 = true
            return x
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Bool });
}

#[test]
fn mixed_integer_widths_are_not_implicitly_convertible() {
    // goal.md §3's "no implicit conversions" — an i32 and an i64 variable
    // cannot be added directly, even though both are backed by the same
    // i64 at runtime. A bare literal (tested elsewhere) is fine; two
    // *declared* variables of different widths are not.
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let a: i32 = 5
            let b: i64 = 10
            return a + b
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::I32, found: Ty::I64 });
}

#[test]
fn integer_literal_is_flexible_against_a_declared_width() {
    // The counterpart to the test above: `n - 1` must NOT require the
    // literal `1` to carry an explicit `i64` annotation.
    let src = r#"
        fn main() -> i64 {
            let n: i64 = 10
            return n - 1
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(9)));
}

#[test]
fn literal_out_of_range_for_declared_width_is_caught_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i8 {
            let x: i8 = 999
            return x
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::LiteralOutOfRange { ty: Ty::I8, value: 999 });
}

#[test]
fn unknown_variable_is_caught_statically_before_any_output() {
    // Also proves error *recovery*: the second, independent unknown-var
    // mistake is reported too, not hidden behind the first.
    let program = parse_ok(
        r#"
        fn main() -> i64 {
            print(999)
            return first_typo + second_typo
        }
    "#,
    );
    let errors = typecheck(&program).expect_err("both undefined names should be caught");
    assert_eq!(errors.len(), 2, "expected both unknown-variable errors, got {errors:?}");
    assert!(errors.iter().any(|e| e.kind == TypeErrorKind::UnknownVar("first_typo".to_string())));
    assert!(errors.iter().any(|e| e.kind == TypeErrorKind::UnknownVar("second_typo".to_string())));
}

#[test]
fn not_all_paths_return_is_caught_statically() {
    let kind = first_type_error(
        r#"
        fn classify(n: i64) -> i64 {
            if n > 0 {
                return 1
            }
            // no `else`, and no trailing return — falls through
        }
        fn main() -> i64 {
            return classify(5)
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::NotAllPathsReturn { fn_name: "classify".to_string() });
}

#[test]
fn if_with_no_else_used_as_a_value_is_rejected() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let x: i64 = if true { 1 }
            return x
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::IfWithoutElseUsedAsValue { expected: Ty::I64 });
}

#[test]
fn if_with_no_else_used_as_a_statement_is_fine() {
    // The distinction the module doc calls out explicitly: same shape,
    // different position, no error when the value is discarded.
    let src = r#"
        fn main() -> i64 {
            let x: i64 = 0
            if true {
                print(x)
            }
            return x
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(0)));
}

#[test]
fn if_branches_disagreeing_in_type_are_rejected_in_value_position() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let x: i64 = if true { 1 } else { false }
            return x
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Bool });
}

#[test]
fn if_used_as_value_with_matching_branches_works_end_to_end() {
    let src = r#"
        fn main() -> i64 {
            let x: i64 = if true { 1 } else { 2 }
            return x
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(1)));
}

#[test]
fn return_nested_inside_a_value_position_if_still_typechecks() {
    // The trickiest case the module doc calls out: `return` inside a
    // branch of an `if` that's itself a `let`'s initializer. The
    // `return`'s type is checked against the function's return type
    // (i64), completely independent of the `let`'s declared type — and
    // the interpreter already runs this correctly (Signal propagation),
    // so the checker has to accept it, not just tolerate it.
    let src = r#"
        fn pick(n: i64) -> i64 {
            let doubled: i64 = if n > 100 { return 999 } else { n * 2 }
            return doubled
        }
        fn main() -> i64 {
            return pick(5)
        }
    "#;
    assert_eq!(run(src), Ok(Value::Int(10)));
}

#[test]
fn duplicate_function_definition_is_caught_statically() {
    let kind = first_type_error(
        r#"
        fn twice(n: i64) -> i64 { return n * 2 }
        fn twice(n: i64) -> i64 { return n * 2 }
        fn main() -> i64 { return twice(1) }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::DuplicateFn("twice".to_string()));
}

#[test]
fn missing_main_is_caught_statically() {
    let kind = first_type_error("fn not_main() -> i64 { return 1 }");
    assert_eq!(kind, TypeErrorKind::NoMainFn);
}

#[test]
fn all_three_examples_pass_static_type_checking() {
    for src in [
        include_str!("../examples/hello.nir"),
        include_str!("../examples/factorial.nir"),
        include_str!("../examples/loop.nir"),
    ] {
        let program = parse_ok(src);
        assert_eq!(typecheck(&program), Ok(()));
    }
}
