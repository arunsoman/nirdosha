//! `workflow { ... }`'s durable state store (`WORKFLOW.md`). SQLite-backed
//! (`rusqlite`, already a dependency — no new crate), directly modeled on
//! `transact_log.rs`'s "one synchronous statement per write, real
//! durability, not batched" shape. Owns everything `WORKFLOW.md`'s
//! desugared `__workflow_start`/`__workflow_advance`/`__workflow_link_advance`
//! builtins (`interpreter.rs`) need at runtime: instance state, an
//! append-only transition history, magic-link mint/consume, and the two
//! pieces of identity bookkeeping no builtin/route in this codebase kept
//! before (`identity_directory` for `Recipient::ByRole`, `identity_presence`
//! for `notify`'s online/offline branch).
//!
//! **Scope note, disclosed rather than silently dropped**: unlike
//! `transact_log.rs`, this module does not (yet) durably log each
//! individual `on_entry`/`on_exit` action call before dispatching it, so
//! there is no `replay_pending_workflow_actions` startup pass. A failed
//! action retries in-process (`Interpreter::run_workflow_action`, the same
//! bounded-backoff shape `run_transact_write_slot` uses for `commit`/
//! `compensate`) and, if the retry budget is exhausted, traps
//! `WorkflowActionError::ActionPending` — the instance's `state` has
//! already durably advanced by that point, so it is not lost, just stuck
//! on that state's actions until an operator (or a future replay pass)
//! retries the transition again. `WORKFLOW.md`'s "deliberate non-goals"
//! section calls this out explicitly.

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
                created_at INTEGER NOT NULL,
                updated_at INTEGER NOT NULL
            );
            CREATE TABLE IF NOT EXISTS workflow_history (
                id INTEGER PRIMARY KEY AUTOINCREMENT,
                instance_id INTEGER NOT NULL,
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                event TEXT NOT NULL,
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
            );",
        )
        .map_err(io_err)?;
        Ok(WorkflowLog { conn: Mutex::new(conn) })
    }

    pub fn create_instance(&self, workflow_name: &str, state: &str, data_json: &str, now: i64) -> Result<i64, String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "INSERT INTO workflow_instance (workflow_name, state, data_json, created_at, updated_at) \
             VALUES (?, ?, ?, ?, ?)",
            params![workflow_name, state, data_json, now, now],
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

    /// Moves `id` to `to_state` and appends the history row in one call —
    /// callers always want both together (`Interpreter`'s `advance`
    /// dispatch never updates state without also recording why).
    pub fn record_transition(&self, id: i64, from_state: &str, to_state: &str, event: &str, now: i64) -> Result<(), String> {
        let conn = self.conn.lock().expect("workflow log mutex poisoned");
        conn.execute(
            "UPDATE workflow_instance SET state = ?, updated_at = ? WHERE id = ?",
            params![to_state, now, id],
        )
        .map_err(io_err)?;
        conn.execute(
            "INSERT INTO workflow_history (instance_id, from_state, to_state, event, at) VALUES (?, ?, ?, ?, ?)",
            params![id, from_state, to_state, event, now],
        )
        .map_err(io_err)?;
        Ok(())
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
}
