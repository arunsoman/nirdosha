//! `workflow { ... }`'s durable state store (`docs/WORKFLOW.md`). Backed by
//! either a local SQLite file (`rusqlite`, the original and still-default
//! shape) or a real, shared Postgres database (`crate::durability` —
//! `docs/ROADMAP.md`'s multi-instance coordination fix); see that module's own
//! doc comment for why sharing a real database, not a local file, is
//! what actually makes `workflow_instance.id` globally unique across
//! replicas. Owns everything `docs/WORKFLOW.md`'s desugared
//! `__workflow_start`/`__workflow_advance`/`__workflow_link_advance`
//! builtins (`interpreter.rs`) need at runtime: instance state, an
//! append-only transition history, magic-link mint/consume, the two
//! pieces of identity bookkeeping no builtin/route in this codebase kept
//! before (`identity_directory` for `Recipient::ByRole`, `identity_presence`
//! for `notify`'s online/offline branch), and — same "log intent *before*
//! running it" crash-safety boundary `transact_log.rs`'s own
//! `begin_pending` gives `network` — a pending row per `on_entry`/
//! `on_exit` action call, written before `Interpreter::run_workflow_actions`
//! dispatches it, so `Interpreter::replay_pending_workflow_actions` (called
//! once at `nirdosha serve` startup, right alongside
//! `replay_pending_transactions`) can resume a crash mid-action-fan-out
//! without needing the original `.nir` call site or environment — the row
//! is self-contained (callee name + already-evaluated, JSON-encoded
//! arguments, decoded back via the callee's own declared parameter types
//! at replay time, `serve::decode_value`).
//!
//! ## Backend dispatch
//!
//! Every public method below matches on `self.backend` and has (up to)
//! three near-identical bodies: the original SQLite query, and one
//! shared Postgres implementation (a free `pg_*` function taking
//! `&mut postgres::Client`) called from both the plain and TLS Postgres
//! pool arms — `PostgresConnectionManager<T>`'s `Connection` associated
//! type is `postgres::Client` regardless of `T`, so TLS-vs-not never
//! needs its own separate query logic, only its own separate pool
//! (`crate::durability::pg_pool`'s own doc comment). This mirrors
//! `dbconn.rs`'s own `DbConn`/`query`/`execute` dispatch shape exactly —
//! explicit match arms, not a generic trait-object abstraction, matching
//! this codebase's existing preference for boring, directly-readable
//! dispatch over cleverness.

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

use crate::durability::{LogTarget, PgPool};
use crate::rq_params;
use crate::rqlite::{RqliteClient, RqliteParam};

enum Backend {
    Sqlite(Mutex<Connection>),
    Postgres(r2d2::Pool<r2d2_postgres::PostgresConnectionManager<postgres::NoTls>>),
    PostgresTls(r2d2::Pool<r2d2_postgres::PostgresConnectionManager<postgres_native_tls::MakeTlsConnector>>),
    Rqlite(RqliteClient),
}

/// `WorkflowLog::open` itself takes no instance lock and is safe to call
/// any number of times against the same file within one process (every
/// `nirdosha serve` request opens its own, `interpreter.rs::workflow_log`'s
/// own laziness) or from a separate short-lived inspection process while
/// a server keeps running (`tests/transact_process_kill.rs`'s own final
/// check does exactly that for `TransactLog`) -- ordinary SQLite file
/// access, never the dangerous case. The actual Phase 0 guard
/// (`instance_lock.rs`, `docs/ROADMAP.md`) is a *process*-level concern, not a
/// *connection*-level one: `serve::run` acquires it exactly once, up
/// front, for the server process's whole lifetime, entirely separate
/// from how many times this type's own `open` is called afterward.
pub struct WorkflowLog {
    backend: Backend,
}

fn io_err(e: rusqlite::Error) -> String {
    format!("workflow log I/O error: {e}")
}

fn pg_err(e: postgres::Error) -> String {
    format!("workflow log I/O error (postgres): {e}")
}

fn pg_pool_err(e: r2d2::Error) -> String {
    format!("workflow log I/O error (postgres pool checkout): {e}")
}

impl WorkflowLog {
    pub fn open(target: &LogTarget) -> Result<Self, String> {
        match target {
            LogTarget::Sqlite(path) => Self::open_sqlite(path),
            LogTarget::Postgres(conn_str) => Self::open_postgres(conn_str),
            LogTarget::Rqlite(conn_str) => Self::open_rqlite(conn_str),
        }
    }

