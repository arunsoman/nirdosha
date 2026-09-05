//! Track C of the plugin-ecosystem plan
//! (`docs/ECOSYSTEM.md` §G1 / the plugin-gaps design conversation this
//! crate came out of): the reusable, plugin-crate-side answer to "how
//! does a Kind-A plugin hold a stateful, possibly async-backed resource
//! — a Cassandra session, a MySQL connection, an ActiveMQ subscription —
//! without every plugin author hand-rolling their own global
//! `OnceLock`/`HashMap` and private `tokio::Runtime`?"
//!
//! ## Why this, and not a new `Ty::Handle` variant
//!
//! The more rigorous answer — a first-class, compiler-enforced affine
//! handle type plugins could return, giving `ownership.rs` real
//! visibility into "this connection was closed twice" — is real, and
//! deliberately **not** built here. It's a language-surface change
//! (`GOVERNANCE.md` gates "the plugin ABI" behind the RFC process) and,
//! separately, it asks a plugin author to get `Box<dyn Any>` downcasting
//! and Nirdosha's own novel affine-checking semantics right — a pattern
//! with zero public precedent, which is exactly the kind of thing an
//! LLM (or a human working from an unfamiliar example) is more likely to
//! get subtly wrong. [`HandleRegistry`] below is deliberately the
//! opposite: a generic `Mutex<HashMap<u64, T>>`, about as ordinary and
//! well-represented a Rust pattern as exists. Reliable now beats
//! rigorous later, when "later" isn't blocking anything real yet.
//!
//! **The honest cost, stated plainly, not glossed over**: a handle
//! minted by [`HandleRegistry::insert`] is just a `Value::Int` (an
//! opaque `i64`) once it crosses into `.nir` source — `ownership.rs`
//! gives it none of the affine "one owner, closed exactly once"
//! guarantees a real `Ty::Db`/`Ty::Mq`/`Ty::Sandbox` handle gets today.
//! A `.nir` program can call a plugin's own `close(id)` builtin twice,
//! or drop the id and leak the underlying resource, and nothing in this
//! crate or the compiler catches either at compile time — only your own
//! plugin's `close` implementation returning a real, spanned
//! [`RuntimeError`] on a bad id catches it at *runtime*. If a plugin
//! author needs stronger guarantees than that, the answer today is
//! "wait for the `Ty::Handle` RFC," not "this crate."

use nirdosha::interpreter::{ErrorKind, RuntimeError, Value};
use nirdosha::token::Span;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, OnceLock};

/// A generic, thread-safe table mapping an opaque `u64` id (handed to
/// `.nir` source as a plain `Value::Int`) to a live Rust value `T` — a
/// `mysql::PooledConn`, a `scylla::Session`, whatever your plugin's
/// "connect" builtin actually produces. Directly generalizes
/// `crates/compiler/src/pool.rs`'s own `PoolRegistry<M: ManageConnection>`
/// past `r2d2`'s connection-pool-specific contract to *any* owned
/// resource — same "one process-wide table, keyed lookup, insert once /
/// remove on close" shape, no manager trait required.
///
/// One `HandleRegistry<T>` per resource *kind* your plugin exposes
/// (a `HandleRegistry<MysqlSession>` and a `HandleRegistry<MysqlTxn>`
/// would be two separate instances, each with its own id space) — ids
/// are never unique across two different `HandleRegistry` instances, so
/// don't mix them.
pub struct HandleRegistry<T> {
    next_id: AtomicU64,
    handles: Mutex<HashMap<u64, T>>,
}

impl<T> Default for HandleRegistry<T> {
    fn default() -> Self {
        Self::new()
    }
}

impl<T> HandleRegistry<T> {
    pub fn new() -> Self {
        HandleRegistry { next_id: AtomicU64::new(1), handles: Mutex::new(HashMap::new()) }
    }

    /// Takes ownership of `value`, mints a fresh id (never `0` — reserve
    /// that as a caller-chosen "no handle"/invalid sentinel if you ever
    /// want one), and returns it as the plain `i64` your builtin's
    /// declared `ret: Ty::I64` hands back to `.nir` source.
    pub fn insert(&self, value: T) -> i64 {
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        self.handles.lock().unwrap().insert(id, value);
        id as i64
    }

