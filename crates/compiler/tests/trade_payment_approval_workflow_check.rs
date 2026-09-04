//! Validation-only: proves the `workflow TradePaymentApproval` sketch
//! (drafted for scratch/extracted_typed_v1.json's WF-TRDPAY-001 example)
//! is real, compiling `.nir` syntax against docs/WORKFLOW.md's locked grammar
//! -- not just prose that looks plausible. Same "build_program" harness
//! `crates/compiler/tests/workflow.rs` already establishes.

use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck_optional_main;

fn build_program(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck_optional_main(&program).unwrap_or_else(|e| panic!("typecheck should succeed: {e:?}"));
    check_ownership(&program).expect("ownership check should succeed");
    program
}

#[test]
fn trade_payment_approval_workflow_compiles() {
    let src = r#"
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

        fn submit_trade_payment_for_approval(payment_id: i64, amount_cents: i64) -> Result(i64, WorkflowActionError) {
            return start_trade_payment_approval(None(), TradePaymentApprovalData(payment_id, amount_cents))
        }

        fn classify_and_advance(identity: VerifiedIdentity, instance_id: i64, amount_cents: i64) -> Result(bool, WorkflowActionError) {
            let event: TradePaymentApprovalEvent = if required_eyes_for_amount(amount_cents) == 2 { ClassifiedHighValue() } else { Classified() }
            return match json_parse("{}") {
                Ok(payload) => advance_trade_payment_approval(identity, instance_id, event, payload),
                Err(e) => Err(NoSuchTransition()),
            }
        }
    "#;
    build_program(src);
}

#[test]
fn escrow_tranche_release_workflow_compiles() {
    let src = r#"
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

        fn open_escrow_tranche(purchase_order_id: i64, tranche_amount_cents: i64) -> Result(i64, WorkflowActionError) {
            return start_escrow_tranche_release(None(), EscrowTrancheReleaseData(purchase_order_id, tranche_amount_cents))
        }

        fn confirm_milestone_and_release(identity: VerifiedIdentity, instance_id: i64) -> Result(bool, WorkflowActionError) {
            return match json_parse("{}") {
                Ok(payload) => advance_escrow_tranche_release(identity, instance_id, MilestoneVerified(), payload),
                Err(e) => Err(NoSuchTransition()),
            }
        }
    "#;
    build_program(src);
}

#[test]
fn wallet_settlement_workflow_compiles() {
    let src = r#"
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

        fn settle_channel_partner_wallet(channel_partner_id: i64, swept_balance_cents: i64) -> Result(i64, WorkflowActionError) {
            return start_wallet_settlement(None(), WalletSettlementData(channel_partner_id, swept_balance_cents))
        }
    "#;
    build_program(src);
}

