//! Same idea as `extracted_typed_v1_verification.rs`, run against
//! `scratch/extracted_typed_v2.json` — a full Module 4 pass against the
//! *current* `scratch/prompt_v2.txt` (owner_role/label/required_decisions
//! on workflow states; required_role/implements/input_fields on user
//! stories), not just the original 3-per-kind sample. All 6 of its
//! workflows and both of its `routing_fn`s are checked for real here,
//! not just read — this is the actual answer to "does this extraction
//! meet expectations," not a visual skim.

use std::collections::HashMap;

use nirdosha::contract_check::{check_fn_contract, ContractCheckResult};
use nirdosha::extraction_schema::ExtractionFile;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck_optional_main;
use nirdosha::workflow_conformance::check_workflow_conformance;

const EXTRACTED_JSON: &str = include_str!("../../../scratch/extracted_typed_v2.json");

fn load_extraction() -> ExtractionFile {
    serde_json::from_str(EXTRACTED_JSON).expect("scratch/extracted_typed_v2.json should match extraction_schema::ExtractionFile")
}

fn build_program(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).unwrap_or_else(|e| panic!("typecheck should succeed: {e:?}"));
    check_ownership(&program).expect("ownership check should succeed");
    program
}

fn extracted_workflow<'a>(file: &'a ExtractionFile, id: &str) -> &'a nirdosha::extraction_schema::ExtractedWorkflow {
    file.workflows.iter().find(|w| w.id == id).unwrap_or_else(|| panic!("no workflow with id {id} in the extraction file"))
}

fn extracted_story<'a>(file: &'a ExtractionFile, id: &str) -> &'a nirdosha::extraction_schema::ExtractedUserStory {
    file.user_stories.iter().find(|s| s.id == id).unwrap_or_else(|| panic!("no user story with id {id} in the extraction file"))
}

// ---- Mirrored verbatim from trade_payment_approval_workflow_check.rs --