    fn open_sqlite(path: &Path) -> Result<Self, String> {
        let conn = Connection::open(path).map_err(io_err)?;
        conn.pragma_update(None, "synchronous", "FULL").map_err(io_err)?;
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS workflow_instance (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_name TEXT NOT NULL,
                state TEXT NOT NULL,
                data_json TEXT NOT NULL,
                started_by_subject TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                event TEXT NOT NULL,
                actor_subject TEXT,
                via_link INTEGER NOT NULL DEFAULT 0,
                comment TEXT,
                at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS magic_link (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                event TEXT NOT NULL,
                token TEXT NOT NULL,
                consumed INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS identity_directory (
                subject TEXT PRIMARY KEY,
                roles_json TEXT NOT NULL,
                last_seen_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS identity_presence (
                subject TEXT PRIMARY KEY,
                online INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_pending_action (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                workflow_name TEXT NOT NULL,
                state TEXT NOT NULL,
                slot TEXT NOT NULL,
                action_index INTEGER NOT NULL,
                action_fn TEXT NOT NULL,
                args_json TEXT NOT NULL,
                status TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );",
        )
        .map_err(io_err)?;
        // Backfill columns added after this table's original shape shipped,
        // the same "ALTER TABLE only the columns actually missing" idiom
        // `transact_log.rs::open` already established — a workflow-log
        // file written by a pre-upgrade binary only ever creates the
        // table fresh, never alters one that's already there, so a
        // reopened pre-existing file needs these added explicitly rather
        // than assumed present. Postgres has no such legacy files (Phase
        // 1's schema is created fresh, in its final shape, from day one),
        // so `open_postgres` below has no equivalent backfill step.
        let backfill = |conn: &Connection, table: &str, column: &str, ddl: &str| -> Result<(), String> {
            let existing: std::collections::HashSet<String> = conn
                .prepare(&format!("SELECT name FROM pragma_table_info('{table}')"))
                .map_err(io_err)?
                .query_map([], |row| row.get::<_, String>(0))
                .map_err(io_err)?
                .collect::<rusqlite::Result<_>>()
                .map_err(io_err)?;
            if !existing.contains(column) {
                conn.execute(&format!("ALTER TABLE \"{table}\" ADD COLUMN {ddl}"), []).map_err(io_err)?;
            }
            Ok(())
        };
        backfill(&conn, "workflow_instance", "started_by_subject", "started_by_subject TEXT")?;
        backfill(&conn, "workflow_history", "actor_subject", "actor_subject TEXT")?;
        backfill(&conn, "workflow_history", "via_link", "via_link INTEGER NOT NULL DEFAULT 0")?;
        backfill(&conn, "workflow_history", "comment", "comment TEXT")?;
        Ok(WorkflowLog { backend: Backend::Sqlite(Mutex::new(conn)) })
    }

    /// Postgres schema, created fresh -- every table BIGSERIAL/BIGINT in
    /// place of SQLite's `INTEGER PRIMARY KEY AUTOINCREMENT`/`INTEGER`,
    /// and real `BOOLEAN` columns for `via_link`/`consumed`/`online`
    /// (`dbconn.rs::Param::Bool`'s own doc comment: Postgres has a native
    /// boolean type, bound directly rather than as a 0/1 integer). This
    /// is the actual fix for `workflow_instance.id` uniqueness across
    /// replicas: one shared table means one shared `BIGSERIAL` sequence,
    /// not a separate per-file `AUTOINCREMENT` counter per replica.
    fn open_postgres(conn_str: &str) -> Result<Self, String> {
        let pool = crate::durability::pg_pool(conn_str)?;
        let ddl = "
            CREATE TABLE IF NOT EXISTS workflow_instance (
                id BIGSERIAL PRIMARY KEY,
                workflow_name TEXT NOT NULL,
                state TEXT NOT NULL,
                data_json TEXT NOT NULL,
                started_by_subject TEXT,
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_history (
                id BIGSERIAL PRIMARY KEY,
                instance_id BIGINT NOT NULL,
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                event TEXT NOT NULL,
                actor_subject TEXT,
                via_link BOOLEAN NOT NULL DEFAULT FALSE,
                comment TEXT,
                at BIGINT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS magic_link (
                id BIGSERIAL PRIMARY KEY,
                instance_id BIGINT NOT NULL,
                event TEXT NOT NULL,
                token TEXT NOT NULL,
                consumed BOOLEAN NOT NULL DEFAULT FALSE
            );
            CREATE TABLE IF NOT EXISTS identity_directory (
                subject TEXT PRIMARY KEY,
                roles_json TEXT NOT NULL,
                last_seen_at BIGINT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS identity_presence (
                subject TEXT PRIMARY KEY,
                online BOOLEAN NOT NULL,
                updated_at BIGINT NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_pending_action (
                id BIGSERIAL PRIMARY KEY,
                instance_id BIGINT NOT NULL,
                workflow_name TEXT NOT NULL,
                state TEXT NOT NULL,
                slot TEXT NOT NULL,
                action_index BIGINT NOT NULL,
                action_fn TEXT NOT NULL,
                args_json TEXT NOT NULL,
                status TEXT NOT NULL,
                attempts BIGINT NOT NULL DEFAULT 0,
                created_at BIGINT NOT NULL,
                updated_at BIGINT NOT NULL
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
        Ok(WorkflowLog { backend })
    }

    /// `docs/ROADMAP.md` Phase 2: a real, Raft-replicated SQLite cluster
    /// (`crate::rqlite`) instead of Postgres, for a deployment that
    /// wants multi-instance without taking on a Postgres dependency.
    /// Reuses `open_sqlite`'s own DDL text verbatim (rqlite *is* SQLite —
    /// `crate::rqlite`'s own module doc explains why no dialect
    /// translation is needed here, unlike `open_postgres` above), split
    /// into individual statements and run as one atomic `/db/execute`
    /// batch (`crate::rqlite::split_ddl_statements`).
    fn open_rqlite(conn_str: &str) -> Result<Self, String> {
        let client = RqliteClient::connect(conn_str)?;
        let ddl = "CREATE TABLE IF NOT EXISTS workflow_instance (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                workflow_name TEXT NOT NULL,
                state TEXT NOT NULL,
                data_json TEXT NOT NULL,
                started_by_subject TEXT,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                event TEXT NOT NULL,
                actor_subject TEXT,
                via_link INTEGER NOT NULL DEFAULT 0,
                comment TEXT,
                at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS magic_link (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                event TEXT NOT NULL,
                token TEXT NOT NULL,
                consumed INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS identity_directory (
                subject TEXT PRIMARY KEY,
                roles_json TEXT NOT NULL,
                last_seen_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS identity_presence (
                subject TEXT PRIMARY KEY,
                online INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_pending_action (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                workflow_name TEXT NOT NULL,
                state TEXT NOT NULL,
                slot TEXT NOT NULL,
                action_index INTEGER NOT NULL,
                action_fn TEXT NOT NULL,
                args_json TEXT NOT NULL,
                status TEXT NOT NULL,
                attempts INTEGER NOT NULL DEFAULT 0,
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );";
        let statements = crate::rqlite::split_ddl_statements(ddl);
        let empty: Vec<RqliteParam> = Vec::new();
        let batch: Vec<(&str, &[RqliteParam])> = statements.iter().map(|s| (s.as_str(), empty.as_slice())).collect();
        client.execute_many(&batch)?;
        Ok(WorkflowLog { backend: Backend::Rqlite(client) })
    }

    /// `started_by_subject`: the calling identity's `subject`, when
    /// `start_<workflow>`'s (now-optional, `docs/WORKFLOW.md`'s "who submitted
    /// this" section) `identity: Option(VerifiedIdentity)` param was
    /// `Some(_)` — `None` for a legitimately anonymous start (e.g.
    /// `kyc_onboarding.nir`'s own public intake, unauthenticated by
    /// design). Backs `list_<workflow>_submitted_by_me`.
    pub fn create_instance(
        &self,
        workflow_name: &str,
        state: &str,
        data_json: &str,
        started_by_subject: Option<&str>,
        now: i64,
    ) -> Result<i64, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute(
                    "INSERT INTO workflow_instance (workflow_name, state, data_json, started_by_subject, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?)",
                    params![workflow_name, state, data_json, started_by_subject, now, now],
                )
                .map_err(io_err)?;
                Ok(conn.last_insert_rowid())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_create_instance(&mut c, workflow_name, state, data_json, started_by_subject, now)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_create_instance(&mut c, workflow_name, state, data_json, started_by_subject, now)
            }
            Backend::Rqlite(client) => rq_create_instance(client, workflow_name, state, data_json, started_by_subject, now),
        }
    }

    /// `(workflow_name, state, data_json)`, or `None` if no such instance.
    pub fn get_instance(&self, id: i64) -> Result<Option<(String, String, String)>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.query_row(
                    "SELECT workflow_name, state, data_json FROM workflow_instance WHERE id = ?",
                    params![id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .optional()
                .map_err(io_err)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_get_instance(&mut c, id)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_get_instance(&mut c, id)
            }
            Backend::Rqlite(client) => rq_get_instance(client, id),
        }
    }

    /// Every instance of `workflow_name`, as `(id, state, data_json)` —
    /// backs `interpreter.rs::workflow_pending_for_me`
    /// (`__workflow_pending_for_me`, `docs/WORKFLOW.md`'s "state ownership"
    /// section): the interpreter, not this query, decides which of these
    /// the caller actually owns (it needs the live `WorkflowDecl` AST to
    /// know each state's `owner`, which this store doesn't have — this
    /// method only ever answers "every instance," the same "SQL does the
    /// narrow mechanical part, the caller does the semantic part"
    /// division `get_instance` already draws for one instance at a time).
    pub fn list_instances(&self, workflow_name: &str) -> Result<Vec<(i64, String, String)>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                let mut stmt = conn
                    .prepare("SELECT id, state, data_json FROM workflow_instance WHERE workflow_name = ? ORDER BY id")
                    .map_err(io_err)?;
                let rows = stmt
                    .query_map(params![workflow_name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .map_err(io_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(io_err)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_instances(&mut c, workflow_name)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_instances(&mut c, workflow_name)
            }
            Backend::Rqlite(client) => rq_list_instances(client, workflow_name),
        }
    }

    /// Every instance of `workflow_name` that `subject` itself started —
    /// backs `list_<workflow>_submitted_by_me`
    /// (`__workflow_submitted_by_me`, `docs/WORKFLOW.md`'s "who submitted
    /// this" section), the read side a requester needs to track their
    /// own request without needing `owner` on any state at all. Unlike
    /// `list_instances`, scoped in SQL, not by the caller — there's no
    /// per-state semantic to apply here (submission isn't a decision
    /// gate), so nothing stops this query from doing the whole filter
    /// itself, unlike `list_instances`' own doc comment explains for the
    /// owner case.
    pub fn list_instances_by_starter(&self, workflow_name: &str, subject: &str) -> Result<Vec<(i64, String, String)>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                let mut stmt = conn
                    .prepare(
                        "SELECT id, state, data_json FROM workflow_instance \
                         WHERE workflow_name = ? AND started_by_subject = ? ORDER BY id DESC",
                    )
                    .map_err(io_err)?;
                let rows = stmt
                    .query_map(params![workflow_name, subject], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                    .map_err(io_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(io_err)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_instances_by_starter(&mut c, workflow_name, subject)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_instances_by_starter(&mut c, workflow_name, subject)
            }
            Backend::Rqlite(client) => rq_list_instances_by_starter(client, workflow_name, subject),
        }
    }

    /// Moves `id` to `to_state` and appends the history row in one call —
    /// callers always want both together (`Interpreter`'s `advance`
    /// dispatch never updates state without also recording why).
    /// `actor_subject`/`via_link` (`docs/WORKFLOW.md`'s "audit trail" section)
    /// record *who* fired this transition and *how* — `None`/`false` for
    /// `actor_subject`/`via_link` only ever happens together (the magic-
    /// link path is the one case with no `identity` to attribute this
    /// to at all, `interpreter.rs::workflow_advance_inner`). `comment`
    /// is the caller-supplied free-text reason, if `advance_<workflow>`'s
    /// `payload` carried one (`{"comment": "..."}`) — still not threaded
    /// into `on_entry`/`on_exit` bindings, `docs/WORKFLOW.md`'s disclosed v1
    /// gap for `payload` in general, just durably logged here now.
    pub fn record_transition(
        &self,
        id: i64,
        from_state: &str,
        to_state: &str,
        event: &str,
        actor_subject: Option<&str>,
        via_link: bool,
        comment: Option<&str>,
        now: i64,
    ) -> Result<(), String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute("UPDATE workflow_instance SET state = ?, updated_at = ? WHERE id = ?", params![to_state, now, id])
                    .map_err(io_err)?;
                conn.execute(
                    "INSERT INTO workflow_history (instance_id, from_state, to_state, event, actor_subject, via_link, comment, at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
                    params![id, from_state, to_state, event, actor_subject, via_link, comment, now],
                )
                .map_err(io_err)?;
                Ok(())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_record_transition(&mut c, id, from_state, to_state, event, actor_subject, via_link, comment, now)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_record_transition(&mut c, id, from_state, to_state, event, actor_subject, via_link, comment, now)
            }
            Backend::Rqlite(client) => {
                rq_record_transition(client, id, from_state, to_state, event, actor_subject, via_link, comment, now)
            }
        }
    }

    /// The full append-only transition log for one instance, oldest
    /// first — backs `get_<workflow>_history` (`__workflow_history`,
    /// `docs/WORKFLOW.md`'s "audit trail" section): `(from_state, to_state,
    /// event, actor_subject, via_link, comment, at)`.
    #[allow(clippy::type_complexity)]
    pub fn list_history(
        &self,
        instance_id: i64,
    ) -> Result<Vec<(String, String, String, Option<String>, bool, Option<String>, i64)>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                let mut stmt = conn
                    .prepare(
                        "SELECT from_state, to_state, event, actor_subject, via_link, comment, at \
                         FROM workflow_history WHERE instance_id = ? ORDER BY at ASC, id ASC",
                    )
                    .map_err(io_err)?;
                let rows = stmt
                    .query_map(params![instance_id], |row| {
                        Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?))
                    })
                    .map_err(io_err)?;
                rows.collect::<Result<Vec<_>, _>>().map_err(io_err)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_history(&mut c, instance_id)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_history(&mut c, instance_id)
            }
            Backend::Rqlite(client) => rq_list_history(client, instance_id),
        }
    }

    pub fn mint_link(&self, instance_id: i64, event: &str, token: &str) -> Result<(), String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute(
                    "INSERT INTO magic_link (instance_id, event, token, consumed) VALUES (?, ?, ?, 0)",
                    params![instance_id, event, token],
                )
                .map_err(io_err)?;
                Ok(())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_mint_link(&mut c, instance_id, event, token)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_mint_link(&mut c, instance_id, event, token)
            }
            Backend::Rqlite(client) => rq_mint_link(client, instance_id, event, token),
        }
    }

    /// `(row_id, stored_token)` for the one not-yet-consumed link minted
    /// for `(instance_id, event)`, if any — split from consuming it so
    /// the caller (`Interpreter::workflow_link_advance`) can compare the
    /// presented token against `stored_token` with a constant-time
    /// comparison *before* touching the database again, the same
    /// timing-side-channel fix `trade_finance.nir:676`'s
    /// `constant_time_str_eq` already established for
    /// `decide_approval_via_link` — a plain SQL `WHERE token = ?` here
    /// would reintroduce exactly that bug.
    pub fn find_unconsumed_link(&self, instance_id: i64, event: &str) -> Result<Option<(i64, String)>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.query_row(
                    "SELECT id, token FROM magic_link WHERE instance_id = ? AND event = ? AND consumed = 0",
                    params![instance_id, event],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()
                .map_err(io_err)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_find_unconsumed_link(&mut c, instance_id, event)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_find_unconsumed_link(&mut c, instance_id, event)
            }
            Backend::Rqlite(client) => rq_find_unconsumed_link(client, instance_id, event),
        }
    }

    /// Single-use consume by row id, once the caller has already verified
    /// the token matches: `true` only for the one caller that wins the
    /// race, via a single `UPDATE ... WHERE consumed = 0` (no separate
    /// read-then-write for *this* step — that would be a TOCTOU
    /// double-spend of the same token).
    pub fn consume_link_by_id(&self, id: i64) -> Result<bool, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                let n = conn.execute("UPDATE magic_link SET consumed = 1 WHERE id = ? AND consumed = 0", params![id]).map_err(io_err)?;
                Ok(n == 1)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_consume_link_by_id(&mut c, id)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_consume_link_by_id(&mut c, id)
            }
            Backend::Rqlite(client) => rq_consume_link_by_id(client, id),
        }
    }

    /// Upserted on every successful `serve.rs::resolve_identity` — the
    /// only writer. `roles_json` is stored verbatim (a JSON array of role
    /// strings, e.g. `["compliance_officer"]`); `subjects_with_role`
    /// below matches against it with a `LIKE`, a deliberately simple v1
    /// (documented in `docs/WORKFLOW.md`) rather than a real JSON-array
    /// membership query.
    pub fn upsert_identity(&self, subject: &str, roles_json: &str, now: i64) -> Result<(), String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute(
                    "INSERT INTO identity_directory (subject, roles_json, last_seen_at) VALUES (?, ?, ?) \
                     ON CONFLICT(subject) DO UPDATE SET roles_json = excluded.roles_json, last_seen_at = excluded.last_seen_at",
                    params![subject, roles_json, now],
                )
                .map_err(io_err)?;
                Ok(())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_upsert_identity(&mut c, subject, roles_json, now)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_upsert_identity(&mut c, subject, roles_json, now)
            }
            Backend::Rqlite(client) => rq_upsert_identity(client, subject, roles_json, now),
        }
    }

    pub fn subjects_with_role(&self, role: &str) -> Result<Vec<String>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                let pattern = format!("%\"{role}\"%");
                let mut stmt = conn.prepare("SELECT subject FROM identity_directory WHERE roles_json LIKE ?").map_err(io_err)?;
                let rows = stmt.query_map(params![pattern], |row| row.get::<_, String>(0)).map_err(io_err)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(io_err)?);
                }
                Ok(out)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_subjects_with_role(&mut c, role)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_subjects_with_role(&mut c, role)
            }
            Backend::Rqlite(client) => rq_subjects_with_role(client, role),
        }
    }

    pub fn set_presence(&self, subject: &str, online: bool, now: i64) -> Result<(), String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute(
                    "INSERT INTO identity_presence (subject, online, updated_at) VALUES (?, ?, ?) \
                     ON CONFLICT(subject) DO UPDATE SET online = excluded.online, updated_at = excluded.updated_at",
                    params![subject, online as i64, now],
                )
                .map_err(io_err)?;
                Ok(())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_set_presence(&mut c, subject, online, now)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_set_presence(&mut c, subject, online, now)
            }
            Backend::Rqlite(client) => rq_set_presence(client, subject, online, now),
        }
    }

    pub fn is_online(&self, subject: &str) -> Result<bool, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                let online: Option<i64> = conn
                    .query_row("SELECT online FROM identity_presence WHERE subject = ?", params![subject], |row| row.get(0))
                    .optional()
                    .map_err(io_err)?;
                Ok(online.unwrap_or(0) != 0)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_is_online(&mut c, subject)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_is_online(&mut c, subject)
            }
            Backend::Rqlite(client) => rq_is_online(client, subject),
        }
    }

    /// Durably records intent to run one `on_entry`/`on_exit` action call
    /// — written *before* `Interpreter::run_workflow_actions` actually
    /// dispatches it, the same "record the intent, then act" ordering
    /// `transact_log.rs::begin_pending` gives `network`. `args_json` is a
    /// JSON array of each already-evaluated argument (`serve::encode_value`,
    /// the same general struct/scalar codec the RPC layer already uses —
    /// not restricted to `Ty::is_transact_scalar` the way `transact`'s own
    /// log is, since a workflow action's arguments are already limited to
    /// `instance_id`/`data.<field>`/`link_<Event>`, never a live resource
    /// handle). Returns the new row's id, used by `mark_action_done`/
    /// `bump_action_attempts` to update this exact row.
    #[allow(clippy::too_many_arguments)]
    pub fn begin_pending_action(
        &self,
        instance_id: i64,
        workflow_name: &str,
        state: &str,
        slot: &str,
        action_index: i64,
        action_fn: &str,
        args_json: &str,
        now: i64,
    ) -> Result<i64, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute(
                    "INSERT INTO workflow_pending_action \
                     (instance_id, workflow_name, state, slot, action_index, action_fn, args_json, status, attempts, created_at, updated_at) \
                     VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)",
                    params![instance_id, workflow_name, state, slot, action_index, action_fn, args_json, now, now],
                )
                .map_err(io_err)?;
                Ok(conn.last_insert_rowid())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_begin_pending_action(&mut c, instance_id, workflow_name, state, slot, action_index, action_fn, args_json, now)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_begin_pending_action(&mut c, instance_id, workflow_name, state, slot, action_index, action_fn, args_json, now)
            }
            Backend::Rqlite(client) => {
                rq_begin_pending_action(client, instance_id, workflow_name, state, slot, action_index, action_fn, args_json, now)
            }
        }
    }

    pub fn mark_action_done(&self, id: i64, now: i64) -> Result<(), String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute("UPDATE workflow_pending_action SET status = 'done', updated_at = ? WHERE id = ?", params![now, id])
                    .map_err(io_err)?;
                Ok(())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_mark_action_done(&mut c, id, now)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_mark_action_done(&mut c, id, now)
            }
            Backend::Rqlite(client) => rq_mark_action_done(client, id, now),
        }
    }

    pub fn bump_action_attempts(&self, id: i64, now: i64) -> Result<(), String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                conn.execute(
                    "UPDATE workflow_pending_action SET attempts = attempts + 1, updated_at = ? WHERE id = ?",
                    params![now, id],
                )
                .map_err(io_err)?;
                Ok(())
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_bump_action_attempts(&mut c, id, now)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_bump_action_attempts(&mut c, id, now)
            }
            Backend::Rqlite(client) => rq_bump_action_attempts(client, id, now),
        }
    }

    /// Every not-yet-`done` action row, oldest first — `Interpreter::
    /// replay_pending_workflow_actions` (`nirdosha serve` startup, once,
    /// right alongside `replay_pending_transactions`) resumes each of
    /// these independently.
    pub fn list_pending_actions(&self) -> Result<Vec<PendingWorkflowAction>, String> {
        match &self.backend {
            Backend::Sqlite(conn) => {
                let conn = conn.lock().expect("workflow log mutex poisoned");
                let mut stmt = conn
                    .prepare(
                        "SELECT id, instance_id, workflow_name, state, slot, action_index, action_fn, args_json, attempts \
                         FROM workflow_pending_action WHERE status = 'pending' ORDER BY id ASC",
                    )
                    .map_err(io_err)?;
                let rows = stmt
                    .query_map([], |row| {
                        Ok(PendingWorkflowAction {
                            id: row.get(0)?,
                            instance_id: row.get(1)?,
                            workflow_name: row.get(2)?,
                            state: row.get(3)?,
                            slot: row.get(4)?,
                            action_index: row.get(5)?,
                            action_fn: row.get(6)?,
                            args_json: row.get(7)?,
                            attempts: row.get(8)?,
                        })
                    })
                    .map_err(io_err)?;
                let mut out = Vec::new();
                for r in rows {
                    out.push(r.map_err(io_err)?);
                }
                Ok(out)
            }
            Backend::Postgres(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_pending_actions(&mut c)
            }
            Backend::PostgresTls(pool) => {
                let mut c = pool.get().map_err(pg_pool_err)?;
                pg_list_pending_actions(&mut c)
            }
            Backend::Rqlite(client) => rq_list_pending_actions(client),
        }
    }
}

