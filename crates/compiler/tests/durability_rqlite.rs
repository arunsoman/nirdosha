//! Real-rqlite tests for `workflow_log.rs`/`transact_log.rs`'s rqlite
//! backend (`docs/ROADMAP.md`'s Phase 2 multi-instance fix, `src/rqlite.rs`)
//! -- same `#[ignore]`d-by-default convention `tests/postgres.rs`/
//! `tests/durability_postgres.rs` already established: a real server
//! (here, a real `rqlited` process) isn't something a plain `cargo test`
//! run can stand up on its own.
//!
//! ```text
//! # one-time: build a real rqlited binary (Go toolchain required)
//! git clone --depth 1 https://github.com/rqlite/rqlite.git /tmp/rqlite_src
//! (cd /tmp/rqlite_src && go build -o /tmp/rqlited ./cmd/rqlited)
//!
//! # start a single node
//! rm -rf /tmp/rqlite_data
//! /tmp/rqlited -http-addr 127.0.0.1:4001 -raft-addr 127.0.0.1:4002 /tmp/rqlite_data &
//!
//! NIRDOSHA_TEST_RQLITE_URL=rqlite://127.0.0.1:4001 \
//!     cargo test --test durability_rqlite -- --ignored
//! ```
//!
//! Defaults to `rqlite://127.0.0.1:4001` if the env var isn't set.

use nirdosha::durability::LogTarget;
use nirdosha::transact_log::TransactLog;
use nirdosha::workflow_log::WorkflowLog;

fn test_rqlite_url() -> String {
    std::env::var("NIRDOSHA_TEST_RQLITE_URL").unwrap_or_else(|_| "rqlite://127.0.0.1:4001".to_string())
}

/// Every rqlite table is a single shared namespace (rqlite has no
/// schema/database-selection concept the way Postgres does) -- so unlike
/// `tests/durability_postgres.rs`'s per-test schema isolation, these
/// tests share one `workflow_instance`/`transact_log` table across the
/// whole file and instead isolate by using distinct `workflow_name`/
/// `txn_id` values per test (a real, if coarser, isolation mechanism --
/// exactly what `list_instances`/`state_of` already filter by).
fn unique_workflow_name(label: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("nirdosha_rqlite_test_{label}_{}_{n}", std::process::id())
}

#[test]
#[ignore]
fn workflow_log_create_instance_transition_and_history_round_trip_through_rqlite() {
    let wf_name = unique_workflow_name("wf_basic");
    let wlog = WorkflowLog::open(&LogTarget::Rqlite(test_rqlite_url())).expect("WorkflowLog::open must succeed against real rqlite");

    let id = wlog.create_instance(&wf_name, "submitted", "{}", Some("alice"), 1_000).expect("create_instance must succeed");
    assert!(id > 0, "rqlite AUTOINCREMENT id must be positive, got {id}");

    let (name, state, data) = wlog.get_instance(id).expect("get_instance must succeed").expect("instance must exist");
    assert_eq!(name, wf_name);
    assert_eq!(state, "submitted");
    assert_eq!(data, "{}");

    wlog.record_transition(id, "submitted", "approved", "approve", Some("bob"), false, Some("looks good"), 2_000)
        .expect("record_transition must succeed");

    let (_, state_after, _) = wlog.get_instance(id).expect("get_instance must succeed").expect("instance must still exist");
    assert_eq!(state_after, "approved", "record_transition must update workflow_instance.state");

    let history = wlog.list_history(id).expect("list_history must succeed");
    assert_eq!(history.len(), 1, "one transition must produce exactly one history row");
    let (from, to, event, actor, via_link, comment, _at) = &history[0];
    assert_eq!(from, "submitted");
    assert_eq!(to, "approved");
    assert_eq!(event, "approve");
    assert_eq!(actor.as_deref(), Some("bob"));
    assert!(!via_link, "via_link must round-trip as an INTEGER 0/1, read back as false");
    assert_eq!(comment.as_deref(), Some("looks good"));
}

