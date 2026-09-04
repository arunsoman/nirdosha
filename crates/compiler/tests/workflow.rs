//! End-to-end tests for `workflow { ... }` (`docs/WORKFLOW.md`): parsing,
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

/// A `VerifiedIdentity` value carrying `roles` in its embedded
/// `claims_json`, field order matching `ast::prelude_structs`'s own
/// declaration (`subject, issuer, audience, expires_at, issued_at,
/// claims_json`) — `advance_<workflow>` (`docs/WORKFLOW.md`'s "state
/// ownership" section) takes one as its leading param, so every direct
/// `call_named("advance_...", ...)` in this file needs one to construct
/// by hand (a real `nirdosha serve` request gets this injected from the
/// bearer token instead — see `serve.rs::dispatch` — but these tests
/// drive `Interpreter` directly, the same "go around the language" shape
/// `link_transition_mints_a_single_use_token_that_advances_the_state`'s
/// own doc comment already uses for reading a minted link token).
fn mock_identity(subject: &str, roles: &[&str]) -> Value {
    let claims = serde_json::json!({ "roles": roles }).to_string();
    Value::Struct(
        Arc::from("VerifiedIdentity"),
        Arc::from(vec![
            Value::Str(Arc::from(subject)),
            Value::Str(Arc::from("test-issuer")),
            Value::Str(Arc::from("test-audience")),
            Value::Int(9_999_999_999),
            Value::Int(0),
            Value::Str(Arc::from(claims.as_str())),
        ]),
    )
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
    let result = interp.call_named("start_onboarding", &[Value::Enum(Arc::from("Option"), Arc::from("None"), Arc::from(vec![])), data]).expect("start_onboarding should not trap");
    let Value::Int(instance_id) = ok_payload(&result) else { panic!("expected Ok(i64)") };
    assert!(instance_id > 0);
}

#[test]
fn advance_with_no_such_event_is_a_clean_err_not_a_trap() {
    let program = build_program(SRC);
    let log_path = temp_path("no-such-transition");
    let interp = Interpreter::new(Arc::new(program), Arc::from(SRC)).with_workflow_log_path(log_path);
    let data = Value::Struct(Arc::from("OnboardingData"), Arc::from(vec![Value::Str(Arc::from("bob"))]));
    let start_result = interp.call_named("start_onboarding", &[Value::Enum(Arc::from("Option"), Arc::from("None"), Arc::from(vec![])), data]).expect("start should not trap");
    let Value::Int(instance_id) = ok_payload(&start_result) else { panic!("expected Ok(i64)") };

    // `Onboarding` only declares transitions on `Start`, reachable via the
    // `link`-only `Verify` event -- `advance_onboarding` has no ordinary
    // (non-link) event to offer here, so this exercises the
    // instance-not-found path on a bogus id instead, proving that shape
    // returns a clean `Err`, not a trap.
    let bogus = interp.call_named("advance_onboarding", &[mock_identity("bob", &[]), Value::Int(instance_id + 1_000_000), Value::Enum(Arc::from("OnboardingEvent"), Arc::from("Verify"), Arc::from(vec![])), Value::Json(Arc::new(serde_json::Value::Null))]).expect("advance should not trap");
    assert_eq!(err_variant(&bogus), "InstanceNotFound");
}

