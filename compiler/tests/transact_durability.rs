//! Tests for `transact { ... }`'s durability log and crash replay
//! (`TRANSACT.md`'s Layers 3-4, `src/transact_log.rs`,
//! `Interpreter::replay_pending_transactions`) — the actual fix for the
//! two failure modes investigated in the session that produced this file:
//! (1) `commit` failing (a trap, or a `Result(_, _)` returned in its
//! `Err` variant) used to be silently discarded, so `transact` still
//! reported `true`; (2) nothing survived a crash between `network`
//! succeeding and `commit` running. See `tests/transact.rs` for the
//! in-process control-flow tests and the typeck-level guardrails
//! (`precheck`, mandatory `txn_id`, the durable-scalar restriction).

use std::io::{Read, Write};
use std::net::TcpListener;
use std::sync::Arc;

use nirdosha::interpreter::{ErrorKind, Interpreter, ReplayOutcome, Value};
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::transact_log::TransactLog;
use nirdosha::typeck::typecheck;

/// Same "bind to an OS-assigned free port" convention `tests/tcp.rs`
/// already establishes -- avoids any fixed port number that could
/// collide with something else already listening on the machine running
/// these tests.
fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

fn build_program(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
    program
}

fn temp_path(name: &str) -> std::path::PathBuf {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    std::env::temp_dir().join(format!("nirdosha-test-{name}-{}-{n}.db", std::process::id()))
}

fn run_with_log(src: &str, log_path: &std::path::Path) -> Result<Value, String> {
    let program = build_program(src);
    Interpreter::new(Arc::new(program), Arc::from(src))
        .with_transact_log_path(log_path.to_path_buf())
        .run_main_on_big_stack()
        .map_err(|e| e.to_string())
}

// ---- precheck: nothing durable when it aborts -----------------------------

#[test]
fn precheck_false_never_touches_the_durability_log() {
    let log_path = temp_path("precheck-false");
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
    assert_eq!(run_with_log(src, &log_path), Ok(Value::Bool(false)));
    assert!(!log_path.exists(), "precheck aborting should never open/create the durability log at all");
}

// ---- the live path writes a real, inspectable durable row -----------------

#[test]
fn successful_transact_ends_with_a_committed_log_row() {
    let log_path = temp_path("committed-row");
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, 10)
                verify:  check(network)
                commit:  update_db(network)
            }
        }
    "#;
    assert_eq!(run_with_log(src, &log_path), Ok(Value::Bool(true)));
    let tlog = TransactLog::open(&log_path).expect("log file should exist and open");
    let unresolved = tlog.list_unresolved().expect("list_unresolved should succeed");
    assert!(unresolved.is_empty(), "a successfully committed transact should leave no unresolved row: {unresolved:?}");
}

#[test]
fn compensated_transact_ends_with_a_compensated_log_row() {
    let log_path = temp_path("compensated-row");
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
    assert_eq!(run_with_log(src, &log_path), Ok(Value::Bool(false)));
    let tlog = TransactLog::open(&log_path).expect("log file should exist and open");
    assert!(tlog.list_unresolved().expect("list_unresolved should succeed").is_empty());
}

// ---- the actual bug fix: commit's failure is no longer discarded ----------