// --- Postgres implementations, one per method, shared by the plain and
// TLS pool arms above (see this module's own doc comment for why). ---

fn pg_create_instance(
    c: &mut postgres::Client,
    workflow_name: &str,
    state: &str,
    data_json: &str,
    started_by_subject: Option<&str>,
    now: i64,
) -> Result<i64, String> {
    let row = c
        .query_one(
            "INSERT INTO workflow_instance (workflow_name, state, data_json, started_by_subject, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $5) RETURNING id",
            &[&workflow_name, &state, &data_json, &started_by_subject, &now],
        )
        .map_err(pg_err)?;
    Ok(row.get::<_, i64>(0))
}

fn pg_get_instance(c: &mut postgres::Client, id: i64) -> Result<Option<(String, String, String)>, String> {
    let row = c
        .query_opt("SELECT workflow_name, state, data_json FROM workflow_instance WHERE id = $1", &[&id])
        .map_err(pg_err)?;
    Ok(row.map(|r| (r.get(0), r.get(1), r.get(2))))
}

fn pg_list_instances(c: &mut postgres::Client, workflow_name: &str) -> Result<Vec<(i64, String, String)>, String> {
    let rows = c
        .query("SELECT id, state, data_json FROM workflow_instance WHERE workflow_name = $1 ORDER BY id", &[&workflow_name])
        .map_err(pg_err)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
}

