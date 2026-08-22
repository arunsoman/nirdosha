//! `transact { ... }`'s durable write-ahead log (`TRANSACT.md`'s Layers
//! 3-4). SQLite-backed (`rusqlite` is already a dependency — no new
//! crate); every write here is a single synchronous statement, never
//! batched or backgrounded, the opposite of `observability.rs`'s
//! fail-open/async `Tracer`. That's deliberate: `observability.rs`'s
//! whole contract is "a tracing failure never becomes a `RuntimeError`",
//! while this module's whole reason to exist is "the write genuinely has
//! to have happened before the interpreter proceeds to the next step" —
//! `TRANSACT.md`'s "recorded **before** `verify` runs" crash-safety
//! boundary would be fiction otherwise.
//!
//! ## What's stored, and why it's enough to replay
//!
//! One row per `transact` invocation, keyed by `txn_id` (the same
//! idempotency key `network`'s own call is required to carry —
//! `typeck.rs`'s `TransactNetworkMustUseTxnId`). Every slot whose
//! invocation a crash might need to reconstruct (`network`, `verify`,
//! `commit`, `compensate` — never `precheck`/`log`, neither of which is
//! ever logged or replayed, see `ast::Expr::Transact`'s doc comment)
//! gets its callee name and fully-evaluated arguments recorded
//! *before* that slot runs, so `Interpreter::replay_pending_transactions`
//! can re-invoke exactly that call via `Interpreter::call` without ever
//! needing the original `ast::Expr::Transact` node or the caller's own
//! environment — the row is self-contained.
//!
//! ## Why only scalars
//!
//! `typeck.rs::infer_transact_slot_durable` restricts every value that
//! reaches this module to `Ty::is_transact_scalar` (`i*`/`u*`/`usize`/
//! `f64`/`bool`/`str`) — a resource handle (`db`/`tcp`/`thread`/
//! `sandbox`) can't survive a process restart, so nothing that crosses
//! `transact`'s durability boundary is allowed to be one. That's what
//! makes `value_to_json`/`value_from_json` below total functions instead
//! of needing to handle (and fail on) an unrepresentable `Value` variant.

use std::path::Path;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use crate::interpreter::Value;

/// A `transact` invocation's durable state. Matches the `transact_log`
/// table's columns 1:1 — see `list_unresolved`'s query for the exact
/// `SELECT` this is built from.
#[derive(Debug, Clone)]
pub struct PendingTxn {
    pub txn_id: String,
    pub state: String,
    pub network_fn: String,
    pub network_args: Vec<Value>,
    /// `network`'s own `retry`/`timeout` modifiers (TRANSACT.md's Layer
    /// 2), captured up front so a `"pending"`-state replay re-invokes
    /// `network` with the *same* budget the original live attempt had —
    /// without this, a `network` call written to legitimately hang
    /// (exactly what `timeout` exists to bound) could hang startup
    /// replay itself indefinitely.
    pub network_retry: Option<i64>,
    pub network_timeout: Option<i64>,
    pub network_result: Option<Value>,
    /// Always known from the moment the row exists (`begin_pending`
    /// captures every slot's *callee name* up front — all four are
    /// static, known before anything runs).
    pub verify_fn: String,
    /// Each element is `"network"` or `"txn_id"` — `verify`'s arguments
    /// are statically restricted to exactly those two implicit bindings
    /// (`typeck.rs::TransactVerifyArgsMustBeImplicitBindings`), so
    /// `replay_pending_transactions` can always rebuild `verify_args`
    /// from `network_result`/`txn_id` alone, even from `state =
    /// "pending"` where `verify` never actually ran in the original
    /// process.
    pub verify_arg_kinds: Vec<String>,
    /// The actual evaluated arguments, once `verify` has actually run
    /// (live or replay) — `None` until then.
    pub verify_args: Option<Vec<Value>>,
    pub verify_result: Option<bool>,
    pub commit_fn: String,
    /// `Some` only once `commit` has actually been *reached* (`verify`
    /// ran and returned `true`) — unlike `verify_fn`, `commit`'s
    /// arguments can freely reference outer-scope variables from the
    /// enclosing function (`amount`, e.g.), which replay has no way to
    /// reconstruct on its own. `None` here from a crash that preempted
    /// `commit` ever being reached is a real, honest gap — see
    /// `Interpreter::replay_pending_transactions`'s own doc comment.
    pub commit_args: Option<Vec<Value>>,
    /// `None` when the `transact` block has no `compensate` slot at all.
    pub compensate_fn: Option<String>,
    pub compensate_args: Option<Vec<Value>>,
}