/// `commit` fails (via a real trap) its first two attempts against a real,
/// file-backed SQLite counter, then succeeds on the third -- proving
/// `run_transact_write_slot` actually retries instead of accepting the
/// first failure. If this regressed to the old behavior (a trap inside
/// `commit` propagating straight out and killing the whole `transact`,
/// or a failure being silently discarded), this test would either see the
/// whole `run` fail outright or see the counter never reach past `1`.
#[test]
fn commit_that_traps_twice_then_succeeds_is_retried_to_success() {
    let counter_db = temp_path("commit-flaky-counter");
    let log_path = temp_path("commit-flaky-log");
    let counter_db_str = counter_db.to_string_lossy().replace('\\', "\\\\");
    let src = format!(
        r#"
        fn call_api(txn_id: str, amount: i64) -> i64 {{ return amount }}
        fn check(resp: i64) -> bool {{ return resp > 0 }}
        fn commit_flaky(amount: i64) -> i64 {{
            return match db_connect("{counter_db_str}") {{
                Ok(conn) => match db_execute(conn, "CREATE TABLE IF NOT EXISTS counter (id INTEGER PRIMARY KEY, n INTEGER)") {{
                    Ok(created) => match db_execute(conn, "INSERT INTO counter (id, n) VALUES (1, 1) ON CONFLICT(id) DO UPDATE SET n = n + 1") {{
                        Ok(upserted) => match db_query(conn, "SELECT n FROM counter WHERE id = 1") {{
                            Ok(rows) => match json_array_get(rows, 0) {{
                                Ok(row) => match json_get_i64(row, "n") {{
                                    Ok(n) => if n < 3 {{ 1 / 0 }} else {{ amount }},
                                    Err(e) => 1 / 0,
                                }},
                                Err(e) => 1 / 0,
                            }},
                            Err(e) => 1 / 0,
                        }},
                        Err(e) => 1 / 0,
                    }},
                    Err(e) => 1 / 0,
                }},
                Err(e) => 1 / 0,
            }}
        }}
        fn main() -> bool {{
            return transact {{
                network: call_api(txn_id, 10)
                verify:  check(network)
                commit:  commit_flaky(network)
            }}
        }}
    "#
    );
    assert_eq!(run_with_log(&src, &log_path), Ok(Value::Bool(true)));

    let conn = rusqlite::Connection::open(&counter_db).expect("counter db should exist");
    let n: i64 = conn.query_row("SELECT n FROM counter WHERE id = 1", [], |r| r.get(0)).expect("counter row should exist");
    assert_eq!(n, 3, "commit_flaky should have been attempted exactly 3 times (fail, fail, succeed)");
}

/// Same failure shape as above, but via `commit`'s return type being a
/// real `Result(i64, str)` in its `Err` variant instead of a trap -- the
/// exact silent-discard bug this whole durability pass exists to close
/// (`commit`'s return value used to be "unconstrained and discarded," so
/// a `db_execute`-style `Err` inside it was invisible to `transact`).
#[test]
fn commit_returning_result_err_is_retried_not_silently_accepted() {
    let counter_db = temp_path("commit-result-err-counter");
    let log_path = temp_path("commit-result-err-log");
    let counter_db_str = counter_db.to_string_lossy().replace('\\', "\\\\");
    let src = format!(
        r#"
        enum ErrorCode {{
            NotYet,
            BuiltinError,
        }}
        fn call_api(txn_id: str, amount: i64) -> i64 {{ return amount }}
        fn check(resp: i64) -> bool {{ return resp > 0 }}
        fn commit_result_flaky(amount: i64) -> Result(i64, ErrorCode) {{
            return match db_connect("{counter_db_str}") {{
                Ok(conn) => match db_execute(conn, "CREATE TABLE IF NOT EXISTS counter (id INTEGER PRIMARY KEY, n INTEGER)") {{
                    Ok(created) => match db_execute(conn, "INSERT INTO counter (id, n) VALUES (1, 1) ON CONFLICT(id) DO UPDATE SET n = n + 1") {{
                        Ok(upserted) => match db_query(conn, "SELECT n FROM counter WHERE id = 1") {{
                            Ok(rows) => match json_array_get(rows, 0) {{
                                Ok(row) => match json_get_i64(row, "n") {{
                                    Ok(n) => if n < 3 {{ Err(NotYet()) }} else {{ Ok(amount) }},
                                    Err(e) => Err(BuiltinError()),
                                }},
                                Err(e) => Err(BuiltinError()),
                            }},
                            Err(e) => Err(BuiltinError()),
                        }},
                        Err(e) => Err(BuiltinError()),
                    }},
                    Err(e) => Err(BuiltinError()),
                }},
                Err(e) => Err(BuiltinError()),
            }}
        }}
        fn main() -> bool {{
            return transact {{
                network: call_api(txn_id, 10)
                verify:  check(network)
                commit:  commit_result_flaky(network)
            }}
        }}
    "#
    );
    assert_eq!(run_with_log(&src, &log_path), Ok(Value::Bool(true)));

    let conn = rusqlite::Connection::open(&counter_db).expect("counter db should exist");
    let n: i64 = conn.query_row("SELECT n FROM counter WHERE id = 1", [], |r| r.get(0)).expect("counter row should exist");
    assert_eq!(n, 3, "commit_result_flaky should have been attempted exactly 3 times (Err, Err, Ok)");
}

