//! `transact { ... }`'s durable write-ahead log (`docs/TRANSACT.md`'s Layers
//! 3-4). Backed by either a local SQLite file (`rusqlite`, the original
//! and still-default shape) or a real, shared Postgres database
//! (`crate::durability` — `docs/ROADMAP.md`'s multi-instance coordination
//! fix); every write here is a single synchronous statement, never
//! batched or backgrounded, the opposite of `observability.rs`'s
//! fail-open/async `Tracer`. That's deliberate: `observability.rs`'s
//! whole contract is "a tracing failure never becomes a `RuntimeError`",
//! while this module's whole reason to exist is "the write genuinely has
//! to have happened before the interpreter proceeds to the next step" —
//! `docs/TRANSACT.md`'s "recorded **before** `verify` runs" crash-safety
//! boundary would be fiction otherwise.
//!
//! ## Why Postgres, not just SQLite, matters here
//!
//! `txn_id` (the caller-supplied idempotency key `network`'s own call is
//! required to carry, `typeck.rs`'s `TransactNetworkMustUseTxnId`) is
//! only a *process-lifetime* guarantee with a local SQLite file: two
//! `nirdosha serve` replicas each have their own independent file, so a
//! retried request that lands on a different replica than its first
//! attempt sees no row for that `txn_id` at all and re-executes `network`
//! from scratch — silently defeating the one idempotency guarantee this
//! whole module exists to provide. Pointing `--transact-log` at a real
//! Postgres database (`crate::durability::pg_pool`) makes every replica
//! share the same `transact_log` table, so `txn_id`'s guarantee actually
//! holds across the whole fleet, not just within one process.
//!
//! ## What's stored, and why it's enough to replay
//!
//! One row per `transact` invocation, keyed by `txn_id`. Every slot whose
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
//!
//! ## Backend dispatch
//!
//! Same shape as `workflow_log.rs`'s own — see that module's doc comment
//! for why the Postgres plain/TLS pool arms share one `&mut
//! postgres::Client`-taking function each, rather than duplicating query
//! logic per TLS shape.

use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection, OptionalExtension};

use crate::durability::{LogTarget, PgPool};
use crate::interpreter::Value;
use crate::rq_params;
use crate::rqlite::RqliteClient;

/// A `transact` invocation's durable state. Matches the `transact_log`
/// table's columns 1:1 — see `list_unresolved`'s query for the exact
/// `SELECT` this is built from.
#[derive(Debug, Clone)]
pub struct PendingTxn {
    pub txn_id: String,
    pub state: String,
    pub network_fn: String,
    pub network_args: Vec<Value>,
    /// `network`'s own `retry`/`timeout` modifiers (docs/TRANSACT.md's Layer
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
    /// Each element is `"network"`, `"txn_id"`, or `"opaque"` — unlike
    /// `verify_arg_kinds`, this is *not* a static guarantee (`commit`'s
    /// arguments are never restricted the way `verify`'s are), just a
    /// best-effort per-argument classification captured up front
    /// (`begin_pending` time, purely syntactic — no evaluation needed):
    /// an argument that's textually exactly the `network`/`txn_id`
    /// implicit binding is reconstructable from those two always-known
    /// values alone; anything else (an outer-scope variable, a computed
    /// expression) is `"opaque"`. What this closes: a crash landing
    /// between `record_verify` and `mark_commit_pending` (i.e., after
    /// `verify` succeeded but before `commit`'s own arguments were
    /// durably captured) used to be an unconditional `Stuck` even when
    /// `commit`'s arguments were exactly the same safe `network`/`txn_id`
    /// shape `verify`'s always are — this lets `Interpreter::replay_one`
    /// reconstruct them the same way it already reconstructs
    /// `verify_args`, instead of guessing or giving up.
    pub commit_arg_kinds: Vec<String>,
    /// `Some` only once `commit` has actually been *reached* (`verify`
    /// ran and returned `true`) — unlike `verify_fn`, `commit`'s
    /// arguments can freely reference outer-scope variables from the
    /// enclosing function (`amount`, e.g.), which replay has no way to
    /// reconstruct on its own unless `commit_arg_kinds` (above) says
    /// every argument is reconstructable anyway. `None` here from a
    /// crash that preempted `commit` ever being reached, combined with
    /// at least one genuinely `"opaque"` argument, is a real, honest gap
    /// — see `Interpreter::replay_pending_transactions`'s own doc comment.
    pub commit_args: Option<Vec<Value>>,
    /// `None` when the `transact` block has no `compensate` slot at all.
    pub compensate_fn: Option<String>,
    /// Same shape and same purpose as `commit_arg_kinds`, for
    /// `compensate`'s arguments — `None` exactly when `compensate_fn` is
    /// `None` (no `compensate` slot declared at all).
    pub compensate_arg_kinds: Option<Vec<String>>,
    pub compensate_args: Option<Vec<Value>>,
}