#[test]
fn batch_payment_approval_workflow_compiles() {
    let src = r#"
        struct Text {
            value: str,
        }

        fn required_eyes_for_batch(amount_a_cents: i64, amount_b_cents: i64, amount_c_cents: i64) -> i64 {
            return if amount_a_cents + amount_b_cents + amount_c_cents >= 5000000 { 2 } else { 1 }
        }

        fn notify_checker(instance_id: i64) -> bool {
            return match db_connect("batch_payment_approval.db") {
                Ok(conn) => match mq_connect("127.0.0.1", 6379) {
                    Ok(mq_conn) => match json_parse("{}") {
                        Ok(vars) => match notify(conn, mq_conn, ByRole("checker"), "batch_payment_pending_approval", vars) {
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
            return match db_connect("batch_payment_approval.db") {
                Ok(conn) => match mq_connect("127.0.0.1", 6379) {
                    Ok(mq_conn) => match json_parse("{}") {
                        Ok(vars) => match notify(conn, mq_conn, ByRole("six_eyes_reviewer"), "batch_payment_pending_six_eyes_review", vars) {
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
            return match db_connect("batch_payment_approval.db") {
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

        workflow BatchPaymentApproval {
            data {
                amount_a_cents: i64,
                amount_b_cents: i64,
                amount_c_cents: i64,
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
                    notify_treasury_decided(instance_id, Text("batch_payment_approved"))
                }
            }

            state Rejected terminal {
                on_entry {
                    notify_treasury_decided(instance_id, Text("batch_payment_rejected"))
                }
            }
        }

        fn submit_batch_payment_for_approval(amount_a_cents: i64, amount_b_cents: i64, amount_c_cents: i64) -> Result(i64, WorkflowActionError) {
            return start_batch_payment_approval(None(), BatchPaymentApprovalData(amount_a_cents, amount_b_cents, amount_c_cents))
        }

        fn classify_batch_and_advance(identity: VerifiedIdentity, instance_id: i64, amount_a_cents: i64, amount_b_cents: i64, amount_c_cents: i64) -> Result(bool, WorkflowActionError) {
            let event: BatchPaymentApprovalEvent = if required_eyes_for_batch(amount_a_cents, amount_b_cents, amount_c_cents) == 2 { ClassifiedHighValue() } else { Classified() }
            return match json_parse("{}") {
                Ok(payload) => advance_batch_payment_approval(identity, instance_id, event, payload),
                Err(e) => Err(NoSuchTransition()),
            }
        }
    "#;
    build_program(src);
}

#[test]
fn commission_waterfall_settlement_workflow_compiles() {
    let src = r#"
        fn compute_commission_waterfall(instance_id: i64, payment_id: i64, settled_amount_cents: i64) -> bool {
            return match db_connect("commission_waterfall_settlement.db") {
                Ok(conn) => match db_execute(conn, "INSERT INTO commission_waterfall (payment_id, settled_amount_cents) VALUES (?, ?)", payment_id, settled_amount_cents) {
                    Ok(n) => n >= 0,
                    Err(e) => false,
                },
                Err(e) => false,
            }
        }

        workflow CommissionWaterfallSettlement {
            data {
                payment_id: i64,
                settled_amount_cents: i64,
            }

            state Settled terminal {
                on_entry {
                    compute_commission_waterfall(instance_id, data.payment_id, data.settled_amount_cents)
                }
            }
        }

        fn settle_commission_waterfall(payment_id: i64, settled_amount_cents: i64) -> Result(i64, WorkflowActionError) {
            return start_commission_waterfall_settlement(None(), CommissionWaterfallSettlementData(payment_id, settled_amount_cents))
        }
    "#;
    build_program(src);
}

#[test]
fn commission_dispute_resolution_workflow_compiles() {
    let src = r#"
        struct Text {
            value: str,
        }

        fn notify_checker(instance_id: i64) -> bool {
            return match db_connect("commission_dispute_resolution.db") {
                Ok(conn) => match mq_connect("127.0.0.1", 6379) {
                    Ok(mq_conn) => match json_parse("{}") {
                        Ok(vars) => match notify(conn, mq_conn, ByRole("checker"), "commission_dispute_pending_correction", vars) {
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

        fn apply_commission_correction(instance_id: i64, commission_waterfall_id: i64, disputed_amount_cents: i64) -> bool {
            return match db_connect("commission_dispute_resolution.db") {
                Ok(conn) => match db_execute(conn, "UPDATE commission_waterfall SET settled_amount_cents = ? WHERE id = ?", disputed_amount_cents, commission_waterfall_id) {
                    Ok(n) => n >= 0,
                    Err(e) => false,
                },
                Err(e) => false,
            }
        }

        fn notify_treasury_decided(instance_id: i64, template: Text) -> bool {
            return match db_connect("commission_dispute_resolution.db") {
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

        workflow CommissionDisputeResolution {
            data {
                commission_waterfall_id: i64,
                disputed_amount_cents: i64,
            }

            state PendingCorrection {
                on_entry {
                    notify_checker(instance_id)
                }
                on Corrected -> Corrected
                on Denied -> Denied
            }

            state Corrected terminal {
                on_entry {
                    apply_commission_correction(instance_id, data.commission_waterfall_id, data.disputed_amount_cents)
                }
            }

            state Denied terminal {
                on_entry {
                    notify_treasury_decided(instance_id, Text("commission_dispute_denied"))
                }
            }
        }

        fn raise_commission_dispute(commission_waterfall_id: i64, disputed_amount_cents: i64) -> Result(i64, WorkflowActionError) {
            return start_commission_dispute_resolution(None(), CommissionDisputeResolutionData(commission_waterfall_id, disputed_amount_cents))
        }

        fn decide_commission_dispute(identity: VerifiedIdentity, instance_id: i64, decision: CommissionDisputeResolutionEvent) -> Result(bool, WorkflowActionError) {
            return match json_parse("{}") {
                Ok(payload) => advance_commission_dispute_resolution(identity, instance_id, decision, payload),
                Err(e) => Err(NoSuchTransition()),
            }
        }
    "#;
    build_program(src);
}