// ---- retry budget exhausted: trap, don't guess -----------------------------

#[test]
fn commit_that_always_fails_traps_with_commit_pending_not_a_silent_true_or_false() {
    let log_path = temp_path("commit-always-fails");
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn commit_always_fails(amount: i64) -> i64 { return 1 / 0 }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, 10)
                verify:  check(network)
                commit:  commit_always_fails(network)
            }
        }
    "#;
    let program = build_program(src);
    let interp = Interpreter::new(Arc::new(program), Arc::from(src)).with_transact_log_path(log_path.clone());
    match interp.run_main_on_big_stack() {
        Err(e) => assert!(matches!(e.kind, ErrorKind::TransactCommitPending { .. }), "expected TransactCommitPending, got {e:?}"),
        Ok(v) => panic!("expected a trap, got Ok({v:?}) -- commit's exhausted retries must never resolve to a guessed bool"),
    }

    let tlog = TransactLog::open(&log_path).expect("log file should exist and open");
    let unresolved = tlog.list_unresolved().expect("list_unresolved should succeed");
    assert_eq!(unresolved.len(), 1, "the row should be left non-terminal, eligible for replay");
    assert_eq!(unresolved[0].state, "commit_pending");
}

// ---- Layer 2: `retry`/`timeout` on `network`, and how they interact -----
// ---- with the durability log and crash replay ---------------------------

/// `network` traps its first two attempts against a real, file-backed
/// SQLite counter, then succeeds on the third -- `retry 5` recovers, and
/// the log's own row lands `committed`, same as any other successful run.
#[test]
fn network_retry_recovers_from_a_transient_trap_and_the_log_still_lands_committed() {
    let counter_db = temp_path("network-flaky-counter");
    let log_path = temp_path("network-flaky-log");
    let counter_db_str = counter_db.to_string_lossy().replace('\\', "\\\\");
    let src = format!(
        r#"
        fn call_api(txn_id: str, amount: i64) -> i64 {{
            return match db_connect("{counter_db_str}") {{
                Ok(conn) => match db_execute(conn, "CREATE TABLE IF NOT EXISTS counter (id INTEGER PRIMARY KEY, n INTEGER)") {{
                    Ok(created) => match db_execute(conn, "INSERT INTO counter (id, n) VALUES (1, 1) ON CONFLICT(id) DO UPDATE SET n = n + 1") {{
                        Ok(upserted) => match db_query(conn, "SELECT n FROM counter WHERE id = 1") {{
                            Ok(rows) => match json_array_get(rows, 0) {{
                                Ok(row) => match json_get_i64(row, "n") {{
                                    Ok(n) => if n < 3 {{ 1 / 0 }} else {{ amount }},
                                    Err(e) => 1 / 0,
                                }},
                                Err(e) => 1 / 0,
                            }},
                            Err(e) => 1 / 0,
                        }},
                        Err(e) => 1 / 0,
                    }},
                    Err(e) => 1 / 0,
                }},
                Err(e) => 1 / 0,
            }}
        }}
        fn check(resp: i64) -> bool {{ return resp > 0 }}
        fn update_db(amount: i64) -> i64 {{ return amount }}
        fn main() -> bool {{
            return transact {{
                network: call_api(txn_id, 10) retry 5
                verify:  check(network)
                commit:  update_db(network)
            }}
        }}
    "#
    );
    assert_eq!(run_with_log(&src, &log_path), Ok(Value::Bool(true)));

    let conn = rusqlite::Connection::open(&counter_db).expect("counter db should exist");
    let n: i64 = conn.query_row("SELECT n FROM counter WHERE id = 1", [], |r| r.get(0)).expect("counter row should exist");
    assert_eq!(n, 3, "network's call_api should have been attempted exactly 3 times (fail, fail, succeed)");

    let tlog = TransactLog::open(&log_path).expect("log file should exist and open");
    assert!(tlog.list_unresolved().expect("list_unresolved should succeed").is_empty());
}