fn pg_list_instances_by_starter(
    c: &mut postgres::Client,
    workflow_name: &str,
    subject: &str,
) -> Result<Vec<(i64, String, String)>, String> {
    let rows = c
        .query(
            "SELECT id, state, data_json FROM workflow_instance \
             WHERE workflow_name = $1 AND started_by_subject = $2 ORDER BY id DESC",
            &[&workflow_name, &subject],
        )
        .map_err(pg_err)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2))).collect())
}

#[allow(clippy::too_many_arguments)]
fn pg_record_transition(
    c: &mut postgres::Client,
    id: i64,
    from_state: &str,
    to_state: &str,
    event: &str,
    actor_subject: Option<&str>,
    via_link: bool,
    comment: Option<&str>,
    now: i64,
) -> Result<(), String> {
    c.execute("UPDATE workflow_instance SET state = $1, updated_at = $2 WHERE id = $3", &[&to_state, &now, &id]).map_err(pg_err)?;
    c.execute(
        "INSERT INTO workflow_history (instance_id, from_state, to_state, event, actor_subject, via_link, comment, at) \
         VALUES ($1, $2, $3, $4, $5, $6, $7, $8)",
        &[&id, &from_state, &to_state, &event, &actor_subject, &via_link, &comment, &now],
    )
    .map_err(pg_err)?;
    Ok(())
}