enum Backend {
    Sqlite(Mutex<Connection>),
    Postgres(r2d2::Pool<r2d2_postgres::PostgresConnectionManager<postgres::NoTls>>),
    PostgresTls(r2d2::Pool<r2d2_postgres::PostgresConnectionManager<postgres_native_tls::MakeTlsConnector>>),
    Rqlite(RqliteClient),
}

/// See `WorkflowLog`'s own doc comment for why `open` itself takes no
/// instance lock -- safe to call any number of times against the same
/// file, from this process or a separate short-lived inspector, while a
/// server keeps running. `serve::run` is the one place that acquires
/// the actual Phase 0 guard (`instance_lock.rs`), once, for the whole
/// process lifetime.
pub struct TransactLog {
    backend: Backend,
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
        // Canonical decimal string, same shape `dec_to_str`/`serve.rs`'s
        // JSON encoding already use — `Decimal` round-trips through its
        // own `Display`/`FromStr` exactly, so this is lossless (unlike
        // `Value::Float`'s arm above, which was already accepting
        // whatever precision loss an `f64` durability slot implies).
        Value::Dec128(d) => serde_json::json!({"t": "d", "v": d.to_string()}),
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
        Some("d") => Value::Dec128(
            rust_decimal::Decimal::from_str(v["v"].as_str().expect("value_to_json's own format"))
                .expect("value_to_json's own format"),
        ),
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

fn io_err(e: rusqlite::Error) -> String {
    format!("transact log I/O error: {e}")
}

fn pg_err(e: postgres::Error) -> String {
    format!("transact log I/O error (postgres): {e}")
}

fn pg_pool_err(e: r2d2::Error) -> String {
    format!("transact log I/O error (postgres pool checkout): {e}")
}

impl TransactLog {
    pub fn open(target: &LogTarget) -> Result<Self, String> {
        match target {
            LogTarget::Sqlite(path) => Self::open_sqlite(path),
            LogTarget::Postgres(conn_str) => Self::open_postgres(conn_str),
            LogTarget::Rqlite(conn_str) => Self::open_rqlite(conn_str),
        }
    }

