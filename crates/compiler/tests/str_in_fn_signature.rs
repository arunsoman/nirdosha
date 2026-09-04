//! Tests for the "enum favoring" rule (`Ty::contains_str` +
//! `TypeErrorKind::StrInFnSignature`, enforced in
//! `typeck.rs::check_fn`): a user-defined `fn`'s parameter or return
//! type may not be, or contain, `str`. Confirms the rule fires for
//! ordinary user functions, and confirms its three carve-outs actually
//! hold: builtins, struct/enum constructors, and `transact`'s
//! synthesized `txn_id` parameter.

use nirdosha::ast::Program;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};

fn parse(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

#[test]
fn a_str_parameter_is_rejected() {
    let program = parse(
        r#"
        fn greet(name: str) -> unit {
            print(name)
        }
        fn main() -> unit {}
        "#,
    );
    let errs = typecheck(&program).expect_err("a str parameter must be rejected");
    assert!(errs.iter().any(|e| matches!(
        &e.kind,
        TypeErrorKind::StrInFnSignature { fn_name, param_name: Some(p) }
            if fn_name == "greet" && p == "name"
    )));
}

#[test]
fn a_str_parameter_error_message_is_self_teaching() {
    // The error text itself must name the exact fix (real `enum` for
    // categorical data, `struct Text { value: str }` for free text),
    // not just point at the problem — so a reader never has to already
    // know `docs/LANGUAGE.md` §6b to recover from hitting this.
    let program = parse(
        r#"
        fn greet(name: str) -> unit {
            print(name)
        }
        fn main() -> unit {}
        "#,
    );
    let errs = typecheck(&program).expect_err("a str parameter must be rejected");
    let msg = errs
        .iter()
        .find(|e| matches!(&e.kind, TypeErrorKind::StrInFnSignature { fn_name, param_name: Some(p) } if fn_name == "greet" && p == "name"))
        .expect("StrInFnSignature error for `greet`'s `name` param")
        .to_string();
    assert!(msg.contains("can't cross a function boundary"), "message was: {msg}");
    assert!(msg.contains("enum Status { Pending, Approved }"), "message was: {msg}");
    assert!(msg.contains("name: Status"), "message was: {msg}");
    assert!(msg.contains("struct Text { value: str }"), "message was: {msg}");
    assert!(msg.contains("name: Text"), "message was: {msg}");
    assert!(msg.contains("name.value"), "message was: {msg}");
}

#[test]
fn a_str_return_type_error_message_is_self_teaching() {
    let program = parse(
        r#"
        fn label() -> str {
            return "hi"
        }
        fn main() -> unit {}
        "#,
    );
    let errs = typecheck(&program).expect_err("a str return type must be rejected");
    let msg = errs
        .iter()
        .find(|e| matches!(&e.kind, TypeErrorKind::StrInFnSignature { fn_name, param_name: None } if fn_name == "label"))
        .expect("StrInFnSignature error for `label`'s return type")
        .to_string();
    assert!(msg.contains("can't cross a function boundary"), "message was: {msg}");
    assert!(msg.contains("struct Text { value: str }"), "message was: {msg}");
    assert!(msg.contains("-> Text"), "message was: {msg}");
    assert!(msg.contains("Text(the_string)"), "message was: {msg}");
}

#[test]
fn a_str_return_type_is_rejected() {
    let program = parse(
        r#"
        fn label() -> str {
            return "hi"
        }
        fn main() -> unit {}
        "#,
    );
    let errs = typecheck(&program).expect_err("a str return type must be rejected");
    assert!(errs.iter().any(|e| matches!(
        &e.kind,
        TypeErrorKind::StrInFnSignature { fn_name, param_name: None } if fn_name == "label"
    )));
}

#[test]
fn a_str_nested_in_result_or_option_is_rejected() {
    let program = parse(
        r#"
        fn risky() -> Result(i64, str) {
            return Ok(1)
        }
        fn main() -> unit {}
        "#,
    );
    let errs = typecheck(&program).expect_err("str nested in Result's error channel must be rejected");
    assert!(errs
        .iter()
        .any(|e| matches!(&e.kind, TypeErrorKind::StrInFnSignature { fn_name, param_name: None } if fn_name == "risky")));
}

#[test]
fn builtins_with_str_arguments_are_unaffected() {
    let program = parse(
        r#"
        fn main() -> unit {
            print("hello")
        }
        "#,
    );
    typecheck(&program).expect("a builtin call with a str argument is not a program.fns entry, so it's unaffected");
}

#[test]
fn struct_constructors_with_str_fields_are_unaffected() {
    let program = parse(
        r#"
        struct Widget {
            id: i64,
            name: str,
        }
        fn main() -> unit {
            let w: Widget = Widget(1, "gadget")
            print(w.name)
        }
        "#,
    );
    typecheck(&program).expect("a struct constructor call is not a program.fns entry, so a str field is unaffected");
    check_ownership(&program).expect("ownership check should succeed");
}

#[test]
fn transacts_synthesized_txn_id_parameter_is_exempt() {
    let program = parse(
        r#"
        fn call_api(txn_id: str, amount: i64) -> i64 {
            return amount
        }
        fn check(resp: i64) -> bool {
            return resp > 0
        }
        fn update_db(amount: i64) -> i64 {
            return amount
        }
        fn checkout(amount: i64) -> bool {
            return transact {
                network: call_api(txn_id, amount)
                verify:  check(network)
                commit:  update_db(amount)
            }
        }
        fn main() -> unit {}
        "#,
    );
    typecheck(&program).expect("a txn_id: str parameter on a transact network-slot fn is the one structural exemption");
}

#[test]
fn enum_equality_works_at_runtime_not_just_typecheck() {
    use nirdosha::run;
    let src = r#"
        enum Currency {
            Usd,
            Eur,
        }
        fn main() -> unit {
            let a: Currency = Usd()
            let b: Currency = Usd()
            let c: Currency = Eur()
            print(a == b)
            print(a == c)
            print(a != c)
        }
    "#;
    run(src).expect("a program comparing two enum values with == should run cleanly");
}