#[test]
#[ignore]
fn workflow_log_magic_link_mint_find_and_single_use_consume_round_trip_through_rqlite() {
    let wf_name = unique_workflow_name("wf_link");
    let wlog = WorkflowLog::open(&LogTarget::Rqlite(test_rqlite_url())).expect("WorkflowLog::open must succeed");

    let id = wlog.create_instance(&wf_name, "submitted", "{}", None, 1_000).expect("create_instance must succeed");
    wlog.mint_link(id, "approve", "tok-rq-abc123").expect("mint_link must succeed");

    let (row_id, token) =
        wlog.find_unconsumed_link(id, "approve").expect("find_unconsumed_link must succeed").expect("link must exist");
    assert_eq!(token, "tok-rq-abc123");

    let consumed_first = wlog.consume_link_by_id(row_id).expect("consume_link_by_id must succeed");
    assert!(consumed_first, "the first consume of a fresh link must win");
    let consumed_second = wlog.consume_link_by_id(row_id).expect("consume_link_by_id must succeed");
    assert!(!consumed_second, "a second consume of the same link must lose -- single-use, enforced by rqlite too");

    let after = wlog.find_unconsumed_link(id, "approve").expect("find_unconsumed_link must succeed");
    assert!(after.is_none(), "a consumed link must no longer show up as unconsumed");
}

#[test]
#[ignore]
fn workflow_log_identity_directory_and_presence_round_trip_through_rqlite() {
    let subject_a = format!("alice_{}", unique_workflow_name("id"));
    let subject_b = format!("bob_{}", unique_workflow_name("id"));
    let wlog = WorkflowLog::open(&LogTarget::Rqlite(test_rqlite_url())).expect("WorkflowLog::open must succeed");

    wlog.upsert_identity(&subject_a, "[\"compliance_officer\"]", 1_000).expect("upsert_identity must succeed");
    wlog.upsert_identity(&subject_b, "[\"analyst\"]", 1_000).expect("upsert_identity must succeed");
    let officers = wlog.subjects_with_role("compliance_officer").expect("subjects_with_role must succeed");
    assert!(officers.contains(&subject_a));
    assert!(!officers.contains(&subject_b));

    assert!(!wlog.is_online(&subject_a).expect("is_online must succeed"), "nobody is online before set_presence");
    wlog.set_presence(&subject_a, true, 2_000).expect("set_presence must succeed");
    assert!(wlog.is_online(&subject_a).expect("is_online must succeed"), "must read back true as INTEGER 1");
    wlog.set_presence(&subject_a, false, 3_000).expect("set_presence must succeed");
    assert!(!wlog.is_online(&subject_a).expect("is_online must succeed"), "set_presence must overwrite, not merely insert");
}

#[test]
#[ignore]
fn workflow_log_pending_action_begin_done_and_attempts_round_trip_through_rqlite() {
    let wf_name = unique_workflow_name("wf_action");
    let wlog = WorkflowLog::open(&LogTarget::Rqlite(test_rqlite_url())).expect("WorkflowLog::open must succeed");

    let id = wlog.create_instance(&wf_name, "submitted", "{}", None, 1_000).expect("create_instance must succeed");
    let action_id = wlog
        .begin_pending_action(id, &wf_name, "submitted", "on_entry", 0, "notify_compliance", "[]", 1_000)
        .expect("begin_pending_action must succeed");
    assert!(action_id > 0);

    let pending: Vec<_> = wlog
        .list_pending_actions()
        .expect("list_pending_actions must succeed")
        .into_iter()
        .filter(|a| a.id == action_id)
        .collect();
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].attempts, 0);

    wlog.bump_action_attempts(action_id, 1_100).expect("bump_action_attempts must succeed");
    let pending: Vec<_> =
        wlog.list_pending_actions().expect("list_pending_actions must succeed").into_iter().filter(|a| a.id == action_id).collect();
    assert_eq!(pending[0].attempts, 1);

    wlog.mark_action_done(action_id, 1_200).expect("mark_action_done must succeed");
    let still_pending =
        wlog.list_pending_actions().expect("list_pending_actions must succeed").into_iter().any(|a| a.id == action_id);
    assert!(!still_pending, "a done action must no longer be listed as pending");
}

/// The direct proof that `docs/ROADMAP.md`'s Phase 2 actually removes the
/// multi-instance wall for the rqlite case: two *independent*
/// `WorkflowLog` handles (standing in for two `nirdosha serve` replicas)
/// opened against the *same* rqlite cluster must mint globally-unique
/// `workflow_instance.id`s and see each other's rows -- the same
/// property `tests/durability_postgres.rs`'s own equivalent test proves
/// for Postgres.
#[test]
#[ignore]
fn two_independent_workflow_log_handles_against_the_same_rqlite_cluster_never_collide_on_instance_id() {
    let wf_name = unique_workflow_name("wf_multi_instance");
    let url = test_rqlite_url();
    let replica_a = WorkflowLog::open(&LogTarget::Rqlite(url.clone())).expect("replica A must open");
    let replica_b = WorkflowLog::open(&LogTarget::Rqlite(url)).expect("replica B must open");

    let mut ids = std::collections::HashSet::new();
    for i in 0..15 {
        let id_a = replica_a.create_instance(&wf_name, "submitted", "{}", None, 1_000 + i).expect("replica A create must succeed");
        let id_b = replica_b.create_instance(&wf_name, "submitted", "{}", None, 1_000 + i).expect("replica B create must succeed");
        assert!(ids.insert(id_a), "replica A minted a duplicate id: {id_a}");
        assert!(ids.insert(id_b), "replica B minted a duplicate id: {id_b}");
    }
    assert_eq!(ids.len(), 30, "30 creates across two independent replicas must yield 30 distinct ids");

    let all_from_a = replica_a.list_instances(&wf_name).expect("list_instances must succeed");
    assert_eq!(all_from_a.len(), 30, "replica A must see every row, including the ones replica B wrote");
}

