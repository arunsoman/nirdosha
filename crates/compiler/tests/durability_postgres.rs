//! Real-Postgres tests for `workflow_log.rs`/`transact_log.rs`'s Postgres
//! backend (`docs/ROADMAP.md`'s multi-instance fix, `durability.rs`) -- layer
//! 2 on top of the SQLite-only coverage `tests/workflow.rs`/
//! `tests/transact_durability.rs` already have, the same "every test here
//! is `#[ignore]`d by default, run explicitly against a real server"
//! convention `tests/postgres.rs` established for `db_connect`'s own
//! Postgres path:
//!
//! ```text
//! NIRDOSHA_TEST_POSTGRES_URL=postgres://user@host:5432/dbname \
//!     cargo test --test durability_postgres -- --ignored
//! ```
//!
//! Defaults to `postgres://postgres@127.0.0.1:5432/postgres` if unset,
//! same as `tests/postgres.rs`.

use nirdosha::durability::LogTarget;
use nirdosha::transact_log::TransactLog;
use nirdosha::workflow_log::WorkflowLog;

fn test_db_url() -> String {
    std::env::var("NIRDOSHA_TEST_POSTGRES_URL").unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".to_string())
}

/// Every test's tables are namespaced into their own schema
/// (`unique_schema`), created fresh and dropped at the end, so parallel
/// `cargo test` threads sharing one live server never collide on
/// `workflow_instance`/`transact_log` -- `workflow_log.rs`/
/// `transact_log.rs`'s own `CREATE TABLE IF NOT EXISTS` has no table-name
/// parameter to vary, unlike `tests/postgres.rs`'s own `unique_table`, so
/// this test file gets the same isolation via `search_path` instead: a
/// connection string with `options=-csearch_path%3D<schema>` makes every
/// unqualified table name in `workflow_log.rs`/`transact_log.rs` resolve
/// inside that one schema, invisible to (and unaffected by) any other
/// test's schema.
fn unique_schema(label: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let pid = std::process::id();
    format!("nirdosha_durability_test_{label}_{pid}_{n}")
}

/// Creates `schema`, returns a connection string whose `search_path`
/// resolves into it, and a cleanup closure the caller runs at the end
/// (`DROP SCHEMA ... CASCADE`) -- a plain function, not a `Drop` guard,
/// so a test can still run its own assertions against a raw connection
/// after the `WorkflowLog`/`TransactLog` under test is dropped, before
/// cleaning up.
fn schema_scoped_url(schema: &str) -> String {
    let base = test_db_url();
    let mut admin = postgres::Client::connect(&base, postgres::NoTls).expect("admin connection for schema setup must succeed");
    admin.batch_execute(&format!("CREATE SCHEMA \"{schema}\"")).expect("CREATE SCHEMA must succeed");
    format!("{base}?options=-csearch_path%3D{schema}")
}

fn drop_schema(schema: &str) {
    let base = test_db_url();
    if let Ok(mut admin) = postgres::Client::connect(&base, postgres::NoTls) {
        let _ = admin.batch_execute(&format!("DROP SCHEMA \"{schema}\" CASCADE"));
    }
}

#[test]
#[ignore]
fn workflow_log_create_instance_transition_and_history_round_trip_through_postgres() {
    let schema = unique_schema("wf_basic");
    let url = schema_scoped_url(&schema);
    let wlog = WorkflowLog::open(&LogTarget::Postgres(url)).expect("WorkflowLog::open must succeed against real Postgres");

    let id = wlog.create_instance("kyc_onboarding", "submitted", "{}", Some("alice"), 1_000).expect("create_instance must succeed");
    assert!(id > 0, "Postgres BIGSERIAL id must be positive, got {id}");

    let (name, state, data) = wlog.get_instance(id).expect("get_instance must succeed").expect("instance must exist");
    assert_eq!(name, "kyc_onboarding");
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
    assert!(!via_link, "via_link must round-trip as a real Postgres boolean, not a stray integer");
    assert_eq!(comment.as_deref(), Some("looks good"));

    drop_schema(&schema);
}