#[allow(clippy::type_complexity)]
fn pg_list_history(
    c: &mut postgres::Client,
    instance_id: i64,
) -> Result<Vec<(String, String, String, Option<String>, bool, Option<String>, i64)>, String> {
    let rows = c
        .query(
            "SELECT from_state, to_state, event, actor_subject, via_link, comment, at \
             FROM workflow_history WHERE instance_id = $1 ORDER BY at ASC, id ASC",
            &[&instance_id],
        )
        .map_err(pg_err)?;
    Ok(rows.iter().map(|r| (r.get(0), r.get(1), r.get(2), r.get(3), r.get(4), r.get(5), r.get(6))).collect())
}

fn pg_mint_link(c: &mut postgres::Client, instance_id: i64, event: &str, token: &str) -> Result<(), String> {
    c.execute(
        "INSERT INTO magic_link (instance_id, event, token, consumed) VALUES ($1, $2, $3, FALSE)",
        &[&instance_id, &event, &token],
    )
    .map_err(pg_err)?;
    Ok(())
}

fn pg_find_unconsumed_link(c: &mut postgres::Client, instance_id: i64, event: &str) -> Result<Option<(i64, String)>, String> {
    let row = c
        .query_opt(
            "SELECT id, token FROM magic_link WHERE instance_id = $1 AND event = $2 AND consumed = FALSE",
            &[&instance_id, &event],
        )
        .map_err(pg_err)?;
    Ok(row.map(|r| (r.get(0), r.get(1))))
}