    /// Opens (creating if absent) the durable log at `path`, and sets
    /// `PRAGMA synchronous = FULL` — SQLite's most durable setting,
    /// fsyncing before every transaction commits. This is the actual
    /// cost of the correctness bar `docs/TRANSACT.md`'s durability section
    /// commits to: every write below is slower than an ordinary
    /// buffered write on purpose, because "recorded before `verify`
    /// runs" has to mean recorded, not "handed to the OS page cache."
    fn open_sqlite(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(io_err)?;
        conn.pragma_update(None, "synchronous", "FULL").map_err(io_err)?;
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
                commit_arg_kinds TEXT NOT NULL,
                commit_args      TEXT,
                compensate_fn    TEXT,
                compensate_arg_kinds TEXT,
                compensate_args  TEXT,
                attempts         INTEGER NOT NULL DEFAULT 0,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL
            )",
            [],
        )
        .map_err(io_err)?;
        // A durability log is exactly the kind of file that has to
        // survive a `nirdosha` binary upgrade across a real restart --
        // `CREATE TABLE IF NOT EXISTS` alone leaves an *existing* log
        // file (opened by a pre-upgrade binary) missing whatever columns
        // a later version added, since it only ever creates the table
        // fresh, never alters one that's already there. `commit_arg_kinds`/
        // `compensate_arg_kinds` (added alongside the fix that lets
        // replay reconstruct `commit`/`compensate`'s arguments from
        // `network`/`txn_id` alone -- see `PendingTxn::commit_arg_kinds`'s
        // own doc comment) are backfilled here, defaulted to a value that
        // always reads back as `"opaque"` -- exactly the pre-fix behavior
        // (fall back to `Stuck` rather than guess) for whatever a
        // pre-upgrade row already had logged, since there's no way to
        // know, after the fact, what a *pre-existing* row's real argument
        // shape was. Postgres has no such legacy files (Phase 1's schema
        // is created fresh, in its final shape) -- `open_postgres` below
        // has no equivalent backfill step.
        let existing_columns: std::collections::HashSet<String> = conn
            .prepare("SELECT name FROM pragma_table_info('transact_log')")
            .map_err(io_err)?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(io_err)?
            .collect::<rusqlite::Result<_>>()
            .map_err(io_err)?;
        if !existing_columns.contains("commit_arg_kinds") {
            conn.execute("ALTER TABLE transact_log ADD COLUMN commit_arg_kinds TEXT NOT NULL DEFAULT '[\"opaque\"]'", [])
                .map_err(io_err)?;
        }
        if !existing_columns.contains("compensate_arg_kinds") {
            conn.execute("ALTER TABLE transact_log ADD COLUMN compensate_arg_kinds TEXT", []).map_err(io_err)?;
        }
        Ok(TransactLog { backend: Backend::Sqlite(Mutex::new(conn)) })
    }

    /// Postgres schema, created fresh — see this module's own doc
    /// comment for why a shared `transact_log` table is what actually
    /// makes `txn_id`'s idempotency guarantee hold across replicas, not
    /// just within one process.
    fn open_postgres(conn_str: &str) -> Result<Self, String> {
        let pool = crate::durability::pg_pool(conn_str)?;
        let ddl = "
            CREATE TABLE IF NOT EXISTS transact_log (
                txn_id           TEXT PRIMARY KEY,
                state            TEXT NOT NULL,
                network_fn       TEXT NOT NULL,
                network_args     TEXT NOT NULL,
                network_retry    BIGINT,
                network_timeout  BIGINT,
                network_result   TEXT,
                verify_fn        TEXT NOT NULL,
                verify_arg_kinds TEXT NOT NULL,
                verify_args      TEXT,
                verify_result    BOOLEAN,
                commit_fn        TEXT NOT NULL,
                commit_arg_kinds TEXT NOT NULL,
                commit_args      TEXT,
                compensate_fn    TEXT,
                compensate_arg_kinds TEXT,
                compensate_args  TEXT,
                attempts         BIGINT NOT NULL DEFAULT 0,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL
            );
        ";
        let backend = match pool {
            PgPool::Plain(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.batch_execute(ddl).map_err(pg_err)?;
                Backend::Postgres(pool)
            }
            PgPool::Tls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.batch_execute(ddl).map_err(pg_err)?;
                Backend::PostgresTls(pool)
            }
        };
        Ok(TransactLog { backend })
    }

    /// `docs/ROADMAP.md` Phase 2: a real, Raft-replicated SQLite cluster
    /// (`crate::rqlite`) instead of Postgres. Reuses `open_sqlite`'s own
    /// DDL text verbatim (rqlite *is* SQLite -- `crate::rqlite`'s own
    /// module doc explains why no dialect translation is needed here).
    /// No `ALTER TABLE` backfill step, matching `open_postgres` above:
    /// Phase 2's schema is created fresh, in its final shape, from day
    /// one -- there's no pre-Phase-2 rqlite-backed log file to upgrade.
    fn open_rqlite(conn_str: &str) -> Result<Self, String> {
        let client = RqliteClient::connect(conn_str)?;
        client.execute(
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
                commit_arg_kinds TEXT NOT NULL,
                commit_args      TEXT,
                compensate_fn    TEXT,
                compensate_arg_kinds TEXT,
                compensate_args  TEXT,
                attempts         INTEGER NOT NULL DEFAULT 0,
                created_at       TEXT NOT NULL,
                updated_at       TEXT NOT NULL
            )",
            &[],
        )?;
        Ok(TransactLog { backend: Backend::Rqlite(client) })
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
    /// comment and `docs/TRANSACT.md`'s durability section).
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
        commit_arg_kinds: &[&str],
        compensate_fn: Option<&str>,
        compensate_arg_kinds: Option<&[&str]>,
    ) -> Result<(), String> {
        let now = now_rfc3339();
        let network_args_json = args_to_json(network_args);
        let verify_kinds_json = serde_json::to_string(verify_arg_kinds).expect("verify_arg_kinds is always a plain string slice");
        let commit_kinds_json = serde_json::to_string(commit_arg_kinds).expect("commit_arg_kinds is always a plain string slice");
        let compensate_kinds_json =
            compensate_arg_kinds.map(|k| serde_json::to_string(k).expect("compensate_arg_kinds is always a plain string slice"));
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .execute(
                    "INSERT INTO transact_log
                         (txn_id, state, network_fn, network_args, network_retry, network_timeout, verify_fn,
                          verify_arg_kinds, commit_fn, commit_arg_kinds, compensate_fn, compensate_arg_kinds,
                          attempts, created_at, updated_at)
                     VALUES (?1, 'pending', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, ?12)",
                    params![
                        txn_id,
                        network_fn,
                        network_args_json,
                        network_retry,
                        network_timeout,
                        verify_fn,
                        verify_kinds_json,
                        commit_fn,
                        commit_kinds_json,
                        compensate_fn,
                        compensate_kinds_json,
                        now
                    ],
                )
                .map(|_| ())
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_begin_pending(
                    &mut c,
                    txn_id,
                    network_fn,
                    &network_args_json,
                    network_retry,
                    network_timeout,
                    verify_fn,
                    &verify_kinds_json,
                    commit_fn,
                    &commit_kinds_json,
                    compensate_fn,
                    compensate_kinds_json.as_deref(),
                    &now,
                )
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_begin_pending(
                    &mut c,
                    txn_id,
                    network_fn,
                    &network_args_json,
                    network_retry,
                    network_timeout,
                    verify_fn,
                    &verify_kinds_json,
                    commit_fn,
                    &commit_kinds_json,
                    compensate_fn,
                    compensate_kinds_json.as_deref(),
                    &now,
                )
            }
            Backend::Rqlite(client) => rq_begin_pending(
                client,
                txn_id,
                network_fn,
                &network_args_json,
                network_retry,
                network_timeout,
                verify_fn,
                &verify_kinds_json,
                commit_fn,
                &commit_kinds_json,
                compensate_fn,
                compensate_kinds_json.as_deref(),
                &now,
            ),
        }
    }

    /// Step 2: `network` returned. Recorded *before* `verify` runs —
    /// `docs/TRANSACT.md`'s named crash-safety boundary. A restart that finds
    /// `state = "network_done"` never re-invokes `network`.
    pub fn record_network_result(&self, txn_id: &str, result: &Value) -> Result<(), String> {
        let result_json = serde_json::to_string(&value_to_json(result)).unwrap();
        let now = now_rfc3339();
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .execute(
                    "UPDATE transact_log SET state = 'network_done', network_result = ?2, updated_at = ?3 WHERE txn_id = ?1",
                    params![txn_id, result_json, now],
                )
                .map(|_| ())
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET state = 'network_done', network_result = $2, updated_at = $3 WHERE txn_id = $1",
                    &[&txn_id, &result_json, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET state = 'network_done', network_result = $2, updated_at = $3 WHERE txn_id = $1",
                    &[&txn_id, &result_json, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::Rqlite(client) => client
                .execute(
                    "UPDATE transact_log SET state = 'network_done', network_result = ?2, updated_at = ?3 WHERE txn_id = ?1",
                    &rq_params![txn_id, result_json, now],
                )
                .map(|_| ()),
        }
    }

    /// Records `verify`'s actual evaluated arguments and result, once
    /// it's actually run (live or replay) — `verify_fn`/`verify_arg_kinds`
    /// are already known from `begin_pending`, so this only ever fills
    /// in the dynamic half.
    pub fn record_verify(&self, txn_id: &str, verify_args: &[Value], result: bool) -> Result<(), String> {
        let args_json = args_to_json(verify_args);
        let now = now_rfc3339();
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .execute(
                    "UPDATE transact_log SET verify_args = ?2, verify_result = ?3, updated_at = ?4 WHERE txn_id = ?1",
                    params![txn_id, args_json, result, now],
                )
                .map(|_| ())
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET verify_args = $2, verify_result = $3, updated_at = $4 WHERE txn_id = $1",
                    &[&txn_id, &args_json, &result, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET verify_args = $2, verify_result = $3, updated_at = $4 WHERE txn_id = $1",
                    &[&txn_id, &args_json, &result, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::Rqlite(client) => client
                .execute(
                    "UPDATE transact_log SET verify_args = ?2, verify_result = ?3, updated_at = ?4 WHERE txn_id = ?1",
                    &rq_params![txn_id, args_json, result, now],
                )
                .map(|_| ()),
        }
    }

    /// Verify was `true`: about to attempt `commit`. Recorded before the
    /// first attempt, same "log intent, then act" discipline as
    /// `begin_pending`. `commit_fn` itself is already known (from
    /// `begin_pending`) -- only its evaluated arguments are new here.
    pub fn mark_commit_pending(&self, txn_id: &str, commit_args: &[Value]) -> Result<(), String> {
        let args_json = args_to_json(commit_args);
        let now = now_rfc3339();
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .execute(
                    "UPDATE transact_log SET state = 'commit_pending', commit_args = ?2, updated_at = ?3 WHERE txn_id = ?1",
                    params![txn_id, args_json, now],
                )
                .map(|_| ())
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET state = 'commit_pending', commit_args = $2, updated_at = $3 WHERE txn_id = $1",
                    &[&txn_id, &args_json, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET state = 'commit_pending', commit_args = $2, updated_at = $3 WHERE txn_id = $1",
                    &[&txn_id, &args_json, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::Rqlite(client) => client
                .execute(
                    "UPDATE transact_log SET state = 'commit_pending', commit_args = ?2, updated_at = ?3 WHERE txn_id = ?1",
                    &rq_params![txn_id, args_json, now],
                )
                .map(|_| ()),
        }
    }

    /// Verify was `false` and a `compensate` slot exists: about to
    /// attempt it. Symmetric to `mark_commit_pending` — `compensate` is
    /// a write too, and can fail the same way.
    pub fn mark_compensate_pending(&self, txn_id: &str, compensate_args: &[Value]) -> Result<(), String> {
        let args_json = args_to_json(compensate_args);
        let now = now_rfc3339();
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .execute(
                    "UPDATE transact_log SET state = 'compensate_pending', compensate_args = ?2, updated_at = ?3
                     WHERE txn_id = ?1",
                    params![txn_id, args_json, now],
                )
                .map(|_| ())
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET state = 'compensate_pending', compensate_args = $2, updated_at = $3 WHERE txn_id = $1",
                    &[&txn_id, &args_json, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute(
                    "UPDATE transact_log SET state = 'compensate_pending', compensate_args = $2, updated_at = $3 WHERE txn_id = $1",
                    &[&txn_id, &args_json, &now],
                )
                .map(|_| ())
                .map_err(pg_err)
            }
            Backend::Rqlite(client) => client
                .execute(
                    "UPDATE transact_log SET state = 'compensate_pending', compensate_args = ?2, updated_at = ?3 WHERE txn_id = ?1",
                    &rq_params![txn_id, args_json, now],
                )
                .map(|_| ()),
        }
    }

    /// A retry attempt (in-process or replay-driven) failed -- best-
    /// effort bookkeeping only, never itself part of what a replay reads
    /// to decide what to do next (that's `state` alone).
    pub fn bump_attempts(&self, txn_id: &str) -> Result<(), String> {
        let now = now_rfc3339();
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .execute("UPDATE transact_log SET attempts = attempts + 1, updated_at = ?2 WHERE txn_id = ?1", params![txn_id, now])
                .map(|_| ())
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute("UPDATE transact_log SET attempts = attempts + 1, updated_at = $2 WHERE txn_id = $1", &[&txn_id, &now])
                    .map(|_| ())
                    .map_err(pg_err)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute("UPDATE transact_log SET attempts = attempts + 1, updated_at = $2 WHERE txn_id = $1", &[&txn_id, &now])
                    .map(|_| ())
                    .map_err(pg_err)
            }
            Backend::Rqlite(client) => client
                .execute("UPDATE transact_log SET attempts = attempts + 1, updated_at = ?2 WHERE txn_id = ?1", &rq_params![txn_id, now])
                .map(|_| ()),
        }
    }

    /// `commit`/`compensate` finally succeeded: the terminal state.
    /// `state` must be `"committed"` or `"compensated"` -- both are
    /// excluded from `list_unresolved`, so this row is never touched
    /// again.
    pub fn mark_terminal(&self, txn_id: &str, state: &str) -> Result<(), String> {
        debug_assert!(state == "committed" || state == "compensated");
        let now = now_rfc3339();
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .execute("UPDATE transact_log SET state = ?2, updated_at = ?3 WHERE txn_id = ?1", params![txn_id, state, now])
                .map(|_| ())
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute("UPDATE transact_log SET state = $2, updated_at = $3 WHERE txn_id = $1", &[&txn_id, &state, &now])
                    .map(|_| ())
                    .map_err(pg_err)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.execute("UPDATE transact_log SET state = $2, updated_at = $3 WHERE txn_id = $1", &[&txn_id, &state, &now])
                    .map(|_| ())
                    .map_err(pg_err)
            }
            Backend::Rqlite(client) => client
                .execute("UPDATE transact_log SET state = ?2, updated_at = ?3 WHERE txn_id = ?1", &rq_params![txn_id, state, now])
                .map(|_| ()),
        }
    }

    /// Every row not yet in a terminal state -- what
    /// `Interpreter::replay_pending_transactions` resumes on startup.
    pub fn list_unresolved(&self) -> Result<Vec<PendingTxn>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().unwrap();
                let mut stmt = conn
                    .prepare(
                        "SELECT txn_id, state, network_fn, network_args, network_retry, network_timeout, network_result,
                                verify_fn, verify_arg_kinds, verify_args, verify_result,
                                commit_fn, commit_arg_kinds, commit_args,
                                compensate_fn, compensate_arg_kinds, compensate_args
                         FROM transact_log
                         WHERE state NOT IN ('committed', 'compensated')",
                    )
                    .map_err(io_err)?;
                let rows = stmt
                    .query_map([], |row| {
                        let network_args_json: String = row.get(3)?;
                        let network_result_json: Option<String> = row.get(6)?;
                        let verify_arg_kinds_json: String = row.get(8)?;
                        let verify_args_json: Option<String> = row.get(9)?;
                        let commit_arg_kinds_json: String = row.get(12)?;
                        let commit_args_json: Option<String> = row.get(13)?;
                        let compensate_arg_kinds_json: Option<String> = row.get(15)?;
                        let compensate_args_json: Option<String> = row.get(16)?;
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
                            verify_arg_kinds: serde_json::from_str(&verify_arg_kinds_json).expect("begin_pending's own format"),
                            verify_args: verify_args_json.as_deref().map(args_from_json),
                            verify_result: row.get(10)?,
                            commit_fn: row.get(11)?,
                            commit_arg_kinds: serde_json::from_str(&commit_arg_kinds_json).expect("begin_pending's own format"),
                            commit_args: commit_args_json.as_deref().map(args_from_json),
                            compensate_fn: row.get(14)?,
                            compensate_arg_kinds: compensate_arg_kinds_json
                                .as_deref()
                                .map(|s| serde_json::from_str(s).expect("begin_pending's own format")),
                            compensate_args: compensate_args_json.as_deref().map(args_from_json),
                        })
                    })
                    .map_err(io_err)?;
                rows.collect::<rusqlite::Result<Vec<_>>>().map_err(io_err)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_unresolved(&mut c)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_unresolved(&mut c)
            }
            Backend::Rqlite(client) => rq_list_unresolved(client),
        }
    }

    /// Test/inspection helper: the raw `state` column for one `txn_id`,
    /// `None` if no such row exists.
    pub fn state_of(&self, txn_id: &str) -> Result<Option<String>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => conn
                .lock()
                .unwrap()
                .query_row("SELECT state FROM transact_log WHERE txn_id = ?1", params![txn_id], |row| row.get(0))
                .optional()
                .map_err(io_err),
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.query_opt("SELECT state FROM transact_log WHERE txn_id = $1", &[&txn_id]).map(|r| r.map(|r| r.get(0))).map_err(pg_err)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                c.query_opt("SELECT state FROM transact_log WHERE txn_id = $1", &[&txn_id]).map(|r| r.map(|r| r.get(0))).map_err(pg_err)
            }
            Backend::Rqlite(client) => Ok(client
                .query_opt("SELECT state FROM transact_log WHERE txn_id = ?1", &rq_params![txn_id])?
                .map(|r| json_str(&r[0]))),
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn pg_begin_pending(
    c: &mut postgres::Client,
    txn_id: &str,
    network_fn: &str,
    network_args_json: &str,
    network_retry: Option<i64>,
    network_timeout: Option<i64>,
    verify_fn: &str,
    verify_kinds_json: &str,
    commit_fn: &str,
    commit_kinds_json: &str,
    compensate_fn: Option<&str>,
    compensate_kinds_json: Option<&str>,
    now: &str,
) -> Result<(), String> {
    c.execute(
        "INSERT INTO transact_log
             (txn_id, state, network_fn, network_args, network_retry, network_timeout, verify_fn,
              verify_arg_kinds, commit_fn, commit_arg_kinds, compensate_fn, compensate_arg_kinds,
              attempts, created_at, updated_at)
         VALUES ($1, 'pending', $2, $3, $4, $5, $6, $7, $8, $9, $10, $11, 0, $12, $12)",
        &[
            &txn_id,
            &network_fn,
            &network_args_json,
            &network_retry,
            &network_timeout,
            &verify_fn,
            &verify_kinds_json,
            &commit_fn,
            &commit_kinds_json,
            &compensate_fn,
            &compensate_kinds_json,
            &now,
        ],
    )
    .map(|_| ())
    .map_err(pg_err)
}

