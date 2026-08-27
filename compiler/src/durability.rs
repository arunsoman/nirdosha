//! Shared plumbing for `workflow_log.rs`/`transact_log.rs`'s durability
//! store target — a local SQLite file (today's only option, and still
//! the default) or a real, shared Postgres database (`ROADMAP.md`'s
//! multi-instance coordination fix, Track A/"deployment story").
//!
//! ## Why this exists as its own module
//!
//! `workflow_log.rs` and `transact_log.rs` are two independent durability
//! stores with completely different schemas and query shapes, but they
//! share the exact same "where does this log actually live" question and
//! the exact same answer: `dbconn.rs`/`pool.rs`'s already-shipped
//! Postgres-pooling pattern, reused verbatim rather than re-derived
//! per-module. `LogTarget` is the parsed answer to that question;
//! `pg_pool` is the one place that turns a `postgres://`/`postgresql://`
//! connection string into a real, process-wide-shared, bounded pool —
//! both durability logs call it, so a `serve` process pointed at the
//! same Postgres database for both `--transact-log`/`--workflow-log`
//! shares one pool per connection string, not two, the same "one
//! `PoolRegistry` entry per distinct key" guarantee `dbconn.rs` already
//! gives `.nir`'s own `db_connect`.
//!
//! ## The correctness problem this solves
//!
//! Before this module existed, `WorkflowLog::open`/`TransactLog::open`
//! unconditionally opened a local `rusqlite::Connection` to a file path
//! — fine for one process, actively wrong for two: run a second
//! `nirdosha serve` replica behind a load balancer and each opens its
//! *own* independent file. `workflow_instance.id` (SQLite
//! `INTEGER PRIMARY KEY AUTOINCREMENT`) is minted from a separate
//! per-file counter on each replica — two replicas can (and eventually
//! will) hand out the *same* `instance_id` to two different workflow
//! instances, and nothing in this codebase ever notices. `transact`'s
//! own `txn_id`-keyed idempotency guarantee (`TRANSACT.md`) is silently
//! narrowed from "once per `txn_id`, process-lifetime" to "once per
//! `txn_id`, per replica" — a retried request that lands on a different
//! replica than its first attempt re-executes `network` from scratch.
//!
//! Pointing `--transact-log`/`--workflow-log` at a real Postgres
//! database fixes both: one shared table means one shared
//! `BIGSERIAL`/`BIGINT GENERATED ALWAYS AS IDENTITY` sequence for
//! `workflow_instance.id` (a real cross-replica-unique id, not a
//! per-file counter), and one shared `transact_log` table means
//! `txn_id`'s idempotency guarantee actually holds process-lifetime
//! *and* replica-lifetime. SQLite-backed apps remain explicitly
//! single-instance — not a bug, an inherent property of an embedded,
//! per-file database, the same category `dbconn.rs`'s own `:memory:`
//! special case already documents. `serve.rs`'s startup lock file
//! (`crate::instance_lock`) turns that limitation into a loud refusal
//! to start a second instance, instead of the silent corruption it used
//! to be.

use crate::pool::{PoolConfig, PoolRegistry};
use r2d2_postgres::PostgresConnectionManager;
use std::sync::LazyLock;

/// Where one durability log actually lives. `Sqlite` is the original,
/// still-default shape (a local file path); `Postgres` is new —
/// selected by `from_cli_arg`/`from_str` whenever the given value starts
/// with `postgres://`/`postgresql://`, mirroring `dbconn::connect`'s own
/// dispatch rule for `.nir`'s `db_connect` exactly, so the same mental
/// model ("a `postgres://` string always means Postgres, everything
/// else is a file") applies here too.
#[derive(Clone, Debug)]
pub enum LogTarget {
    Sqlite(std::path::PathBuf),
    /// The raw `postgres://`/`postgresql://` connection string, unparsed
    /// — parsed lazily by `pg_pool` only once a pool for this exact
    /// string doesn't already exist (`PoolRegistry::get_or_create`'s own
    /// laziness).
    Postgres(String),
    /// The raw `rqlite://`/`rqlites://` connection string — `ROADMAP.md`
    /// Phase 2 of the multi-instance fix: a real, Raft-replicated SQLite
    /// cluster (`crate::rqlite`), for a deployment that specifically
    /// wants multi-instance *without* taking on Postgres as a dependency.
    /// Unlike `Postgres` above, parsed eagerly by `RqliteClient::connect`
    /// at `WorkflowLog`/`TransactLog::open` time — rqlite has no
    /// equivalent pooling story to defer into (each request is its own
    /// short-lived HTTP round trip, `crate::rqlite`'s own doc comment).
    Rqlite(String),
}

