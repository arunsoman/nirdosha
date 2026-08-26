//! End-to-end demonstration of the two new verbatim-verification
//! constructs against the real, checked-in extraction file
//! `scratch/extracted_typed_v1.json` — not a synthetic fixture.
//!
//! - `workflow_conformance::check_workflow_conformance` — structural,
//!   exact (no solver): does a real `workflow { ... }` declare exactly
//!   the states/transitions/data fields the extraction says it should?
//! - `contract_check::check_fn_contract` — Tier-1 SMT: does a real,
//!   pure, loop-free function actually satisfy a `pre_logic`/
//!   `post_logic` Hoare pair, proven for *every* input its parameter
//!   types admit, not just the ones a hand-written test happened to
//!   try?
//!
//! The three `.nir` workflow snippets below are mirrored verbatim from
//! `compiler/tests/trade_payment_approval_workflow_check.rs` (which the
//! extraction file's own `compiled_and_verified` field on each workflow
//! entry points at) — same reason that file gives for inlining rather
//! than `include_str!`-ing the real, much larger, DB-backed
//! `trade_finance.nir`: a small, self-contained snippet a unit test can
//! actually typecheck/own-check/run on its own.

use std::collections::HashMap;

use nirdosha::contract_check::{check_fn_contract, ContractCheckResult};
use nirdosha::extraction_schema::ExtractionFile;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck_optional_main;
use nirdosha::workflow_conformance::check_workflow_conformance;

const EXTRACTED_JSON: &str = include_str!("../../scratch/extracted_typed_v1.json");

fn load_extraction() -> ExtractionFile {
    serde_json::from_str(EXTRACTED_JSON).expect("scratch/extracted_typed_v1.json should match extraction_schema::ExtractionFile")
}

fn build_program(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).unwrap_or_else(|e| panic!("typecheck should succeed: {e:?}"));
    check_ownership(&program).expect("ownership check should succeed");
    program
}

/// Mirrored verbatim from `trade_payment_approval_workflow_check.rs::
/// trade_payment_approval_workflow_compiles`.
const TRADE_PAYMENT_APPROVAL_NIR: &str = r#"
    struct Text {
        value: str,
    }

    fn required_eyes_for_amount(amount_cents: i64) -> i64 {
        return if amount_cents >= 5000000 { 2 } else { 1 }
    }

    fn notify_checker(instance_id: i64) -> bool {
        return match db_connect("trade_payment_approval.db") {
            Ok(conn) => match mq_connect("127.0.0.1", 6379) {
                Ok(mq_conn) => match json_parse("{}") {
                    Ok(vars) => match notify(conn, mq_conn, ByRole("checker"), "trade_payment_pending_approval", vars) {
                        Ok(sent) => sent,
                        Err(e) => false,
                    },
                    Err(e) => false,
                },
                Err(e) => false,
            },
            Err(e) => false,
        }
    }

    fn notify_six_eyes_reviewer(instance_id: i64) -> bool {
        return match db_connect("trade_payment_approval.db") {
            Ok(conn) => match mq_connect("127.0.0.1", 6379) {
                Ok(mq_conn) => match json_parse("{}") {
                    Ok(vars) => match notify(conn, mq_conn, ByRole("six_eyes_reviewer"), "trade_payment_pending_six_eyes_review", vars) {
                        Ok(sent) => sent,
                        Err(e) => false,
                    },
                    Err(e) => false,
                },
                Err(e) => false,
            },
            Err(e) => false,
        }
    }

    fn notify_treasury_decided(instance_id: i64, template: Text) -> bool {
        return match db_connect("trade_payment_approval.db") {
            Ok(conn) => match mq_connect("127.0.0.1", 6379) {
                Ok(mq_conn) => match json_parse("{}") {
                    Ok(vars) => match notify(conn, mq_conn, ByRole("treasury"), template.value, vars) {
                        Ok(sent) => sent,
                        Err(e) => false,
                    },
                    Err(e) => false,
                },
                Err(e) => false,
            },
            Err(e) => false,
        }
    }

    workflow TradePaymentApproval {
        data {
            payment_id: i64,
            amount_cents: i64,
        }

        state PendingClassification {
            on Classified -> PendingMakerChecker
            on ClassifiedHighValue -> PendingSixEyes
        }

        state PendingMakerChecker {
            on_entry {
                notify_checker(instance_id)
            }
            on Approved -> Approved
            on Rejected -> Rejected
        }

        state PendingSixEyes {
            on_entry {
                notify_six_eyes_reviewer(instance_id)
            }
            on Approved -> Approved
            on Rejected -> Rejected
        }

        state Approved terminal {
            on_entry {
                notify_treasury_decided(instance_id, Text("trade_payment_approved"))
            }
        }

        state Rejected terminal {
            on_entry {
                notify_treasury_decided(instance_id, Text("trade_payment_rejected"))
            }
        }
    }
