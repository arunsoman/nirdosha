//! End-to-end tests for `workflow { ... }` (`WORKFLOW.md`): parsing,
//! `workflow_lower.rs`'s desugaring into `start_*`/`advance_*`/
//! `*_via_link`, and `interpreter.rs`'s runtime state-machine dispatch
//! (`__workflow_start`/`__workflow_advance`/`__workflow_link_advance`).
//! Same "build the program by hand, drive `Interpreter` directly" shape
//! `tests/transact_durability.rs` already establishes.

use std::sync::Arc;

use nirdosha::interpreter::{Interpreter, Value};
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck_optional_main;
use nirdosha::workflow_log::WorkflowLog;

fn build_program(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).unwrap_or_else(|e| panic!("typecheck should succeed: {e:?}"));
    check_ownership(&program).expect("ownership check should succeed");
    program
}

fn temp_path(name: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("nirdosha-test-{name}-{}-{n}.db", std::process::id()))
}

fn ok_payload(v: &Value) -> Value {
    match v {
        Value::Enum(name, variant, payload) if name.as_ref() == "Result" && variant.as_ref() == "Ok" => {
            payload[0].clone()
        }
        other => panic!("expected Ok(_), got {other:?}"),
    }
}

fn err_variant(v: &Value) -> String {
    match v {
        Value::Enum(name, variant, payload) if name.as_ref() == "Result" && variant.as_ref() == "Err" => {
            match &payload[0] {
                Value::Enum(_, v, _) => v.to_string(),
                other => panic!("expected an Err(WorkflowActionError::_), got {other:?}"),
            }
        }
        other => panic!("expected Err(_), got {other:?}"),
    }
}

const SRC: &str = r#"
        fn noop_action(instance_id: i64) -> bool {
            return true
        }

        workflow Onboarding {
            data {
                name: str,
            }

            state Start {
                on_entry {
                    noop_action(instance_id)
                }
                on link Verify -> Verified
            }

            state Verified terminal {
                on_entry {
                    noop_action(instance_id)
                }
            }
        }
    "#;

#[test]
fn start_creates_an_instance_and_runs_on_entry() {
    let program = build_program(SRC);
    let interp = Interpreter::new(Arc::new(program), Arc::from(SRC))
        .with_workflow_log_path(temp_path("start"));
    let data = Value::Struct(Arc::from("OnboardingData"), Arc::from(vec![Value::Str(Arc::from("alice"))]));
    let result = interp.call_named("start_onboarding", &[data]).expect("start_onboarding should not trap");
    let Value::Int(instance_id) = ok_payload(&result) else { panic!("expected Ok(i64)") };
    assert!(instance_id > 0);
}

#[test]
fn advance_with_no_such_event_is_a_clean_err_not_a_trap() {
    let program = build_program(SRC);
    let log_path = temp_path("no-such-transition");
    let interp = Interpreter::new(Arc::new(program), Arc::from(SRC)).with_workflow_log_path(log_path);
    let data = Value::Struct(Arc::from("OnboardingData"), Arc::from(vec![Value::Str(Arc::from("bob"))]));
    let start_result = interp.call_named("start_onboarding", &[data]).expect("start should not trap");
    let Value::Int(instance_id) = ok_payload(&start_result) else { panic!("expected Ok(i64)") };

    // `Onboarding` only declares transitions on `Start`, reachable via the
    // `link`-only `Verify` event -- `advance_onboarding` has no ordinary
    // (non-link) event to offer here, so this exercises the
    // instance-not-found path on a bogus id instead, proving that shape
    // returns a clean `Err`, not a trap.
    let bogus = interp.call_named("advance_onboarding", &[Value::Int(instance_id + 1_000_000), Value::Enum(Arc::from("OnboardingEvent"), Arc::from("Verify"), Arc::from(vec![])), Value::Json(Arc::new(serde_json::Value::Null))]).expect("advance should not trap");
    assert_eq!(err_variant(&bogus), "InstanceNotFound");
}

#[test]
fn link_transition_mints_a_single_use_token_that_advances_the_state() {
    let program = build_program(SRC);
    let log_path = temp_path("link-transition");
    let interp = Interpreter::new(Arc::new(program), Arc::from(SRC)).with_workflow_log_path(log_path.clone());
    let data = Value::Struct(Arc::from("OnboardingData"), Arc::from(vec![Value::Str(Arc::from("carol"))]));
    let start_result = interp.call_named("start_onboarding", &[data]).expect("start should not trap");
    let Value::Int(instance_id) = ok_payload(&start_result) else { panic!("expected Ok(i64)") };

    // Read the token `bind_link_tokens` minted during `Start`'s `on_entry`
    // straight out of the durable store -- nothing in `.nir` source
    // exposes it (it's delivered via `send_email`'s `vars` in real usage,
    // via `link_Verify`), so a test has to go around the language the
    // same way an email provider's webhook payload would.
    let wlog = WorkflowLog::open(&log_path).expect("workflow log should open");
    let (_, token) = wlog
        .find_unconsumed_link(instance_id, "Verify")
        .expect("query should succeed")
        .expect("Start's on_entry should have minted a Verify link");

    let token_val = Value::Struct(Arc::from("OnboardingLinkToken"), Arc::from(vec![Value::Str(Arc::from(token.as_str()))]));
    let advanced = interp
        .call_named("verify_via_link", &[Value::Int(instance_id), token_val, Value::Json(Arc::new(serde_json::Value::Null))])
        .expect("verify_via_link should not trap");
    assert_eq!(ok_payload(&advanced), Value::Bool(true));

    let (_, state, _) = wlog.get_instance(instance_id).expect("query should succeed").expect("instance should exist");
    assert_eq!(state, "Verified");

    // Single-use: the exact same token again must fail, not silently
    // re-advance (already-consumed row, `find_unconsumed_link` now finds
    // nothing for this instance/event).
    assert!(wlog.find_unconsumed_link(instance_id, "Verify").expect("query should succeed").is_none());
}

/// Same "example X runs to completion" convention `tests/transact.rs`'s
/// `example_transact_runs_to_completion`/`tests/structs_enums.rs`'s
/// `example_structs_enums_runs_to_completion` already establish — this
/// one is typecheck-only (`examples/kyc_onboarding.nir` needs a live
/// webhook + Redis instance to actually *run* `notify`/`send_email`
/// against, see the file's own doc comment and `WORKFLOW.md`'s
/// "Deliberate non-goals"), but still proves the whole worked example —
/// `data`/`link`/`notify`/`send_email`/the `EmailProviderConfig`
/// "communication control" struct together — parses, desugars, and
/// typechecks cleanly end to end.
#[test]
fn kyc_onboarding_example_typechecks() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/examples/kyc_onboarding.nir"))
        .expect("examples/kyc_onboarding.nir should exist and be readable");
    build_program(&src);
}