const TRADE_PAYMENT_APPROVAL_NIR: &str = r#"
    struct Text { value: str }
    fn required_eyes_for_amount(amount_cents: i64) -> i64 {
        return if amount_cents >= 5000000 { 2 } else { 1 }
    }
    fn notify_checker(instance_id: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("checker"), "t", vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    fn notify_six_eyes_reviewer(instance_id: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("six_eyes_reviewer"), "t", vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    fn notify_treasury_decided(instance_id: i64, template: Text) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("treasury"), template.value, vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    workflow TradePaymentApproval {
        data { payment_id: i64, amount_cents: i64 }
        state PendingClassification {
            on Classified -> PendingMakerChecker
            on ClassifiedHighValue -> PendingSixEyes
        }
        state PendingMakerChecker {
            on_entry { notify_checker(instance_id) }
            on Approved -> Approved
            on Rejected -> Rejected
        }
        state PendingSixEyes {
            on_entry { notify_six_eyes_reviewer(instance_id) }
            on Approved -> Approved
            on Rejected -> Rejected
        }
        state Approved terminal { on_entry { notify_treasury_decided(instance_id, Text("a")) } }
        state Rejected terminal { on_entry { notify_treasury_decided(instance_id, Text("r")) } }
    }
    fn submit_trade_payment_for_approval(payment_id: i64, amount_cents: i64) -> Result(i64, WorkflowActionError) {
        return start_trade_payment_approval(None(), TradePaymentApprovalData(payment_id, amount_cents))
    }
    fn classify_and_advance(identity: VerifiedIdentity, instance_id: i64, amount_cents: i64) -> Result(bool, WorkflowActionError) {
        let event: TradePaymentApprovalEvent = if required_eyes_for_amount(amount_cents) == 2 { ClassifiedHighValue() } else { Classified() }
        return match json_parse("{}") { Ok(payload) => advance_trade_payment_approval(identity, instance_id, event, payload), Err(e) => Err(NoSuchTransition()) }
    }
"#;

const BATCH_PAYMENT_APPROVAL_NIR: &str = r#"
    struct Text { value: str }
    fn required_eyes_for_batch(amount_a_cents: i64, amount_b_cents: i64, amount_c_cents: i64) -> i64 {
        return if amount_a_cents + amount_b_cents + amount_c_cents >= 5000000 { 2 } else { 1 }
    }
    fn notify_checker(instance_id: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("checker"), "t", vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    fn notify_six_eyes_reviewer(instance_id: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("six_eyes_reviewer"), "t", vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    fn notify_treasury_decided(instance_id: i64, template: Text) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("treasury"), template.value, vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    workflow BatchPaymentApproval {
        data { amount_a_cents: i64, amount_b_cents: i64, amount_c_cents: i64 }
        state PendingClassification {
            on Classified -> PendingMakerChecker
            on ClassifiedHighValue -> PendingSixEyes
        }
        state PendingMakerChecker {
            on_entry { notify_checker(instance_id) }
            on Approved -> Approved
            on Rejected -> Rejected
        }
        state PendingSixEyes {
            on_entry { notify_six_eyes_reviewer(instance_id) }
            on Approved -> Approved
            on Rejected -> Rejected
        }
        state Approved terminal { on_entry { notify_treasury_decided(instance_id, Text("a")) } }
        state Rejected terminal { on_entry { notify_treasury_decided(instance_id, Text("r")) } }
    }
    fn submit_batch_payment_for_approval(amount_a_cents: i64, amount_b_cents: i64, amount_c_cents: i64) -> Result(i64, WorkflowActionError) {
        return start_batch_payment_approval(None(), BatchPaymentApprovalData(amount_a_cents, amount_b_cents, amount_c_cents))
    }
    fn classify_batch_and_advance(identity: VerifiedIdentity, instance_id: i64, amount_a_cents: i64, amount_b_cents: i64, amount_c_cents: i64) -> Result(bool, WorkflowActionError) {
        let event: BatchPaymentApprovalEvent = if required_eyes_for_batch(amount_a_cents, amount_b_cents, amount_c_cents) == 2 { ClassifiedHighValue() } else { Classified() }
        return match json_parse("{}") { Ok(payload) => advance_batch_payment_approval(identity, instance_id, event, payload), Err(e) => Err(NoSuchTransition()) }
    }
"#;

const ESCROW_TRANCHE_RELEASE_NIR: &str = r#"
    fn release_tranche(instance_id: i64, purchase_order_id: i64, tranche_amount_cents: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match db_execute(conn, "UPDATE purchase_order SET escrow_balance_cents = escrow_balance_cents - ? WHERE id = ?", tranche_amount_cents, purchase_order_id) { Ok(n) => n >= 0, Err(e) => false }, Err(e) => false }
    }
    workflow EscrowTrancheRelease {
        data { purchase_order_id: i64, tranche_amount_cents: i64 }
        state EscrowHeld { on MilestoneVerified -> TrancheReleased }
        state TrancheReleased terminal { on_entry { release_tranche(instance_id, data.purchase_order_id, data.tranche_amount_cents) } }
    }
    fn open_escrow_tranche(purchase_order_id: i64, tranche_amount_cents: i64) -> Result(i64, WorkflowActionError) {
        return start_escrow_tranche_release(None(), EscrowTrancheReleaseData(purchase_order_id, tranche_amount_cents))
    }
    fn confirm_milestone_and_release(identity: VerifiedIdentity, instance_id: i64) -> Result(bool, WorkflowActionError) {
        return match json_parse("{}") { Ok(payload) => advance_escrow_tranche_release(identity, instance_id, MilestoneVerified(), payload), Err(e) => Err(NoSuchTransition()) }
    }
"#;

const COMMISSION_WATERFALL_SETTLEMENT_NIR: &str = r#"
    fn compute_commission_waterfall(instance_id: i64, payment_id: i64, settled_amount_cents: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match db_execute(conn, "INSERT INTO commission_waterfall (payment_id, settled_amount_cents) VALUES (?, ?)", payment_id, settled_amount_cents) { Ok(n) => n >= 0, Err(e) => false }, Err(e) => false }
    }
    workflow CommissionWaterfallSettlement {
        data { payment_id: i64, settled_amount_cents: i64 }
        state Settled terminal { on_entry { compute_commission_waterfall(instance_id, data.payment_id, data.settled_amount_cents) } }
    }
    fn settle_commission_waterfall(payment_id: i64, settled_amount_cents: i64) -> Result(i64, WorkflowActionError) {
        return start_commission_waterfall_settlement(None(), CommissionWaterfallSettlementData(payment_id, settled_amount_cents))
    }
"#;

const COMMISSION_DISPUTE_RESOLUTION_NIR: &str = r#"
    struct Text { value: str }
    fn notify_checker(instance_id: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("checker"), "t", vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    fn apply_commission_correction(instance_id: i64, commission_waterfall_id: i64, disputed_amount_cents: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match db_execute(conn, "UPDATE commission_waterfall SET settled_amount_cents = ? WHERE id = ?", disputed_amount_cents, commission_waterfall_id) { Ok(n) => n >= 0, Err(e) => false }, Err(e) => false }
    }
    fn notify_treasury_decided(instance_id: i64, template: Text) -> bool {
        return match db_connect("d.db") { Ok(conn) => match mq_connect("127.0.0.1", 6379) { Ok(mq_conn) => match json_parse("{}") { Ok(vars) => match notify(conn, mq_conn, ByRole("treasury"), template.value, vars) { Ok(s) => s, Err(e) => false }, Err(e) => false }, Err(e) => false }, Err(e) => false }
    }
    workflow CommissionDisputeResolution {
        data { commission_waterfall_id: i64, disputed_amount_cents: i64 }
        state PendingCorrection {
            on_entry { notify_checker(instance_id) }
            on Corrected -> Corrected
            on Denied -> Denied
        }
        state Corrected terminal { on_entry { apply_commission_correction(instance_id, data.commission_waterfall_id, data.disputed_amount_cents) } }
        state Denied terminal { on_entry { notify_treasury_decided(instance_id, Text("d")) } }
    }
    fn raise_commission_dispute(commission_waterfall_id: i64, disputed_amount_cents: i64) -> Result(i64, WorkflowActionError) {
        return start_commission_dispute_resolution(None(), CommissionDisputeResolutionData(commission_waterfall_id, disputed_amount_cents))
    }
    fn decide_commission_dispute(identity: VerifiedIdentity, instance_id: i64, decision: CommissionDisputeResolutionEvent) -> Result(bool, WorkflowActionError) {
        return match json_parse("{}") { Ok(payload) => advance_commission_dispute_resolution(identity, instance_id, decision, payload), Err(e) => Err(NoSuchTransition()) }
    }
"#;

const WALLET_SETTLEMENT_NIR: &str = r#"
    fn transfer_to_partner_bank_account(instance_id: i64, channel_partner_id: i64, swept_balance_cents: i64) -> bool {
        return match db_connect("d.db") { Ok(conn) => match db_execute(conn, "UPDATE channel_partner_wallet SET balance_cents = 0 WHERE channel_partner_id = ?", channel_partner_id) { Ok(n) => n >= 0, Err(e) => false }, Err(e) => false }
    }
    workflow WalletSettlement {
        data { channel_partner_id: i64, swept_balance_cents: i64 }
        state Settled terminal { on_entry { transfer_to_partner_bank_account(instance_id, data.channel_partner_id, data.swept_balance_cents) } }
    }
    fn settle_channel_partner_wallet(channel_partner_id: i64, swept_balance_cents: i64) -> Result(i64, WorkflowActionError) {
        return start_wallet_settlement(None(), WalletSettlementData(channel_partner_id, swept_balance_cents))
    }
"#;

fn assert_conforms(nir: &str, file: &ExtractionFile, id: &str) {
    let program = build_program(nir);
    let report = check_workflow_conformance(&program, extracted_workflow(file, id));
    assert!(report.is_exact_match(), "{id}: conformance mismatches: {:#?}", report.mismatches);
}

#[test]
fn all_six_v2_workflows_match_their_real_nir_verbatim() {
    let file = load_extraction();
    assert_conforms(TRADE_PAYMENT_APPROVAL_NIR, &file, "WF-TRDPAY-001");
    assert_conforms(BATCH_PAYMENT_APPROVAL_NIR, &file, "WF-TRDPAY-003");
    assert_conforms(ESCROW_TRANCHE_RELEASE_NIR, &file, "WF-TRDPAY-002");
    assert_conforms(COMMISSION_WATERFALL_SETTLEMENT_NIR, &file, "WF-COMM-002");
    assert_conforms(COMMISSION_DISPUTE_RESOLUTION_NIR, &file, "WF-COMM-003");
    assert_conforms(WALLET_SETTLEMENT_NIR, &file, "WF-COMM-001");
}

#[test]
fn trade_payment_approval_routing_fn_is_proved_against_v2() {
    let file = load_extraction();
    let program = build_program(TRADE_PAYMENT_APPROVAL_NIR);
    let routing_fn = extracted_workflow(&file, "WF-TRDPAY-001").routing_fn.as_ref().unwrap();
    let bindings = HashMap::from([("high_value_threshold".to_string(), 5_000_000i64)]);
    let result = check_fn_contract(&program, &routing_fn.name, &routing_fn.pre_logic, &routing_fn.post_logic, &bindings);
    assert_eq!(result, ContractCheckResult::Proved, "{result:?}");
}

/// The new, three-parameter routing function `WF-TRDPAY-003` introduces —
/// `required_eyes_for_batch`'s post_logic sums three named params with
/// plain `+`, exactly the shape the extraction's own `modeling_note`
/// says it deliberately used instead of `sum(...)` (no array parameter
/// type in Nirdosha). Confirms `contract_check.rs` handles a multi-
/// parameter arithmetic contract, not just the single-param case
/// `extracted_typed_v1_verification.rs` already covered.
#[test]
fn batch_payment_approval_routing_fn_is_proved_against_v2() {
    let file = load_extraction();
    let program = build_program(BATCH_PAYMENT_APPROVAL_NIR);
    let routing_fn = extracted_workflow(&file, "WF-TRDPAY-003").routing_fn.as_ref().unwrap();
    let bindings = HashMap::from([("high_value_threshold".to_string(), 5_000_000i64)]);
    let result = check_fn_contract(&program, &routing_fn.name, &routing_fn.pre_logic, &routing_fn.post_logic, &bindings);
    assert_eq!(result, ContractCheckResult::Proved, "{result:?}");
}

/// Same threshold-unbound case as v1, re-confirmed against the fresh
/// extraction — `high_value_threshold` still isn't one of
/// `required_eyes_for_amount`'s real parameters.
#[test]
fn trade_payment_approval_routing_fn_is_unbound_without_a_threshold_binding() {
    let file = load_extraction();
    let program = build_program(TRADE_PAYMENT_APPROVAL_NIR);
    let routing_fn = extracted_workflow(&file, "WF-TRDPAY-001").routing_fn.as_ref().unwrap();
    let result = check_fn_contract(&program, &routing_fn.name, &routing_fn.pre_logic, &routing_fn.post_logic, &HashMap::new());
    assert_eq!(result, ContractCheckResult::UnboundIdentifier("high_value_threshold".to_string()));
}

/// Sanity-checks the new UserStory fields this extraction is meant to
/// demonstrate: `required_role` (a literal token, not
/// `required_permission`'s prose), `implements` (bound to a real
/// function), and `input_fields` (typed, form-renderable).
#[test]
fn user_story_ui_fields_are_populated_and_well_formed() {
    let file = load_extraction();

    let initiate = extracted_story(&file, "US-TRDPAY-005");
    assert_eq!(initiate.required_role.as_deref(), Some("treasury_user"));
    assert_eq!(initiate.implements, vec!["submit_trade_payment".to_string()]);
    assert!(!initiate.input_fields.is_empty());
    assert!(initiate.input_fields.iter().all(|f| ["i64", "f64", "str", "bool"].contains(&f.ty.as_str())));

    // `required_role` is a bare, `requires(role: "...")`-pasteable token
    // for every story that sets one — no spaces, no punctuation.
    for story in &file.user_stories {
        if let Some(role) = &story.required_role {
            assert!(
                role.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "US {}: required_role {role:?} isn't a bare snake_case token",
                story.id
            );
        }
    }

    // The one story the extraction's own `action_incomplete_reason`
    // flags as underspecified (US-COMM-006, no fulfillment mechanism
    // named in the PRD) correctly has no `implements` binding — there's
    // nothing concrete yet to bind to.
    let withdrawal = extracted_story(&file, "US-COMM-006");
    assert!(withdrawal.implements.is_empty());
}

/// State-ownership fields: `owner_role`/`required_decisions` should
/// correctly distinguish Maker-Checker (1 other decider) from six-eyes
/// (2 distinct ones) from a no-owner automatic state.
#[test]
fn state_ownership_fields_distinguish_maker_checker_from_six_eyes() {
    let file = load_extraction();
    let wf = extracted_workflow(&file, "WF-TRDPAY-001");

    let maker_checker = wf.states.iter().find(|s| s.name == "PendingMakerChecker").unwrap();
    assert_eq!(maker_checker.owner_role.as_deref(), Some("checker"));
    assert_eq!(maker_checker.required_decisions, Some(1));

    let six_eyes = wf.states.iter().find(|s| s.name == "PendingSixEyes").unwrap();
    assert_eq!(six_eyes.owner_role.as_deref(), Some("six_eyes_reviewer"));
    assert_eq!(six_eyes.required_decisions, Some(2));

    let classification = wf.states.iter().find(|s| s.name == "PendingClassification").unwrap();
    assert!(classification.owner_role.is_none());
    assert!(classification.required_decisions.is_none());

    // The dispute-resolution workflow's own `modeling_note` insists this
    // is Maker-Checker (1), not six-eyes (2), despite superficially
    // similar "pending correction" phrasing — confirms the extractor
    // didn't default to whichever number appeared most in-context.
    let dispute_wf = extracted_workflow(&file, "WF-COMM-003");
    let pending_correction = dispute_wf.states.iter().find(|s| s.name == "PendingCorrection").unwrap();
    assert_eq!(pending_correction.required_decisions, Some(1));
}