    /// Runs `f` against the live resource for `id` without removing it —
    /// what a "query"/"send"/"use the connection" builtin calls. `None`
    /// if `id` isn't a currently-open handle (already closed, or never
    /// existed) — turn that into a real [`plugin_error`] at the call
    /// site, not a panic.
    pub fn with<R>(&self, id: i64, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        let mut handles = self.handles.lock().unwrap();
        handles.get_mut(&(id as u64)).map(f)
    }

    /// Removes and returns the resource for `id` — what a "close"/"stop"
    /// builtin calls; the returned `T` is dropped at the call site,
    /// running whatever real teardown its own `Drop` impl does. `None`
    /// if `id` was already removed (a double-close) or never existed —
    /// this crate does not treat that as a panic; it's the plugin
    /// author's job to turn a `None` here into a clear, spanned
    /// [`RuntimeError`] via [`plugin_error`], the same way a real
    /// `Ty::Db`'s use-after-`stop` is a runtime error, not a crash.
    pub fn remove(&self, id: i64) -> Option<T> {
        self.handles.lock().unwrap().remove(&(id as u64))
    }

    /// Number of currently-open handles — diagnostic/test visibility,
    /// not used on any hot path.
    pub fn len(&self) -> usize {
        self.handles.lock().unwrap().len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// A process-wide, lazily-started multi-thread Tokio runtime, shared by
/// every plugin crate linked into the same binary that calls
/// [`shared_runtime`] — so N different async-backed plugins (a Cassandra
/// session here, a Neo4j session there) pay the cost of exactly one
/// runtime between them, not one each.
///
/// Nirdosha's own interpreter is deliberately synchronous end to end
/// (`crates/compiler/src/pool.rs`'s own doc comment: "no async runtime,
/// no tokio dependency — this whole compiler is synchronous") and has no
/// plan to change that (`docs/adr` records this as an explicit
/// non-goal). A plugin builtin's `call` closure is a plain, blocking
/// `Fn(&[Value], Span) -> Result<Value, RuntimeError>` — there is no
/// `.await` point anywhere in the interpreter's call path. If the
/// client library you're wrapping is async-only (most modern Cassandra/
/// Neo4j/etc. drivers are), this is the sanctioned bridge: call
/// [`shared_runtime`] and `.block_on(...)` the async call *inside* your
/// synchronous `PluginFn` — exactly the pattern this crate's own
/// reference plugins (`crates/plugin-example-cassandra`,
/// `crates/plugin-example-neo4j`) use.
static SHARED_RUNTIME: OnceLock<tokio::runtime::Runtime> = OnceLock::new();

pub fn shared_runtime() -> &'static tokio::runtime::Runtime {
    SHARED_RUNTIME.get_or_init(|| {
        tokio::runtime::Runtime::new()
            .expect("nirdosha-plugin-support: failed to start the shared Tokio runtime")
    })
}

/// Blocks the calling (interpreter) thread on `fut`, using the one
/// [`shared_runtime`] every async-backed plugin in this binary shares.
/// Thin, but this is the one line every async plugin builtin needs to
/// get right, and it's worth naming so a plugin author writes
/// `plugin_support::block_on(...)` instead of reaching for
/// `tokio::runtime::Handle::current().block_on(...)` (which panics
/// outside a runtime context — the wrong tool here) or spinning up a
/// fresh `Runtime` per call (correct but wasteful, and the exact
/// per-plugin duplication this crate exists to remove).
pub fn block_on<F: std::future::Future>(fut: F) -> F::Output {
    shared_runtime().block_on(fut)
}

/// Builds the real, spanned [`RuntimeError`] every plugin builtin should
/// return on failure — never a Rust-level panic. `plugin` is your
/// crate's own short name (shows up in the error message as
/// `` plugin `mysql`: ... ``, matching `rot13`'s own
/// `ErrorKind::PluginError` usage); `message` is free-form, typically the
/// underlying client library's own `Display`/`to_string()` error text.
pub fn plugin_error(plugin: &str, span: Span, message: impl Into<String>) -> RuntimeError {
    RuntimeError { kind: ErrorKind::PluginError { plugin: plugin.to_string(), message: message.into() }, span }
}

/// Extracts a `&str` argument at `idx`, or a real [`plugin_error`] naming
/// which argument and what it actually got — typecheck has already
/// proven the *declared* type matches by the time a builtin's `call`
/// runs (the same trust `rot13`'s own `rot13_call` places in
/// `typeck.rs`), so a mismatch here would mean a bug in how `params` was
/// declared, not a `.nir`-author mistake; still a clean error, never a
/// panic or an out-of-bounds index.
pub fn str_arg<'a>(args: &'a [Value], idx: usize, plugin: &str, span: Span) -> Result<&'a str, RuntimeError> {
    match args.get(idx) {
        Some(Value::Str(s)) => Ok(s),
        other => Err(plugin_error(plugin, span, format!("expected a str argument at position {idx}, got {other:?}"))),
    }
}