/// `timeout` bounds each individual attempt: a `network` that legitimately
/// hangs longer than its budget traps with `TransactNetworkTimedOut`
/// instead of blocking the whole `transact` (and, transitively, a
/// `nirdosha serve` process's startup replay) forever.
#[test]
fn network_timeout_traps_with_the_specific_error_kind_and_leaves_the_row_pending() {
    let log_path = temp_path("network-timeout-log");
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
    let program = build_program(src);
    let interp = Interpreter::new(Arc::new(program), Arc::from(src)).with_transact_log_path(log_path.clone());
    match interp.run_main_on_big_stack() {
        Err(e) => assert!(
            matches!(e.kind, ErrorKind::TransactNetworkTimedOut { seconds: 1 }),
            "expected TransactNetworkTimedOut{{seconds:1}}, got {e:?}"
        ),
        Ok(v) => panic!("expected a timeout trap, got Ok({v:?})"),
    }

    let tlog = TransactLog::open(&log_path).expect("log file should exist and open");
    let unresolved = tlog.list_unresolved().expect("list_unresolved should succeed");
    assert_eq!(unresolved.len(), 1);
    assert_eq!(unresolved[0].state, "pending", "network never confirmed -- must stay pending, eligible for replay");
}

/// Replay respects the same `retry`/`timeout` budget the original live
/// attempt had -- a `"pending"` row's `network` re-invocation isn't a
/// single unconditional, un-timed-out call. Seeds a row exactly as a
/// crash mid-`network` would leave it (no live `transact` execution at
/// all), with `network_retry`/`network_timeout` captured, and confirms
/// replay both honors the retry budget (recovers from a transient trap)
/// and reaches `committed`.
#[test]
fn replay_honors_the_captured_retry_budget_for_a_pending_network_row() {
    let counter_db = temp_path("replay-retry-counter");
    let log_path = temp_path("replay-retry-log");
    let counter_db_str = counter_db.to_string_lossy().replace('\\', "\\\\");
    let src = format!(
        r#"
        fn call_api(txn_id: str, amount: i64) -> i64 {{
            return match db_connect("{counter_db_str}") {{
                Ok(conn) => match db_execute(conn, "CREATE TABLE IF NOT EXISTS counter (id INTEGER PRIMARY KEY, n INTEGER)") {{
                    Ok(created) => match db_execute(conn, "INSERT INTO counter (id, n) VALUES (1, 1) ON CONFLICT(id) DO UPDATE SET n = n + 1") {{
                        Ok(upserted) => match db_query(conn, "SELECT n FROM counter WHERE id = 1") {{
                            Ok(rows) => match json_array_get(rows, 0) {{
                                Ok(row) => match json_get_i64(row, "n") {{
                                    Ok(n) => if n < 2 {{ 1 / 0 }} else {{ amount }},
                                    Err(e) => 1 / 0,
                                }},
                                Err(e) => 1 / 0,
                            }},
                            Err(e) => 1 / 0,
                        }},
                        Err(e) => 1 / 0,
                    }},
                    Err(e) => 1 / 0,
                }},
                Err(e) => 1 / 0,
            }}
        }}
        fn check(resp: i64) -> bool {{ return resp > 0 }}
        fn update_db(amount: i64) -> i64 {{ return amount }}
        fn main() -> unit {{}}
    "#
    );
    let program = build_program(&src);
    let tlog = TransactLog::open(&log_path).expect("open should succeed");
    tlog.begin_pending(
        "txn-retry",
        "call_api",
        &[Value::Str(Arc::from("txn-retry")), Value::Int(10)],
        Some(5),
        None,
        "check",
        &["network"],
        "update_db",
        None,
    )
    .expect("begin_pending should succeed");

    // This row was only ever `begin_pending`'d (simulating a crash before
    // `verify` ever ran in the original process) -- `commit`'s arguments
    // were never durably captured, so replay correctly reports `Stuck`
    // once it gets past `network`/`verify` (the same honest gap
    // `replay_resumes_network_from_pending_but_reports_stuck_when_
    // commit_args_were_never_captured` already covers). What this test
    // is actually checking is narrower: that replay's own `network`
    // re-invocation used the *captured* `retry 5` budget to get there at
    // all, rather than giving up after a single failed attempt.
    let interp = Interpreter::new(Arc::new(program), Arc::from(src.as_str())).with_transact_log_path(log_path.clone());
    let outcomes = interp.replay_pending_transactions().expect("replay should succeed");
    match &outcomes[0] {
        ReplayOutcome::Stuck { txn_id, .. } => assert_eq!(txn_id, "txn-retry"),
        other => panic!("expected Stuck (commit's args were never captured), got {other:?}"),
    }
    assert_eq!(tlog.state_of("txn-retry").unwrap(), Some("network_done".to_string()));

    let conn = rusqlite::Connection::open(&counter_db).expect("counter db should exist");
    let n: i64 = conn.query_row("SELECT n FROM counter WHERE id = 1", [], |r| r.get(0)).expect("counter row should exist");
    assert_eq!(n, 2, "replay's own network re-invocation should have retried (fail, then succeed) using the captured retry:5 budget, not given up after one attempt");
}

