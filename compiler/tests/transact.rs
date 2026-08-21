//! Tests for `transact { ... }` (`TRANSACT.md`) -- Layer 1 only: in-process,
//! no durability log, no `retry`/`timeout` on `network`. See
//! `examples/transact.nir` for the worked, documented example this file's
//! `example_transact_runs_to_completion` runs end to end.

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

fn is_parse_error(src: &str) -> bool {
    match Lexer::new(src).tokenize() {
        Ok(toks) => Parser::new(toks).parse_program().is_err(),
        Err(_) => true,
    }
}

// ---- the example, run end to end ----------------------------------------

#[test]
fn example_transact_runs_to_completion() {
    let src = include_str!("../examples/transact.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- the five-step protocol ----------------------------------------------

#[test]
fn verify_true_commits_and_yields_true() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn refund(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network:    call_api(10)
                verify:     check(network)
                commit:     update_db(network)
                compensate: refund(network)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn verify_false_compensates_and_yields_false() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn refund(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network:    call_api(-5)
                verify:     check(network)
                commit:     update_db(network)
                compensate: refund(network)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(false)));
}

#[test]
fn verify_false_with_no_compensate_slot_still_yields_false() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api(-1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(false)));
}

#[test]
fn log_slot_sees_both_network_and_verify() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn write_log(amount: i64, ok: bool) -> unit { }
        fn main() -> bool {
            return transact {
                network: call_api(3)
                verify:  check(network)
                commit:  update_db(network)
                log:     write_log(network, verify)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn transact_can_be_used_as_a_bare_statement() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {
            transact {
                network: call_api(1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- static rejections ----------------------------------------------------

#[test]
fn verify_must_return_bool() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn not_bool(resp: i64) -> i64 { return resp }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api(1)
                verify:  not_bool(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::TransactVerifyMustReturnBool { found: nirdosha::ast::Ty::I64 }
    ));
}

#[test]
fn a_builtin_cannot_be_used_as_a_transact_slot() {
    let src = r#"
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: print(1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::CannotUseBuiltinInTransact { name } if name == "print"
    ));
}

#[test]
fn a_transact_slot_must_be_a_plain_call_not_an_arbitrary_expression() {
    let src = r#"
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: 1 + 1
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert!(is_parse_error(src));
}

#[test]
fn a_missing_required_slot_is_a_parse_error() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn main() -> bool {
            return transact {
                network: call_api(1)
                verify:  check(network)
            }
        }
    "#;
    assert!(is_parse_error(src));
}

#[test]
fn network_and_verify_bindings_do_not_escape_the_transact_block() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> i64 {
            transact {
                network: call_api(1)
                verify:  check(network)
                commit:  update_db(network)
            }
            return network
        }
    "#;
    assert!(matches!(first_type_error(src), TypeErrorKind::UnknownVar(name) if name == "network"));
}

// ---- codegen: interpreter-only for now, per TRANSACT.md's own decision --

#[test]
fn transact_is_rejected_by_codegen_not_silently_miscompiled() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {
            transact {
                network: call_api(1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should type-check");
    assert!(nirdosha::codegen::check_supported(&program).is_err());
}
