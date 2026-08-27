//! Phase 0 of `ROADMAP.md`'s workflow/transact multi-instance fix: an
//! OS-level exclusive lock on a SQLite-backed durability log's file,
//! held for the log's whole process lifetime, so a second `nirdosha
//! serve` process accidentally pointed at the exact same log file fails
//! fast at startup with a clear error, instead of the two processes
//! silently interleaving writes to it.
//!
//! ## What this deliberately does NOT solve
//!
//! Two replicas each with their OWN independent local file (the real
//! horizontal-scaling correctness wall `ROADMAP.md`'s Track A names)
//! never touch the same filesystem path at all — there is nothing for a
//! local file lock to contend on, so this guard structurally cannot
//! detect or prevent that divergence; each process opens its lock file
//! successfully because it's genuinely the only one touching *that*
//! path. This module only catches the narrower, same-host "started
//! twice" accident: a stuck old process during a rolling restart, an
//! orchestrator briefly overlapping two instances on one volume, an
//! operator running `nirdosha serve` a second time by hand while one's
//! already running. Real multi-instance deployments need
//! `--transact-log`/`--workflow-log` pointed at a real Postgres database
//! instead (`durability.rs`, `pg_pool`) — that's what actually removes
//! the wall; this module never claims to.
//!
//! ## Mechanism
//!
//! SQLite's own `PRAGMA locking_mode=EXCLUSIVE`, on a tiny sidecar file
//! (`<log path>.lock`) separate from the log's own connection/pragmas —
//! chosen over hand-rolled `fcntl`/`LockFile` syscalls specifically
//! because SQLite already implements this correctly and portably on
//! every platform this project ships for (the same "reuse solved infra
//! already in the dependency tree" reasoning `dist`'s vendored-Z3 and
//! `r2d2_sqlite`'s `bundled` feature already follow), and because it
//! needs no new crate. Once a connection sets `EXCLUSIVE` locking mode
//! and performs its first read or write, SQLite acquires and holds that
//! OS-level lock for the connection's entire lifetime — a second
//! connection to the same file gets `SQLITE_BUSY` on its own first
//! read/write. No `busy_timeout` is set on this connection, on purpose:
//! a startup lock conflict should fail immediately and loudly, not hang
//! retrying for several seconds first.

use rusqlite::Connection;

/// Held only for its `Drop` impl (closing the connection releases the
/// OS-level lock) — never queried again once `acquire` returns `Ok`.
pub struct InstanceLock(#[allow(dead_code)] Connection);

// SAFETY: `InstanceLock` exposes no method that ever touches the inner
// `Connection` again after `acquire` returns it -- it's held purely so
// `Drop` releases the OS-level lock at the right time. `rusqlite::
// Connection` itself isn't `Sync` only because of interior `RefCell`s
// (a statement cache, an open-transaction flag) that a *second* live
// reference could race on; since no second reference to this one is
// ever created, there's nothing to race. This is what lets
// `WorkflowLog`/`TransactLog` -- which embed an `Option<InstanceLock>`
// directly, not behind their own `Mutex` -- stay `Sync`, which
// `Arc<WorkflowLog>`/`Arc<TransactLog>` need to cross threads (e.g.
// `interpreter.rs::run_on_big_stack`'s spawned thread).
unsafe impl Sync for InstanceLock {}

impl InstanceLock {
    /// `log_path` is the durability log's own file path — the lock file
    /// itself lives alongside it at `<log_path>.lock`, a tiny, separate
    /// SQLite file this module owns exclusively (nothing else ever reads
    /// or writes it).
    pub fn acquire(log_path: &std::path::Path) -> Result<Self, String> {
        let lock_path = {
            let mut s = log_path.as_os_str().to_owned();
            s.push(".lock");
            std::path::PathBuf::from(s)
        };
        let conn = Connection::open(&lock_path)
            .map_err(|e| format!("durability log lock I/O error opening {}: {e}", lock_path.display()))?;
        conn.pragma_update(None, "locking_mode", "EXCLUSIVE")
            .map_err(|e| format!("durability log lock I/O error at {}: {e}", lock_path.display()))?;
        conn.execute("CREATE TABLE IF NOT EXISTS nirdosha_instance_lock (held_at INTEGER)", [])
            .map_err(|e| busy_to_message(e, log_path, &lock_path))?;
        Ok(InstanceLock(conn))
    }
}

fn busy_to_message(e: rusqlite::Error, log_path: &std::path::Path, lock_path: &std::path::Path) -> String {
    let is_busy = matches!(
        &e,
        rusqlite::Error::SqliteFailure(err, _) if err.code == rusqlite::ErrorCode::DatabaseBusy
    );
    if is_busy {
        format!(
            "another nirdosha instance already holds the durability log at {} (lock file {}) -- \
             a second process pointed at the same file is refused here, to avoid the two silently \
             diverging. Stop the other instance, or for a real multi-instance deployment point \
             --transact-log/--workflow-log at a shared Postgres database (postgres://...) instead \
             of a local file.",
            log_path.display(),
            lock_path.display(),
        )
    } else {
        format!("durability log lock I/O error at {}: {e}", lock_path.display())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_second_lock_on_the_same_path_is_refused_while_the_first_is_held() {
        let dir = std::env::temp_dir().join(format!("nirdosha_instance_lock_test_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let log_path = dir.join("workflow.db");

        let first = InstanceLock::acquire(&log_path).expect("first lock must succeed");
        let second = InstanceLock::acquire(&log_path);
        let msg = match second {
            Err(m) => m,
            Ok(_) => panic!("a second lock on the same path must be refused while the first is held"),
        };
        assert!(msg.contains("already holds"), "error should explain why, got: {msg}");

        drop(first);
        let third = InstanceLock::acquire(&log_path);
        assert!(third.is_ok(), "once the first lock is dropped, a new lock on the same path must succeed");

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn locks_on_different_paths_never_conflict() {
        let dir = std::env::temp_dir().join(format!("nirdosha_instance_lock_test_diff_{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let a = InstanceLock::acquire(&dir.join("a.db"));
        let b = InstanceLock::acquire(&dir.join("b.db"));
        assert!(a.is_ok() && b.is_ok(), "independent paths must never contend with each other");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