/// Every existing call site that built a `PathBuf` for `--transact-log`/
/// `--workflow-log` before this module existed keeps compiling unchanged
/// — `with_transact_log_path`/`with_workflow_log_path` accept
/// `impl Into<LogTarget>`, and a bare `PathBuf` always means "SQLite
/// file," the same as it always did.
impl From<std::path::PathBuf> for LogTarget {
    fn from(p: std::path::PathBuf) -> Self {
        LogTarget::Sqlite(p)
    }
}

impl LogTarget {
    /// `--transact-log=<value>`/`--workflow-log=<value>`'s own parsing
    /// (`main.rs`). A `postgres://`/`postgresql://` value selects the
    /// shared, multi-instance-safe Postgres backend documented in this
    /// module's own doc comment above; `rqlite://`/`rqlites://` selects
    /// the Raft-replicated-SQLite backend (`crate::rqlite`, `ROADMAP.md`
    /// Phase 2); anything else is a local SQLite file path, byte-for-byte
    /// the pre-existing behavior.
    pub fn from_cli_arg(s: &str) -> Self {
        if s.starts_with("postgres://") || s.starts_with("postgresql://") {
            LogTarget::Postgres(s.to_string())
        } else if s.starts_with("rqlite://") || s.starts_with("rqlites://") {
            LogTarget::Rqlite(s.to_string())
        } else {
            LogTarget::Sqlite(std::path::PathBuf::from(s))
        }
    }
}

/// One pool per TLS-shape, `dbconn.rs`'s own `POSTGRES_POOLS`/
/// `POSTGRES_TLS_POOLS` split copied verbatim (see that module's doc
/// comment for why two registries, not one: `PostgresConnectionManager<T>`
/// is a different concrete type per `T`). Deliberately a *separate* pair
/// of statics from `dbconn.rs`'s own, even though the underlying
/// `PoolRegistry` type is identical — `dbconn.rs`'s pools back arbitrary
/// `.nir`-authored `db_connect` calls, these back only the two
/// durability logs; sharing one registry between them would mean an
/// unrelated `.nir` program's own Postgres traffic and this process's
/// durability-log traffic silently compete for the same
/// `NIRDOSHA_DB_POOL_MAX_SIZE` budget. `NIRDOSHA_DURABILITY_POOL_*`
/// (`PoolConfig::from_env`'s own prefix mechanism) tunes these two
/// independently instead.
static POSTGRES_POOLS: LazyLock<PoolRegistry<PostgresConnectionManager<postgres::NoTls>>> = LazyLock::new(PoolRegistry::new);
static POSTGRES_TLS_POOLS: LazyLock<PoolRegistry<PostgresConnectionManager<postgres_native_tls::MakeTlsConnector>>> =
    LazyLock::new(PoolRegistry::new);

/// A pool for one of the two TLS shapes — `workflow_log.rs`/
/// `transact_log.rs`'s `Backend` enum holds exactly this, so every query
/// method dispatches on it the same way `dbconn.rs::DbConn` already
/// dispatches `Postgres`/`PostgresTls` query calls to one shared
/// `&mut postgres::Client`-taking function (both variants' `Connection`
/// associated type is `postgres::Client`, TLS-vs-not only affects how
/// the socket itself is opened).
pub enum PgPool {
    Plain(r2d2::Pool<PostgresConnectionManager<postgres::NoTls>>),
    Tls(r2d2::Pool<PostgresConnectionManager<postgres_native_tls::MakeTlsConnector>>),
}

/// Resolves `conn_str` to a real, process-wide-shared pool — creating it
/// on first use for that exact string, reusing it for every later call
/// with the same string (`PoolRegistry::get_or_create`). TLS opt-in via
/// `sslmode=require`/`verify-ca`/`verify-full` in the connection string
/// itself, identical rule to `dbconn.rs::connect_postgres`.
pub fn pg_pool(conn_str: &str) -> Result<PgPool, String> {
    let wants_tls =
        ["sslmode=require", "sslmode=verify-ca", "sslmode=verify-full"].iter().any(|flag| conn_str.contains(flag));
    let config: postgres::Config = conn_str.parse().map_err(|e| format!("invalid postgres connection string: {e}"))?;
    let pool_config = PoolConfig::from_env("DURABILITY");
    if wants_tls {
        let tls = native_tls::TlsConnector::new().map_err(|e| format!("failed to initialize TLS: {e}"))?;
        let connector = postgres_native_tls::MakeTlsConnector::new(tls);
        let pool =
            POSTGRES_TLS_POOLS.get_or_create(conn_str, pool_config, || Ok(PostgresConnectionManager::new(config, connector)))?;
        Ok(PgPool::Tls(pool))
    } else {
        let pool =
            POSTGRES_POOLS.get_or_create(conn_str, pool_config, || Ok(PostgresConnectionManager::new(config, postgres::NoTls)))?;
        Ok(PgPool::Plain(pool))
    }
}