fn pg_consume_link_by_id(c: &mut postgres::Client, id: i64) -> Result<bool, String> {
    let n = c.execute("UPDATE magic_link SET consumed = TRUE WHERE id = $1 AND consumed = FALSE", &[&id]).map_err(pg_err)?;
    Ok(n == 1)
}

fn pg_upsert_identity(c: &mut postgres::Client, subject: &str, roles_json: &str, now: i64) -> Result<(), String> {
    c.execute(
        "INSERT INTO identity_directory (subject, roles_json, last_seen_at) VALUES ($1, $2, $3) \
         ON CONFLICT (subject) DO UPDATE SET roles_json = excluded.roles_json, last_seen_at = excluded.last_seen_at",
        &[&subject, &roles_json, &now],
    )
    .map_err(pg_err)?;
    Ok(())
}

fn pg_subjects_with_role(c: &mut postgres::Client, role: &str) -> Result<Vec<String>, String> {
    let pattern = format!("%\"{role}\"%");
    let rows = c.query("SELECT subject FROM identity_directory WHERE roles_json LIKE $1", &[&pattern]).map_err(pg_err)?;
    Ok(rows.iter().map(|r| r.get(0)).collect())
}

fn pg_set_presence(c: &mut postgres::Client, subject: &str, online: bool, now: i64) -> Result<(), String> {
    c.execute(
        "INSERT INTO identity_presence (subject, online, updated_at) VALUES ($1, $2, $3) \
         ON CONFLICT (subject) DO UPDATE SET online = excluded.online, updated_at = excluded.updated_at",
        &[&subject, &online, &now],
    )
    .map_err(pg_err)?;
    Ok(())
}

fn pg_is_online(c: &mut postgres::Client, subject: &str) -> Result<bool, String> {
    let row = c.query_opt("SELECT online FROM identity_presence WHERE subject = $1", &[&subject]).map_err(pg_err)?;
    Ok(row.map(|r| r.get::<_, bool>(0)).unwrap_or(false))
}

#[allow(clippy::too_many_arguments)]
fn pg_begin_pending_action(
    c: &mut postgres::Client,
    instance_id: i64,
    workflow_name: &str,
    state: &str,
    slot: &str,
    action_index: i64,
    action_fn: &str,
    args_json: &str,
    now: i64,
) -> Result<i64, String> {
    let row = c
        .query_one(
            "INSERT INTO workflow_pending_action \
             (instance_id, workflow_name, state, slot, action_index, action_fn, args_json, status, attempts, created_at, updated_at) \
             VALUES ($1, $2, $3, $4, $5, $6, $7, 'pending', 0, $8, $8) RETURNING id",
            &[&instance_id, &workflow_name, &state, &slot, &action_index, &action_fn, &args_json, &now],
        )
        .map_err(pg_err)?;
    Ok(row.get::<_, i64>(0))
}