#[test]
fn link_transition_mints_a_single_use_token_that_advances_the_state() {
    let program = build_program(SRC);
    let log_path = temp_path("link-transition");
    let interp = Interpreter::new(Arc::new(program), Arc::from(SRC)).with_workflow_log_path(log_path.clone());
    let data = Value::Struct(Arc::from("OnboardingData"), Arc::from(vec![Value::Str(Arc::from("carol"))]));
    let start_result = interp.call_named("start_onboarding", &[Value::Enum(Arc::from("Option"), Arc::from("None"), Arc::from(vec![])), data]).expect("start should not trap");
    let Value::Int(instance_id) = ok_payload(&start_result) else { panic!("expected Ok(i64)") };

    // Read the token `bind_link_tokens` minted during `Start`'s `on_entry`
    // straight out of the durable store -- nothing in `.nir` source
    // exposes it (it's delivered via `send_email`'s `vars` in real usage,
    // via `link_Verify`), so a test has to go around the language the
    // same way an email provider's webhook payload would.
    let wlog = WorkflowLog::open(&nirdosha::durability::LogTarget::Sqlite(log_path.clone())).expect("workflow log should open");
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
/// against, see the file's own doc comment and `docs/WORKFLOW.md`'s
/// "Deliberate non-goals"), but still proves the whole worked example —
/// `data`/`link`/`notify`/`send_email`/the `EmailProviderConfig`
/// "communication control" struct together — parses, desugars, and
/// typechecks cleanly end to end.
#[test]
fn kyc_onboarding_example_typechecks() {
    let src = std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples/kyc_onboarding.nir"))
        .expect("examples/kyc_onboarding.nir should exist and be readable");
    build_program(&src);
}

// ---- crash-durable action replay -------------------------------------
//
// `WorkflowLog::begin_pending_action` durably records an `on_entry`/
// `on_exit` action's callee + arguments *before* `run_workflow_actions`
// ever dispatches it (`workflow_log.rs`'s own doc comment) -- so a crash
// between that write and the action actually running must still be
// resumable by `Interpreter::replay_pending_workflow_actions` on the next
// startup, from nothing but the durable row itself (no live `.nir` call
// site, no environment). This constructs exactly that "recorded, never
// dispatched" row by hand (`begin_pending_action` directly, bypassing
// `run_workflow_actions`) -- the narrowest possible crash window, and the
// same one `tests/transact_durability.rs`'s own replay tests target for
// `transact`.

const REPLAY_SRC: &str = r#"
        fn mark_ran_inner(conn: db, instance_id: i64) -> bool {
            let t1: i64 = match db_execute(conn, "CREATE TABLE IF NOT EXISTS ran (instance_id INTEGER)") { Ok(n) => n, Err(e) => -1 }
            let t2: i64 = match db_execute(conn, "INSERT INTO ran (instance_id) VALUES (?)", instance_id) { Ok(n) => n, Err(e) => -1 }
            return true
        }

        fn mark_ran(instance_id: i64) -> bool {
            return match db_connect("__OBSERVE_DB__") {
                Ok(conn) => mark_ran_inner(conn, instance_id),
                Err(e) => false,
            }
        }
    "#;

#[test]
fn replay_resumes_an_action_that_was_logged_but_never_dispatched() {
    let observe_db = temp_path("replay-observe");
    let src = REPLAY_SRC.replace("__OBSERVE_DB__", observe_db.to_str().expect("temp path should be valid UTF-8"));
    let program = build_program(&src);
    let workflow_log_path = temp_path("replay-workflow-log");

    // Simulate "crashed right after logging intent, before ever calling
    // `mark_ran`" -- write the pending row directly, the same shape
    // `Interpreter::run_workflow_actions` would have written.
    {
        let wlog = WorkflowLog::open(&nirdosha::durability::LogTarget::Sqlite(workflow_log_path.clone())).expect("workflow log should open");
        let args_json = serde_json::json!([99]).to_string();
        wlog.begin_pending_action(1, "ReplayDemo", "SomeState", "entry", 0, "mark_ran", &args_json, 0)
            .expect("begin_pending_action should succeed");
    }

    // Nothing should have run yet -- no fresh `Interpreter` has replayed
    // this row, and `mark_ran` was never called live.
    assert!(!observe_db.exists(), "mark_ran must not have run before replay");

    let interp = Interpreter::new(Arc::new(program), Arc::from(src.as_str())).with_workflow_log_path(workflow_log_path.clone());
    let outcomes = interp.replay_pending_workflow_actions().expect("replay should not trap");
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        nirdosha::interpreter::WorkflowReplayOutcome::Resolved { instance_id, action } => {
            assert_eq!(*instance_id, 1);
            assert_eq!(action, "mark_ran");
        }
        other => panic!("expected Resolved, got {other:?}"),
    }

    // `mark_ran` really ran (a real, separate SQLite file it created) --
    // and the pending row is gone, so a second replay finds nothing left.
    assert!(observe_db.exists(), "replay should have actually dispatched mark_ran");
    let wlog = WorkflowLog::open(&nirdosha::durability::LogTarget::Sqlite(workflow_log_path.clone())).expect("workflow log should reopen");
    assert!(wlog.list_pending_actions().expect("query should succeed").is_empty());
}