pub struct TransactLog {
    conn: Mutex<Connection>,
}

/// `Value` -> a small, self-describing JSON shape (`{"t": "i"|"f"|"b"|"s",
/// "v": ...}`) — self-describing so `value_from_json` never needs the
/// callee's declared parameter `Ty` back to know how to reconstruct a
/// value, only the JSON it already wrote.
fn value_to_json(v: &Value) -> serde_json::Value {
    match v {
        Value::Int(n) => serde_json::json!({"t": "i", "v": n}),
        Value::Float(n) => serde_json::json!({"t": "f", "v": n}),
        Value::Bool(b) => serde_json::json!({"t": "b", "v": b}),
        Value::Str(s) => serde_json::json!({"t": "s", "v": s.as_ref()}),
        other => unreachable!(
            "typeck.rs::infer_transact_slot_durable already restricted every value that reaches \
             this module to Ty::is_transact_scalar -- got {other:?}"
        ),
    }
}

fn value_from_json(v: &serde_json::Value) -> Value {
    match v.get("t").and_then(|t| t.as_str()) {
        Some("i") => Value::Int(v["v"].as_i64().expect("value_to_json's own format")),
        Some("f") => Value::Float(v["v"].as_f64().expect("value_to_json's own format")),
        Some("b") => Value::Bool(v["v"].as_bool().expect("value_to_json's own format")),
        Some("s") => Value::Str(Arc::from(v["v"].as_str().expect("value_to_json's own format"))),
        _ => unreachable!("value_from_json only ever reads back value_to_json's own format"),
    }
}

fn args_to_json(vals: &[Value]) -> String {
    let arr: Vec<serde_json::Value> = vals.iter().map(value_to_json).collect();
    serde_json::to_string(&arr).expect("value_to_json's output is always valid JSON")
}

fn args_from_json(s: &str) -> Vec<Value> {
    let arr: Vec<serde_json::Value> =
        serde_json::from_str(s).expect("args_to_json's own format, always round-trips");
    arr.iter().map(value_from_json).collect()
}