fn pg_mark_action_done(c: &mut postgres::Client, id: i64, now: i64) -> Result<(), String> {
    c.execute("UPDATE workflow_pending_action SET status = 'done', updated_at = $1 WHERE id = $2", &[&now, &id]).map_err(pg_err)?;
    Ok(())
}

fn pg_bump_action_attempts(c: &mut postgres::Client, id: i64, now: i64) -> Result<(), String> {
    c.execute(
        "UPDATE workflow_pending_action SET attempts = attempts + 1, updated_at = $1 WHERE id = $2",
        &[&now, &id],
    )
    .map_err(pg_err)?;
    Ok(())
}

fn pg_list_pending_actions(c: &mut postgres::Client) -> Result<Vec<PendingWorkflowAction>, String> {
    let rows = c
        .query(
            "SELECT id, instance_id, workflow_name, state, slot, action_index, action_fn, args_json, attempts \
             FROM workflow_pending_action WHERE status = 'pending' ORDER BY id ASC",
            &[],
        )
        .map_err(pg_err)?;
    Ok(rows
        .iter()
        .map(|r| PendingWorkflowAction {
            id: r.get(0),
            instance_id: r.get(1),
            workflow_name: r.get(2),
            state: r.get(3),
            slot: r.get(4),
            action_index: r.get(5),
            action_fn: r.get(6),
            args_json: r.get(7),
            attempts: r.get(8),
        })
        .collect())
}

// --- rqlite implementations, one per method (`docs/ROADMAP.md` Phase 2) ---
// Reuses the exact `?`-placeholder SQL text `Backend::Sqlite`'s own arms
// already use, unlike the `pg_*` functions above (this module's own doc
// comment explains why: rqlite *is* SQLite). `RqliteExecResult`'s
// `last_insert_id` stands in for `rusqlite::Connection::last_insert_rowid`;
// row values come back as `serde_json::Value` (`crate::rqlite::RqliteClient::
// query`'s own doc comment), read positionally via the small `json_*`
// helpers just below, the same "SELECT column order, not by name"
// convention `row.get(0)`/`row.get(1)` already establish for the other
// two backends.

fn json_i64(v: &serde_json::Value) -> i64 {
    v.as_i64().expect("workflow_log.rs's own INTEGER columns always round-trip as JSON numbers")
}
fn json_opt_i64(v: &serde_json::Value) -> Option<i64> {
    v.as_i64()
}
fn json_str(v: &serde_json::Value) -> String {
    v.as_str().expect("workflow_log.rs's own TEXT NOT NULL columns always round-trip as JSON strings").to_string()
}
fn json_opt_str(v: &serde_json::Value) -> Option<String> {
    v.as_str().map(str::to_string)
}
/// `via_link`/`consumed`/`online` are all `INTEGER` columns (0/1), the
/// same convention `rusqlite`'s own `bool` `ToSql`/`FromSql` impls
/// already use for `Backend::Sqlite` -- rqlite returns that column's
/// value as a JSON number, never a JSON boolean, so this reads it back
/// the same way `row.get::<_, bool>` implicitly does for the SQLite
/// backend: nonzero is `true`.
fn json_bool(v: &serde_json::Value) -> bool {
    v.as_i64().expect("workflow_log.rs's own 0/1 INTEGER columns always round-trip as JSON numbers") != 0
}

fn rq_create_instance(
    client: &RqliteClient,
    workflow_name: &str,
    state: &str,
    data_json: &str,
    started_by_subject: Option<&str>,
    now: i64,
) -> Result<i64, String> {
    let res = client.execute(
        "INSERT INTO workflow_instance (workflow_name, state, data_json, started_by_subject, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?)",
        &rq_params![workflow_name, state, data_json, started_by_subject, now, now],
    )?;
    res.last_insert_id.ok_or_else(|| "rqlite: INSERT into workflow_instance returned no last_insert_id".to_string())
}

fn rq_get_instance(client: &RqliteClient, id: i64) -> Result<Option<(String, String, String)>, String> {
    let row = client.query_opt("SELECT workflow_name, state, data_json FROM workflow_instance WHERE id = ?", &rq_params![id])?;
    Ok(row.map(|r| (json_str(&r[0]), json_str(&r[1]), json_str(&r[2]))))
}

fn rq_list_instances(client: &RqliteClient, workflow_name: &str) -> Result<Vec<(i64, String, String)>, String> {
    let rows = client.query(
        "SELECT id, state, data_json FROM workflow_instance WHERE workflow_name = ? ORDER BY id",
        &rq_params![workflow_name],
    )?;
    Ok(rows.iter().map(|r| (json_i64(&r[0]), json_str(&r[1]), json_str(&r[2]))).collect())
}

fn rq_list_instances_by_starter(client: &RqliteClient, workflow_name: &str, subject: &str) -> Result<Vec<(i64, String, String)>, String> {
    let rows = client.query(
        "SELECT id, state, data_json FROM workflow_instance \
         WHERE workflow_name = ? AND started_by_subject = ? ORDER BY id DESC",
        &rq_params![workflow_name, subject],
    )?;
    Ok(rows.iter().map(|r| (json_i64(&r[0]), json_str(&r[1]), json_str(&r[2]))).collect())
}

#[allow(clippy::too_many_arguments)]
fn rq_record_transition(
    client: &RqliteClient,
    id: i64,
    from_state: &str,
    to_state: &str,
    event: &str,
    actor_subject: Option<&str>,
    via_link: bool,
    comment: Option<&str>,
    now: i64,
) -> Result<(), String> {
    client.execute("UPDATE workflow_instance SET state = ?, updated_at = ? WHERE id = ?", &rq_params![to_state, now, id])?;
    client.execute(
        "INSERT INTO workflow_history (instance_id, from_state, to_state, event, actor_subject, via_link, comment, at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        &rq_params![id, from_state, to_state, event, actor_subject, via_link, comment, now],
    )?;
    Ok(())
}