#[test]
fn replay_reports_stuck_for_a_callee_that_no_longer_exists() {
    // A minimal, unrelated program -- no `mark_ran` in it at all -- so
    // replaying a row naming that callee can't possibly find it, the same
    // way a program edited between a crash and a restart legitimately
    // could leave a stale pending row behind.
    let program = build_program("fn unrelated() -> bool { return true }");
    let workflow_log_path = temp_path("replay-stuck");
    {
        let wlog = WorkflowLog::open(&nirdosha::durability::LogTarget::Sqlite(workflow_log_path.clone())).expect("workflow log should open");
        let args_json = serde_json::json!([1]).to_string();
        wlog.begin_pending_action(2, "ReplayDemo", "SomeState", "entry", 0, "mark_ran", &args_json, 0)
            .expect("begin_pending_action should succeed");
    }
    let interp =
        Interpreter::new(Arc::new(program), Arc::from("")).with_workflow_log_path(workflow_log_path.clone());
    let outcomes = interp.replay_pending_workflow_actions().expect("replay should not trap");
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        nirdosha::interpreter::WorkflowReplayOutcome::Stuck { instance_id, action, .. } => {
            assert_eq!(*instance_id, 2);
            assert_eq!(action, "mark_ran");
        }
        other => panic!("expected Stuck, got {other:?}"),
    }
    // Left exactly as it was -- still pending, not silently dropped.
    let wlog = WorkflowLog::open(&nirdosha::durability::LogTarget::Sqlite(workflow_log_path.clone())).expect("workflow log should reopen");
    assert_eq!(wlog.list_pending_actions().expect("query should succeed").len(), 1);
}

// ---- on_entry/on_exit action calls are really type-checked ------------
//
// `check_workflow_decl` used to only check that `data.<field>`/
// `link_<Event>` *names* existed, never that an action call's arguments
// actually matched its callee's declared parameter types -- so a
// `needs_i64(data.name)` where `name: str` typechecked cleanly and would
// only have failed at runtime, inside `self.call`, far from the actual
// mistake. Fixed by routing every action call through this checker's own
// `infer`/`infer_call`, the same machinery an ordinary function body's
// calls already go through.

#[test]
fn action_call_argument_type_mismatch_is_a_compile_error() {
    let toks = Lexer::new(
        r#"
        fn needs_i64(x: i64) -> bool { return true }

        workflow Demo {
            data { name: str }
            state Start {
                on_entry {
                    needs_i64(data.name)
                }
                on Go -> Done
            }
            state Done terminal {}
        }
    "#,
    )
    .tokenize()
    .expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    let errors = typecheck_optional_main(&program).expect_err("a str passed where i64 is expected must be rejected");
    assert!(
        errors.iter().any(|e| matches!(&e.kind, nirdosha::typeck::TypeErrorKind::TypeMismatch { .. })),
        "expected a TypeMismatch, got {errors:?}"
    );
}

#[test]
fn action_call_wrong_arity_is_a_compile_error() {
    let toks = Lexer::new(
        r#"
        fn needs_one_arg(x: i64) -> bool { return true }

        workflow Demo {
            data { }
            state Start {
                on_entry {
                    needs_one_arg(instance_id, instance_id)
                }
                on Go -> Done
            }
            state Done terminal {}
        }
    "#,
    )
    .tokenize()
    .expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    let errors = typecheck_optional_main(&program).expect_err("calling a 1-arg fn with 2 args must be rejected");
    assert!(
        errors.iter().any(|e| matches!(&e.kind, nirdosha::typeck::TypeErrorKind::ArityMismatch { .. })),
        "expected an ArityMismatch, got {errors:?}"
    );
}

#[test]
fn action_call_unknown_data_field_is_a_compile_error() {
    let toks = Lexer::new(
        r#"
        fn noop(x: i64) -> bool { return true }

        workflow Demo {
            data { name: str }
            state Start {
                on_entry {
                    noop(data.not_a_real_field)
                }
                on Go -> Done
            }
            state Done terminal {}
        }
    "#,
    )
    .tokenize()
    .expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).expect_err("an unknown `data` field must be rejected");
}

#[test]
fn action_call_link_binding_out_of_scope_is_a_compile_error() {
    // `link_Verify` only exists in the `on_entry` of a state that
    // declares `on link Verify -> ...` as one of *its own* outgoing
    // transitions -- referencing it from a state that doesn't must fail,
    // the same as referencing any other undeclared variable.
    let toks = Lexer::new(
        r#"
        fn noop(t: SomeLinkToken) -> bool { return true }

        struct SomeLinkToken { value: str }

        workflow Demo {
            data { }
            state Start {
                on_entry {
                    noop(link_Verify)
                }
                on Go -> Done
            }
            state Done terminal {
                on link Verify -> Done
            }
        }
    "#,
    )
    .tokenize()
    .expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).expect_err("a link binding from a different state must be out of scope");
}