impl TransactLog {
    /// Opens (creating if absent) the durable log at `path`, and sets
    /// `PRAGMA synchronous = FULL` — SQLite's most durable setting,
    /// fsyncing before every transaction commits. This is the actual
    /// cost of the correctness bar `TRANSACT.md`'s durability section
    /// commits to: every write below is slower than an ordinary
    /// buffered write on purpose, because "recorded before `verify`
    /// runs" has to mean recorded, not "handed to the OS page cache."
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        let conn = Connection::open(path)?;
        conn.pragma_update(None, "synchronous", "FULL")?;
        conn.execute(
            "CREATE TABLE IF NOT EXISTS transact_log (
                txn_id           TEXT PRIMARY KEY,
                state            TEXT NOT NULL,
                network_fn       TEXT NOT NULL,
                network_args     TEXT NOT NULL,
                network_retry    INTEGER,
                network_timeout  INTEGER,
                network_result   TEXT,
                verify_fn        TEXT NOT NULL,
                verify_arg_kinds TEXT NOT NULL,
                verify_args      TEXT,
                verify_result    INTEGER,
                commit_fn        TEXT NOT NULL,
                commit_args      TEXT,
                compensate_fn    TEXT,
                compensate_args  TEXT,
                attempts         INTEGER NOT NULL DEFAULT 0,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL
            )",
            [],
        )?;
        Ok(TransactLog { conn: Mutex::new(conn) })
    }

    /// Step 1 of the protocol: durably record intent *before* `network`
    /// runs -- every slot's *callee name* is captured right here, up
    /// front, since all four are statically known the instant the block
    /// starts (`Expr::Transact`'s own AST), long before any of them
    /// actually run. A crash between this write and
    /// `record_network_result` leaves the row in `state = "pending"` —
    /// `network`'s outcome is genuinely unknown, so replay re-invokes it
    /// with this same `txn_id` (the one point in the whole system that
    /// depends on downstream idempotency — see this module's own doc
    /// comment and `TRANSACT.md`'s durability section).
    #[allow(clippy::too_many_arguments)]
    pub fn begin_pending(
        &self,
        txn_id: &str,
        network_fn: &str,
        network_args: &[Value],
        network_retry: Option<i64>,
        network_timeout: Option<i64>,
        verify_fn: &str,
        verify_arg_kinds: &[&str],
        commit_fn: &str,
        compensate_fn: Option<&str>,
    ) -> rusqlite::Result<()> {
        let now = now_rfc3339();
        let kinds_json =
            serde_json::to_string(verify_arg_kinds).expect("verify_arg_kinds is always a plain string slice");
        self.conn.lock().unwrap().execute(
            "INSERT INTO transact_log
                 (txn_id, state, network_fn, network_args, network_retry, network_timeout, verify_fn,
                  verify_arg_kinds, commit_fn, compensate_fn, attempts, created_at, updated_at)
             VALUES (?1, 'pending', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?10)",
            params![
                txn_id,
                network_fn,
                args_to_json(network_args),
                network_retry,
                network_timeout,
                verify_fn,
                kinds_json,
                commit_fn,
                compensate_fn,
                now
            ],
        )?;
        Ok(())
    }

    /// Step 2: `network` returned. Recorded *before* `verify` runs —
    /// `TRANSACT.md`'s named crash-safety boundary. A restart that finds
    /// `state = "network_done"` never re-invokes `network`.
    pub fn record_network_result(&self, txn_id: &str, result: &Value) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE transact_log SET state = 'network_done', network_result = ?2, updated_at = ?3 WHERE txn_id = ?1",
            params![txn_id, serde_json::to_string(&value_to_json(result)).unwrap(), now_rfc3339()],
        )?;
        Ok(())
    }

    /// Records `verify`'s actual evaluated arguments and result, once
    /// it's actually run (live or replay) — `verify_fn`/`verify_arg_kinds`
    /// are already known from `begin_pending`, so this only ever fills
    /// in the dynamic half.
    pub fn record_verify(&self, txn_id: &str, verify_args: &[Value], result: bool) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE transact_log SET verify_args = ?2, verify_result = ?3, updated_at = ?4 WHERE txn_id = ?1",
            params![txn_id, args_to_json(verify_args), result, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Verify was `true`: about to attempt `commit`. Recorded before the
    /// first attempt, same "log intent, then act" discipline as
    /// `begin_pending`. `commit_fn` itself is already known (from
    /// `begin_pending`) -- only its evaluated arguments are new here.
    pub fn mark_commit_pending(&self, txn_id: &str, commit_args: &[Value]) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE transact_log SET state = 'commit_pending', commit_args = ?2, updated_at = ?3 WHERE txn_id = ?1",
            params![txn_id, args_to_json(commit_args), now_rfc3339()],
        )?;
        Ok(())
    }

    /// Verify was `false` and a `compensate` slot exists: about to
    /// attempt it. Symmetric to `mark_commit_pending` — `compensate` is
    /// a write too, and can fail the same way.
    pub fn mark_compensate_pending(&self, txn_id: &str, compensate_args: &[Value]) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE transact_log SET state = 'compensate_pending', compensate_args = ?2, updated_at = ?3
             WHERE txn_id = ?1",
            params![txn_id, args_to_json(compensate_args), now_rfc3339()],
        )?;
        Ok(())
    }

    /// A retry attempt (in-process or replay-driven) failed -- best-
    /// effort bookkeeping only, never itself part of what a replay reads
    /// to decide what to do next (that's `state` alone).
    pub fn bump_attempts(&self, txn_id: &str) -> rusqlite::Result<()> {
        self.conn.lock().unwrap().execute(
            "UPDATE transact_log SET attempts = attempts + 1, updated_at = ?2 WHERE txn_id = ?1",
            params![txn_id, now_rfc3339()],
        )?;
        Ok(())
    }

    /// `commit`/`compensate` finally succeeded: the terminal state.
    /// `state` must be `"committed"` or `"compensated"` -- both are
    /// excluded from `list_unresolved`, so this row is never touched
    /// again.
    pub fn mark_terminal(&self, txn_id: &str, state: &str) -> rusqlite::Result<()> {
        debug_assert!(state == "committed" || state == "compensated");
        self.conn.lock().unwrap().execute(
            "UPDATE transact_log SET state = ?2, updated_at = ?3 WHERE txn_id = ?1",
            params![txn_id, state, now_rfc3339()],
        )?;
        Ok(())
    }

    /// Every row not yet in a terminal state -- what
    /// `Interpreter::replay_pending_transactions` resumes on startup.
    pub fn list_unresolved(&self) -> rusqlite::Result<Vec<PendingTxn>> {
        let conn = self.conn.lock().unwrap();
        let mut stmt = conn.prepare(
            "SELECT txn_id, state, network_fn, network_args, network_retry, network_timeout, network_result,
                    verify_fn, verify_arg_kinds, verify_args, verify_result,
                    commit_fn, commit_args, compensate_fn, compensate_args
             FROM transact_log
             WHERE state NOT IN ('committed', 'compensated')",
        )?;
        let rows = stmt.query_map([], |row| {
            let network_args_json: String = row.get(3)?;
            let network_result_json: Option<String> = row.get(6)?;
            let verify_arg_kinds_json: String = row.get(8)?;
            let verify_args_json: Option<String> = row.get(9)?;
            let commit_args_json: Option<String> = row.get(12)?;
            let compensate_args_json: Option<String> = row.get(14)?;
            Ok(PendingTxn {
                txn_id: row.get(0)?,
                state: row.get(1)?,
                network_fn: row.get(2)?,
                network_args: args_from_json(&network_args_json),
                network_retry: row.get(4)?,
                network_timeout: row.get(5)?,
                network_result: network_result_json
                    .map(|s| value_from_json(&serde_json::from_str(&s).expect("record_network_result's own format"))),
                verify_fn: row.get(7)?,
                verify_arg_kinds: serde_json::from_str(&verify_arg_kinds_json)
                    .expect("begin_pending's own format"),
                verify_args: verify_args_json.as_deref().map(args_from_json),
                verify_result: row.get(10)?,
                commit_fn: row.get(11)?,
                commit_args: commit_args_json.as_deref().map(args_from_json),
                compensate_fn: row.get(13)?,
                compensate_args: compensate_args_json.as_deref().map(args_from_json),
            })
        })?;
        rows.collect()
    }

    /// Test/inspection helper: the raw `state` column for one `txn_id`,
    /// `None` if no such row exists.
    pub fn state_of(&self, txn_id: &str) -> rusqlite::Result<Option<String>> {
        self.conn
            .lock()
            .unwrap()
            .query_row("SELECT state FROM transact_log WHERE txn_id = ?1", params![txn_id], |row| row.get(0))
            .optional()
    }
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // No `chrono` dependency for one timestamp column that's never
    // parsed back by this module (only ever read by a human via
    // `sqlite3`) -- raw epoch seconds, honest about the precision it
    // actually has.
    secs.to_string()
}