fn pg_list_unresolved(c: &mut postgres::Client) -> Result<Vec<PendingTxn>, String> {
    let rows = c
        .query(
            "SELECT txn_id, state, network_fn, network_args, network_retry, network_timeout, network_result,
                    verify_fn, verify_arg_kinds, verify_args, verify_result,
                    commit_fn, commit_arg_kinds, commit_args,
                    compensate_fn, compensate_arg_kinds, compensate_args
             FROM transact_log
             WHERE state NOT IN ('committed', 'compensated')",
            &[],
        )
        .map_err(pg_err)?;
    Ok(rows
        .iter()
        .map(|row| {
            let network_args_json: String = row.get(3);
            let network_result_json: Option<String> = row.get(6);
            let verify_arg_kinds_json: String = row.get(8);
            let verify_args_json: Option<String> = row.get(9);
            let commit_arg_kinds_json: String = row.get(12);
            let commit_args_json: Option<String> = row.get(13);
            let compensate_arg_kinds_json: Option<String> = row.get(15);
            let compensate_args_json: Option<String> = row.get(16);
            PendingTxn {
                txn_id: row.get(0),
                state: row.get(1),
                network_fn: row.get(2),
                network_args: args_from_json(&network_args_json),
                network_retry: row.get(4),
                network_timeout: row.get(5),
                network_result: network_result_json
                    .map(|s| value_from_json(&serde_json::from_str(&s).expect("record_network_result's own format"))),
                verify_fn: row.get(7),
                verify_arg_kinds: serde_json::from_str(&verify_arg_kinds_json).expect("begin_pending's own format"),
                verify_args: verify_args_json.as_deref().map(args_from_json),
                verify_result: row.get(10),
                commit_fn: row.get(11),
                commit_arg_kinds: serde_json::from_str(&commit_arg_kinds_json).expect("begin_pending's own format"),
                commit_args: commit_args_json.as_deref().map(args_from_json),
                compensate_fn: row.get(14),
                compensate_arg_kinds: compensate_arg_kinds_json
                    .as_deref()
                    .map(|s| serde_json::from_str(s).expect("begin_pending's own format")),
                compensate_args: compensate_args_json.as_deref().map(args_from_json),
            }
        })
        .collect())
}