// ---- the pre-existing bug this pass also fixed: `log`'s trap must not -----
// ---- undo an already-successful commit/compensate -------------------------

#[test]
fn a_trap_inside_log_never_undoes_an_already_committed_transact() {
    let log_path = temp_path("log-slot-traps");
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn write_log_traps(amount: i64, ok: bool) -> unit {
            let boom: i64 = 1 / 0
        }
        fn main() -> bool {
            return transact {
                network: call_api(txn_id, 10)
                verify:  check(network)
                commit:  update_db(network)
                log:     write_log_traps(network, verify)
            }
        }
    "#;
    assert_eq!(
        run_with_log(src, &log_path),
        Ok(Value::Bool(true)),
        "commit already succeeded before `log` ran -- a trap inside `log` must be swallowed, not propagated"
    );
}

// ---- crash replay -----------------------------------------------------------

/// A row left `commit_pending` (as if the process crashed right after
/// logging intent to commit but before -- or during -- the write itself)
/// is fully resumable: `commit`'s callee and exact arguments were already
/// durably captured, so replay just retries it, same as the live path.
#[test]
fn replay_resumes_a_commit_pending_row_to_committed() {
    let log_path = temp_path("replay-commit-pending");
    let src = r#"
        fn update_db(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn main() -> unit {}
    "#;
    let program = build_program(src);
    let tlog = TransactLog::open(&log_path).expect("open should succeed");
    tlog.begin_pending(
        "txn-1",
        "call_api",
        &[Value::Str(Arc::from("txn-1")), Value::Int(10)],
        None,
        None,
        "check",
        &["network"],
        "update_db",
        None,
    )
    .expect("begin_pending should succeed");
    tlog.record_network_result("txn-1", &Value::Int(10)).expect("record_network_result should succeed");
    tlog.mark_commit_pending("txn-1", &[Value::Int(10)]).expect("mark_commit_pending should succeed");

    let interp = Interpreter::new(Arc::new(program), Arc::from(src)).with_transact_log_path(log_path.clone());
    let outcomes = interp.replay_pending_transactions().expect("replay should succeed");
    assert_eq!(outcomes, vec![ReplayOutcome::Resolved { txn_id: "txn-1".to_string(), committed: true }]);
    assert_eq!(tlog.state_of("txn-1").unwrap(), Some("committed".to_string()));
}

/// Same shape, for `compensate_pending`.
#[test]
fn replay_resumes_a_compensate_pending_row_to_compensated() {
    let log_path = temp_path("replay-compensate-pending");
    let src = r#"
        fn refund(amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {}
    "#;
    let program = build_program(src);
    let tlog = TransactLog::open(&log_path).expect("open should succeed");
    tlog.begin_pending(
        "txn-2",
        "call_api",
        &[Value::Str(Arc::from("txn-2")), Value::Int(-5)],
        None,
        None,
        "check",
        &["network"],
        "update_db",
        Some("refund"),
    )
    .expect("begin_pending should succeed");
    tlog.record_network_result("txn-2", &Value::Int(-5)).expect("record_network_result should succeed");
    tlog.mark_compensate_pending("txn-2", &[Value::Int(-5)]).expect("mark_compensate_pending should succeed");

    let interp = Interpreter::new(Arc::new(program), Arc::from(src)).with_transact_log_path(log_path.clone());
    let outcomes = interp.replay_pending_transactions().expect("replay should succeed");
    assert_eq!(outcomes, vec![ReplayOutcome::Resolved { txn_id: "txn-2".to_string(), committed: false }]);
    assert_eq!(tlog.state_of("txn-2").unwrap(), Some("compensated".to_string()));
}

/// A row still `"pending"` (crash during/right after `network`, before
/// its result was ever durably recorded) -- replay re-invokes `network`
/// with the same `txn_id` (the idempotency-key contract), reconstructs
/// `verify`'s arguments from `network`'s freshly-recomputed result (the
/// static `network`/`txn_id`-only restriction on `verify`'s arguments is
/// what makes this always possible), and runs it. Here `commit`'s own
/// arguments were never captured (the original process crashed before
/// ever reaching that point) -- exactly `replay_pending_transactions`'s
/// one honest, named gap: reported `Stuck`, not guessed at.
#[test]
fn replay_resumes_network_from_pending_but_reports_stuck_when_commit_args_were_never_captured() {
    let log_path = temp_path("replay-pending-stuck");
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {}
    "#;
    let program = build_program(src);
    let tlog = TransactLog::open(&log_path).expect("open should succeed");
    tlog.begin_pending(
        "txn-3",
        "call_api",
        &[Value::Str(Arc::from("txn-3")), Value::Int(7)],
        None,
        None,
        "check",
        &["network"],
        "update_db",
        None,
    )
    .expect("begin_pending should succeed");
    // Deliberately no `record_network_result`/`mark_commit_pending` --
    // this row is exactly where a crash right after `begin_pending`
    // would leave it: `state = "pending"`.

    let interp = Interpreter::new(Arc::new(program), Arc::from(src)).with_transact_log_path(log_path.clone());
    let outcomes = interp.replay_pending_transactions().expect("replay should succeed");
    assert_eq!(outcomes.len(), 1);
    match &outcomes[0] {
        ReplayOutcome::Stuck { txn_id, reason } => {
            assert_eq!(txn_id, "txn-3");
            assert!(reason.contains("commit"), "reason should name commit's arguments as the gap: {reason}");
        }
        other => panic!("expected Stuck, got {other:?}"),
    }
    // `network` was still durably re-recorded even though replay
    // couldn't finish -- the highest-stakes ambiguity (did the real
    // side effect happen) is resolved even when the rest isn't.
    assert_eq!(tlog.state_of("txn-3").unwrap(), Some("network_done".to_string()));
}

/// `state = "pending"` with no `compensate` slot and `verify` false: fully
/// resolvable even from the narrowest crash window, since there's no
/// write left to reconstruct arguments for at all.
#[test]
fn replay_resumes_pending_straight_to_compensated_when_verify_is_false_and_no_compensate_slot_exists() {
    let log_path = temp_path("replay-pending-no-compensate");
    let src = r#"
        fn call_api(txn_id: str, amount: i64) -> i64 { return amount }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {}
    "#;
    let program = build_program(src);
    let tlog = TransactLog::open(&log_path).expect("open should succeed");
    tlog.begin_pending(
        "txn-4",
        "call_api",
        &[Value::Str(Arc::from("txn-4")), Value::Int(-1)],
        None,
        None,
        "check",
        &["network"],
        "update_db",
        None,
    )
    .expect("begin_pending should succeed");

    let interp = Interpreter::new(Arc::new(program), Arc::from(src)).with_transact_log_path(log_path.clone());
    let outcomes = interp.replay_pending_transactions().expect("replay should succeed");
    assert_eq!(outcomes, vec![ReplayOutcome::Resolved { txn_id: "txn-4".to_string(), committed: false }]);
    assert_eq!(tlog.state_of("txn-4").unwrap(), Some("compensated".to_string()));
}

/// `network` still failing when replay tries it again: reported `Stuck`,
/// row left exactly at `"pending"` so a later replay call can try again.
#[test]
fn replay_leaves_a_still_failing_network_at_pending_for_the_next_attempt() {
    let log_path = temp_path("replay-network-still-failing");
    let src = r#"
        fn call_api_traps(txn_id: str, amount: i64) -> i64 { return 1 / 0 }
        fn check(resp: i64) -> bool { return resp > 0 }
        fn update_db(amount: i64) -> i64 { return amount }
        fn main() -> unit {}
    "#;
    let program = build_program(src);
    let tlog = TransactLog::open(&log_path).expect("open should succeed");
    tlog.begin_pending(
        "txn-5",
        "call_api_traps",
        &[Value::Str(Arc::from("txn-5")), Value::Int(1)],
        None,
        None,
        "check",
        &["network"],
        "update_db",
        None,
    )
    .expect("begin_pending should succeed");

    let interp = Interpreter::new(Arc::new(program), Arc::from(src)).with_transact_log_path(log_path.clone());
    let outcomes = interp.replay_pending_transactions().expect("replay should succeed");
    match &outcomes[0] {
        ReplayOutcome::Stuck { txn_id, .. } => assert_eq!(txn_id, "txn-5"),
        other => panic!("expected Stuck, got {other:?}"),
    }
    assert_eq!(tlog.state_of("txn-5").unwrap(), Some("pending".to_string()), "network never confirmed -- must stay pending, not silently advance");
}

// ---- Layer 5: `network`'s function using a real cross-process effect ----
// ---- internally (`connect`/`tcp`) -----------------------------------------

/// `network`'s slot is still just one named `Expr::Call` -- what
/// `call_gateway`'s own body does internally is unconstrained, and here
/// it's a real TCP round trip to a separate listener (a thread standing
/// in for a separate process, the same self-contained convention
/// `tests/tcp.rs` already establishes -- see `examples/
/// transact_cross_process.nir` for the version that needs a real
/// external listener instead). Durability, retry/timeout, and the
/// `txn_id` idempotency-key contract all apply exactly the same way they
/// do to an in-process `network` function -- this is the whole point of
/// TRANSACT.md's "no grammar change needed" Layer 5 decision, proven
/// end to end rather than just asserted.
#[test]
fn network_slot_talks_to_a_real_separate_process_over_tcp() {
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let received_txn_ids = Arc::new(std::sync::Mutex::new(Vec::new()));
    let received_clone = Arc::clone(&received_txn_ids);
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap();
        received_clone.lock().unwrap().push(String::from_utf8_lossy(&buf[..n]).to_string());
        stream.write_all(b"charged").unwrap();
    });

    let log_path = temp_path("cross-process-tcp");
    let src = format!(
        r#"
        fn call_gateway(txn_id: str, amount: i64) -> i64 {{
            let conn: tcp = connect("127.0.0.1", {port})
            send(conn, txn_id)
            let reply: str = recv(conn)
            stop conn
            return amount
        }}
        fn check(resp: i64) -> bool {{ return resp > 0 }}
        fn update_db(amount: i64) -> i64 {{ return amount }}
        fn main() -> bool {{
            return transact {{
                network: call_gateway(txn_id, 42) retry 3 timeout 5
                verify:  check(network)
                commit:  update_db(network)
            }}
        }}
    "#
    );
    assert_eq!(run_with_log(&src, &log_path), Ok(Value::Bool(true)));
    server.join().unwrap();

    let received = received_txn_ids.lock().unwrap();
    assert_eq!(received.len(), 1, "the gateway should have been contacted exactly once");
    assert!(!received[0].is_empty(), "the real `txn_id` idempotency key should have reached the far side");

    let tlog = TransactLog::open(&log_path).expect("log file should exist and open");
    assert!(tlog.list_unresolved().expect("list_unresolved should succeed").is_empty());
}