#[allow(clippy::type_complexity)]
fn rq_list_history(
    client: &RqliteClient,
    instance_id: i64,
) -> Result<Vec<(String, String, String, Option<String>, bool, Option<String>, i64)>, String> {
    let rows = client.query(
        "SELECT from_state, to_state, event, actor_subject, via_link, comment, at \
         FROM workflow_history WHERE instance_id = ? ORDER BY at ASC, id ASC",
        &rq_params![instance_id],
    )?;
    Ok(rows
        .iter()
        .map(|r| (json_str(&r[0]), json_str(&r[1]), json_str(&r[2]), json_opt_str(&r[3]), json_bool(&r[4]), json_opt_str(&r[5]), json_i64(&r[6])))
        .collect())
}

fn rq_mint_link(client: &RqliteClient, instance_id: i64, event: &str, token: &str) -> Result<(), String> {
    client.execute(
        "INSERT INTO magic_link (instance_id, event, token, consumed) VALUES (?, ?, ?, 0)",
        &rq_params![instance_id, event, token],
    )?;
    Ok(())
}

fn rq_find_unconsumed_link(client: &RqliteClient, instance_id: i64, event: &str) -> Result<Option<(i64, String)>, String> {
    let row = client.query_opt(
        "SELECT id, token FROM magic_link WHERE instance_id = ? AND event = ? AND consumed = 0",
        &rq_params![instance_id, event],
    )?;
    Ok(row.map(|r| (json_i64(&r[0]), json_str(&r[1]))))
}

fn rq_consume_link_by_id(client: &RqliteClient, id: i64) -> Result<bool, String> {
    let res = client.execute("UPDATE magic_link SET consumed = 1 WHERE id = ? AND consumed = 0", &rq_params![id])?;
    Ok(res.rows_affected == 1)
}

fn rq_upsert_identity(client: &RqliteClient, subject: &str, roles_json: &str, now: i64) -> Result<(), String> {
    client.execute(
        "INSERT INTO identity_directory (subject, roles_json, last_seen_at) VALUES (?, ?, ?) \
         ON CONFLICT(subject) DO UPDATE SET roles_json = excluded.roles_json, last_seen_at = excluded.last_seen_at",
        &rq_params![subject, roles_json, now],
    )?;
    Ok(())
}

fn rq_subjects_with_role(client: &RqliteClient, role: &str) -> Result<Vec<String>, String> {
    let pattern = format!("%\"{role}\"%");
    let rows = client.query("SELECT subject FROM identity_directory WHERE roles_json LIKE ?", &rq_params![pattern])?;
    Ok(rows.iter().map(|r| json_str(&r[0])).collect())
}

fn rq_set_presence(client: &RqliteClient, subject: &str, online: bool, now: i64) -> Result<(), String> {
    client.execute(
        "INSERT INTO identity_presence (subject, online, updated_at) VALUES (?, ?, ?) \
         ON CONFLICT(subject) DO UPDATE SET online = excluded.online, updated_at = excluded.updated_at",
        &rq_params![subject, online as i64, now],
    )?;
    Ok(())
}

fn rq_is_online(client: &RqliteClient, subject: &str) -> Result<bool, String> {
    let row = client.query_opt("SELECT online FROM identity_presence WHERE subject = ?", &rq_params![subject])?;
    Ok(row.map(|r| json_opt_i64(&r[0]).unwrap_or(0) != 0).unwrap_or(false))
}

#[allow(clippy::too_many_arguments)]
fn rq_begin_pending_action(
    client: &RqliteClient,
    instance_id: i64,
    workflow_name: &str,
    state: &str,
    slot: &str,
    action_index: i64,
    action_fn: &str,
    args_json: &str,
    now: i64,
) -> Result<i64, String> {
    let res = client.execute(
        "INSERT INTO workflow_pending_action \
         (instance_id, workflow_name, state, slot, action_index, action_fn, args_json, status, attempts, created_at, updated_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)",
        &rq_params![instance_id, workflow_name, state, slot, action_index, action_fn, args_json, now, now],
    )?;
    res.last_insert_id.ok_or_else(|| "rqlite: INSERT into workflow_pending_action returned no last_insert_id".to_string())
}

fn rq_mark_action_done(client: &RqliteClient, id: i64, now: i64) -> Result<(), String> {
    client.execute("UPDATE workflow_pending_action SET status = 'done', updated_at = ? WHERE id = ?", &rq_params![now, id])?;
    Ok(())
}

fn rq_bump_action_attempts(client: &RqliteClient, id: i64, now: i64) -> Result<(), String> {
    client.execute(
        "UPDATE workflow_pending_action SET attempts = attempts + 1, updated_at = ? WHERE id = ?",
        &rq_params![now, id],
    )?;
    Ok(())
}

fn rq_list_pending_actions(client: &RqliteClient) -> Result<Vec<PendingWorkflowAction>, String> {
    let rows = client.query(
        "SELECT id, instance_id, workflow_name, state, slot, action_index, action_fn, args_json, attempts \
         FROM workflow_pending_action WHERE status = 'pending' ORDER BY id ASC",
        &[],
    )?;
    Ok(rows
        .iter()
        .map(|r| PendingWorkflowAction {
            id: json_i64(&r[0]),
            instance_id: json_i64(&r[1]),
            workflow_name: json_str(&r[2]),
            state: json_str(&r[3]),
            slot: json_str(&r[4]),
            action_index: json_i64(&r[5]),
            action_fn: json_str(&r[6]),
            args_json: json_str(&r[7]),
            attempts: json_i64(&r[8]),
        })
        .collect())
}

/// One `workflow_pending_action` row — see `WorkflowLog::begin_pending_action`'s
/// doc comment for what each field means and when it's written.
#[derive(Debug, Clone)]
pub struct PendingWorkflowAction {
    pub id: i64,
    pub instance_id: i64,
    pub workflow_name: String,
    pub state: String,
    pub slot: String,
    pub action_index: i64,
    pub action_fn: String,
    pub args_json: String,
    pub attempts: i64,
}