#[test]
#[ignore]
fn workflow_log_magic_link_mint_find_and_single_use_consume_round_trip_through_postgres() {
    let schema = unique_schema("wf_link");
    let url = schema_scoped_url(&schema);
    let wlog = WorkflowLog::open(&LogTarget::Postgres(url)).expect("WorkflowLog::open must succeed");

    let id = wlog.create_instance("kyc_onboarding", "submitted", "{}", None, 1_000).expect("create_instance must succeed");
    wlog.mint_link(id, "approve", "tok-abc123").expect("mint_link must succeed");

    let (row_id, token) = wlog.find_unconsumed_link(id, "approve").expect("find_unconsumed_link must succeed").expect("link must exist");
    assert_eq!(token, "tok-abc123");

    let consumed_first = wlog.consume_link_by_id(row_id).expect("consume_link_by_id must succeed");
    assert!(consumed_first, "the first consume of a fresh link must win");
    let consumed_second = wlog.consume_link_by_id(row_id).expect("consume_link_by_id must succeed");
    assert!(!consumed_second, "a second consume of the same link must lose -- single-use, enforced by Postgres too");

    let after = wlog.find_unconsumed_link(id, "approve").expect("find_unconsumed_link must succeed");
    assert!(after.is_none(), "a consumed link must no longer show up as unconsumed");

    drop_schema(&schema);
}

#[test]
#[ignore]
fn workflow_log_identity_directory_and_presence_round_trip_through_postgres() {
    let schema = unique_schema("wf_identity");
    let url = schema_scoped_url(&schema);
    let wlog = WorkflowLog::open(&LogTarget::Postgres(url)).expect("WorkflowLog::open must succeed");

    wlog.upsert_identity("alice", "[\"compliance_officer\"]", 1_000).expect("upsert_identity must succeed");
    wlog.upsert_identity("bob", "[\"analyst\"]", 1_000).expect("upsert_identity must succeed");
    let officers = wlog.subjects_with_role("compliance_officer").expect("subjects_with_role must succeed");
    assert_eq!(officers, vec!["alice".to_string()]);

    assert!(!wlog.is_online("alice").expect("is_online must succeed"), "nobody is online before set_presence");
    wlog.set_presence("alice", true, 2_000).expect("set_presence must succeed");
    assert!(wlog.is_online("alice").expect("is_online must succeed"), "must read back true as a real Postgres boolean");
    wlog.set_presence("alice", false, 3_000).expect("set_presence must succeed");
    assert!(!wlog.is_online("alice").expect("is_online must succeed"), "set_presence must overwrite, not merely insert");

    drop_schema(&schema);
}

#[test]
#[ignore]
fn workflow_log_pending_action_begin_done_and_attempts_round_trip_through_postgres() {
    let schema = unique_schema("wf_action");
    let url = schema_scoped_url(&schema);
    let wlog = WorkflowLog::open(&LogTarget::Postgres(url)).expect("WorkflowLog::open must succeed");

    let id = wlog.create_instance("kyc_onboarding", "submitted", "{}", None, 1_000).expect("create_instance must succeed");
    let action_id = wlog
        .begin_pending_action(id, "kyc_onboarding", "submitted", "on_entry", 0, "notify_compliance", "[]", 1_000)
        .expect("begin_pending_action must succeed");
    assert!(action_id > 0);

    let pending = wlog.list_pending_actions().expect("list_pending_actions must succeed");
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].id, action_id);
    assert_eq!(pending[0].attempts, 0);

    wlog.bump_action_attempts(action_id, 1_100).expect("bump_action_attempts must succeed");
    let pending = wlog.list_pending_actions().expect("list_pending_actions must succeed");
    assert_eq!(pending[0].attempts, 1);

    wlog.mark_action_done(action_id, 1_200).expect("mark_action_done must succeed");
    let pending = wlog.list_pending_actions().expect("list_pending_actions must succeed");
    assert!(pending.is_empty(), "a done action must no longer be listed as pending");

    drop_schema(&schema);
}