"#;

const ESCROW_TRANCHE_RELEASE_NIR: &str = r#"
    fn release_tranche(instance_id: i64, purchase_order_id: i64, tranche_amount_cents: i64) -> bool {
        return match db_connect("escrow_tranche_release.db") {
            Ok(conn) => match db_execute(conn, "UPDATE purchase_order SET escrow_balance_cents = escrow_balance_cents - ? WHERE id = ?", tranche_amount_cents, purchase_order_id) {
                Ok(n) => n >= 0,
                Err(e) => false,
            },
            Err(e) => false,
        }
    }

    workflow EscrowTrancheRelease {
        data {
            purchase_order_id: i64,
            tranche_amount_cents: i64,
        }

        state EscrowHeld {
            on MilestoneVerified -> TrancheReleased
        }

        state TrancheReleased terminal {
            on_entry {
                release_tranche(instance_id, data.purchase_order_id, data.tranche_amount_cents)
            }
        }
    }
"#;

const WALLET_SETTLEMENT_NIR: &str = r#"
    fn transfer_to_partner_bank_account(instance_id: i64, channel_partner_id: i64, swept_balance_cents: i64) -> bool {
        return match db_connect("wallet_settlement.db") {
            Ok(conn) => match db_execute(conn, "UPDATE channel_partner_wallet SET balance_cents = 0 WHERE channel_partner_id = ?", channel_partner_id) {
                Ok(n) => n >= 0,
                Err(e) => false,
            },
            Err(e) => false,
        }
    }

    workflow WalletSettlement {
        data {
            channel_partner_id: i64,
            swept_balance_cents: i64,
        }

        state Settled terminal {
            on_entry {
                transfer_to_partner_bank_account(instance_id, data.channel_partner_id, data.swept_balance_cents)
            }
        }
    }
"#;

fn extracted_workflow<'a>(file: &'a ExtractionFile, id: &str) -> &'a nirdosha::extraction_schema::ExtractedWorkflow {
    file.workflows.iter().find(|w| w.id == id).unwrap_or_else(|| panic!("no workflow with id {id} in the extraction file"))
}

#[test]
fn wf_trdpay_001_matches_the_real_workflow_verbatim() {
    let file = load_extraction();
    let program = build_program(TRADE_PAYMENT_APPROVAL_NIR);
    let report = check_workflow_conformance(&program, extracted_workflow(&file, "WF-TRDPAY-001"));
    assert!(report.is_exact_match(), "conformance mismatches: {:#?}", report.mismatches);
}

#[test]
fn wf_trdpay_002_escrow_tranche_release_matches_verbatim() {
    let file = load_extraction();
    let program = build_program(ESCROW_TRANCHE_RELEASE_NIR);
    let report = check_workflow_conformance(&program, extracted_workflow(&file, "WF-TRDPAY-002"));
    assert!(report.is_exact_match(), "conformance mismatches: {:#?}", report.mismatches);
}

#[test]
fn wf_comm_001_wallet_settlement_matches_verbatim() {
    let file = load_extraction();
    let program = build_program(WALLET_SETTLEMENT_NIR);
    let report = check_workflow_conformance(&program, extracted_workflow(&file, "WF-COMM-001"));
    assert!(report.is_exact_match(), "conformance mismatches: {:#?}", report.mismatches);
}

/// Proves the checker actually catches a real drift, rather than
/// trivially reporting a match — drop one transition from the real
/// workflow and confirm it's reported by name, not silently ignored.
#[test]
fn conformance_check_actually_detects_a_missing_transition() {
    let file = load_extraction();
    let mutated = TRADE_PAYMENT_APPROVAL_NIR.replacen("on ClassifiedHighValue -> PendingSixEyes\n", "", 1);
    let program = build_program(&mutated);
    let report = check_workflow_conformance(&program, extracted_workflow(&file, "WF-TRDPAY-001"));
    assert!(!report.is_exact_match());
    let found = report.mismatches.iter().any(|m| {
        matches!(
            m,
            nirdosha::workflow_conformance::Mismatch::MissingTransition { from, event, .. }
                if from == "PendingClassification" && event == "ClassifiedHighValue"
        )
    });
    assert!(found, "expected a MissingTransition for PendingClassification/ClassifiedHighValue, got {:#?}", report.mismatches);
}