fn now_rfc3339() -> String {
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    // No `chrono` dependency for one timestamp column that's never
    // parsed back by this module (only ever read by a human via
    // `sqlite3`) -- raw epoch seconds, honest about the precision it
    // actually has.
    secs.to_string()
}

// --- rqlite implementations (`docs/ROADMAP.md` Phase 2) --- reuses the exact
// `?N`-placeholder SQL text `Backend::Sqlite`'s own arms already use
// (verified live: SQLite's numbered-placeholder syntax works unmodified
// through rqlite's HTTP API) -- see `workflow_log.rs`'s own "rqlite
// implementations" section for why no dialect translation is needed here,
// unlike the `pg_*` functions above.

fn json_opt_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
}
fn json_str(v: &serde_json::Value) -> String {
    v.as_str().expect("transact_log.rs's own TEXT NOT NULL columns always round-trip as JSON strings").to_string()
}
fn json_opt_str(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::to_string)
}
/// `verify_result` is an `INTEGER` column (0/1/NULL), the same
/// convention `rusqlite`'s own `bool`/`Option<bool>` impls already use
/// for `Backend::Sqlite` -- rqlite returns it as a JSON number or
/// `null`, never a JSON boolean.
fn json_opt_bool(v: &serde_json::Value) -> Option<bool> {
    v.as_i64().map(|n| n != 0)
}