/// The actual, direct proof of `docs/ROADMAP.md`'s multi-instance fix: two
/// *independent* `WorkflowLog` handles (standing in for two `nirdosha
/// serve` replicas that never talk to each other directly) opened
/// against the *same* Postgres schema must mint globally-unique
/// `workflow_instance.id`s across both -- the exact property a
/// per-file SQLite `AUTOINCREMENT` counter cannot give two replicas,
/// each with their own independent file.
#[test]
#[ignore]
fn two_independent_workflow_log_handles_against_the_same_postgres_schema_never_collide_on_instance_id() {
    let schema = unique_schema("wf_multi_instance");
    let url = schema_scoped_url(&schema);

    let replica_a = WorkflowLog::open(&LogTarget::Postgres(url.clone())).expect("replica A must open");
    let replica_b = WorkflowLog::open(&LogTarget::Postgres(url)).expect("replica B must open");

    let mut ids = std::collections::HashSet::new();
    for i in 0..25 {
        let id_a = replica_a.create_instance("kyc_onboarding", "submitted", "{}", None, 1_000 + i).expect("replica A create must succeed");
        let id_b = replica_b.create_instance("kyc_onboarding", "submitted", "{}", None, 1_000 + i).expect("replica B create must succeed");
        assert!(ids.insert(id_a), "replica A minted a duplicate id: {id_a}");
        assert!(ids.insert(id_b), "replica B minted a duplicate id: {id_b}");
    }
    assert_eq!(ids.len(), 50, "50 creates across two independent replicas must yield 50 distinct ids");

    // Each replica must also see the OTHER replica's rows -- proof this
    // is genuinely one shared table, not two replicas that happen not to
    // collide by luck.
    let all_from_a = replica_a.list_instances("kyc_onboarding").expect("list_instances must succeed");
    assert_eq!(all_from_a.len(), 50, "replica A must see every row, including the ones replica B wrote");

    drop_schema(&schema);
}

#[test]
#[ignore]
fn transact_log_full_lifecycle_round_trips_through_postgres() {
    let schema = unique_schema("tx_basic");
    let url = schema_scoped_url(&schema);
    let tlog = TransactLog::open(&LogTarget::Postgres(url)).expect("TransactLog::open must succeed against real Postgres");

    let txn_id = "txn-pg-001";
    tlog.begin_pending(
        txn_id,
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

    assert_eq!(tlog.state_of(txn_id).expect("state_of must succeed"), Some("pending".to_string()));

    tlog.record_network_result(txn_id, &nirdosha::interpreter::Value::Bool(true)).expect("record_network_result must succeed");
    assert_eq!(tlog.state_of(txn_id).expect("state_of must succeed"), Some("network_done".to_string()));

    tlog.record_verify(txn_id, &[nirdosha::interpreter::Value::Bool(true)], true).expect("record_verify must succeed");
    tlog.mark_commit_pending(txn_id, &[nirdosha::interpreter::Value::Int(500)]).expect("mark_commit_pending must succeed");
    assert_eq!(tlog.state_of(txn_id).expect("state_of must succeed"), Some("commit_pending".to_string()));

    let unresolved = tlog.list_unresolved().expect("list_unresolved must succeed");
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].txn_id, txn_id);
    assert_eq!(unresolved[0].verify_result, Some(true), "verify_result must round-trip as a real Postgres boolean");
    assert_eq!(unresolved[0].commit_args.as_ref().map(|a| a.len()), Some(1));

    tlog.mark_terminal(txn_id, "committed").expect("mark_terminal must succeed");
    assert_eq!(tlog.state_of(txn_id).expect("state_of must succeed"), Some("committed".to_string()));
    let unresolved_after = tlog.list_unresolved().expect("list_unresolved must succeed");
    assert!(unresolved_after.is_empty(), "a committed row must no longer be unresolved");

    drop_schema(&schema);
}

/// Same shared-table proof as `WorkflowLog`'s own, for `TransactLog`:
/// this module's own doc comment names the exact defect a local SQLite
/// file has -- a retried request landing on a different replica sees no
/// row for its `txn_id` and re-executes `network`. With a shared
/// Postgres table, the second replica must see the FIRST replica's row.
#[test]
#[ignore]
fn a_txn_id_begun_by_one_replica_is_visible_to_another_replica_sharing_the_same_postgres_table() {
    let schema = unique_schema("tx_multi_instance");
    let url = schema_scoped_url(&schema);
    let replica_a = TransactLog::open(&LogTarget::Postgres(url.clone())).expect("replica A must open");
    let replica_b = TransactLog::open(&LogTarget::Postgres(url)).expect("replica B must open");

    let txn_id = "txn-shared-001";
    replica_a
        .begin_pending(
            txn_id,
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

    // Replica B never called begin_pending for this txn_id -- if it
    // still sees a "pending" row, this proves the idempotency guarantee
    // holds across replicas, not just within replica A's own process.
    let seen_by_b = replica_b.state_of(txn_id).expect("state_of must succeed");
    assert_eq!(seen_by_b, Some("pending".to_string()), "replica B must see replica A's own in-flight txn_id via the shared table");

    drop_schema(&schema);
}
