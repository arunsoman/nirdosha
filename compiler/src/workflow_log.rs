//! `workflow { ... }`'s durable state store (`WORKFLOW.md`). SQLite-backed
//! (`rusqlite`, already a dependency — no new crate), directly modeled on
//! `transact_log.rs`'s "one synchronous statement per write, real
//! durability, not batched" shape. Owns everything `WORKFLOW.md`'s
//! desugared `__workflow_start`/`__workflow_advance`/`__workflow_link_advance`
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

use std::path::Path;
use std::sync::Mutex;

use rusqlite::{params, Connection, OptionalExtension};

pub struct WorkflowLog {
    conn: Mutex<Connection>,
}

fn io_err(e: rusqlite::Error) -> String {
    format!("workflow log I/O error: {e}")
}

impl WorkflowLog {
    pub fn open(path: &Path) -> Result<Self, String> {
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
        // than assumed present.
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
        Ok(WorkflowLog { conn: Mutex::new(conn) })
    }

    /// `started_by_subject`: the calling identity's `subject`, when
    /// `start_<workflow>`'s (now-optional, `WORKFLOW.md`'s "who submitted
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
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "INSERT INTO workflow_instance (workflow_name, state, data_json, started_by_subject, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?)",
            params![workflow_name, state, data_json, started_by_subject, now, now],
        )
        .map_err(io_err)?;
        Ok(conn.last_insert_rowid())
    }

    /// `(workflow_name, state, data_json)`, or `None` if no such instance.
    pub fn get_instance(&self, id: i64) -> Result<Option<(String, String, String)>, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.query_row(
            "SELECT workflow_name, state, data_json FROM workflow_instance WHERE id = ?",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(io_err)
    }

    /// Every instance of `workflow_name`, as `(id, state, data_json)` —
    /// backs `interpreter.rs::workflow_pending_for_me`
    /// (`__workflow_pending_for_me`, `WORKFLOW.md`'s "state ownership"
    /// section): the interpreter, not this query, decides which of these
    /// the caller actually owns (it needs the live `WorkflowDecl` AST to
    /// know each state's `owner`, which this store doesn't have — this
    /// method only ever answers "every instance," the same "SQL does the
    /// narrow mechanical part, the caller does the semantic part"
    /// division `get_instance` already draws for one instance at a time).
    pub fn list_instances(&self, workflow_name: &str) -> Result<Vec<(i64, String, String)>, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        let mut stmt = conn
            .prepare("SELECT id, state, data_json FROM workflow_instance WHERE workflow_name = ? ORDER BY id")
            .map_err(io_err)?;
        let rows = stmt
            .query_map(params![workflow_name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(io_err)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(io_err)
    }

    /// Every instance of `workflow_name` that `subject` itself started —
    /// backs `list_<workflow>_submitted_by_me`
    /// (`__workflow_submitted_by_me`, `WORKFLOW.md`'s "who submitted
    /// this" section), the read side a requester needs to track their
    /// own request without needing `owner` on any state at all. Unlike
    /// `list_instances`, scoped in SQL, not by the caller — there's no
    /// per-state semantic to apply here (submission isn't a decision
    /// gate), so nothing stops this query from doing the whole filter
    /// itself, unlike `list_instances`' own doc comment explains for the
    /// owner case.
    pub fn list_instances_by_starter(&self, workflow_name: &str, subject: &str) -> Result<Vec<(i64, String, String)>, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
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

    /// Moves `id` to `to_state` and appends the history row in one call —
    /// callers always want both together (`Interpreter`'s `advance`
    /// dispatch never updates state without also recording why).
    /// `actor_subject`/`via_link` (`WORKFLOW.md`'s "audit trail" section)
    /// record *who* fired this transition and *how* — `None`/`false` for
    /// `actor_subject`/`via_link` only ever happens together (the magic-
    /// link path is the one case with no `identity` to attribute this
    /// to at all, `interpreter.rs::workflow_advance_inner`). `comment`
    /// is the caller-supplied free-text reason, if `advance_<workflow>`'s
    /// `payload` carried one (`{"comment": "..."}`) — still not threaded
    /// into `on_entry`/`on_exit` bindings, `WORKFLOW.md`'s disclosed v1
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
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "UPDATE workflow_instance SET state = ?, updated_at = ? WHERE id = ?",
            params![to_state, now, id],
        )
        .map_err(io_err)?;
        conn.execute(
            "INSERT INTO workflow_history (instance_id, from_state, to_state, event, actor_subject, via_link, comment, at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
            params![id, from_state, to_state, event, actor_subject, via_link, comment, now],
        )
        .map_err(io_err)?;
        Ok(())
    }

    /// The full append-only transition log for one instance, oldest
    /// first — backs `get_<workflow>_history` (`__workflow_history`,
    /// `WORKFLOW.md`'s "audit trail" section): `(from_state, to_state,
    /// event, actor_subject, via_link, comment, at)`.
    #[allow(clippy::type_complexity)]
    pub fn list_history(
        &self,
        instance_id: i64,
    ) -> Result<Vec<(String, String, String, Option<String>, bool, Option<String>, i64)>, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
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

    pub fn mint_link(&self, instance_id: i64, event: &str, token: &str) -> Result<(), String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "INSERT INTO magic_link (instance_id, event, token, consumed) VALUES (?, ?, ?, 0)",
            params![instance_id, event, token],
        )
        .map_err(io_err)?;
        Ok(())
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
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.query_row(
            "SELECT id, token FROM magic_link WHERE instance_id = ? AND event = ? AND consumed = 0",
            params![instance_id, event],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(io_err)
    }

    /// Single-use consume by row id, once the caller has already verified
    /// the token matches: `true` only for the one caller that wins the
    /// race, via a single `UPDATE ... WHERE consumed = 0` (no separate
    /// read-then-write for *this* step — that would be a TOCTOU
    /// double-spend of the same token).
    pub fn consume_link_by_id(&self, id: i64) -> Result<bool, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        let n = conn.execute("UPDATE magic_link SET consumed = 1 WHERE id = ? AND consumed = 0", params![id]).map_err(io_err)?;
        Ok(n == 1)
    }

    /// Upserted on every successful `serve.rs::resolve_identity` — the
    /// only writer. `roles_json` is stored verbatim (a JSON array of role
    /// strings, e.g. `["compliance_officer"]`); `subjects_with_role`
    /// below matches against it with a `LIKE`, a deliberately simple v1
    /// (documented in `WORKFLOW.md`) rather than a real JSON-array
    /// membership query.
    pub fn upsert_identity(&self, subject: &str, roles_json: &str, now: i64) -> Result<(), String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "INSERT INTO identity_directory (subject, roles_json, last_seen_at) VALUES (?, ?, ?) \
             ON CONFLICT(subject) DO UPDATE SET roles_json = excluded.roles_json, last_seen_at = excluded.last_seen_at",
            params![subject, roles_json, now],
        )
        .map_err(io_err)?;
        Ok(())
    }

    pub fn subjects_with_role(&self, role: &str) -> Result<Vec<String>, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        let pattern = format!("%\"{role}\"%");
        let mut stmt = conn.prepare("SELECT subject FROM identity_directory WHERE roles_json LIKE ?").map_err(io_err)?;
        let rows = stmt.query_map(params![pattern], |row| row.get::<_, String>(0)).map_err(io_err)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(io_err)?);
        }
        Ok(out)
    }

    pub fn set_presence(&self, subject: &str, online: bool, now: i64) -> Result<(), String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "INSERT INTO identity_presence (subject, online, updated_at) VALUES (?, ?, ?) \
             ON CONFLICT(subject) DO UPDATE SET online = excluded.online, updated_at = excluded.updated_at",
            params![subject, online as i64, now],
        )
        .map_err(io_err)?;
        Ok(())
    }

    pub fn is_online(&self, subject: &str) -> Result<bool, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        let online: Option<i64> = conn
            .query_row("SELECT online FROM identity_presence WHERE subject = ?", params![subject], |row| row.get(0))
            .optional()
            .map_err(io_err)?;
        Ok(online.unwrap_or(0) != 0)
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
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "INSERT INTO workflow_pending_action \
             (instance_id, workflow_name, state, slot, action_index, action_fn, args_json, status, attempts, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?, ?, ?, 'pending', 0, ?, ?)",
            params![instance_id, workflow_name, state, slot, action_index, action_fn, args_json, now, now],
        )
        .map_err(io_err)?;
        Ok(conn.last_insert_rowid())
    }

    pub fn mark_action_done(&self, id: i64, now: i64) -> Result<(), String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute("UPDATE workflow_pending_action SET status = 'done', updated_at = ? WHERE id = ?", params![now, id])
            .map_err(io_err)?;
        Ok(())
    }

    pub fn bump_action_attempts(&self, id: i64, now: i64) -> Result<(), String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "UPDATE workflow_pending_action SET attempts = attempts + 1, updated_at = ? WHERE id = ?",
            params![now, id],
        )
        .map_err(io_err)?;
        Ok(())
    }

    /// Every not-yet-`done` action row, oldest first — `Interpreter::
    /// replay_pending_workflow_actions` (`nirdosha serve` startup, once,
    /// right alongside `replay_pending_transactions`) resumes each of
    /// these independently.
    pub fn list_pending_actions(&self) -> Result<Vec<PendingWorkflowAction>, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
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