/// Same as [`str_arg`], for an `i64` argument — the shape every handle
/// id, port number, and timeout in this crate's reference plugins uses.
pub fn int_arg(args: &[Value], idx: usize, plugin: &str, span: Span) -> Result<i64, RuntimeError> {
    match args.get(idx) {
        Some(Value::Int(n)) => Ok(*n),
        other => Err(plugin_error(plugin, span, format!("expected an i64 argument at position {idx}, got {other:?}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn insert_then_with_sees_the_same_value() {
        let reg: HandleRegistry<String> = HandleRegistry::new();
        let id = reg.insert("hello".to_string());
        let seen = reg.with(id, |v| v.clone());
        assert_eq!(seen, Some("hello".to_string()));
    }

    #[test]
    fn ids_are_unique_and_start_above_zero() {
        let reg: HandleRegistry<i32> = HandleRegistry::new();
        let a = reg.insert(1);
        let b = reg.insert(2);
        assert_ne!(a, b);
        assert!(a > 0 && b > 0, "0 is reserved as a caller-chosen invalid sentinel");
    }

    #[test]
    fn remove_returns_the_value_once_then_none() {
        let reg: HandleRegistry<String> = HandleRegistry::new();
        let id = reg.insert("bye".to_string());
        assert_eq!(reg.remove(id), Some("bye".to_string()));
        assert_eq!(reg.remove(id), None, "a double-close must be a clean None, not a panic");
        assert_eq!(reg.with(id, |v| v.clone()), None, "use-after-close must be visible as None too");
    }

    #[test]
    fn unknown_id_is_none_not_a_panic() {
        let reg: HandleRegistry<i32> = HandleRegistry::new();
        assert_eq!(reg.with(999, |v| *v), None);
        assert_eq!(reg.remove(999), None);
    }

    #[test]
    fn len_tracks_open_handles() {
        let reg: HandleRegistry<i32> = HandleRegistry::new();
        assert!(reg.is_empty());
        let a = reg.insert(1);
        let _b = reg.insert(2);
        assert_eq!(reg.len(), 2);
        reg.remove(a);
        assert_eq!(reg.len(), 1);
    }

    #[test]
    fn shared_runtime_is_reused_across_calls() {
        // Two "different plugins" calling shared_runtime() get the
        // exact same runtime instance -- the actual point of this
        // helper (one runtime for the whole binary, not one per plugin).
        let a: *const tokio::runtime::Runtime = shared_runtime();
        let b: *const tokio::runtime::Runtime = shared_runtime();
        assert_eq!(a, b, "shared_runtime() must return the same runtime every call");
    }

    #[test]
    fn block_on_actually_runs_an_async_future() {
        let result = block_on(async {
            tokio::time::sleep(std::time::Duration::from_millis(1)).await;
            42
        });
        assert_eq!(result, 42);
    }

    #[test]
    fn str_arg_and_int_arg_extract_or_error_cleanly() {
        let span = Span { line: 0, col: 0 };
        let args = vec![Value::Str(std::sync::Arc::from("hi")), Value::Int(7)];
        assert_eq!(str_arg(&args, 0, "test", span).unwrap(), "hi");
        assert_eq!(int_arg(&args, 1, "test", span).unwrap(), 7);
        assert!(str_arg(&args, 1, "test", span).is_err(), "wrong variant must error, not panic");
        assert!(int_arg(&args, 5, "test", span).is_err(), "out-of-range index must error, not panic");
    }
}
