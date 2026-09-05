//! Tests for `transact { ... }` (`docs/TRANSACT.md`) -- the full protocol
//! (`precheck`?, `network`, `verify`, `commit`, `compensate`?, `log`?),
//! in-process control flow plus the durability log and its typeck-level
//! guardrails. See `examples/transact.nir` for the worked, documented
//! example this file's `example_transact_runs_to_completion` runs end to
//! end, and `tests/transact_durability.rs` for the WAL/crash-replay/
//! retry-and-escalate behavior specifically.

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
    let src = include_str!("fixtures/transact.nir");
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- the five-step protocol ----------------------------------------------

#[test]
fn verify_true_commits_and_yields_true() {
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn refund(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network:    call_api(txn_id, 10)
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
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn refund(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network:    call_api(txn_id, -5)
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
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, -1)
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
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn write_log(amount: i64, ok: bool) -> unit { }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, 3)
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
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {
            transact {
                network: call_api(txn_id, 1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Unit));
}

// ---- Layer 2: `retry`/`timeout` on `network` -------------------------

#[test]
fn network_without_retry_traps_immediately_on_first_failure() {
    // No `retry` clause -- docs/TRANSACT.md's "default 1, i.e. no retry":
    // exactly one attempt, the trap propagates straight out.
    let src = r#"
        fn call_api_traps(txn_id: str, amount: i64) -> i64 { return 1 / 0 }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api_traps(txn_id, 1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    match run(src) {
        Err(msg) => assert!(msg.contains("division by zero"), "expected the trap itself to propagate: {msg}"),
        other => panic!("expected an Err (the trap propagating, no retry), got {other:?}"),
    }
}

#[test]
fn network_timeout_traps_when_the_call_blocks_past_the_deadline() {
    // `sleep_ms` is the one concrete way `.nir` source can simulate a
    // slow/hanging call -- see its own doc comment in `ast.rs`.
    let src = r#"
        fn call_api_slow(txn_id: str, amount: i64) -> i64 {
            sleep_ms(2000)
            return amount
        }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api_slow(txn_id, 1) timeout 1
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    let start = std::time::Instant::now();
    match run(src) {
        Err(msg) => {
            assert!(msg.contains("timed out after 1s"), "expected a timeout error, got: {msg}");
            assert!(start.elapsed().as_secs() < 2, "should time out around 1s, not wait for the full 2s sleep");
        }
        other => panic!("expected an Err (timeout), got {other:?}"),
    }
}

// ---- `precheck` -------------------------------------------------------

#[test]
fn precheck_false_aborts_before_network_runs() {
    let src = r#"
        fn always_false() -> bool { return false }
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                precheck: always_false()
                network:  call_api(txn_id, 1)
                verify:   check(network)
                commit:   update_db(network)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(false)));
}

#[test]
fn precheck_true_lets_the_block_proceed() {
    let src = r#"
        fn always_true() -> bool { return true }
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                precheck: always_true()
                network:  call_api(txn_id, 1)
                verify:   check(network)
                commit:   update_db(network)
            }
        }
    "#;
    assert_eq!(run(src), Ok(Value::Bool(true)));
}

#[test]
fn precheck_must_return_bool() {
    let src = r#"
        fn not_bool() -> i64 { return 1 }
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                precheck: not_bool()
                network:  call_api(txn_id, 1)
                verify:   check(network)
                commit:   update_db(network)
            }
        }
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::TransactPrecheckMustReturnBool { found: nirdosha::ast::Ty::I64 }
    ));
}

// ---- static rejections ----------------------------------------------------

#[test]
fn verify_must_return_bool() {
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn not_bool(resp: i64) -> i64 { return resp }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, 1)
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
fn network_must_pass_txn_id() {
    let src = r#"
        fn call_api(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api(1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert!(matches!(first_type_error(src), TypeErrorKind::TransactNetworkMustUseTxnId));
}

#[test]
fn verify_args_must_be_network_or_txn_id() {
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64, extra: i64) -> bool { return resp > extra }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, 1)
                verify:  check(network, 5)
                commit:  update_db(network)
            }
        }
    "#;
    assert!(matches!(first_type_error(src), TypeErrorKind::TransactVerifyArgsMustBeImplicitBindings));
}

#[test]
fn a_db_handle_cannot_cross_the_durability_boundary() {
    let src = r#"
        fn call_api(txn_id: str, conn: db) -> i64 { return 1 }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return match db_connect("x.db") {
                Ok(conn) => transact {
                    network: call_api(txn_id, conn)
                    verify:  check(network)
                    commit:  update_db(network)
                },
                Err(e) => false,
            }
        }
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::TransactValueNotDurable { where_, .. } if where_ == "network"
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
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, 1)
                verify:  check(network)
            }
        }
    "#;
    assert!(is_parse_error(src));
}

#[test]
fn network_and_verify_bindings_do_not_escape_the_transact_block() {
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> i64 {
            transact {
                network: call_api(txn_id, 1)
                verify:  check(network)
                commit:  update_db(network)
            }
            return network
        }
    "#;
    assert!(matches!(first_type_error(src), TypeErrorKind::UnknownVar(name) if name == "network"));
}

// ---- codegen: interpreter-only for now, per docs/TRANSACT.md's own decision --

#[test]
fn transact_is_rejected_by_codegen_not_silently_miscompiled() {
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {
            transact {
                network: call_api(txn_id, 1)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should type-check");
    assert!(nirdosha::codegen::check_supported(&program).is_err());
}