#[test]
#[ignore]
fn transact_log_full_lifecycle_round_trips_through_rqlite() {
    let txn_id = format!("txn-rq-{}", unique_workflow_name("basic"));
    let tlog = TransactLog::open(&LogTarget::Rqlite(test_rqlite_url())).expect("TransactLog::open must succeed against real rqlite");

    tlog.begin_pending(
        &txn_id,
        "do_charge",
        &[nirdosha::interpreter::Value::Int(500)],
        Some(3),
        Some(30),
        "verify_charge",
        &["network", "txn_id"],
        "commit_charge",
        &["network", "txn_id"],
        None,
        None,
    )
    .expect("begin_pending must succeed");

    assert_eq!(tlog.state_of(&txn_id).expect("state_of must succeed"), Some("pending".to_string()));

    tlog.record_network_result(&txn_id, &nirdosha::interpreter::Value::Bool(true)).expect("record_network_result must succeed");
    assert_eq!(tlog.state_of(&txn_id).expect("state_of must succeed"), Some("network_done".to_string()));

    tlog.record_verify(&txn_id, &[nirdosha::interpreter::Value::Bool(true)], true).expect("record_verify must succeed");
    tlog.mark_commit_pending(&txn_id, &[nirdosha::interpreter::Value::Int(500)]).expect("mark_commit_pending must succeed");
    assert_eq!(tlog.state_of(&txn_id).expect("state_of must succeed"), Some("commit_pending".to_string()));

    let unresolved: Vec<_> =
        tlog.list_unresolved().expect("list_unresolved must succeed").into_iter().filter(|t| t.txn_id == txn_id).collect();
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].verify_result, Some(true), "verify_result must round-trip as INTEGER 1 -> true");
    assert_eq!(unresolved[0].commit_args.as_ref().map(|a| a.len()), Some(1));

    tlog.mark_terminal(&txn_id, "committed").expect("mark_terminal must succeed");
    assert_eq!(tlog.state_of(&txn_id).expect("state_of must succeed"), Some("committed".to_string()));
    let still_unresolved =
        tlog.list_unresolved().expect("list_unresolved must succeed").into_iter().any(|t| t.txn_id == txn_id);
    assert!(!still_unresolved, "a committed row must no longer be unresolved");
}

/// Same shared-table proof as `WorkflowLog`'s own, for `TransactLog`:
/// `src/transact_log.rs`'s own doc comment names the exact defect a
/// local SQLite file has -- a retried request landing on a different
/// replica sees no row for its `txn_id` and re-executes `network`. With
/// a shared rqlite table, the second replica must see the first
/// replica's row.
#[test]
#[ignore]
fn a_txn_id_begun_by_one_replica_is_visible_to_another_replica_sharing_the_same_rqlite_cluster() {
    let txn_id = format!("txn-rq-shared-{}", unique_workflow_name("shared"));
    let url = test_rqlite_url();
    let replica_a = TransactLog::open(&LogTarget::Rqlite(url.clone())).expect("replica A must open");
    let replica_b = TransactLog::open(&LogTarget::Rqlite(url)).expect("replica B must open");

    replica_a
        .begin_pending(
            &txn_id,
            "do_charge",
            &[nirdosha::interpreter::Value::Int(42)],
            None,
            None,
            "verify_charge",
            &["network", "txn_id"],
            "commit_charge",
            &["network", "txn_id"],
            None,
            None,
        )
        .expect("replica A begin_pending must succeed");

    let seen_by_b = replica_b.state_of(&txn_id).expect("state_of must succeed");
    assert_eq!(seen_by_b, Some("pending".to_string()), "replica B must see replica A's own in-flight txn_id via the shared table");
}