#[allow(clippy::too_many_arguments)]
fn rq_begin_pending(
    client: &RqliteClient,
    txn_id: &str,
    network_fn: &str,
    network_args_json: &str,
    network_retry: Option<i64>,
    network_timeout: Option<i64>,
    verify_fn: &str,
    verify_kinds_json: &str,
    commit_fn: &str,
    commit_kinds_json: &str,
    compensate_fn: Option<&str>,
    compensate_kinds_json: Option<&str>,
    now: &str,
) -> Result<(), String> {
    client
        .execute(
            "INSERT INTO transact_log
                 (txn_id, state, network_fn, network_args, network_retry, network_timeout, verify_fn,
                  verify_arg_kinds, commit_fn, commit_arg_kinds, compensate_fn, compensate_arg_kinds,
                  attempts, created_at, updated_at)
             VALUES (?1, 'pending', ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 0, ?12, ?12)",
            &rq_params![
                txn_id,
                network_fn,
                network_args_json,
                network_retry,
                network_timeout,
                verify_fn,
                verify_kinds_json,
                commit_fn,
                commit_kinds_json,
                compensate_fn,
                compensate_kinds_json,
                now
            ],
        )
        .map(|_| ())
}

fn rq_list_unresolved(client: &RqliteClient) -> Result<Vec<PendingTxn>, String> {
    let rows = client.query(
        "SELECT txn_id, state, network_fn, network_args, network_retry, network_timeout, network_result,
                verify_fn, verify_arg_kinds, verify_args, verify_result,
                commit_fn, commit_arg_kinds, commit_args,
                compensate_fn, compensate_arg_kinds, compensate_args
         FROM transact_log
         WHERE state NOT IN ('committed', 'compensated')",
        &[],
    )?;
    Ok(rows
        .iter()
        .map(|row| {
            let network_args_json = json_str(&row[3]);
            let network_result_json = json_opt_str(&row[6]);
            let verify_arg_kinds_json = json_str(&row[8]);
            let verify_args_json = json_opt_str(&row[9]);
            let commit_arg_kinds_json = json_str(&row[12]);
            let commit_args_json = json_opt_str(&row[13]);
            let compensate_arg_kinds_json = json_opt_str(&row[15]);
            let compensate_args_json = json_opt_str(&row[16]);
            PendingTxn {
                txn_id: json_str(&row[0]),
                state: json_str(&row[1]),
                network_fn: json_str(&row[2]),
                network_args: args_from_json(&network_args_json),
                network_retry: json_opt_i64(&row[4]),
                network_timeout: json_opt_i64(&row[5]),
                network_result: network_result_json
                    .map(|s| value_from_json(&serde_json::from_str(&s).expect("record_network_result's own format"))),
                verify_fn: json_str(&row[7]),
                verify_arg_kinds: serde_json::from_str(&verify_arg_kinds_json).expect("begin_pending's own format"),
                verify_args: verify_args_json.as_deref().map(args_from_json),
                verify_result: json_opt_bool(&row[10]),
                commit_fn: json_str(&row[11]),
                commit_arg_kinds: serde_json::from_str(&commit_arg_kinds_json).expect("begin_pending's own format"),
                commit_args: commit_args_json.as_deref().map(args_from_json),
                compensate_fn: json_opt_str(&row[14]),
                compensate_arg_kinds: compensate_arg_kinds_json
                    .as_deref()
                    .map(|s| serde_json::from_str(s).expect("begin_pending's own format")),
                compensate_args: compensate_args_json.as_deref().map(args_from_json),
            }
        })
        .collect())
}