/// Same idea for a terminal-flag drift. Marking `PendingMakerChecker`
/// `terminal` (rather than un-marking an already-terminal, transition-
/// free state like `Settled`) keeps the mutated program typecheckable —
/// `WorkflowStateHasNoTransitions` only requires a *non*-terminal state
/// to have a way out; a terminal state with existing transitions still
/// compiles fine, exactly the shape needed to isolate this one flag.
#[test]
fn conformance_check_actually_detects_a_terminal_flag_mismatch() {
    let file = load_extraction();
    let mutated = TRADE_PAYMENT_APPROVAL_NIR.replacen("state PendingMakerChecker {", "state PendingMakerChecker terminal {", 1);
    let program = build_program(&mutated);
    let report = check_workflow_conformance(&program, extracted_workflow(&file, "WF-TRDPAY-001"));
    assert!(!report.is_exact_match());
    let found = report.mismatches.iter().any(|m| {
        matches!(
            m,
            nirdosha::workflow_conformance::Mismatch::TerminalFlagMismatch { name, extracted_terminal: false, actual_terminal: true }
                if name == "PendingMakerChecker"
        )
    });
    assert!(found, "expected a TerminalFlagMismatch for PendingMakerChecker, got {:#?}", report.mismatches);
}

fn wf_trdpay_001_routing_fn(file: &ExtractionFile) -> &nirdosha::extraction_schema::ExtractedRoutingFn {
    extracted_workflow(file, "WF-TRDPAY-001").routing_fn.as_ref().expect("WF-TRDPAY-001 has a routing_fn")
}

/// The real Tier-1 proof: `required_eyes_for_amount`'s actual body
/// satisfies the extraction's own `post_logic`
/// (`"(result == 2) == (amount_cents >= high_value_threshold)"`) for
/// *every* `i64` `amount_cents` — not just a handful of example calls —
/// once told what the PRD's abstract "configured high-value threshold"
/// concretely is today (the code's own hardcoded 5,000,000 cents,
/// `ROADMAP.md` A9). This is real: Z3 is asked to find a violation and
/// fails, over the whole `i64` domain.
#[test]
fn required_eyes_for_amount_satisfies_extracted_post_logic_when_threshold_is_bound() {
    let file = load_extraction();
    let program = build_program(TRADE_PAYMENT_APPROVAL_NIR);
    let routing_fn = wf_trdpay_001_routing_fn(&file);
    let bindings = HashMap::from([("high_value_threshold".to_string(), 5_000_000i64)]);
    let result = check_fn_contract(&program, &routing_fn.name, &routing_fn.pre_logic, &routing_fn.post_logic, &bindings);
    assert_eq!(result, ContractCheckResult::Proved, "unexpected result: {result:?}");
}

/// §7.1a's exact case, made a concrete, honest outcome instead of a
/// confusing solver failure: the extraction's `post_logic` references
/// `high_value_threshold`, which is not one of `required_eyes_for_
/// amount`'s own parameters — the real code hardcodes the number
/// instead. Without a supplied binding, this is correctly reported as
/// unresolvable, not silently treated as "any value" or skipped.
#[test]
fn required_eyes_for_amount_post_logic_is_unbound_without_a_threshold_binding() {
    let file = load_extraction();
    let program = build_program(TRADE_PAYMENT_APPROVAL_NIR);
    let routing_fn = wf_trdpay_001_routing_fn(&file);
    let result = check_fn_contract(&program, &routing_fn.name, &routing_fn.pre_logic, &routing_fn.post_logic, &HashMap::new());
    assert_eq!(result, ContractCheckResult::UnboundIdentifier("high_value_threshold".to_string()));
}

/// The other side of the same coin: if the *wrong* concrete threshold
/// is supplied (one that doesn't match what the code actually
/// hardcodes), the checker must find a real counterexample, not report
/// `Proved` anyway.
#[test]
fn required_eyes_for_amount_post_logic_fails_against_a_mismatched_threshold() {
    let file = load_extraction();
    let program = build_program(TRADE_PAYMENT_APPROVAL_NIR);
    let routing_fn = wf_trdpay_001_routing_fn(&file);
    let bindings = HashMap::from([("high_value_threshold".to_string(), 6_000_000i64)]);
    let result = check_fn_contract(&program, &routing_fn.name, &routing_fn.pre_logic, &routing_fn.post_logic, &bindings);
    match result {
        ContractCheckResult::Counterexample { violated_predicate, bindings, result } => {
            assert_eq!(violated_predicate, routing_fn.post_logic[0]);
            let amount_cents = bindings.iter().find(|(n, _)| n == "amount_cents").map(|(_, v)| *v).expect("amount_cents binding");
            // Real, independent check: does the mismatch actually reproduce
            // against the real function, run for real (not just trusted
            // from the SMT model)?
            let expected_result = if amount_cents >= 5_000_000 { 2 } else { 1 };
            assert_eq!(result, Some(expected_result));
            let predicate_holds_at_5m_threshold = (expected_result == 2) == (amount_cents >= 5_000_000);
            let predicate_holds_at_6m_threshold = (expected_result == 2) == (amount_cents >= 6_000_000);
            assert!(predicate_holds_at_5m_threshold, "sanity: the real code's own invariant should still hold");
            assert!(!predicate_holds_at_6m_threshold, "counterexample should actually violate the 6,000,000 threshold reading");
        }
        other => panic!("expected a Counterexample, got {other:?}"),
    }
}
