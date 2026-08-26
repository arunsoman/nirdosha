//! Tree-walking interpreter. Deliberately no effect checking, no
//! SMT-discharged bounds — those are goal.md rows 4, 9, Phase 2 work. What
//! this *does* enforce now: declared integer widths are checked at every
//! `let`, `return`, and function boundary (row 4's Tier-2 "checked"
//! behavior as a runtime stand-in for the eventual compile-time proof —
//! see `Ty::in_range` in ast.rs), and every error is a structured
//! `RuntimeError` with a span and a machine-matchable `kind`, not a
//! formatted string a caller has to re-parse (row 9).
//!
//! Ownership (goal.md row 1) is enforced *statically*, by `ownership.rs`,
//! before this file ever runs — this interpreter doesn't re-check moves at
//! runtime. Its own memory safety for `Value::Boxed` is simply inherited
//! from Rust's ownership system (a real `Box<Value>`, dropped by Rust when
//! it goes out of scope); see `Value`'s doc comment for why that's an
//! honest thing to say, not a shortcut being hidden.
//!
//! **`spawn`/`join` (goal.md rows 2–3) are real OS threads** — the
//! honestly-scoped "first implementation" (real concurrency now, not a
//! simulation), with the door left open for a lighter-weight virtual-
//! thread scheduler later without changing the *language* semantics:
//! `spawn`/`join` are the whole surface a Nirdosha program sees, and
//! nothing about swapping the OS-thread backing for an M:N one later
//! would change what a program written against that surface means. The
//! race-freedom claim doesn't come from anything in this file — it comes
//! from `ownership.rs` already requiring every argument moved into a
//! `spawn` to be consumed, the same as a normal call argument, so no two
//! concurrent computations can ever alias the same `box`-typed data. This
//! file only has to actually run threads correctly; the safety argument
//! was already proved before it got here.
//!
//! Because a spawned thread needs to look up functions independently of
//! whoever spawned it, `Interpreter` no longer borrows the `Program` (a
//! borrow can't cross `std::thread::spawn`'s `'static` bound) — it holds
//! an `Arc<Program>` instead, cheaply cloned into each spawned thread's
//! own `Interpreter`.
//!
//! Control flow: `if` is a genuine expression in the grammar (GRAMMAR.md),
//! and a block's value is its last expression-statement (Unit if the block
//! is empty or ends in `let`/`return`/`while`). `return` has to be able to
//! unwind out of a `let`'s initializer, an `if`'s condition, a binary
//! operand — anywhere an expression can appear — not just out of a
//! statement list. `Signal` is what carries that: every `eval_expr` site
//! propagates it with `?`, and only `call()` catches `Signal::Return`.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use serde::{Deserialize, Serialize};

use crate::ast::*;
use crate::effects::{self, EffectSet};
use crate::observability;
use crate::token::Span;
use crate::transact_log::{PendingTxn, TransactLog};
use crate::workflow_log::{PendingWorkflowAction, WorkflowLog};

/// Renamed on import — this file's own `Value` enum would otherwise
/// collide with `serde_json::Value`'s name at every use site.
use serde_json::Value as JsonDoc;

use rust_decimal::Decimal;
use std::str::FromStr;

// Row 12: relying-party identity validation (mock OIDC/JWT).
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::Mac;
use sha2::{Digest, Sha256};

/// Not `Copy` — `Value::Boxed` owns a heap allocation (a real Rust `Box`),
/// and giving the enum a free bitwise-copy would let two `Value`s alias the
/// same allocation with neither aware of the other, exactly the hazard
/// `Ty::Box`'s affine-ness (ast.rs) and `ownership.rs`'s move-checker exist
/// to prevent. This interpreter doesn't implement allocation or
/// deallocation itself — it inherits Rust's — so its actual contribution
/// to the "no GC" claim (goal.md row 1) is the *static* proof
/// (`ownership.rs`) that a Nirdosha program never uses a value after
/// giving it up, which is the same proof a real (future, LLVM-compiled)
/// backend would need to free deterministically with no garbage collector
/// at all. Don't read "the interpreter runs `box` correctly" as "row 1 is
/// done" — it's the checker that's doing the row-1 work, not this file.
/// Thin wrapper solely so `Value::Mq` can derive `Debug` — `redis::
/// Connection` itself doesn't implement it. `Deref`/`DerefMut` make it
/// otherwise transparent at call sites.
pub struct MqConn(pub redis::Connection);

impl std::fmt::Debug for MqConn {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("MqConn(..)")
    }
}

impl std::ops::Deref for MqConn {
    type Target = redis::Connection;
    fn deref(&self) -> &redis::Connection {
        &self.0
    }
}

impl std::ops::DerefMut for MqConn {
    fn deref_mut(&mut self) -> &mut redis::Connection {
        &mut self.0
    }
}

#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
    /// A fixed-length dense array (`Ty::Vector`). `Arc<[Value]>`, not
    /// `Vec<Value>` -- the same cheap-clone-on-read reasoning
    /// `Value::Str`'s `Arc<str>` already documents: `Env::get` clones
    /// every `Value` on read, and this phase declares `Vector`/`Matrix`
    /// non-affine (see `ast.rs::Ty::is_affine`'s doc comment), so every
    /// read of a vector-typed binding is a real clone, not just a
    /// borrow — `Arc` makes that a refcount bump instead of an
    /// element-by-element copy.
    Vector(Arc<[Value]>),
    /// A fixed-shape dense array (`Ty::Matrix`), row-major, flattened
    /// into one contiguous `Arc<[Value]>` — element `(i, j)` lives at
    /// `i * cols + j`. Same `Arc` reasoning as `Value::Vector`.
    Matrix(Arc<[Value]>, usize, usize),
    /// IEEE 754 double (`Ty::F64`). No range/overflow story the way
    /// `Value::Int` needs one (`check_ty` just matches the type tag, no
    /// `in_range` call) -- floats saturate to `inf`/`NaN` instead of
    /// trapping, per this phase's float semantics (see `ast.rs::Ty::F64`).
    Float(f64),
    /// A 128-bit fixed-point decimal (`Ty::Dec128` — its doc comment has
    /// the full design). `rust_decimal::Decimal` directly, not wrapped:
    /// it's already `Copy`/`Clone`/cheap, unlike `Value::Str`/`Value::
    /// Json`'s `Arc`-wrapped content, so there's no cheap-clone-on-read
    /// problem here to solve. Scale lives on the value itself (`Decimal`
    /// tracks its own exponent) — there's no separate scale field to
    /// keep in sync.
    Dec128(Decimal),
    Bool(bool),
    Unit,
    Boxed(Box<Value>),
    /// A shared borrow. Under the hood this is still just a *clone* of the
    /// pointee's `Value` (this interpreter has no real aliasing — see
    /// `Value`'s doc comment above), so `Value::Ref` is observably
    /// identical to `Value::Boxed` at runtime. It exists as its own
    /// variant anyway so `Expr::Deref` can be honest about *why* a
    /// dereference is allowed: `ownership.rs`/`typeck.rs` enforce "you
    /// can't move affine content out through a reference" using the
    /// *static* `Ty::Ref` distinction, and this variant is what that
    /// static distinction corresponds to at the value level, even though
    /// nothing here currently depends on telling the two apart at runtime.
    Ref(Box<Value>),
    /// A handle to a real OS thread running a spawned computation. The
    /// `Mutex<Option<..>>` wrapper exists for exactly one reason: `join`
    /// needs to *take* the `JoinHandle` out (handles aren't `Clone` — a
    /// thread can only be joined once, by design, matching `Ty::Thread`'s
    /// affine-ness), but this interpreter's `Env` clones every `Value` on
    /// read (documented above), so the handle needs a container that's
    /// cheap to clone (an `Arc`) while still letting exactly one clone
    /// ever successfully extract the real handle. `Arc`, not `Rc`: this
    /// has to be `Send` to be moved into a spawned thread's own captured
    /// arguments (e.g. a function spawning another function and handing
    /// it a thread handle), and `Rc` isn't.
    Thread(Arc<Mutex<Option<ThreadHandle>>>),
    /// A handle to an unbounded, multi-producer multi-consumer message
    /// queue. `Arc`, not `Rc` (same reason as `Value::Thread`): has to be
    /// `Send` to cross into a spawned thread's captured arguments. Unlike
    /// `Value::Thread`'s `Mutex<Option<..>>`, there's no one-time `.take()`
    /// here — a channel handle is meant to be read many times (see
    /// `Ty::Channel`'s doc comment), so every clone of this `Arc` is just
    /// another equally-valid handle to the same shared queue.
    Channel(Arc<ChannelInner>),
    /// A handle to a real, separate OS process (see `Ty::Sandbox`'s doc
    /// comment). `Mutex<Option<..>>`, same shape and reason as
    /// `Value::Thread`: `stop` needs to `.take()` the process out
    /// exactly once. Unlike `Value::Thread`, dropping every `Arc` to this
    /// *without* ever calling `stop` still kills the process — see
    /// `SandboxChild`'s `Drop` impl — which is the actual "deterministic
    /// teardown" guarantee SANDBOXING.md's layer 1 promises.
    Sandbox(Arc<Mutex<Option<SandboxChild>>>),
    /// UTF-8 text (`Ty::Str`). `Arc<str>`, not `String`: not affine (see
    /// `Ty::Str`'s doc comment), so every `Env` read clones it — an `Arc`
    /// clone is a refcount bump, a `String` clone would copy the bytes
    /// every single time a string-typed binding is merely read. Same
    /// reasoning as `Value::Channel`'s `Arc`, just for content instead of
    /// shared state.
    Str(Arc<str>),
    /// A handle to a real TCP connection (`connect(host, port)` — see
    /// `Ty::Tcp`'s doc comment). `Mutex<Option<..>>`, same shape and
    /// reason as `Value::Sandbox`: `stop` needs to `.take()` the stream
    /// out exactly once. Unlike `SandboxChild`, no custom `Drop` is
    /// needed here — a `TcpStream` closes its own socket on drop with no
    /// help required (there's no analog to a leaked OS *process* to
    /// guard against; a dropped socket is just... closed).
    Tcp(Arc<Mutex<Option<std::net::TcpStream>>>),
    /// A handle to a real, listening TCP server socket (`listen(port)` —
    /// see `Ty::TcpListener`'s doc comment). `Mutex<Option<..>>`, same
    /// shape as `Value::Tcp`: `stop` needs to `.take()` it out exactly
    /// once. `accept` reads through the `Mutex` without taking, since
    /// the listener stays usable across many `accept` calls.
    TcpListener(Arc<Mutex<Option<std::net::TcpListener>>>),
    /// A handle to a real, local file (`open(path, mode)` — see
    /// `Ty::File`'s doc comment). `Mutex<Option<..>>`, same shape and
    /// reason as `Value::Tcp`: `stop` needs to `.take()` the file out
    /// exactly once. Same as `Value::Tcp`, no custom `Drop` is needed —
    /// a `std::fs::File` closes its own descriptor on drop with no help
    /// required.
    File(Arc<Mutex<Option<std::fs::File>>>),
    /// An already-parsed JSON document or sub-document (`json_parse`,
    /// and every navigation builtin's own result — see `Ty::Json`'s doc
    /// comment). `Arc`, not owned directly, for the same cheap-clone-on-
    /// read reason `Value::Str`'s `Arc<str>` already documents — not
    /// affine, freely read many times.
    Json(Arc<JsonDoc>),
    /// A handle to a real, open database connection (`db_connect(path)` —
    /// see `Ty::Db`'s doc comment). `Mutex<Option<..>>`, same shape and
    /// reason as `Value::Tcp`/`Value::File`: `stop` needs to `.take()` the
    /// connection out exactly once. No custom `Drop` is needed — a
    /// `rusqlite::Connection`/`postgres::Client` (`dbconn::DbConn`, chosen
    /// by `db_connect`'s connection-string scheme — `dbconn.rs`'s module
    /// doc) each close their own connection on drop with no help required,
    /// the same as every other affine handle here.
    Db(Arc<Mutex<Option<crate::dbconn::DbConn>>>),
    /// A handle to a real, open message-queue connection (`mq_connect(host,
    /// port)` — see `Ty::Mq`'s doc comment). Same `Mutex<Option<..>>` shape
    /// and reason as `Value::Db`. Backed by Redis (`redis` crate); no
    /// custom `Drop` needed — a `redis::Connection` closes its own socket
    /// on drop. Wrapped in `MqConn` because `redis::Connection` itself
    /// doesn't implement `Debug` (unlike `rusqlite::Connection`, which
    /// does, so `Value::Db` needs no such wrapper).
    Mq(Arc<Mutex<Option<MqConn>>>),
    /// A `struct` value (Row 11) — the declared struct's name plus its
    /// field values, positional, in declaration order (construction is
    /// positional-only — `ast::StructDecl`'s doc comment). `Arc<[Value]>`,
    /// not `Vec<Value>`, same cheap-clone-on-read reasoning `Value::
    /// Vector`'s doc comment already gives: `Env::get` clones every
    /// `Value` on read, and a struct is non-affine unless one of its
    /// fields is (`ownership.rs`'s `TypeRegistry::is_affine`), so most
    /// reads of a struct-typed binding are a real, if rare, deep clone —
    /// `Arc` at least makes the *handle* itself a refcount bump. The name
    /// is carried for `ty_name`/`render`/`check_ty`'s own diagnostics;
    /// nothing here re-derives it by looking the value up in `Program`.
    Struct(Arc<str>, Arc<[Value]>),
    /// An `enum` value (Row 11) — the declared enum's name, which variant
    /// this value actually is, and that variant's payload, positional
    /// (same shape as `Value::Struct`, plus the variant tag `match`
    /// dispatches on).
    Enum(Arc<str>, Arc<str>, Arc<[Value]>),
    /// A first-class function value (`Ty::Fn`) — just the target
    /// top-level function's name, `Arc<str>` for the same cheap-clone-on-
    /// read reason `Value::Str` already documents (there's no closure
    /// environment to carry — this language has none). Produced by
    /// naming a plain top-level fn directly, or by a successful
    /// `Expr::Acquire`; never by any other path (`typeck.rs` proves a
    /// `requires`-gated function's name can't reach here any other way).
    Fn(Arc<str>),
}

/// A spawned computation's raw join handle. Its own alias, not just inline
/// in `Value::Thread`, purely to keep clippy's `type_complexity` lint (and
/// human readers) from tripping over four levels of generic nesting.
type ThreadHandle = std::thread::JoinHandle<Result<Value, RuntimeError>>;

/// The shared state behind a `Value::Channel` — SANDBOXING.md's "one
/// primitive, multiple transports" decision, made real: `send`/`recv`'s
/// language-level meaning never changes, only what backs them does.
///
/// - `InMemory` is transport #1, unchanged from before layer 2: a plain
///   `Mutex`-guarded FIFO queue plus a `Condvar` so `recv` blocks until
///   `send` wakes it, rather than spin-polling. This is what every `chan`
///   expression creates, always — nothing chooses the cross-process
///   transport up front.
/// - `PendingListener`/`Socket` are transport #2, added for layer 2: a
///   real Unix domain socket, used when a `chan`-typed value crosses into
///   a `sandbox` argument (see `Interpreter::spawn_sandbox`). A channel
///   only ever *becomes* socket-backed at that moment — `prepare_for_sandbox`
///   binds a fresh listener and transitions `InMemory` straight to
///   `PendingListener`; `accept()` itself is deferred to the first real
///   `send`/`recv` (`ensure_connected`), so spawning a sandbox with a
///   `chan` argument doesn't itself become a blocking call. `Socket`'s
///   own `send`/`recv` can genuinely fail (a real `io::Error` — the peer
///   process crashed, the pipe broke) in a way `InMemory`'s never could;
///   see `ErrorKind::ChannelIoError` for where that surfaces.
///
/// **Known, deliberate scope limit:** `prepare_for_sandbox` requires the
/// channel's `InMemory` queue to be empty at the moment it's handed to
/// `sandbox` — a channel created and used purely in-process for a while
/// *first*, then later passed to a sandbox, would need those already-
/// queued messages replayed onto the new socket, which this layer
/// doesn't attempt (SANDBOXING.md layer 2 is scoped to "create a channel
/// specifically to hand to a sandbox," not "reuse an arbitrary existing
/// one"). Returns a clear runtime error rather than silently dropping
/// messages if this is violated.
#[derive(Debug)]
pub struct ChannelInner {
    state: Mutex<TransportState>,
    not_empty: Condvar,
}

// The sandbox channel's cross-process transport (SANDBOXING.md layer 2):
// a Unix domain socket bound at a temp-file path on Unix, since std only
// exposes `std::os::unix::net::{UnixListener, UnixStream}` under
// `cfg(unix)` — there's no portable stdlib abstraction over "local
// socket" that also covers Windows (Windows *does* support `AF_UNIX` at
// the OS level since 10 1803+, but Rust's std doesn't wrap it for the
// `windows` target). `ChanListener`/`ChanStream` swap in a TCP listener
// bound to `127.0.0.1:0` (an OS-assigned ephemeral port) on Windows
// instead — same "one accept, exactly one child" shape, just addressed
// by `host:port` instead of a filesystem path. `bind_chan_listener`/
// `connect_chan` are the only platform-conditional surface; every other
// method on `ChannelInner` (and `write_value`/`read_value`, generic over
// `Read`/`Write`) is platform-agnostic.
#[cfg(unix)]
type ChanListener = std::os::unix::net::UnixListener;
#[cfg(unix)]
type ChanStream = std::os::unix::net::UnixStream;
#[cfg(windows)]
type ChanListener = std::net::TcpListener;
#[cfg(windows)]
type ChanStream = std::net::TcpStream;

/// Binds a fresh listener for a `chan` about to be handed to `sandbox`,
/// returning it alongside the address string the spawned child should be
/// given (via argv) to connect back — a filesystem path on Unix, a
/// `127.0.0.1:<port>` string on Windows. Both are just opaque strings as
/// far as the caller (`prepare_for_sandbox`) and `connect_chan` below are
/// concerned.
#[cfg(unix)]
fn bind_chan_listener() -> std::io::Result<(ChanListener, std::path::PathBuf)> {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut path = std::env::temp_dir();
    path.push(format!("nirdosha_sandbox_chan_{}_{n}.sock", std::process::id()));
    let listener = ChanListener::bind(&path)?;
    Ok((listener, path))
}
#[cfg(windows)]
fn bind_chan_listener() -> std::io::Result<(ChanListener, std::path::PathBuf)> {
    let listener = ChanListener::bind(("127.0.0.1", 0))?;
    let port = listener.local_addr()?.port();
    Ok((listener, std::path::PathBuf::from(format!("127.0.0.1:{port}"))))
}

/// The child side's connect, matching whichever address form
/// `bind_chan_listener` handed the parent to pass along (a Unix socket
/// path, or a `host:port` string on Windows).
#[cfg(unix)]
pub fn connect_chan(addr: &std::path::Path) -> std::io::Result<ChanStream> {
    ChanStream::connect(addr)
}
#[cfg(windows)]
pub fn connect_chan(addr: &std::path::Path) -> std::io::Result<ChanStream> {
    // `addr` is always valid UTF-8 here -- `bind_chan_listener` built it
    // from a `format!("{host}:{port}")` string, never arbitrary OS bytes.
    let addr = addr.to_str().ok_or_else(|| std::io::Error::other("sandbox channel address is not valid UTF-8"))?;
    ChanStream::connect(addr)
}

#[derive(Debug)]
enum TransportState {
    InMemory(VecDeque<Value>),
    PendingListener(ChanListener, std::path::PathBuf),
    Socket(ChanStream),
}

impl ChannelInner {
    fn new() -> Self {
        ChannelInner { state: Mutex::new(TransportState::InMemory(VecDeque::new())), not_empty: Condvar::new() }
    }

    /// The child side's constructor: a `Value::Channel` built directly
    /// from an already-connected socket, no `InMemory`/`PendingListener`
    /// detour — the child never creates a channel via `chan`, it's always
    /// handed one that's already meant to cross a process boundary.
    pub fn from_socket(stream: ChanStream) -> Self {
        ChannelInner { state: Mutex::new(TransportState::Socket(stream)), not_empty: Condvar::new() }
    }

    /// Binds a fresh Unix domain socket and transitions this channel from
    /// `InMemory` to `PendingListener`, returning the socket's path (for
    /// the caller to pass to the spawned child via argv, the same way the
    /// source file's temp path already is). Doesn't `accept()` — that's
    /// deferred to first use (see the type's doc comment) — so this never
    /// blocks.
    fn prepare_for_sandbox(&self) -> std::io::Result<std::path::PathBuf> {
        let mut state = self.state.lock().unwrap();
        match &*state {
            TransportState::InMemory(queue) if !queue.is_empty() => {
                return Err(std::io::Error::other(
                    "a `chan` with already-queued messages can't be passed to `sandbox` yet \
                     (SANDBOXING.md layer 2 only supports channels created fresh for the sandbox)",
                ));
            }
            TransportState::InMemory(_) => {}
            TransportState::PendingListener(_, _) | TransportState::Socket(_) => {
                return Err(std::io::Error::other(
                    "this `chan` was already passed to a `sandbox` once — a channel can only \
                     cross into one sandboxed process",
                ));
            }
        }
        let (listener, addr) = bind_chan_listener()?;
        *state = TransportState::PendingListener(listener, addr.clone());
        Ok(addr)
    }

    /// If still `PendingListener`, blocks until the child connects, then
    /// transitions to `Socket` — the one place this file's "sandbox
    /// spawning never blocks, only send/recv do" rule gets enforced for
    /// the channel transport specifically. On Unix, unlinks the socket's
    /// filesystem path immediately on a successful accept: once
    /// connected, a Unix domain socket doesn't need its path anymore, and
    /// nothing else will ever connect to it (exactly one child, exactly
    /// one accept). No such cleanup on Windows — `addr` there is a
    /// `host:port` string, not a filesystem path.
    fn ensure_connected(state: &mut TransportState) -> std::io::Result<()> {
        if let TransportState::PendingListener(listener, _addr) = state {
            let (stream, _peer) = listener.accept()?;
            #[cfg(unix)]
            let _ = std::fs::remove_file(_addr);
            *state = TransportState::Socket(stream);
        }
        Ok(())
    }

    fn send(&self, v: Value) -> std::io::Result<()> {
        let mut state = self.state.lock().unwrap();
        Self::ensure_connected(&mut state)?;
        match &mut *state {
            TransportState::InMemory(queue) => {
                queue.push_back(v);
                drop(state);
                self.not_empty.notify_one();
                Ok(())
            }
            TransportState::Socket(stream) => write_value(stream, &v),
            TransportState::PendingListener(..) => unreachable!("ensure_connected already resolved this"),
        }
    }

    /// A cheap, best-effort peek used only to decide whether a blocking
    /// `recv` is even eligible for `DeadlockRegistry`'s in-process check
    /// — a `Socket`-backed channel (crossed into a `sandbox`) can always,
    /// in principle, still be woken by that *external* process regardless
    /// of what any Nirdosha thread is doing, so treating it as part of
    /// the in-process whole-universe check would be a real false
    /// positive, not just imprecision. Not synchronized with the
    /// subsequent `recv()` call (state could transition in between) —
    /// the same narrow, pre-existing race `prepare_for_sandbox`'s own doc
    /// comment already accepts, not made meaningfully worse here.
    fn is_in_memory(&self) -> bool {
        matches!(&*self.state.lock().unwrap(), TransportState::InMemory(_))
    }

    /// Non-blocking: pops a value if one's already queued, otherwise
    /// returns `None` immediately without ever touching the `Condvar`.
    /// The deadlock-detection fast path (`Expr::Recv`'s handler) —
    /// `send(c, v); recv(c)` in the *same* thread must never be flagged
    /// as blocked at all, since it demonstrably isn't (the value's
    /// already sitting in the queue); only calling `check_and_register`
    /// when a real wait is actually about to happen is what keeps that
    /// sound. Only meaningful for `InMemory` — a socket-backed channel
    /// doesn't get a non-blocking peek here (not needed: `Expr::Recv`
    /// only calls this after `is_in_memory()` already confirmed the
    /// transport).
    fn try_recv(&self) -> Option<Value> {
        let mut state = self.state.lock().unwrap();
        match &mut *state {
            TransportState::InMemory(queue) => queue.pop_front(),
            _ => None,
        }
    }

    fn recv(&self) -> std::io::Result<Value> {
        let mut state = self.state.lock().unwrap();
        loop {
            Self::ensure_connected(&mut state)?;
            match &mut *state {
                TransportState::InMemory(queue) => {
                    if let Some(v) = queue.pop_front() {
                        return Ok(v);
                    }
                    state = self.not_empty.wait(state).unwrap();
                }
                TransportState::Socket(stream) => return read_value(stream),
                TransportState::PendingListener(..) => unreachable!("ensure_connected already resolved this"),
            }
        }
    }

    /// Same as `recv`, but gives up (returning `Ok(None)`) after `timeout`
    /// instead of waiting indefinitely — `Expr::Recv`'s deadlock-detection
    /// poll loop uses this so a thread blocked here periodically comes
    /// back up for air and re-checks `DeadlockRegistry` for a *newly*
    /// formed deadlock, not just the one snapshot taken the instant it
    /// first blocked. Only ever called on the `InMemory` transport (see
    /// `Expr::Recv`'s own `is_in_memory` gate) — `ensure_connected`/
    /// `Socket` are unreachable here in practice, kept only so this
    /// doesn't have to duplicate `recv`'s own transport `match`.
    fn recv_timeout(&self, timeout: std::time::Duration) -> std::io::Result<Option<Value>> {
        let mut state = self.state.lock().unwrap();
        Self::ensure_connected(&mut state)?;
        match &mut *state {
            TransportState::InMemory(queue) => {
                if let Some(v) = queue.pop_front() {
                    return Ok(Some(v));
                }
                let (mut state, _timeout_result) = self.not_empty.wait_timeout(state, timeout).unwrap();
                if let TransportState::InMemory(queue) = &mut *state {
                    Ok(queue.pop_front())
                } else {
                    Ok(None)
                }
            }
            TransportState::Socket(stream) => read_value(stream).map(Some),
            TransportState::PendingListener(..) => unreachable!("ensure_connected already resolved this"),
        }
    }
}

impl Drop for ChannelInner {
    /// The only cleanup a channel ever needs beyond Rust's own (closing
    /// the socket fd, which `UnixListener`/`UnixStream`'s own `Drop`
    /// already does): a *bound but never-accepted* listener's socket file
    /// would otherwise leak on disk — `ensure_connected` already unlinks
    /// it on the success path, so this only ever fires for a channel that
    /// was prepared for a sandbox but never actually used.
    fn drop(&mut self) {
        // Windows' `addr` is a `host:port` string, not a filesystem path
        // — nothing to unlink there, so this cleanup is Unix-only (see
        // `ensure_connected`'s doc comment for the same split).
        #[cfg(unix)]
        {
            let state = match self.state.get_mut() {
                Ok(s) => s,
                Err(poisoned) => poisoned.into_inner(),
            };
            if let TransportState::PendingListener(_, path) = state {
                let _ = std::fs::remove_file(path);
            }
        }
    }
}

/// Real, sound runtime deadlock detection for `chan`/`thread` —
/// README.md's "no deadlock is expressible" claim was only ever true for
/// *lock-ordering* deadlock (there's no mutex primitive in the language
/// at all, so nothing to acquire out of order). A `recv` with no
/// corresponding `send`, or two threads mutually `join`-ing each other,
/// both compiled and typechecked fine and hung the process forever, with
/// zero diagnostic, before this existed — verified directly: `fn main()
/// -> i64 { let c: chan i64 = chan; return recv(c) }` typechecks and then
/// never returns. This doesn't make deadlock *inexpressible* (a `while
/// true {}` busy-loop still is one, and this can't see a `recv` blocked
/// forever while some *other*, unrelated thread stays busy doing
/// something else entirely — seen below) — it turns every deadlock this
/// *can* prove is permanent into a clean `ErrorKind::Deadlock` trap
/// instead of a silent hang, the same "never guess, fail loud" shape
/// `TransactCommitPending`/`WorkflowActionPending` already use for a
/// different class of permanently-stuck state.
///
/// **What's actually proven, and why it's sound (never a false
/// positive):**
/// - `join`-cycles are checked *precisely*: `join`'s argument names
///   exactly one target thread, so a real wait-for graph over `Join`
///   edges is exact, not an approximation. A → B → A (or any longer
///   cycle) is a mathematical certainty of permanent deadlock the
///   instant the closing edge is added — nothing else in the program
///   could ever break it, so reporting it immediately is sound.
/// - `recv` gets a coarser fallback, for a real reason: a `chan` handle
///   is freely copyable (`SANDBOXING.md`), so precisely *which* thread(s)
///   could still legally `send` on a given channel is, in general, an
///   alias-analysis problem this doesn't attempt. Instead: if *every*
///   thread this registry knows about (`main` plus every currently-live
///   `spawn`ed thread) is *simultaneously* blocked on `recv`/`join`, none
///   of them can ever run code again, which means none of them can ever
///   call `send` — a real, sound conclusion (the same "all goroutines are
///   asleep" condition Go's own runtime detector checks), just reached
///   without needing to know *which* thread would have sent.
/// - **The disclosed gap**: a `recv` blocked forever while some *other*
///   live thread stays busy on unrelated work (never touches this
///   channel, never finishes) is invisible to the whole-universe check —
///   that thread isn't blocked, so the universe isn't "all blocked," even
///   though the `recv` in question is, in fact, permanently stuck. Precise
///   detection of that case needs real points-to tracking of channel
///   handles, not built here. Named so it isn't mistaken for solved.
///
/// One registry per top-level program run — shared into every
/// `Expr::Spawn`-created child's own `Interpreter` via `Arc::clone`, the
/// same pattern `tracer`/`sandbox_exe` already use — never across two
/// independent runs (e.g. two concurrent `nirdosha serve` requests), each
/// of which gets its own fresh registry from `Interpreter::new`.
struct DeadlockRegistry {
    /// **Not** captured at registry construction — `Interpreter::new`
    /// (and so `DeadlockRegistry::new`) often runs on a throwaway setup
    /// thread, not the thread that actually executes `.nir` code:
    /// `run_main_on_big_stack`/`call_named_on_big_stack` construct the
    /// interpreter on the *caller's* thread but then move it into a
    /// freshly `std::thread::spawn`ed worker with a bigger stack
    /// (`run_on_big_stack`) before ever calling `main`/the named
    /// function — verified the hard way: capturing this at construction
    /// made a plain `recv(chan)`-with-no-`send` program still hang
    /// forever, silently, because `std::thread::current().id()` at
    /// `new()` time never matched the id `check_and_register` later saw.
    /// Set instead by `Interpreter::run_main`/`call_named` — the two
    /// entry points a genuine top-level run always goes through and
    /// `Expr::Spawn`'s own handler never does (it calls the lower-level
    /// `Interpreter::call` directly) — the first time either actually
    /// runs, on whichever real OS thread that turns out to be.
    main_thread_id: std::sync::OnceLock<std::thread::ThreadId>,
    /// Spawned threads currently alive — inserted at the top of
    /// `Expr::Spawn`'s closure, removed once it returns. Doesn't include
    /// `main_thread_id`; the whole-universe check adds that separately.
    live_spawned: Mutex<HashSet<std::thread::ThreadId>>,
    /// Every thread (main or spawned) currently blocked on `recv`/`join`,
    /// and what it's waiting on.
    blocked: Mutex<HashMap<std::thread::ThreadId, BlockedOn>>,
}

/// How often a thread blocked on `recv`/`join` comes back up for air to
/// re-check `DeadlockRegistry` for a deadlock that's formed *since* it
/// first blocked — not just the one snapshot taken the instant it
/// committed to waiting. Verified necessary, not just theoretical: a
/// single check-then-block-forever design missed exactly this case (two
/// threads deadlocked on each other while a third was still blocking on
/// a `recv` no one would ever satisfy — by the time the third thread's
/// *own* wait began, the first two hadn't finished unwinding yet, so its
/// one-shot check legitimately saw "not everyone's blocked yet" and
/// committed to a real, otherwise-unstoppable wait). Small enough that a
/// real deadlock surfaces promptly; large enough that polling overhead on
/// an ordinary, long-lived `recv`/`join` is not meaningfully different
/// from a true blocking wait.
const DEADLOCK_POLL_INTERVAL: std::time::Duration = std::time::Duration::from_millis(10);

#[derive(Debug, Clone, Copy)]
enum BlockedOn {
    /// Blocked in `recv` — *which* channel isn't tracked; see
    /// `DeadlockRegistry`'s own doc comment for why (alias analysis).
    Recv,
    /// Blocked joining a specific thread — a precise edge, unlike `Recv`.
    Join(std::thread::ThreadId),
}

impl DeadlockRegistry {
    fn new() -> Self {
        DeadlockRegistry {
            main_thread_id: std::sync::OnceLock::new(),
            live_spawned: Mutex::new(HashSet::new()),
            blocked: Mutex::new(HashMap::new()),
        }
    }

    /// Idempotent — safe to call more than once (e.g. `run_main` calling
    /// `self.call(...)`, which never itself calls this). Only the first
    /// call on any given registry actually sets anything.
    fn mark_main_thread(&self) {
        self.main_thread_id.get_or_init(|| std::thread::current().id());
    }

    /// Registers `id` as live — called by the *parent*, synchronously,
    /// immediately after `std::thread::spawn` returns, using the new
    /// `JoinHandle`'s own `.thread().id()`. **Must not** be done by
    /// having the *child* register itself as its first action instead —
    /// verified the hard way: `std::thread::spawn` returning in the
    /// parent doesn't mean the child's closure has started running yet,
    /// so a parent that immediately calls `recv`/`join` after spawning
    /// can (and, under real scheduling, sometimes does) run its own
    /// `check_and_register` before the child ever reaches a
    /// self-registering line — the exact race that made ordinary,
    /// correct `spawn` + `recv` programs intermittently report a false
    /// deadlock. A `JoinHandle`'s `ThreadId` is assigned at OS thread-
    /// creation time, available synchronously the instant `spawn`
    /// returns, which is what makes registering here race-free.
    fn spawn_started(&self, id: std::thread::ThreadId) {
        self.live_spawned.lock().unwrap().insert(id);
    }

    fn spawn_finished(&self) {
        self.live_spawned.lock().unwrap().remove(&std::thread::current().id());
    }

    /// Registers the current thread as about to block on `on`, then
    /// checks whether that provably makes the situation permanent (see
    /// the two checks in this type's own doc comment). On `Err`, the
    /// current thread's own registration is removed again (it's not
    /// actually going to block — it's about to return this error
    /// instead) and the caller must **not** proceed to the real,
    /// potentially-blocking call. Any *other* thread named in the
    /// returned message is left registered as blocked — accurate, not
    /// stale, since a thread genuinely inside an unresolvable `recv`/
    /// `join` wait will never call this registry again to say otherwise.
    /// On `Ok(())`, the registration stays in place and the caller must
    /// call `unblock` once its real wait returns.
    fn check_and_register(&self, on: BlockedOn) -> Result<(), String> {
        let me = std::thread::current().id();
        let mut blocked = self.blocked.lock().unwrap();
        blocked.insert(me, on);

        // Precise check: does `me`'s new `Join` edge close a cycle?
        // Sound to report immediately — the newest edge is the only way
        // a cycle could have just formed, and once one exists nothing in
        // the program can ever break it.
        let mut path = vec![me];
        let mut visited: HashSet<_> = std::iter::once(me).collect();
        let mut cursor = me;
        let cycle = loop {
            match blocked.get(&cursor).copied() {
                Some(BlockedOn::Join(target)) => {
                    if !visited.insert(target) {
                        let start = path.iter().position(|&t| t == target).unwrap();
                        break Some(path[start..].to_vec());
                    }
                    path.push(target);
                    cursor = target;
                }
                _ => break None,
            }
        };
        if let Some(cycle) = cycle {
            blocked.remove(&me);
            let chain = cycle.iter().map(|t| format!("{t:?}")).collect::<Vec<_>>().join(" -> ");
            return Err(format!(
                "deadlock: {} thread(s) waiting on each other via `join`, in a cycle: {chain} -> {:?}",
                cycle.len(),
                cycle[0]
            ));
        }

        // Coarse fallback: is *everyone* this registry knows about now
        // blocked? See the type's own doc comment for why this is sound
        // and what it deliberately doesn't catch.
        let mut universe: HashSet<_> = self.live_spawned.lock().unwrap().iter().copied().collect();
        // Falls back to treating `me` as main if somehow never set (should
        // never happen for a real top-level run — see this field's own
        // doc comment) — sound either way: worst case this just narrows
        // the universe by one thread that was going to be `me` regardless.
        universe.insert(*self.main_thread_id.get().unwrap_or(&me));
        if universe.iter().all(|t| blocked.contains_key(t)) {
            blocked.remove(&me);
            return Err(format!(
                "deadlock: all {} live thread(s) are permanently blocked on `recv`/`join`, none left running to ever unblock any of them",
                universe.len()
            ));
        }

        Ok(())
    }

    fn unblock(&self) {
        self.blocked.lock().unwrap().remove(&std::thread::current().id());
    }
}

/// The wire format layer 2 needs and no more: a one-byte tag plus a
/// fixed-size payload for exactly the two scalar shapes `typeck.rs`
/// allows to cross into a `sandbox` argument (`Ty::Channel(inner)` where
/// `inner` is an integer type or `bool` — see `SandboxArgMustBeScalar`).
/// `Value::Int` is always `i64` internally regardless of the *declared*
/// width (`i8`..`usize`), so one integer encoding covers every integer
/// type; there's no narrower-type tag to get wrong. Not the general,
/// formally-checked serialization boundary SANDBOXING.md's layer 3 is —
/// this only has to be correct for the types layer 1 already proved safe
/// to move across a process boundary at all.
fn write_value(stream: &mut impl std::io::Write, v: &Value) -> std::io::Result<()> {
    match v {
        Value::Int(n) => {
            let mut buf = [0u8; 9];
            buf[0] = 0;
            buf[1..9].copy_from_slice(&n.to_le_bytes());
            stream.write_all(&buf)
        }
        Value::Bool(b) => stream.write_all(&[1, u8::from(*b)]),
        other => unreachable!("typeck.rs only allows scalar payloads across a sandbox channel, got {other:?}"),
    }
}

fn read_value(stream: &mut impl std::io::Read) -> std::io::Result<Value> {
    let mut tag = [0u8; 1];
    let result = stream.read_exact(&mut tag).and_then(|()| match tag[0] {
        0 => {
            let mut buf = [0u8; 8];
            stream.read_exact(&mut buf)?;
            Ok(Value::Int(i64::from_le_bytes(buf)))
        }
        1 => {
            let mut buf = [0u8; 1];
            stream.read_exact(&mut buf)?;
            Ok(Value::Bool(buf[0] != 0))
        }
        other => Err(std::io::Error::other(format!("corrupt sandbox channel wire tag {other}"))),
    });
    // The overwhelmingly common cause of an EOF here, by far, is the
    // *far* end of the socket closing -- the sandboxed process exited or
    // was killed before sending anything. Rust's own message ("failed to
    // fill whole buffer") is technically accurate but useless to a user;
    // this is the one real place layer 2's own error family (SANDBOXING.md's
    // "channel-closed" case, promised back at the Decisions section) earns
    // its keep over a generic io::Error passthrough.
    result.map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            std::io::Error::other(
                "the sandboxed process closed this channel (exited or was killed) \
                 before sending a value",
            )
        } else {
            e
        }
    })
}

/// `tcp`'s wire format: raw UTF-8 bytes, no tag/framing at all — unlike
/// `write_value`/`read_value`, the peer here is never assumed to be
/// another Nirdosha interpreter that agrees on a wire protocol (that's
/// the whole reason `tcp` exists: to talk to *anything*, an arbitrary
/// service speaking its own protocol, e.g. HTTP text over the wire).
fn write_tcp(stream: &mut std::net::TcpStream, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    stream.write_all(text.as_bytes())
}

/// One read syscall, not a loop until some message boundary — there is
/// no message boundary to look for in an arbitrary external protocol.
/// This returns whatever bytes are available right now (up to 64KiB),
/// which is exactly "one chunk," not "one complete response": a reply
/// larger than one read (or one that arrives in several TCP segments) is
/// genuinely not fully reassembled by a single `recv` call. Honest,
/// deliberate first-cut scope (no string concatenation exists yet to
/// stitch multiple chunks together either) — not a bug to silently paper
/// over, see SANDBOXING.md.
fn read_tcp(stream: &mut std::net::TcpStream) -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = [0u8; 65536];
    let n = stream.read(&mut buf)?;
    if n == 0 {
        return Err(std::io::Error::other("the tcp connection was closed by the peer"));
    }
    String::from_utf8(buf[..n].to_vec())
        .map_err(|_| std::io::Error::other("tcp connection received bytes that were not valid UTF-8"))
}

/// `send(file, s)`'s implementation — reuses `Expr::Send`, same as `tcp`.
fn write_file(file: &mut std::fs::File, text: &str) -> std::io::Result<()> {
    use std::io::Write;
    file.write_all(text.as_bytes())
}

/// `recv(file)`'s implementation — one read syscall into a fixed 64KiB
/// buffer, same shape as `read_tcp`, but with the opposite EOF
/// convention: a live TCP peer closing the connection is a genuine error
/// (`read_tcp`'s `n == 0` check), while a file simply running out of
/// bytes to read is the normal, expected way a file ends. `recv` on a
/// `file` therefore returns `Ok("")` at EOF, not an error — see
/// `PROTOLANG_PORT.md`'s file I/O design for this exact convention.
fn read_file(file: &mut std::fs::File) -> std::io::Result<String> {
    use std::io::Read;
    let mut buf = [0u8; 65536];
    let n = file.read(&mut buf)?;
    String::from_utf8(buf[..n].to_vec())
        .map_err(|_| std::io::Error::other("file contained bytes that were not valid UTF-8"))
}

/// A sandboxed process, plus the temp file its source was written to
/// (removed once the process is known to have exited — not before, since
/// the child re-reads that file at its own startup and deleting it out
/// from under a not-yet-started child would be a real, if narrow, race).
///
/// **`stop` is idempotent by construction, not by a tracked flag.**
/// `Expr::StopSandbox` calls `stop()` explicitly and then this value goes
/// out of scope, running `Drop::drop` — which calls `stop()` again. A
/// second `try_wait`/`kill`/`wait`/`remove_file` on an already-reaped,
/// already-deleted target is a harmless OS-level no-op (ignored errors),
/// not a bug: simpler than threading an extra "already stopped" bool
/// through a type that can't be partially moved-out-of once it has a
/// `Drop` impl.
pub struct SandboxChild {
    child: std::process::Child,
    tmp_source_path: std::path::PathBuf,
}

impl std::fmt::Debug for SandboxChild {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "SandboxChild(pid={})", self.child.id())
    }
}

impl SandboxChild {
    /// The OS process id — exposed for tests that need to independently
    /// verify (e.g. via `kill -0`) that dropping a handle without `stop`
    /// really did terminate the process, not just that this file's own
    /// bookkeeping thinks it did.
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Kills the process if it's still running, waits on it (reaping it
    /// either way — a `Child` that's never `wait()`-ed leaks a zombie
    /// entry in the OS process table even after it exits), and cleans up
    /// its temp source file. Returns the OS exit code, or `-1` if this
    /// call is the one that killed it, or if the wait itself failed.
    ///
    /// The "did we kill it" case is tracked explicitly (`killed_by_us`)
    /// rather than inferred from `status.code()`, because that inference
    /// is platform-dependent: on Unix, `kill()` sends `SIGKILL`, and a
    /// signal-terminated process has no exit code at all
    /// (`status.code()` is `None`), so falling back to `-1` happened to
    /// work. On Windows, `Child::kill()` is `TerminateProcess(handle,
    /// 1)` — a *real* exit code of `1`, indistinguishable from a process
    /// that legitimately called `exit(1)` if you only look at
    /// `status.code()`. Found on real Windows CI: this returned `Int(1)`
    /// instead of the documented `Int(-1)` for a process this same call
    /// killed.
    fn stop(&mut self) -> i64 {
        let killed_by_us = if self.child.try_wait().ok().flatten().is_none() {
            self.child.kill().is_ok()
        } else {
            false
        };
        let code = match self.child.wait() {
            Ok(status) if killed_by_us => {
                let _ = status; // exit code is meaningless when we killed it
                -1
            }
            Ok(status) => status.code().unwrap_or(-1) as i64,
            Err(_) => -1,
        };
        let _ = std::fs::remove_file(&self.tmp_source_path);
        code
    }
}

impl Drop for SandboxChild {
    fn drop(&mut self) {
        let _ = self.stop();
    }
}

/// Manual, not derived: `JoinHandle` has no `PartialEq`, so `Value` can't
/// derive it once `Thread` exists. Two thread handles are equal only if
/// they're literally the same handle (`Arc::ptr_eq`) — there's no
/// sensible *value* equality for "is this running computation the same
/// as that one" otherwise.
impl PartialEq for Value {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Value::Int(a), Value::Int(b)) => a == b,
            (Value::Float(a), Value::Float(b)) => a == b,
            (Value::Dec128(a), Value::Dec128(b)) => a == b,
            (Value::Vector(a), Value::Vector(b)) => a == b,
            (Value::Matrix(a, ar, ac), Value::Matrix(b, br, bc)) => ar == br && ac == bc && a == b,
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Boxed(a), Value::Boxed(b)) => a == b,
            (Value::Ref(a), Value::Ref(b)) => a == b,
            (Value::Thread(a), Value::Thread(b)) => Arc::ptr_eq(a, b),
            (Value::Channel(a), Value::Channel(b)) => Arc::ptr_eq(a, b),
            (Value::Sandbox(a), Value::Sandbox(b)) => Arc::ptr_eq(a, b),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Tcp(a), Value::Tcp(b)) => Arc::ptr_eq(a, b),
            (Value::TcpListener(a), Value::TcpListener(b)) => Arc::ptr_eq(a, b),
            (Value::Json(a), Value::Json(b)) => a == b,
            (Value::Db(a), Value::Db(b)) => Arc::ptr_eq(a, b),
            (Value::Struct(an, af), Value::Struct(bn, bf)) => an == bn && af == bf,
            (Value::Enum(an, av, af), Value::Enum(bn, bv, bf)) => an == bn && av == bv && af == bf,
            _ => false,
        }
    }
}

/// Whether a literal-pattern `match` arm's pattern
/// (`typeck.rs::check_literal_match` already proved `value` is a
/// `str`/`i64`/`bool` and every non-wildcard pattern agrees with it in
/// type) matches `value` -- `Value`'s own `PartialEq` above does the
/// real comparison, `Wildcard` always matches.
fn literal_pattern_matches(pattern: &Option<LiteralPattern>, value: &Value) -> bool {
    match pattern {
        Some(LiteralPattern::Wildcard) => true,
        Some(LiteralPattern::Str(s)) => matches!(value, Value::Str(v) if v.as_ref() == s.as_str()),
        Some(LiteralPattern::Int(n)) => matches!(value, Value::Int(v) if v == n),
        Some(LiteralPattern::Bool(b)) => matches!(value, Value::Bool(v) if v == b),
        None => false,
    }
}

impl Value {
    fn ty_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "f64",
            Value::Dec128(_) => "dec128",
            Value::Vector(_) => "vector",
            Value::Matrix(..) => "matrix",
            Value::Bool(_) => "bool",
            Value::Unit => "unit",
            Value::Boxed(_) => "box",
            Value::Ref(_) => "ref",
            Value::Thread(_) => "thread",
            Value::Channel(_) => "chan",
            Value::Sandbox(_) => "sandbox",
            Value::Str(_) => "str",
            Value::Tcp(_) => "tcp",
            Value::TcpListener(_) => "tcp_listener",
            Value::File(_) => "file",
            Value::Json(_) => "json",
            Value::Db(_) => "db",
            Value::Mq(_) => "mq",
            // A generic tag, not the real declared name — every real call
            // site that needs the *actual* struct/enum name already has
            // the `Ty::Named`/`Value::Struct`/`Value::Enum` name field
            // directly at hand and doesn't need to go through this
            // generic method; this exists only so `ty_name` stays total.
            Value::Struct(..) => "struct",
            Value::Enum(..) => "enum",
            Value::Fn(_) => "fn",
        }
    }

    fn render(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(n) => n.to_string(),
            Value::Dec128(d) => d.to_string(),
            Value::Vector(elems) => {
                format!("[{}]", elems.iter().map(Value::render).collect::<Vec<_>>().join(", "))
            }
            Value::Matrix(elems, rows, cols) => {
                let rows_rendered: Vec<String> = (0..*rows)
                    .map(|i| {
                        let row = &elems[i * cols..(i + 1) * cols];
                        format!("[{}]", row.iter().map(Value::render).collect::<Vec<_>>().join(", "))
                    })
                    .collect();
                format!("[{}]", rows_rendered.join(", "))
            }
            Value::Bool(b) => b.to_string(),
            Value::Unit => "()".to_string(),
            Value::Boxed(inner) => format!("box({})", inner.render()),
            Value::Ref(inner) => format!("&{}", inner.render()),
            Value::Thread(_) => "thread(..)".to_string(),
            Value::Channel(_) => "chan(..)".to_string(),
            Value::Sandbox(_) => "sandbox(..)".to_string(),
            Value::Str(s) => s.to_string(),
            Value::Tcp(_) => "tcp(..)".to_string(),
            Value::TcpListener(_) => "tcp_listener(..)".to_string(),
            Value::File(_) => "file(..)".to_string(),
            Value::Json(doc) => format!("json({doc})"),
            Value::Db(_) => "db(..)".to_string(),
            Value::Mq(_) => "mq(..)".to_string(),
            Value::Struct(name, fields) => {
                format!("{name}({})", fields.iter().map(Value::render).collect::<Vec<_>>().join(", "))
            }
            Value::Enum(_, variant, payload) => {
                format!("{variant}({})", payload.iter().map(Value::render).collect::<Vec<_>>().join(", "))
            }
            Value::Fn(name) => format!("fn({name})"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum ErrorKind {
    UnknownFn(String),
    UnknownVar(String),
    ArityMismatch { fn_name: String, want: usize, got: usize },
    // `String`, not `&'static str`: `Ty::name()` has to render recursively
    // for `box box i64`-style types, so it can no longer be a compile-time
    // constant.
    TypeMismatch { expected: String, found: String },
    OutOfRange { ty: String, value: i64 },
    DivByZero,
    MissingReturn { fn_name: String },
    /// `call()`'s `MAX_CALL_DEPTH` guard tripped — see that constant's
    /// doc comment. A normal, catchable error instead of the uncatchable
    /// Rust-stack-overflow abort deep `.nir` recursion would otherwise
    /// cause.
    CallStackOverflow { fn_name: String },
    /// The spawned thread panicked instead of returning normally — Rust's
    /// own panic payload isn't `Display`-friendly in general, so this
    /// carries a best-effort message rather than the raw payload.
    ThreadPanicked { message: String },
    /// Defense in depth, not a case `ownership.rs` should ever let
    /// through: joining a handle that was already joined. Kept as a real,
    /// structured runtime error (matching this file's existing pattern —
    /// see `MissingReturn`) rather than a Rust-level `panic!`/`unwrap`,
    /// the same "the static checker is the real gate, the runtime check
    /// is the backstop" shape as everywhere else in this file.
    AlreadyJoined,
    /// `sandbox`'s own failure category (SANDBOXING.md's decision to give
    /// sandboxes a distinct error family rather than reusing
    /// `ThreadPanicked`): launching the child process itself failed —
    /// writing its temp source file, or the OS-level `spawn()` call.
    SandboxSpawnFailed { message: String },
    /// Defense in depth, mirroring `AlreadyJoined`: `stop`ping a handle
    /// that was already stopped.
    AlreadySandboxStopped,
    /// SANDBOXING.md layer 2: `send`/`recv` on a `chan` that's crossed
    /// into a sandboxed process, where the underlying socket I/O itself
    /// failed — the peer process crashed or was killed mid-conversation,
    /// the pipe broke, or (see `ChannelInner::prepare_for_sandbox`) the
    /// channel was already queued-up or already handed to another
    /// sandbox. Never reachable for the in-process transport, which
    /// can't fail this way at all. Also reused for `connect`/`send`/
    /// `recv` on a `tcp` connection — a raw TCP socket is just another
    /// real-world I/O transport, the same failure category, not a
    /// separate one.
    ChannelIoError { message: String },
    /// `v[i]`/`m[i, j]`'s Tier-2 runtime bounds check (goal.md §4) —
    /// `typeck.rs` proves the index is *an integer*, not that it's *in
    /// range*; SMT-proven bounds (Tier 1) are Phase 5.
    IndexOutOfBounds { index: i64, len: usize },
    /// `inv`/`solve` -- the matrix is (numerically) singular, detected by
    /// Gaussian elimination hitting a zero (below-tolerance) pivot after
    /// partial pivoting already tried every remaining row. A *value*-
    /// dependent failure, not a shape one -- `typeck.rs` already proved
    /// the matrix is square, but squareness doesn't imply invertibility.
    SingularMatrix,
    /// `rand_f64`/`rand_gaussian` called before `rand_seed` -- no
    /// implicit default seed exists (see `Interpreter::rng`'s doc
    /// comment): "deterministic by default" means every draw traces
    /// back to an explicit seed, not a silently-chosen one.
    RngNotSeeded,
    /// `transact`'s durability log (`transact_log.rs`) couldn't be
    /// opened, or a write to it failed -- a real I/O condition (disk
    /// full, permissions, a locked file), not a `.nir` program bug.
    /// Deliberately fatal to the whole `transact` block rather than
    /// silently downgraded to "no durability this time": the entire
    /// point of this construct is that a caller can rely on the log
    /// having actually been written before proceeding, so a log that
    /// can't be written to must stop the block, not quietly drop the
    /// guarantee it exists to provide.
    TransactLogUnavailable { message: String },
    /// `commit`'s retry-with-backoff budget (`run_transact_write_slot`)
    /// was exhausted while `network` had already succeeded -- a
    /// deliberate trap, not a guess at `true`/`false`: the durable log
    /// entry is left `commit_pending`, and `replay_pending_transactions`
    /// (a restart, or the same reconciliation run again later) keeps
    /// retrying it independently of whatever this trap's caller does.
    /// See `TRANSACT.md`'s durability section for why this is "stuck and
    /// visible" on purpose rather than auto-`compensate`d.
    TransactCommitPending { txn_id: String },
    /// Same as `TransactCommitPending`, for the `compensate` slot --
    /// `verify` was `false` and `compensate` itself is the write that's
    /// now stuck, left `compensate_pending` in the durable log.
    TransactCompensatePending { txn_id: String },
    /// `network`'s own `timeout N` modifier (TRANSACT.md's Layer 2)
    /// elapsed before the call returned -- counted as a failed attempt
    /// by `Interpreter::call_network_with_retry`, same as a trap. Only
    /// ever the *final* error reported once `retry`'s attempt budget
    /// (default 1) is exhausted.
    TransactNetworkTimedOut { seconds: i64 },
    /// `workflow_log.rs`'s durable store couldn't be opened/written to
    /// (disk full, permissions, a locked file) — same "fatal to the whole
    /// operation, never a silent no-durability downgrade" reasoning as
    /// `TransactLogUnavailable`.
    WorkflowLogUnavailable { message: String },
    /// An `on_entry`/`on_exit` action call's bounded retry
    /// (`Interpreter::run_workflow_action`, the same backoff shape
    /// `run_transact_write_slot` uses for `commit`/`compensate`) was
    /// exhausted — a deliberate trap, not a guessed outcome.
    /// `Interpreter::eval_workflow_advance` runs the *old* state's
    /// `on_exit` before moving `state`, so a trap here on `on_exit` means
    /// the transition never happened (still the old state, safe to retry
    /// the whole `advance_*` call again); a trap on the *new* state's
    /// `on_entry` (which runs after the move) means the transition
    /// already durably happened and only this state's own actions are
    /// stuck — `state` names which case by naming the state whose
    /// actions were running.
    WorkflowActionPending { instance_id: i64, state: String, action: String },
    /// `DeadlockRegistry::check_and_register` proved a `recv`/`join`
    /// could never resolve — see that type's own module doc for exactly
    /// what's proven (a real `join`-cycle, or every live thread
    /// simultaneously blocked) versus what's a disclosed gap (a `recv`
    /// blocked forever alongside an unrelated thread that's still busy).
    /// A structured trap, never a silent hang — `ROADMAP.md`'s
    /// deadlock-detection entry.
    Deadlock { message: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RuntimeError {
    pub kind: ErrorKind,
    pub span: Span,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Span { line, col } = self.span;
        match &self.kind {
            ErrorKind::UnknownFn(n) => write!(f, "{line}:{col}: unknown function `{n}`"),
            ErrorKind::UnknownVar(n) => write!(f, "{line}:{col}: unknown variable `{n}`"),
            ErrorKind::ArityMismatch { fn_name, want, got } => write!(
                f,
                "{line}:{col}: `{fn_name}` expects {want} argument(s), got {got}"
            ),
            ErrorKind::TypeMismatch { expected, found } => {
                write!(f, "{line}:{col}: expected {expected}, found {found}")
            }
            ErrorKind::OutOfRange { ty, value } => write!(
                f,
                "{line}:{col}: value {value} does not fit in `{ty}` (Tier-2 checked op — \
                 see goal.md §4; not yet proved absent at compile time)"
            ),
            ErrorKind::DivByZero => write!(f, "{line}:{col}: division by zero"),
            ErrorKind::MissingReturn { fn_name } => {
                write!(f, "{line}:{col}: `{fn_name}` did not return a value")
            }
            ErrorKind::ThreadPanicked { message } => {
                write!(f, "{line}:{col}: spawned thread panicked: {message}")
            }
            ErrorKind::AlreadyJoined => write!(f, "{line}:{col}: this thread was already joined"),
            ErrorKind::SandboxSpawnFailed { message } => {
                write!(f, "{line}:{col}: failed to spawn sandbox: {message}")
            }
            ErrorKind::AlreadySandboxStopped => {
                write!(f, "{line}:{col}: this sandbox was already stopped")
            }
            ErrorKind::ChannelIoError { message } => {
                write!(f, "{line}:{col}: channel I/O error: {message}")
            }
            ErrorKind::IndexOutOfBounds { index, len } => write!(
                f,
                "{line}:{col}: index {index} out of bounds (length {len}) (Tier-2 checked op — \
                 see goal.md §4; not yet proved absent at compile time)"
            ),
            ErrorKind::SingularMatrix => write!(f, "{line}:{col}: matrix is singular"),
            ErrorKind::RngNotSeeded => {
                write!(f, "{line}:{col}: rand_f64/rand_gaussian called before rand_seed")
            }
            ErrorKind::CallStackOverflow { fn_name } => write!(
                f,
                "{line}:{col}: call stack depth exceeded while calling `{fn_name}` — \
                 likely unbounded recursion"
            ),
            ErrorKind::TransactLogUnavailable { message } => {
                write!(f, "{line}:{col}: transact durability log unavailable: {message}")
            }
            ErrorKind::TransactCommitPending { txn_id } => write!(
                f,
                "{line}:{col}: transact {txn_id}: `network` succeeded but `commit` has not been confirmed \
                 (retries exhausted) — durably recorded as `commit_pending`, will keep being retried"
            ),
            ErrorKind::TransactCompensatePending { txn_id } => write!(
                f,
                "{line}:{col}: transact {txn_id}: `compensate` has not been confirmed (retries exhausted) — \
                 durably recorded as `compensate_pending`, will keep being retried"
            ),
            ErrorKind::TransactNetworkTimedOut { seconds } => {
                write!(f, "{line}:{col}: `transact`'s `network` slot timed out after {seconds}s")
            }
            ErrorKind::WorkflowLogUnavailable { message } => {
                write!(f, "{line}:{col}: workflow log unavailable: {message}")
            }
            ErrorKind::WorkflowActionPending { instance_id, state, action } => write!(
                f,
                "{line}:{col}: workflow instance {instance_id}, state `{state}`: action `{action}` has not \
                 succeeded (retries exhausted)"
            ),
            ErrorKind::Deadlock { message } => write!(f, "{line}:{col}: {message}"),
        }
    }
}

fn err<T>(kind: ErrorKind, span: Span) -> Result<T, RuntimeError> {
    Err(RuntimeError { kind, span })
}

/// A single scalar arithmetic step, shared by `eval_binary`'s elementwise
/// (`+`/`-`), Hadamard (`.*`/`./`), and matrix-product (`*`, via repeated
/// multiply-accumulate) array arms — `a`/`b` are individual `Vector`/
/// `Matrix` elements, not the arrays themselves. `typeck.rs` already
/// proved both are the same numeric scalar type, so this never needs to
/// handle a type mismatch, only (for `Int`) the same `DivByZero` a plain
/// scalar `/` already checks — `Vector`/`Matrix` division doesn't exist
/// this phase, but Hadamard `./` on integer elements can still divide by
/// zero, so this can't be infallible the way `Value`'s own `PartialEq`
/// is.
fn scalar_binop(op: BinOp, a: &Value, b: &Value, span: Span) -> Result<Value, RuntimeError> {
    match (a, b) {
        (Value::Int(x), Value::Int(y)) => match op {
            BinOp::Add => Ok(Value::Int(x + y)),
            BinOp::Sub => Ok(Value::Int(x - y)),
            BinOp::Mul | BinOp::ElemMul => Ok(Value::Int(x * y)),
            BinOp::Div | BinOp::ElemDiv => {
                if *y == 0 {
                    err(ErrorKind::DivByZero, span)
                } else {
                    Ok(Value::Int(x / y))
                }
            }
            _ => unreachable!("scalar_binop is only ever called with Add/Sub/Mul/Div/ElemMul/ElemDiv"),
        },
        (Value::Float(x), Value::Float(y)) => match op {
            BinOp::Add => Ok(Value::Float(x + y)),
            BinOp::Sub => Ok(Value::Float(x - y)),
            BinOp::Mul | BinOp::ElemMul => Ok(Value::Float(x * y)),
            BinOp::Div | BinOp::ElemDiv => Ok(Value::Float(x / y)),
            _ => unreachable!("scalar_binop is only ever called with Add/Sub/Mul/Div/ElemMul/ElemDiv"),
        },
        // `Decimal`'s own `+`/`-`/`*` panic on overflow past its 28-29
        // digit representation limit — the same "bare arithmetic panic
        // is the trap" idiom `i64` overflow already relies on (see
        // `Cargo.toml`'s `overflow-checks = true` comment). `/` gets an
        // explicit `DivByZero` check instead of letting `Decimal`'s own
        // divide-by-zero panic fire, matching `Value::Int`'s arm above —
        // LANGUAGE.md §2/§6c/§6d promise a real `ErrorKind`, not a raw
        // panic, for this one case specifically.
        (Value::Dec128(x), Value::Dec128(y)) => match op {
            BinOp::Add => Ok(Value::Dec128(x + y)),
            BinOp::Sub => Ok(Value::Dec128(x - y)),
            BinOp::Mul | BinOp::ElemMul => Ok(Value::Dec128(x * y)),
            BinOp::Div | BinOp::ElemDiv => {
                if y.is_zero() {
                    err(ErrorKind::DivByZero, span)
                } else {
                    Ok(Value::Dec128(x / y))
                }
            }
            _ => unreachable!("scalar_binop is only ever called with Add/Sub/Mul/Div/ElemMul/ElemDiv"),
        },
        _ => unreachable!("typeck.rs already proved matching, numeric-scalar element types"),
    }
}

/// `Ok(v)` as a real Nirdosha `Result(_, str)` value — every JSON and
/// HTTP builtin's success case. Constructed directly, not through `find_
/// variant` (this is a free function with no `&Interpreter`/`&Program`
/// to look one up from, unlike `Expr::Call`'s own construction path) —
/// sound because the shape is fixed and known up front: `Result`'s own
/// prelude declaration (`ast::prelude_enums`) is never user-alterable.
fn result_ok(v: Value) -> Value {
    Value::Enum(Arc::from("Result"), Arc::from("Ok"), Arc::from(vec![v]))
}

/// `Err(message)` as a real Nirdosha `Result(_, str)` value — every JSON
/// and HTTP builtin's failure case (a missing JSON key, a value of the
/// wrong shape, malformed input to `json_parse`, a failed connection or
/// malformed HTTP response). Never a Rust-level trap: `Result` is
/// precisely the mechanism for a *recoverable* failure a Nirdosha
/// program can `match` on, as opposed to `RuntimeError` (an
/// unrecoverable one, per `ast.rs`'s own `12.5`-entry framing in
/// `PROTOLANG_PORT.md`).
fn result_err(message: impl Into<String>) -> Value {
    let s: Arc<str> = Arc::from(message.into());
    Value::Enum(Arc::from("Result"), Arc::from("Err"), Arc::from(vec![Value::Str(s)]))
}

/// True iff `v` is a real Nirdosha `Result(_, _)` value in its `Err`
/// variant -- `run_transact_write_slot`'s "auto-recognize `Result<T,E>`,
/// treat `Err` as a failure" half of the fix for `commit`'s previously-
/// discarded return value. `false` for anything else, including a `Result`
/// in its `Ok` variant and every non-`Result` type (a `commit`/`compensate`
/// slot's return type is otherwise unconstrained, per `TRANSACT.md`'s own
/// "Decisions" section -- this only ever narrows a `Result`, never widens
/// what counts as failure for any other return shape).
fn is_result_err(v: &Value) -> bool {
    matches!(v, Value::Enum(name, variant, _) if name.as_ref() == "Result" && variant.as_ref() == "Err")
}

/// Rebuilds a `commit`/`compensate` slot's arguments from its
/// `*_arg_kinds` classification alone (`PendingTxn::commit_arg_kinds`'s own
/// doc comment) -- `None` the instant any kind is `"opaque"`, since that
/// argument genuinely can't be recovered from `network_result`/`txn_id`
/// alone. Mirrors `replay_one`'s `verify_args` reconstruction, generalized
/// to a fallible (rather than statically-guaranteed) classification.
fn reconstruct_transact_args(kinds: &[String], network_result: &Value, txn_id: &str) -> Option<Vec<Value>> {
    kinds
        .iter()
        .map(|k| match k.as_str() {
            "network" => Some(network_result.clone()),
            "txn_id" => Some(Value::Str(Arc::from(txn_id))),
            _ => None,
        })
        .collect()
}

/// `Err(WorkflowActionError::<variant>(payload))` as a real Nirdosha
/// `Result(_, WorkflowActionError)` value — every `send_email`/`send_sms`/
/// `send_push`/`notify`/`__workflow_*` builtin's `.nir`-visible failure
/// case (as opposed to a Rust-level trap, e.g. `WorkflowLogUnavailable`,
/// for a condition that isn't the calling program's fault to recover
/// from). Same "constructed directly, sound because the shape is fixed"
/// reasoning as `result_ok`/`result_err`.
/// `advance_<workflow>`'s trailing `payload: json` argument, read for a
/// `"comment"` string field if present — `WORKFLOW.md`'s "audit trail"
/// section, the one piece of `payload`'s otherwise-still-unused v1 shape
/// (see `workflow_advance`'s own doc comment) that now goes somewhere.
/// Lenient by design: not a JSON object, or no `comment` field, or a
/// non-string value there, all mean "no comment," never an error --
/// `payload` is optional texture on an already-decided transition, not
/// itself validated input.
fn extract_comment(payload: &Value) -> Option<String> {
    let Value::Json(doc) = payload else { return None };
    doc.as_object()?.get("comment")?.as_str().map(|s| s.to_string())
}

fn workflow_err(variant: &str, payload: Vec<Value>) -> Value {
    let err = Value::Enum(Arc::from("WorkflowActionError"), Arc::from(variant), Arc::from(payload));
    Value::Enum(Arc::from("Result"), Arc::from("Err"), Arc::from(vec![err]))
}

/// A real wall-clock read, Rust-side only (never reachable from `.nir`
/// source) — same documented, precedented exception to the determinism
/// story `generate_txn_id`/`resolve_identity`'s `expires_at` check
/// already make (see `WorkflowLog`'s own module doc, `WORKFLOW.md`).
fn now_secs() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0)
}

fn provider_table_name(channel: &str) -> &'static str {
    match channel {
        "email" => "email_provider_config",
        "sms" => "sms_provider_config",
        "push" => "push_provider_config",
        _ => unreachable!("send_via_channel's own callers only ever pass one of these three"),
    }
}

/// `send_email`/`send_sms`/`send_push`/`notify`'s offline-fallback
/// transport: an authenticated HTTPS POST to an admin-configured
/// webhook, carrying a small fixed JSON envelope. Deliberately not
/// hardcoded to one vendor's exact schema (SendGrid's `/v3/mail/send`,
/// Twilio's REST API, FCM's HTTP v1 API) — those can't be verified
/// without live accounts; this ships correct, working delivery to
/// whatever endpoint the admin-editable provider-config row names
/// (`WORKFLOW.md`'s "deliberate non-goals").
fn build_notify_request(host: &str, path: &str, api_key: &str, body: &str) -> Vec<u8> {
    let path = if path.is_empty() || !path.starts_with('/') { format!("/{path}") } else { path.to_string() };
    let body_bytes = body.as_bytes();
    let header = format!(
        "POST {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: nirdosha\r\n\
         Accept: */*\r\nAuthorization: Bearer {api_key}\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n\r\n",
        body_bytes.len()
    );
    let mut out = header.into_bytes();
    out.extend_from_slice(body_bytes);
    out
}

/// Reuses `send_and_receive`'s existing connect/write/read/parse
/// machinery (the same one `https_get`/`https_post` already share) —
/// only the request-building step differs (an `Authorization` header
/// `build_http_request` has no slot for).
fn notify_https_post(host: &str, port: i64, path: &str, api_key: &str, body: &str) -> Result<(), String> {
    let port = u16::try_from(port).map_err(|_| format!("port {port} is not a valid 0-65535 TCP port"))?;
    let request = build_notify_request(host, path, api_key, body);
    let tcp = std::net::TcpStream::connect((host, port)).map_err(|e| e.to_string())?;
    let connector = native_tls::TlsConnector::new().map_err(|e| format!("failed to initialize TLS: {e}"))?;
    let stream = connector.connect(host, tcp).map_err(|e| format!("TLS handshake failed: {e}"))?;
    match send_and_receive(stream, &request) {
        Value::Enum(name, variant, payload) if name.as_ref() == "Result" && variant.as_ref() == "Ok" => {
            let Value::Struct(_, fields) = &payload[0] else {
                return Err("malformed HTTP response".to_string());
            };
            let Value::Int(status) = &fields[0] else {
                return Err("malformed HTTP response".to_string());
            };
            if (200..300).contains(status) {
                Ok(())
            } else {
                Err(format!("provider returned HTTP {status}"))
            }
        }
        Value::Enum(name, variant, payload) if name.as_ref() == "Result" && variant.as_ref() == "Err" => {
            match &payload[0] {
                Value::Str(msg) => Err(msg.to_string()),
                _ => Err("provider request failed".to_string()),
            }
        }
        _ => Err("unexpected response shape".to_string()),
    }
}

/// A short, human-readable name for a JSON value's own dynamic shape --
/// used only in error messages (`result_err`'s payload), never anywhere
/// type-directed.
fn json_kind(v: &JsonDoc) -> &'static str {
    match v {
        JsonDoc::Null => "null",
        JsonDoc::Bool(_) => "a bool",
        JsonDoc::Number(_) => "a number",
        JsonDoc::String(_) => "a string",
        JsonDoc::Array(_) => "an array",
        JsonDoc::Object(_) => "an object",
    }
}

fn json_type_err(key: &str, expected: &str, found: &JsonDoc) -> String {
    format!("field `{key}` is not {expected} (found {})", json_kind(found))
}

/// `json_get`/`json_get_str`/`_i64`/`_f64`/`_bool`'s shared first step:
/// look `key` up in `doc`, which must be a JSON object. `Err` carries a
/// ready-to-wrap message, not a structured type -- matching every other
/// JSON builtin's flat, stringly-typed `Result(_, str)` error convention.
fn json_field<'a>(doc: &'a JsonDoc, key: &str) -> Result<&'a JsonDoc, String> {
    match doc.as_object() {
        None => Err(format!("expected a JSON object, found {}", json_kind(doc))),
        Some(obj) => obj.get(key).ok_or_else(|| format!("no field `{key}`")),
    }
}

// ---- Row 12: relying-party identity helpers --------------------------------

/// Row 12 mock OIDC/JWT validation. The token format is
/// `base64url(header).base64url(payload).base64url(signature)` with an
/// HMAC-SHA256 signature checked against a key drawn from the supplied JWKS
/// JSON. This is a deliberately narrow, self-contained demo of the relying-
/// party pattern: Nirdosha validates an externally-issued token, never mints
/// one, and returns the result as a `VerifiedIdentity` value.
pub(crate) fn validate_oidc_token(token: &str, expected_issuer: &str, expected_audience: &str, jwks_json: &str) -> Result<Value, String> {
    let mut parts = token.split('.');
    let header_b64 = parts.next().ok_or_else(|| "malformed JWT: missing header".to_string())?;
    let payload_b64 = parts.next().ok_or_else(|| "malformed JWT: missing payload".to_string())?;
    let signature_b64 = parts.next().ok_or_else(|| "malformed JWT: missing signature".to_string())?;
    if parts.next().is_some() {
        return Err("malformed JWT: too many segments".to_string());
    }

    let header_json = String::from_utf8(
        URL_SAFE_NO_PAD
            .decode(header_b64)
            .map_err(|_| "malformed JWT: header is not valid base64url".to_string())?,
    )
    .map_err(|_| "malformed JWT: header is not valid UTF-8".to_string())?;
    let header: JsonDoc = serde_json::from_str(&header_json).map_err(|e| format!("malformed JWT header: {e}"))?;

    let kid = header
        .get("kid")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "malformed JWT: header has no `kid`".to_string())?
        .to_string();
    let alg = header
        .get("alg")
        .and_then(|v| v.as_str())
        .ok_or_else(|| "malformed JWT: header has no `alg`".to_string())?
        .to_string();

    let payload_json = String::from_utf8(
        URL_SAFE_NO_PAD
            .decode(payload_b64)
            .map_err(|_| "malformed JWT: payload is not valid base64url".to_string())?,
    )
    .map_err(|_| "malformed JWT: payload is not valid UTF-8".to_string())?;
    let payload: JsonDoc = serde_json::from_str(&payload_json).map_err(|e| format!("malformed JWT payload: {e}"))?;

    let iss = json_field(&payload, "iss")?.as_str().ok_or_else(|| "claim `iss` is not a string".to_string())?;
    let aud = json_field(&payload, "aud")?.as_str().ok_or_else(|| "claim `aud` is not a string".to_string())?;
    let sub = json_field(&payload, "sub")?.as_str().ok_or_else(|| "claim `sub` is not a string".to_string())?;
    let exp = json_field(&payload, "exp")?
        .as_i64()
        .ok_or_else(|| "claim `exp` is not an integer".to_string())?;
    let iat = json_field(&payload, "iat")?
        .as_i64()
        .ok_or_else(|| "claim `iat` is not an integer".to_string())?;

    if iss != expected_issuer {
        return Err(format!("untrusted issuer: expected `{expected_issuer}`, found `{iss}`"));
    }
    if aud != expected_audience {
        return Err(format!("wrong audience: expected `{expected_audience}`, found `{aud}`"));
    }
    // Deliberately no `exp`/`now` check here: this function (and the
    // `.nir`-facing `oidc_validate_token` builtin wrapping it) must stay
    // a pure function of its inputs, no wall-clock read, for the same
    // determinism reason `mock_issue_token`'s own doc comment states
    // (LANGUAGE.md §9) — `identity_expired(identity, now)` exists
    // specifically so an explicit, caller-supplied `now` decides
    // expiry, not a hidden clock inside validation itself. A red-team
    // finding is right that nothing calls that automatically against a
    // *real* clock for `nirdosha serve`'s bearer-token HTTP path — see
    // `serve.rs::dispatch`, which is where that belongs (real Rust
    // infrastructure at the actual network boundary, not part of the
    // `.nir` determinism contract), not here.

    let jwks: JsonDoc = serde_json::from_str(jwks_json).map_err(|e| format!("malformed JWKS: {e}"))?;
    let key = jwks_key(&jwks, &kid)?;
    let signed_input = format!("{header_b64}.{payload_b64}").into_bytes();
    verify_jwt_signature(&alg, &key, &kid, &signed_input, signature_b64)?;

    // Retain every claim except the registered OIDC ones for later
    // role/claim extraction.
    let mut claims = serde_json::Map::new();
    if let JsonDoc::Object(map) = &payload {
        for (k, v) in map {
            if !matches!(k.as_str(), "iss" | "aud" | "sub" | "exp" | "iat") {
                claims.insert(k.clone(), v.clone());
            }
        }
    }
    let claims_json = serde_json::to_string(&JsonDoc::Object(claims)).unwrap_or_else(|_| "{}".to_string());

    Ok(verified_identity_value(sub, iss, aud, exp, iat, &claims_json))
}

/// A JWKS key's actual key material, keyed off its `kty` — the piece
/// `validate_oidc_token` was missing entirely before this revision (only
/// `Symmetric` existed, read unconditionally from a key's `k` member with
/// no `kty` check at all). Matching on this alongside the JWT header's own
/// `alg` (`verify_jwt_signature`) is what closes the classic algorithm-
/// confusion attack: an RSA public key can no longer be replayed as an
/// HMAC secret, because `kty: "RSA"` never produces a `Symmetric` variant.
enum JwksKeyMaterial {
    /// `kty: "oct"` — a raw symmetric secret, `HS256` only.
    Symmetric(Vec<u8>),
    /// `kty: "RSA"` — `n`/`e`, big-endian, `RS256` only.
    Rsa { n: Vec<u8>, e: Vec<u8> },
    /// `kty: "EC"` — `crv`/`x`/`y`, `ES256` only (`crv` must be `P-256`).
    Ec { crv: String, x: Vec<u8>, y: Vec<u8> },
}

fn jwks_key(jwks: &JsonDoc, kid: &str) -> Result<JwksKeyMaterial, String> {
    let keys = jwks.get("keys").and_then(|k| k.as_array()).ok_or_else(|| "JWKS has no `keys` array".to_string())?;
    for k in keys {
        let k_kid = k.get("kid").and_then(|v| v.as_str()).unwrap_or("");
        if k_kid != kid {
            continue;
        }
        // Defaults to `oct` for backward compatibility with a JWKS that
        // predates this revision and never set `kty` at all — every real
        // JWKS this codebase ships (`examples/store_jwks.json`, every
        // test fixture) already sets it explicitly.
        let kty = k.get("kty").and_then(|v| v.as_str()).unwrap_or("oct");
        return match kty {
            "oct" => {
                let b64 = k.get("k").and_then(|v| v.as_str()).ok_or_else(|| format!("JWKS key `{kid}` has no `k`"))?;
                URL_SAFE_NO_PAD
                    .decode(b64)
                    .map(JwksKeyMaterial::Symmetric)
                    .map_err(|_| format!("JWKS key `{kid}` is not valid base64url"))
            }
            "RSA" => {
                let n = decode_jwks_component(k, kid, "n")?;
                let e = decode_jwks_component(k, kid, "e")?;
                Ok(JwksKeyMaterial::Rsa { n, e })
            }
            "EC" => {
                let crv = k.get("crv").and_then(|v| v.as_str()).ok_or_else(|| format!("JWKS EC key `{kid}` has no `crv`"))?.to_string();
                let x = decode_jwks_component(k, kid, "x")?;
                let y = decode_jwks_component(k, kid, "y")?;
                Ok(JwksKeyMaterial::Ec { crv, x, y })
            }
            other => Err(format!("JWKS key `{kid}` has unsupported `kty` `{other}` — expected `oct`, `RSA`, or `EC`")),
        };
    }
    Err(format!("JWKS has no key with kid `{kid}`"))
}

fn decode_jwks_component(key: &JsonDoc, kid: &str, member: &str) -> Result<Vec<u8>, String> {
    let b64 = key.get(member).and_then(|v| v.as_str()).ok_or_else(|| format!("JWKS key `{kid}` has no `{member}`"))?;
    URL_SAFE_NO_PAD.decode(b64).map_err(|_| format!("JWKS key `{kid}`'s `{member}` is not valid base64url"))
}

/// Checks `signature_b64` over `signed_input` against `key`, dispatching
/// on the JWT header's own `alg` — the fix for the JWKS-is-symmetric-only
/// gap (`ROADMAP.md` A11 / `API_TRUST_MODEL.md` §3): `RS256`/`ES256` are
/// real RSA-PKCS1v1.5/ECDSA-P256 signature verification via `ring`, not
/// just HMAC. `alg` and the resolved key's `kty` must agree (enforced by
/// the match arms below having no fallthrough that ignores either) — an
/// `RS256` header can never be satisfied by a `Symmetric` key or vice
/// versa, which is what actually prevents algorithm confusion (a caller
/// presenting `alg: "HS256"` and using the server's own public RSA key
/// bytes as the HMAC secret).
fn verify_jwt_signature(alg: &str, key: &JwksKeyMaterial, kid: &str, signed_input: &[u8], signature_b64: &str) -> Result<(), String> {
    match (alg, key) {
        ("HS256", JwksKeyMaterial::Symmetric(secret)) => {
            let expected_sig = hmac_sha256_base64url(secret, signed_input);
            if !constant_time_eq(signature_b64, &expected_sig) {
                return Err("invalid JWT signature".to_string());
            }
            Ok(())
        }
        ("RS256", JwksKeyMaterial::Rsa { n, e }) => {
            let signature = URL_SAFE_NO_PAD
                .decode(signature_b64)
                .map_err(|_| "malformed JWT: signature is not valid base64url".to_string())?;
            let public_key = ring::signature::RsaPublicKeyComponents { n: n.as_slice(), e: e.as_slice() };
            public_key
                .verify(&ring::signature::RSA_PKCS1_2048_8192_SHA256, signed_input, &signature)
                .map_err(|_| "invalid JWT signature".to_string())
        }
        ("ES256", JwksKeyMaterial::Ec { crv, x, y }) => {
            if crv != "P-256" {
                return Err(format!("JWKS EC key `{kid}` has unsupported `crv` `{crv}` — expected `P-256`"));
            }
            let signature = URL_SAFE_NO_PAD
                .decode(signature_b64)
                .map_err(|_| "malformed JWT: signature is not valid base64url".to_string())?;
            let mut point = Vec::with_capacity(1 + x.len() + y.len());
            point.push(0x04);
            point.extend_from_slice(x);
            point.extend_from_slice(y);
            let public_key = ring::signature::UnparsedPublicKey::new(&ring::signature::ECDSA_P256_SHA256_FIXED, point);
            public_key.verify(signed_input, &signature).map_err(|_| "invalid JWT signature".to_string())
        }
        (other_alg, _) => Err(format!(
            "JWT `alg` `{other_alg}` is unsupported, or doesn't match JWKS key `{kid}`'s key type — expected `HS256` (kty `oct`), `RS256` (kty `RSA`), or `ES256` (kty `EC`)"
        )),
    }
}

fn hmac_sha256_base64url(key: &[u8], message: &[u8]) -> String {
    let mut mac = hmac::Hmac::<Sha256>::new_from_slice(key).expect("HMAC can take any key length");
    mac.update(message);
    URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes())
}

/// A red-team finding, fixed: the JWT signature check used to be a
/// plain `!=` on the two base64url strings — a short-circuiting
/// comparison that returns on the first differing byte, a timing side
/// channel on the sole signature gate protecting every authenticated
/// endpoint. This walks every byte of both sides regardless of where
/// they first differ (accumulating mismatches with `|`, not branching
/// on them), so the time taken doesn't depend on *where* a forged
/// signature first diverges from the real one. Different lengths are
/// rejected up front (real signatures are always the same fixed
/// length), which does leak a length comparison, not a content one —
/// no `Ord`/short-circuit over the actual signature bytes either way.
pub(crate) fn constant_time_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    if a.len() != b.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (x, y) in a.iter().zip(b.iter()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// The deliberate inverse of `validate_oidc_token` — mints a token
/// instead of verifying one, reusing the exact same `jwks_key`/
/// `hmac_sha256_base64url` helpers, so a token minted here round-trips
/// through the real, unmodified `validate_oidc_token` unchanged. Row 12's
/// one narrow, explicitly-named exception to "the runtime never mints
/// tokens; it only consumes externally-issued ones" (this file's own
/// `validate_oidc_token` doc comment) — for local dev/testing only,
/// never a stand-in for a real IdP; the `mock_` prefix on the Nirdosha-
/// facing builtin name is meant to keep that honest. `issued_at` is a
/// required argument, not a hidden wall-clock read, so this stays
/// deterministic — `effects.rs` classifies `mock_issue_token` as pure
/// via its default arm, same as the `json_*` builtins.
fn mock_issue_token(
    subject: &str,
    issuer: &str,
    audience: &str,
    issued_at: i64,
    ttl_secs: i64,
    claims_json: &str,
    jwks_json: &str,
) -> Result<String, String> {
    let jwks: JsonDoc = serde_json::from_str(jwks_json).map_err(|e| format!("malformed JWKS: {e}"))?;
    let keys = jwks.get("keys").and_then(|k| k.as_array()).ok_or_else(|| "JWKS has no `keys` array".to_string())?;
    let kid = keys
        .first()
        .and_then(|k| k.get("kid"))
        .and_then(|v| v.as_str())
        .ok_or_else(|| "JWKS has no keys, or its first key has no `kid`".to_string())?;

    let claims: JsonDoc = serde_json::from_str(claims_json).map_err(|e| format!("malformed claims_json: {e}"))?;
    let claims_obj = claims.as_object().ok_or_else(|| "claims_json must be a JSON object".to_string())?;

    let header = serde_json::json!({"alg": "HS256", "kid": kid});
    let mut payload_map = serde_json::Map::new();
    payload_map.insert("iss".to_string(), JsonDoc::String(issuer.to_string()));
    payload_map.insert("aud".to_string(), JsonDoc::String(audience.to_string()));
    payload_map.insert("sub".to_string(), JsonDoc::String(subject.to_string()));
    payload_map.insert("iat".to_string(), JsonDoc::from(issued_at));
    payload_map.insert("exp".to_string(), JsonDoc::from(issued_at + ttl_secs));
    for (k, v) in claims_obj {
        payload_map.insert(k.clone(), v.clone());
    }
    let payload = JsonDoc::Object(payload_map);

    let header_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&header).map_err(|e| e.to_string())?);
    let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).map_err(|e| e.to_string())?);
    // `mock_issue_token` only ever mints `HS256` tokens (see this fn's own
    // doc comment) — the picked `kid` must therefore resolve to a
    // symmetric key; an RSA/EC entry has no matching private key this
    // codebase could sign with anyway.
    let JwksKeyMaterial::Symmetric(key) = jwks_key(&jwks, kid)? else {
        return Err(format!("mock_issue_token only supports a symmetric (`oct`) JWKS key; kid `{kid}` is not one"));
    };
    let signature_b64 = hmac_sha256_base64url(&key, format!("{header_b64}.{payload_b64}").as_bytes());
    Ok(format!("{header_b64}.{payload_b64}.{signature_b64}"))
}

/// `identity`'s (a `VerifiedIdentity` struct value) embedded `subject`
/// field (index 0) — `WORKFLOW.md`'s "who submitted this"/"audit trail"
/// sections use this to record *which* identity started an instance or
/// fired a transition, the same field order `identity_claims` below
/// already relies on for `claims_json` (index 5).
fn identity_subject(identity: &Value) -> Arc<str> {
    let Value::Struct(_, fields) = identity else {
        unreachable!("typeck.rs already proved this is a VerifiedIdentity")
    };
    let Value::Str(subject) = &fields[0] else {
        unreachable!("VerifiedIdentity.subject is str")
    };
    Arc::clone(subject)
}

/// Reads `identity`'s (a `VerifiedIdentity` struct value) embedded
/// `claims_json` field (index 5) as a parsed JSON document. Shared root
/// of `identity_has_role`/`identity_claim` below.
fn identity_claims(identity: &Value) -> Result<JsonDoc, String> {
    let Value::Struct(_, fields) = identity else {
        unreachable!("typeck.rs already proved this is a VerifiedIdentity")
    };
    let Value::Str(claims_json) = &fields[5] else {
        unreachable!("VerifiedIdentity.claims_json is str")
    };
    serde_json::from_str(claims_json).map_err(|e| format!("bad claims json: {e}"))
}

/// Whether `identity` carries `role` in its embedded `claims_json`'s
/// `"roles"` array. Extracted out of the `check_role` builtin so
/// `serve.rs`'s runtime `requires(role: ...)` enforcement can reuse the
/// exact same check — `Interpreter::call_named` itself doesn't enforce
/// `FnDecl.requires` at all (only `Expr::Acquire` does), so `serve.rs`
/// has to do it itself, and duplicating this logic there instead of
/// sharing it would be one drift risk too many for an authz check.
pub(crate) fn identity_has_role(identity: &Value, role: &str) -> Result<bool, String> {
    let claims = identity_claims(identity)?;
    let roles = json_field(&claims, "roles")?.as_array().ok_or_else(|| "`roles` is not an array".to_string())?;
    Ok(roles.iter().any(|v| v.as_str() == Some(role)))
}

/// `state Name { owner: role("...")/claim("...", "...") }` — `None` means
/// unrestricted (`WORKFLOW.md`'s "state ownership" section: any
/// authenticated caller may fire this state's outgoing events). Looked up
/// from `StateDecl.entries`, the same open-ended `key: value` slot list
/// `ScreenDecl.entries` already uses.
pub(crate) fn state_owner(state: &StateDecl) -> Option<&Expr> {
    state.entries.iter().find(|(k, _)| k == "owner").map(|(_, v)| v)
}

/// Whether `identity` satisfies `owner` — already proven by
/// `typeck.rs::check_visibility_expr` to be exactly `role("...")` or
/// `claim("...", "...")` with string-literal arguments, so the shape
/// match below never needs to report a *new* error, only reuse
/// `identity_has_role`/`identity_claim`'s own (a malformed/missing
/// `claims_json`, same as any other identity check in this file).
fn identity_satisfies_owner(owner: &Expr, identity: &Value) -> Result<bool, String> {
    match owner {
        Expr::Call(name, args, _) if name == "role" => {
            let Expr::Str(role, _) = &args[0] else {
                unreachable!("typeck.rs already proved role(...) takes a string literal")
            };
            identity_has_role(identity, role)
        }
        Expr::Call(name, args, _) if name == "claim" => {
            let (Expr::Str(key, _), Expr::Str(value, _)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved claim(...) takes two string-literal arguments")
            };
            Ok(identity_claim(identity, key).map(|v| v == *value).unwrap_or(false))
        }
        _ => unreachable!("typeck.rs already proved owner is role(...)/claim(...)"),
    }
}

/// The string value of claim `key` in `identity`'s embedded
/// `claims_json`. Extracted out of the `extract_claim` builtin for the
/// same reuse-by-`serve.rs` reason as `identity_has_role` above.
pub(crate) fn identity_claim(identity: &Value, key: &str) -> Result<String, String> {
    let claims = identity_claims(identity)?;
    let v = json_field(&claims, key)?;
    v.as_str().map(|s| s.to_string()).ok_or_else(|| format!("claim `{key}` is not a string"))
}

/// `json_field`'s multi-level sibling: walks `path` (dot-separated, e.g.
/// `"realm_access.roles"`) through nested JSON objects, one flat
/// `json_field` step per segment. Backs `check_role_path`/
/// `extract_claim_path` only -- `check_role`/`extract_claim` (and every
/// general-purpose `json_get*` builtin) stay on plain `json_field`
/// unchanged, since a claim name can itself contain a literal dot
/// (Auth0-style namespaced claims, `"https://myapp.example.com/roles"`)
/// -- that's a flat single key, not a nested path, and splitting it on
/// `.` would silently misread it as one. Callers choose the right tool
/// for their IdP's claim shape; this helper only ever walks *actual*
/// nesting.
fn json_field_path<'a>(doc: &'a JsonDoc, path: &str) -> Result<&'a JsonDoc, String> {
    let mut cur = doc;
    for segment in path.split('.') {
        cur = json_field(cur, segment)?;
    }
    Ok(cur)
}

/// `identity_has_role`'s dotted-path sibling, for IdPs that nest the
/// roles array under a path instead of a flat top-level `"roles"` field
/// (Keycloak's `realm_access.roles`, for one real example). Backs
/// `check_role_path`; `check_role`/`identity_has_role` are untouched.
pub(crate) fn identity_has_role_at(identity: &Value, path: &str, role: &str) -> Result<bool, String> {
    let claims = identity_claims(identity)?;
    let roles = json_field_path(&claims, path)?.as_array().ok_or_else(|| format!("`{path}` is not an array"))?;
    Ok(roles.iter().any(|v| v.as_str() == Some(role)))
}

/// `identity_claim`'s dotted-path sibling, for a claim nested under an
/// object instead of a flat top-level field. Backs `extract_claim_path`;
/// `extract_claim`/`identity_claim` are untouched.
pub(crate) fn identity_claim_at(identity: &Value, path: &str) -> Result<String, String> {
    let claims = identity_claims(identity)?;
    let v = json_field_path(&claims, path)?;
    v.as_str().map(|s| s.to_string()).ok_or_else(|| format!("claim `{path}` is not a string"))
}

/// `identity`'s `expires_at` field — extracted for the same reuse-by-
/// `serve.rs` reason as `identity_has_role`/`identity_claim` above:
/// `serve.rs::dispatch` is real Rust infrastructure at the actual
/// network boundary (not part of the `.nir` determinism contract the
/// rest of this file's identity builtins deliberately keep — see
/// `validate_oidc_token`'s own doc comment on why *it* never reads a
/// real clock), so it's the correct place to reject an actually-expired
/// bearer token against a real `SystemTime::now()` read automatically,
/// rather than leaving that to `identity_expired`, an opt-in builtin a
/// `.nir` program has to call itself with its own supplied `now`.
pub(crate) fn identity_expires_at(identity: &Value) -> Result<i64, String> {
    let Value::Struct(name, fields) = identity else {
        return Err("not a VerifiedIdentity".to_string());
    };
    if name.as_ref() != "VerifiedIdentity" {
        return Err("not a VerifiedIdentity".to_string());
    }
    match fields.get(3) {
        Some(Value::Int(n)) => Ok(*n),
        _ => Err("VerifiedIdentity.expires_at is not an i64".to_string()),
    }
}

fn verified_identity_value(subject: &str, issuer: &str, audience: &str, expires_at: i64, issued_at: i64, claims_json: &str) -> Value {
    Value::Struct(
        Arc::from("VerifiedIdentity"),
        Arc::from(vec![
            Value::Str(Arc::from(subject)),
            Value::Str(Arc::from(issuer)),
            Value::Str(Arc::from(audience)),
            Value::Int(expires_at),
            Value::Int(issued_at),
            Value::Str(Arc::from(claims_json)),
        ]),
    )
}

// ---- Row 12 continued: session, refresh, revocation, API-key helpers ------

/// A per-process random secret, generated once, for `session_id`'s
/// unpredictable component (see that function's doc comment for why it
/// needs one at all). `/dev/urandom` is real OS entropy, no new crate
/// dependency needed (this codebase already avoids pulling in `rand`'s
/// trait ecosystem even for the *deterministic* `.nir`-facing PRNG — see
/// `RngState`'s own doc comment); the fallback only matters on a
/// platform without it, and is still strictly better than a fixed
/// constant.
fn session_secret_hex() -> &'static str {
    static SECRET: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    SECRET.get_or_init(|| {
        let mut buf = [0u8; 32];
        let got_os_random = std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
            .is_ok();
        if !got_os_random {
            let pid = std::process::id() as u64;
            let addr = &buf as *const _ as u64; // ASLR-randomized
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let mixed = sha256_hex_chain(&[&pid.to_string(), &addr.to_string(), &nanos.to_string()]);
            let mixed_bytes = mixed.as_bytes();
            for (i, b) in buf.iter_mut().enumerate() {
                *b = mixed_bytes[i % mixed_bytes.len()];
            }
        }
        buf.iter().map(|b| format!("{b:02x}")).collect()
    })
}

/// A red-team finding, fixed: `session_id` used to be
/// `format!("{issuer}_{subject}_{now}")` with `now` a *fixed* constant
/// (`create_application_session`'s call site) — the same (issuer,
/// subject) pair always produced the identical session cookie, no
/// randomness or server secret involved at all. Anyone who knew a
/// victim's subject/issuer (rarely secret) could compute their exact
/// session cookie — full hijack, no forgery needed. The
/// `{issuer}_{subject}_` prefix stays (existing behavior/tests key off
/// it for readability/log-grepping), but the suffix is now
/// `sha256_hex_chain(secret, subject, issuer, real-time nanoseconds,
/// a per-process counter)` — unpredictable without the process-local
/// secret, and unique even for two sessions created in the same
/// nanosecond. Real time/counter here (unlike `now: i64`, which stays
/// exactly as before for `expires_at`'s bookkeeping) is fine specifically
/// because this value's only job is to be an opaque, unguessable
/// credential — nothing about the `.nir` determinism contract depends
/// on a session *id* being reproducible the way a program's own return
/// value must be.
/// `transact`'s implicit `txn_id: str` binding -- the idempotency key a
/// crash-replayed resend of `network` relies on the downstream system to
/// dedupe (`TRANSACT.md`'s durability section). Same construction as
/// `application_session_value`'s own unpredictable, always-unique suffix
/// just below (`session_secret_hex()` + real-time nanoseconds + a
/// per-process counter, hashed) -- reused wholesale rather than
/// duplicated, since the requirement is identical: unpredictable,
/// unique even for two `transact` blocks entered in the same nanosecond,
/// with no bearing on the `.nir` determinism contract (a `txn_id`'s only
/// job is to be an opaque, never-repeating key, not a reproducible
/// value).
/// `Interpreter::transact_log_path`'s default -- a unique per-instance
/// file under the OS temp dir, so parallel `cargo test` runs (or any
/// two `Interpreter`s constructed without an explicit
/// `with_transact_log_path`) never share, and can't corrupt, the same
/// SQLite file. `main.rs`'s `run`/`serve` commands override this with a
/// stable, source-file-derived path instead -- see
/// `with_transact_log_path`'s own doc comment for why a random default
/// would defeat crash-replay's whole purpose there.
fn default_transact_log_path() -> std::path::PathBuf {
    static PATH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = PATH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("nirdosha-transact-{}-{nanos}-{counter}.db", std::process::id()))
}

/// `Interpreter::workflow_log_path`'s default -- same "unique per-instance
/// temp file" reasoning as `default_transact_log_path`, for the same
/// parallel-test-safety reason.
fn default_workflow_log_path() -> std::path::PathBuf {
    static PATH_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = PATH_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    std::env::temp_dir().join(format!("nirdosha-workflow-{}-{nanos}-{counter}.db", std::process::id()))
}

fn generate_txn_id() -> String {
    static TXN_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = TXN_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let real_now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    sha256_hex_chain(&[session_secret_hex(), &real_now_nanos.to_string(), &counter.to_string()])
}

fn application_session_value(subject: &str, issuer: &str, now: i64) -> Value {
    static SESSION_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let counter = SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let real_now_nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    let unpredictable =
        sha256_hex_chain(&[session_secret_hex(), subject, issuer, &real_now_nanos.to_string(), &counter.to_string()]);
    let session_id = format!("{}_{}_{}", issuer.replace("https://", ""), subject, unpredictable);
    let eight_hours = 8 * 60 * 60;
    Value::Struct(
        Arc::from("ApplicationSession"),
        Arc::from(vec![
            Value::Str(Arc::from(session_id)),
            Value::Str(Arc::from(subject)),
            Value::Str(Arc::from(issuer)),
            Value::Int(now),
            Value::Int(now + eight_hours),
            Value::Int(now),
        ]),
    )
}

fn refresh_token_value(expires_at: i64) -> Value {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let handle = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed) as i64;
    Value::Struct(
        Arc::from("RefreshTokenHandle"),
        Arc::from(vec![Value::Boxed(Box::new(Value::Int(handle))), Value::Int(expires_at)]),
    )
}

fn sha256_hex(s: &str) -> String {
    sha256_hex_chain(&[s])
}

/// The 2-arg form of the `sha256_hex` builtin — hashes each part in
/// sequence (`Sha256::update` called once per part) rather than
/// concatenating first, since Nirdosha `str` has no concatenation
/// operator at all (LANGUAGE.md §2). This is what makes a real
/// hash-chained audit log (`hash = sha256_hex(prev_hash, payload)`)
/// expressible from Nirdosha source — the "combine two strings" step
/// happens here, in Rust, not via string concatenation in the language.
fn sha256_hex_chain(parts: &[&str]) -> String {
    let mut hasher = Sha256::new();
    for p in parts {
        hasher.update(p.as_bytes());
    }
    let result = hasher.finalize();
    result.iter().map(|b| format!("{b:02x}")).collect()
}

// ---- DB, layer 1 + layer 2: SQLite + Postgres (`Ty::Db`'s doc comment,
// `dbconn.rs`'s module doc) -----------------------------------------------

/// Converts `db_query`/`db_execute`'s trailing bind-value arguments
/// (`typeck.rs`'s arity-ranged `(2..=10)` signature) into `dbconn::Param`
/// -- backend-neutral, since a given `Value::Db` handle might be either
/// driver underneath. Scalars only — `Int`/`Float`/`Str`/`Bool` map
/// directly (`dbconn.rs`'s `pg_bind`/`sqlite_bind` decide, per backend,
/// how `Bool` actually gets bound); anything else (struct, handle, ...)
/// is a clear runtime error rather than a silent misbind. See
/// `typeck.rs`'s doc comment on why these args aren't type-constrained
/// ahead of time — this is the actual (runtime) gate.
fn sql_bind_params(args: &[Value]) -> Result<Vec<crate::dbconn::Param>, String> {
    args.iter()
        .map(|v| match v {
            Value::Int(n) => Ok(crate::dbconn::Param::Int(*n)),
            Value::Float(f) => Ok(crate::dbconn::Param::Float(*f)),
            Value::Str(s) => Ok(crate::dbconn::Param::Text(s.to_string())),
            Value::Bool(b) => Ok(crate::dbconn::Param::Bool(*b)),
            // `dec128` binds as its canonical decimal string, same as
            // `dec_to_str` (`LANGUAGE.md` §5's "Decimal arithmetic") --
            // a `NUMERIC`/`DECIMAL` Postgres column or `TEXT` SQLite
            // column, never a float column, to avoid reintroducing the
            // float-rounding drift this type exists to prevent.
            Value::Dec128(d) => Ok(crate::dbconn::Param::Text(d.to_string())),
            // A zero-payload ("unit") enum variant -- e.g. a categorical
            // `enum RiskRating { Low, Medium, High }` field -- binds as
            // its own variant name, TEXT. A payload-carrying variant
            // can't sensibly occupy one SQL column (it has more than one
            // value to store), so it still falls through to the error
            // below, unchanged.
            Value::Enum(_, variant, payload) if payload.is_empty() => {
                Ok(crate::dbconn::Param::Text(variant.to_string()))
            }
            other => Err(format!("db bind value must be str/i64/f64/bool/dec128, got {}", other.ty_name())),
        })
        .collect()
}

/// One row → one JSON object, column name to value. SQLite's own dynamic
/// typing (`rusqlite::types::Value`) maps onto JSON's own dynamic typing
/// almost exactly — `Null`/`Integer`/`Real`/`Text` all have a direct JSON
/// counterpart. `Blob` doesn't (this language has no `bytes` type — same
/// gap `Ty::Tcp`/`Ty::File`'s own doc comments already name): represented
/// as a lowercase-hex string rather than dropped or erroring, so a `Blob`
/// column is still *readable*, just not round-trippable back into SQL from
/// Nirdosha yet — a real, narrow, named limitation, not silently assumed
/// away.
pub(crate) fn db_row_to_json(row: &rusqlite::Row, column_names: &[String]) -> rusqlite::Result<JsonDoc> {
    let mut map = serde_json::Map::with_capacity(column_names.len());
    for (i, name) in column_names.iter().enumerate() {
        let value: rusqlite::types::Value = row.get(i)?;
        let json_value = match value {
            rusqlite::types::Value::Null => JsonDoc::Null,
            rusqlite::types::Value::Integer(n) => JsonDoc::from(n),
            rusqlite::types::Value::Real(f) => {
                serde_json::Number::from_f64(f).map(JsonDoc::Number).unwrap_or(JsonDoc::Null)
            }
            rusqlite::types::Value::Text(s) => JsonDoc::String(s),
            rusqlite::types::Value::Blob(bytes) => {
                JsonDoc::String(bytes.iter().map(|b| format!("{b:02x}")).collect())
            }
        };
        map.insert(name.clone(), json_value);
    }
    Ok(JsonDoc::Object(map))
}

/// Builds the raw request bytes `http_request`/`https_request` both send
/// — one HTTP/1.1 request line, a fixed header set, and the body (if
/// any). Shared so the two transports can never drift on what they
/// actually put on the wire. Returns `Err` if any component contains
/// newlines, which would let an attacker inject arbitrary headers or
/// split the request.
fn build_http_request(method: &str, host: &str, path: &str, body: Option<&str>) -> Result<Vec<u8>, String> {
    // Prevent request-line / header injection. `method` is supplied by the
    // compiler as a fixed literal, but validate it anyway; `host` and `path`
    // come from user-provided `str` values and must not contain line breaks.
    let allowed_methods: &[&str] = &["GET", "POST", "PUT", "DELETE", "PATCH", "HEAD", "OPTIONS"];
    if !allowed_methods.iter().any(|m| *m == method) {
        return Err(format!("unsupported HTTP method `{method}`"));
    }
    if method.chars().any(|c| c == '\r' || c == '\n')
        || host.chars().any(|c| c == '\r' || c == '\n')
        || path.chars().any(|c| c == '\r' || c == '\n')
    {
        return Err("HTTP method, host, or path contains a line break".to_string());
    }
    // A request target that is empty or lacks a leading `/` can confuse some
    // servers; normalize to `/` while still allowing query strings and paths.
    let path = if path.is_empty() || !path.starts_with('/') { format!("/{path}") } else { path.to_string() };
    let mut request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {host}\r\nConnection: close\r\nUser-Agent: nirdosha\r\nAccept: */*\r\n"
    );
    let req_body_bytes = body.unwrap_or("").as_bytes();
    if body.is_some() {
        request.push_str(&format!("Content-Type: text/plain; charset=utf-8\r\nContent-Length: {}\r\n", req_body_bytes.len()));
    }
    request.push_str("\r\n");
    let mut request_bytes = request.into_bytes();
    request_bytes.extend_from_slice(req_body_bytes);
    Ok(request_bytes)
}

/// Splits a raw HTTP response into a status code and a body — the other
/// half of what `http_request`/`https_request` share. Every failure — a
/// malformed status line, a non-UTF-8 body — is `Err(message)`, a real
/// Nirdosha value, never a Rust-level trap.
fn parse_http_response(raw: &[u8]) -> Value {
    // Split at the first blank line -- CRLF CRLF, per RFC 7230 -- into
    // the header block and the body. Headers are always ASCII; only the
    // body might not be valid UTF-8 (`str` requires it -- `Ty::Str`'s
    // doc comment).
    let Some(sep) = raw.windows(4).position(|w| w == b"\r\n\r\n") else {
        return result_err("malformed HTTP response: no header/body separator found");
    };
    let header_block = match std::str::from_utf8(&raw[..sep]) {
        Ok(s) => s,
        Err(_) => return result_err("malformed HTTP response: headers are not valid UTF-8"),
    };
    let response_body = match std::str::from_utf8(&raw[sep + 4..]) {
        Ok(s) => s.to_string(),
        Err(_) => return result_err("HTTP response body is not valid UTF-8"),
    };

    let Some(status_line) = header_block.split("\r\n").next() else {
        return result_err("malformed HTTP response: empty status line");
    };
    let Some(status) = status_line.split_whitespace().nth(1).and_then(|s| s.parse::<i64>().ok()) else {
        return result_err(format!("malformed HTTP status line: {status_line:?}"));
    };

    result_ok(Value::Struct(
        Arc::from("HttpResponse"),
        Arc::from(vec![Value::Int(status), Value::Str(Arc::from(response_body))]),
    ))
}

/// Half-closes a stream's write side once a request has been fully sent
/// — a courtesy for servers that wait for EOF on their read end before
/// responding to a `Connection: close` request, not a requirement (a
/// well-behaved server already knows the request is complete from
/// `Content-Length`, or from the blank line after a bodyless GET's
/// headers). Meaningful for a plain `TcpStream`; deliberately a no-op
/// over TLS (`native_tls::TlsStream`) — see that impl's own doc comment
/// for why attempting one there would be actively wrong, not just
/// unnecessary.
trait HalfCloseWrite {
    fn half_close_write(&mut self);
}

impl HalfCloseWrite for std::net::TcpStream {
    fn half_close_write(&mut self) {
        let _ = self.shutdown(std::net::Shutdown::Write);
    }
}

/// **Deliberate no-op**, not a missing feature: TLS has no raw half-close
/// the way TCP does — `TlsStream::shutdown` sends a `close_notify` and
/// tears down the whole session in both directions, which would prevent
/// ever reading the response that's about to be requested. The request
/// itself already tells the server exactly how much to expect
/// (`Content-Length` for a body, or the blank line terminating a
/// bodyless GET's headers), so there's nothing this step needs to do for
/// a well-behaved HTTPS server.
impl<S: std::io::Read + std::io::Write> HalfCloseWrite for native_tls::TlsStream<S> {
    fn half_close_write(&mut self) {}
}

/// `http_request`/`https_request`'s shared send/receive step, generic
/// over the transport (a plain `TcpStream`, or a `native_tls::TlsStream`
/// wrapping one) — writes the request, half-closes if the transport
/// supports it, reads the response to EOF (the peer closing the
/// connection *is* the end-of-body signal — no `Content-Length`/
/// chunked-transfer-encoding parsing needed for this first cut, since
/// `Connection: close` means there's no persistent connection to need it
/// for), and parses it.
fn send_and_receive<S: std::io::Read + std::io::Write + HalfCloseWrite>(mut stream: S, request: &[u8]) -> Value {
    if let Err(e) = stream.write_all(request) {
        return result_err(e.to_string());
    }
    stream.half_close_write();
    // Cap the total response size to avoid unbounded allocation when a peer
    // sends a huge or infinite response. 10 MiB is generous for the JSON
    // payloads this language consumes.
    const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len() + n > MAX_RESPONSE_BYTES {
                    return result_err("HTTP response exceeded maximum size (10 MiB)".to_string());
                }
                raw.extend_from_slice(&buf[..n]);
            }
            Err(e) => return result_err(e.to_string()),
        }
    }
    parse_http_response(&raw)
}

/// `http_get`/`http_post`'s shared implementation (`ast::BUILTIN_NAMES`'s
/// doc comment) — connects a real TCP socket (the same
/// `std::net::TcpStream::connect` `Expr::Connect` already uses) and
/// speaks plain HTTP over it.
fn http_request(host: &str, port: i64, method: &str, path: &str, body: Option<&str>) -> Value {
    let port = match u16::try_from(port) {
        Ok(p) => p,
        Err(_) => return result_err(format!("port {port} is not a valid 0-65535 TCP port")),
    };
    let request = match build_http_request(method, host, path, body) {
        Ok(r) => r,
        Err(e) => return result_err(e),
    };
    let stream = match std::net::TcpStream::connect((host, port)) {
        Ok(s) => s,
        Err(e) => return result_err(e.to_string()),
    };
    send_and_receive(stream, &request)
}

/// `https_get`/`https_post`'s shared implementation — same request/
/// response handling as `http_request`, over a `native_tls::TlsStream`
/// wrapping the same real `TcpStream` instead of a bare one.
/// `TlsConnector::new()`'s defaults are what's actually doing the
/// security-critical work here (certificate-chain and hostname
/// verification against the platform's trust store) — deliberately not
/// hand-rolled, per this design's own "TLS should be a vetted library
/// binding" stance (`PROTOLANG_PORT.md`'s std_io §12 entry).
fn https_request(host: &str, port: i64, method: &str, path: &str, body: Option<&str>) -> Value {
    let port = match u16::try_from(port) {
        Ok(p) => p,
        Err(_) => return result_err(format!("port {port} is not a valid 0-65535 TCP port")),
    };
    let request = match build_http_request(method, host, path, body) {
        Ok(r) => r,
        Err(e) => return result_err(e),
    };
    let tcp = match std::net::TcpStream::connect((host, port)) {
        Ok(s) => s,
        Err(e) => return result_err(e.to_string()),
    };
    let connector = match native_tls::TlsConnector::new() {
        Ok(c) => c,
        Err(e) => return result_err(format!("failed to initialize TLS: {e}")),
    };
    let stream = match connector.connect(host, tcp) {
        Ok(s) => s,
        Err(e) => return result_err(format!("TLS handshake failed: {e}")),
    };
    send_and_receive(stream, &request)
}

fn as_f64(v: &Value) -> f64 {
    match v {
        Value::Float(f) => *f,
        _ => unreachable!("typeck.rs already proved this element is f64"),
    }
}

/// Sum of a nonempty scalar slice via repeated `scalar_binop(Add, ..)` --
/// shared by `sum`, `dot`'s and matrix-multiply's accumulation, and
/// `trace`. Never called on an empty slice: a `Value::Vector`/`Matrix`
/// only exists via `Expr::ArrayLit`, which always has at least one
/// element/row (see `Expr::ArrayLit`'s doc comment in ast.rs).
fn sum_all(elems: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let mut acc = elems[0].clone();
    for v in &elems[1..] {
        acc = scalar_binop(BinOp::Add, &acc, v, span)?;
    }
    Ok(acc)
}

/// Gaussian elimination with partial pivoting, row-major `n x n`.
/// Returns the determinant; `0.0` for a singular matrix (a real,
/// legitimate answer for `det` specifically — unlike `inv`/`solve`,
/// there's no result to fail to produce).
fn matrix_det(elems: &[f64], n: usize) -> f64 {
    let mut a: Vec<f64> = elems.to_vec();
    let mut det = 1.0;
    for col in 0..n {
        let mut pivot_row = col;
        let mut max_val = a[col * n + col].abs();
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                pivot_row = row;
            }
        }
        if max_val == 0.0 {
            return 0.0;
        }
        if pivot_row != col {
            for k in 0..n {
                a.swap(col * n + k, pivot_row * n + k);
            }
            det = -det;
        }
        det *= a[col * n + col];
        for row in (col + 1)..n {
            let factor = a[row * n + col] / a[col * n + col];
            for k in col..n {
                a[row * n + k] -= factor * a[col * n + k];
            }
        }
    }
    det
}

/// Numerically-singular threshold shared by `inv`/`solve`/`rank`'s pivot
/// checks — a plain `== 0.0` would accept a pivot that's technically
/// nonzero but so small that dividing by it produces garbage; this is
/// the standard "close enough to singular to refuse" tolerance, not a
/// principled bound.
const SINGULAR_EPSILON: f64 = 1e-10;

/// Gauss-Jordan elimination with partial pivoting, augmenting with the
/// identity matrix. Returns `None` for a singular matrix.
fn matrix_inv(elems: &[f64], n: usize) -> Option<Vec<f64>> {
    let mut a: Vec<f64> = elems.to_vec();
    let mut inv = vec![0.0; n * n];
    for i in 0..n {
        inv[i * n + i] = 1.0;
    }
    for col in 0..n {
        let mut pivot_row = col;
        let mut max_val = a[col * n + col].abs();
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                pivot_row = row;
            }
        }
        if max_val < SINGULAR_EPSILON {
            return None;
        }
        if pivot_row != col {
            for k in 0..n {
                a.swap(col * n + k, pivot_row * n + k);
                inv.swap(col * n + k, pivot_row * n + k);
            }
        }
        let pivot = a[col * n + col];
        for k in 0..n {
            a[col * n + k] /= pivot;
            inv[col * n + k] /= pivot;
        }
        for row in 0..n {
            if row == col {
                continue;
            }
            let factor = a[row * n + col];
            if factor != 0.0 {
                for k in 0..n {
                    a[row * n + k] -= factor * a[col * n + k];
                    inv[row * n + k] -= factor * inv[col * n + k];
                }
            }
        }
    }
    Some(inv)
}

/// Gaussian elimination with partial pivoting, then back substitution.
/// Returns `None` for a singular `a`.
fn matrix_solve(a_elems: &[f64], n: usize, b_elems: &[f64]) -> Option<Vec<f64>> {
    let mut a: Vec<f64> = a_elems.to_vec();
    let mut b: Vec<f64> = b_elems.to_vec();
    for col in 0..n {
        let mut pivot_row = col;
        let mut max_val = a[col * n + col].abs();
        for row in (col + 1)..n {
            let v = a[row * n + col].abs();
            if v > max_val {
                max_val = v;
                pivot_row = row;
            }
        }
        if max_val < SINGULAR_EPSILON {
            return None;
        }
        if pivot_row != col {
            for k in 0..n {
                a.swap(col * n + k, pivot_row * n + k);
            }
            b.swap(col, pivot_row);
        }
        for row in (col + 1)..n {
            let factor = a[row * n + col] / a[col * n + col];
            for k in col..n {
                a[row * n + k] -= factor * a[col * n + k];
            }
            b[row] -= factor * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let mut sum = b[row];
        for k in (row + 1)..n {
            sum -= a[row * n + k] * x[k];
        }
        x[row] = sum / a[row * n + row];
    }
    Some(x)
}

/// Row-echelon reduction, `rows x cols` (not necessarily square) —
/// returns the number of nonzero pivot rows found.
fn matrix_rank(elems: &[f64], rows: usize, cols: usize) -> usize {
    let mut a: Vec<f64> = elems.to_vec();
    let mut rank = 0;
    let mut pivot_row = 0;
    for col in 0..cols {
        if pivot_row >= rows {
            break;
        }
        let mut best_row = pivot_row;
        let mut max_val = a[pivot_row * cols + col].abs();
        for row in (pivot_row + 1)..rows {
            let v = a[row * cols + col].abs();
            if v > max_val {
                max_val = v;
                best_row = row;
            }
        }
        if max_val < SINGULAR_EPSILON {
            continue; // this column contributes no new pivot
        }
        if best_row != pivot_row {
            for k in 0..cols {
                a.swap(pivot_row * cols + k, best_row * cols + k);
            }
        }
        for row in (pivot_row + 1)..rows {
            let factor = a[row * cols + col] / a[pivot_row * cols + col];
            for k in col..cols {
                a[row * cols + k] -= factor * a[pivot_row * cols + k];
            }
        }
        pivot_row += 1;
        rank += 1;
    }
    rank
}

// ---- geometry (WGS84) -------------------------------------------------

/// WGS84 ellipsoid semi-major axis (meters) and flattening — the
/// standard reference ellipsoid every GPS/GNSS coordinate is already
/// expressed against, so this is the only sane default rather than a
/// configurable parameter this phase doesn't otherwise need.
const WGS84_A: f64 = 6_378_137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;

fn wgs84_e2() -> f64 {
    WGS84_F * (2.0 - WGS84_F)
}

/// `[lat_deg, lon_deg, alt_m]` -> `[x, y, z]` ECEF meters.
fn lla_to_ecef(lat_deg: f64, lon_deg: f64, alt: f64) -> (f64, f64, f64) {
    let lat = lat_deg.to_radians();
    let lon = lon_deg.to_radians();
    let e2 = wgs84_e2();
    let n = WGS84_A / (1.0 - e2 * lat.sin().powi(2)).sqrt();
    let x = (n + alt) * lat.cos() * lon.cos();
    let y = (n + alt) * lat.cos() * lon.sin();
    let z = (n * (1.0 - e2) + alt) * lat.sin();
    (x, y, z)
}

/// `[x, y, z]` ECEF meters -> `[lat_deg, lon_deg, alt_m]` — iterative
/// (a fixed-point refinement on latitude/altitude), not a closed-form
/// solution: simpler to get right than Bowring's method, and five
/// iterations converges to sub-millimeter accuracy for any point near
/// Earth's surface, the only regime this builtin is for.
fn ecef_to_lla(x: f64, y: f64, z: f64) -> (f64, f64, f64) {
    let e2 = wgs84_e2();
    let lon = y.atan2(x);
    let p = (x * x + y * y).sqrt();
    let mut lat = z.atan2(p * (1.0 - e2));
    let mut alt = 0.0;
    for _ in 0..5 {
        let n = WGS84_A / (1.0 - e2 * lat.sin().powi(2)).sqrt();
        alt = p / lat.cos() - n;
        lat = z.atan2(p * (1.0 - e2 * n / (n + alt)));
    }
    (lat.to_degrees(), lon.to_degrees(), alt)
}

/// The rotation from ECEF-relative-to-reference into local East-North-Up
/// — shared by `ecef_to_enu`/`enu_to_ecef`, which apply it (and its
/// transpose/inverse, the same rotation matrix run backwards) in
/// opposite directions.
fn enu_rotation(ref_lat_deg: f64, ref_lon_deg: f64) -> [[f64; 3]; 3] {
    let lat = ref_lat_deg.to_radians();
    let lon = ref_lon_deg.to_radians();
    [
        [-lon.sin(), lon.cos(), 0.0],
        [-lat.sin() * lon.cos(), -lat.sin() * lon.sin(), lat.cos()],
        [lat.cos() * lon.cos(), lat.cos() * lon.sin(), lat.sin()],
    ]
}

fn ecef_to_enu(ecef: (f64, f64, f64), ref_lla: (f64, f64, f64)) -> (f64, f64, f64) {
    let ref_ecef = lla_to_ecef(ref_lla.0, ref_lla.1, ref_lla.2);
    let d = (ecef.0 - ref_ecef.0, ecef.1 - ref_ecef.1, ecef.2 - ref_ecef.2);
    let r = enu_rotation(ref_lla.0, ref_lla.1);
    (
        r[0][0] * d.0 + r[0][1] * d.1 + r[0][2] * d.2,
        r[1][0] * d.0 + r[1][1] * d.1 + r[1][2] * d.2,
        r[2][0] * d.0 + r[2][1] * d.1 + r[2][2] * d.2,
    )
}

fn enu_to_ecef(enu: (f64, f64, f64), ref_lla: (f64, f64, f64)) -> (f64, f64, f64) {
    let ref_ecef = lla_to_ecef(ref_lla.0, ref_lla.1, ref_lla.2);
    let r = enu_rotation(ref_lla.0, ref_lla.1);
    // The inverse of a rotation matrix is its transpose.
    let d = (
        r[0][0] * enu.0 + r[1][0] * enu.1 + r[2][0] * enu.2,
        r[0][1] * enu.0 + r[1][1] * enu.1 + r[2][1] * enu.2,
        r[0][2] * enu.0 + r[1][2] * enu.1 + r[2][2] * enu.2,
    );
    (ref_ecef.0 + d.0, ref_ecef.1 + d.1, ref_ecef.2 + d.2)
}

/// Initial great-circle bearing (degrees, `[0, 360)`) from `(lat1,lon1)`
/// to `(lat2,lon2)`, both in decimal degrees.
fn bearing_deg(lat1: f64, lon1: f64, lat2: f64, lon2: f64) -> f64 {
    let lat1 = lat1.to_radians();
    let lat2 = lat2.to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let y = dlon.sin() * lat2.cos();
    let x = lat1.cos() * lat2.sin() - lat1.sin() * lat2.cos() * dlon.cos();
    let deg = y.atan2(x).to_degrees();
    (deg + 360.0) % 360.0
}

// ---- linear Kalman filter ----------------------------------------------

fn mat_mul_f64(a: &[f64], ar: usize, ac: usize, b: &[f64], bc: usize) -> Vec<f64> {
    let mut out = vec![0.0; ar * bc];
    for i in 0..ar {
        for j in 0..bc {
            out[i * bc + j] = (0..ac).map(|k| a[i * ac + k] * b[k * bc + j]).sum();
        }
    }
    out
}

fn mat_vec_mul_f64(a: &[f64], ar: usize, ac: usize, v: &[f64]) -> Vec<f64> {
    (0..ar).map(|i| (0..ac).map(|k| a[i * ac + k] * v[k]).sum()).collect()
}

fn mat_transpose_f64(a: &[f64], r: usize, c: usize) -> Vec<f64> {
    let mut out = vec![0.0; r * c];
    for i in 0..r {
        for j in 0..c {
            out[j * r + i] = a[i * c + j];
        }
    }
    out
}

fn vec_add_f64(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| x + y).collect()
}

fn vec_sub_f64(a: &[f64], b: &[f64]) -> Vec<f64> {
    a.iter().zip(b).map(|(x, y)| x - y).collect()
}

/// `x' = F x`, `P' = F P F^T + Q` — the linear KF prediction step.
fn kf_predict(x: &[f64], p: &[f64], f: &[f64], q: &[f64], n: usize) -> (Vec<f64>, Vec<f64>) {
    let x_new = mat_vec_mul_f64(f, n, n, x);
    let ft = mat_transpose_f64(f, n, n);
    let fp = mat_mul_f64(f, n, n, p, n);
    let fpft = mat_mul_f64(&fp, n, n, &ft, n);
    let p_new = vec_add_f64(&fpft, q);
    (x_new, p_new)
}

/// `y = z - Hx`, `S = HPH^T + R`, `K = PH^T S^-1`, `x' = x + Ky`,
/// `P' = (I - KH)P` — the linear KF update step. `None` if `S` is
/// singular (reuses `matrix_inv`'s own Gauss-Jordan directly, now that
/// both take plain `&[f64]` — see `eval_builtin`'s `kf_update_state`/
/// `kf_update_cov` arm).
fn kf_update(x: &[f64], p: &[f64], z: &[f64], h: &[f64], r: &[f64], n: usize, m: usize) -> Option<(Vec<f64>, Vec<f64>)> {
    let hx = mat_vec_mul_f64(h, m, n, x);
    let y = vec_sub_f64(z, &hx);
    let ht = mat_transpose_f64(h, m, n);
    let hp = mat_mul_f64(h, m, n, p, n);
    let hpht = mat_mul_f64(&hp, m, n, &ht, m);
    let s = vec_add_f64(&hpht, r);
    let s_inv = matrix_inv(&s, m)?;
    let pht = mat_mul_f64(p, n, n, &ht, m);
    let k = mat_mul_f64(&pht, n, m, &s_inv, m);
    let ky = mat_vec_mul_f64(&k, n, m, &y);
    let x_new = vec_add_f64(x, &ky);
    let kh = mat_mul_f64(&k, n, m, h, n);
    let mut i_minus_kh = vec![0.0; n * n];
    for i in 0..n {
        for j in 0..n {
            i_minus_kh[i * n + j] = if i == j { 1.0 } else { 0.0 } - kh[i * n + j];
        }
    }
    let p_new = mat_mul_f64(&i_minus_kh, n, n, p, n);
    Some((x_new, p_new))
}

/// Every builtin's evaluation, dispatched by name — `typeck.rs`'s
/// `infer_builtin_call` already proved `args`' shapes/types are legal
/// for whichever `name` this is (see `ast.rs::BUILTIN_NAMES`'s doc
/// comment for why the two dispatches are independent, not a shared
/// table), so every arm here just computes; a mismatched pattern is
/// `unreachable!()`, the same "the checker is the real gate" convention
/// this whole file already follows.
fn eval_builtin(name: &str, args: &[Value], span: Span, rng: &std::cell::RefCell<Option<RngState>>) -> Result<Value, RuntimeError> {
    match name {
        "rand_seed" => {
            let Value::Int(seed) = &args[0] else { unreachable!("typeck.rs already proved this is an integer") };
            *rng.borrow_mut() = Some(RngState::seed(*seed as u64));
            Ok(Value::Unit)
        }
        "sleep_ms" => {
            let Value::Int(ms) = &args[0] else { unreachable!("typeck.rs already proved this is an integer") };
            std::thread::sleep(std::time::Duration::from_millis((*ms).max(0) as u64));
            Ok(Value::Unit)
        }
        "rand_f64" => match rng.borrow_mut().as_mut() {
            Some(r) => Ok(Value::Float(r.next_f64())),
            None => Err(RuntimeError { kind: ErrorKind::RngNotSeeded, span }),
        },
        "rand_gaussian" => {
            let (Value::Float(mean), Value::Float(stddev)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are f64")
            };
            match rng.borrow_mut().as_mut() {
                Some(r) => Ok(Value::Float(r.next_gaussian(*mean, *stddev))),
                None => Err(RuntimeError { kind: ErrorKind::RngNotSeeded, span }),
            }
        }
        "distance" => {
            let (Value::Vector(a), Value::Vector(b)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are Vector(f64, _) of matching length")
            };
            let sum_sq: f64 = a.iter().zip(b.iter()).map(|(x, y)| (as_f64(x) - as_f64(y)).powi(2)).sum();
            Ok(Value::Float(sum_sq.sqrt()))
        }
        "bearing" => {
            let (Value::Vector(from), Value::Vector(to)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are Vector(f64, 2)")
            };
            Ok(Value::Float(bearing_deg(as_f64(&from[0]), as_f64(&from[1]), as_f64(&to[0]), as_f64(&to[1]))))
        }
        "lla_to_ecef" => {
            let Value::Vector(lla) = &args[0] else { unreachable!("typeck.rs already proved this is a Vector(f64, 3)") };
            let (x, y, z) = lla_to_ecef(as_f64(&lla[0]), as_f64(&lla[1]), as_f64(&lla[2]));
            Ok(Value::Vector(Arc::from(vec![Value::Float(x), Value::Float(y), Value::Float(z)])))
        }
        "ecef_to_lla" => {
            let Value::Vector(ecef) = &args[0] else { unreachable!("typeck.rs already proved this is a Vector(f64, 3)") };
            let (lat, lon, alt) = ecef_to_lla(as_f64(&ecef[0]), as_f64(&ecef[1]), as_f64(&ecef[2]));
            Ok(Value::Vector(Arc::from(vec![Value::Float(lat), Value::Float(lon), Value::Float(alt)])))
        }
        "ecef_to_enu" | "enu_to_ecef" => {
            let (Value::Vector(a), Value::Vector(refp)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are Vector(f64, 3)")
            };
            let av = (as_f64(&a[0]), as_f64(&a[1]), as_f64(&a[2]));
            let refv = (as_f64(&refp[0]), as_f64(&refp[1]), as_f64(&refp[2]));
            let out = if name == "ecef_to_enu" { ecef_to_enu(av, refv) } else { enu_to_ecef(av, refv) };
            Ok(Value::Vector(Arc::from(vec![Value::Float(out.0), Value::Float(out.1), Value::Float(out.2)])))
        }
        "kf_predict_state" | "kf_predict_cov" => {
            let (Value::Vector(x), Value::Matrix(p, n, _), Value::Matrix(f, _, _), Value::Matrix(q, _, _)) =
                (&args[0], &args[1], &args[2], &args[3])
            else {
                unreachable!("typeck.rs already proved x/P/F/Q are Vector(f64,n)/Matrix(f64,n,n) each")
            };
            let n = *n;
            let xv: Vec<f64> = x.iter().map(as_f64).collect();
            let pv: Vec<f64> = p.iter().map(as_f64).collect();
            let fv: Vec<f64> = f.iter().map(as_f64).collect();
            let qv: Vec<f64> = q.iter().map(as_f64).collect();
            let (x_new, p_new) = kf_predict(&xv, &pv, &fv, &qv, n);
            if name == "kf_predict_state" {
                Ok(Value::Vector(Arc::from(x_new.into_iter().map(Value::Float).collect::<Vec<_>>())))
            } else {
                Ok(Value::Matrix(Arc::from(p_new.into_iter().map(Value::Float).collect::<Vec<_>>()), n, n))
            }
        }
        "kf_update_state" | "kf_update_cov" => {
            let (Value::Vector(x), Value::Matrix(p, n, _), Value::Vector(z), Value::Matrix(h, m, _), Value::Matrix(r, _, _)) =
                (&args[0], &args[1], &args[2], &args[3], &args[4])
            else {
                unreachable!("typeck.rs already proved x/P/z/H/R have matching dimensions")
            };
            let (n, m) = (*n, *m);
            let xv: Vec<f64> = x.iter().map(as_f64).collect();
            let pv: Vec<f64> = p.iter().map(as_f64).collect();
            let zv: Vec<f64> = z.iter().map(as_f64).collect();
            let hv: Vec<f64> = h.iter().map(as_f64).collect();
            let rv: Vec<f64> = r.iter().map(as_f64).collect();
            match kf_update(&xv, &pv, &zv, &hv, &rv, n, m) {
                Some((x_new, p_new)) => {
                    if name == "kf_update_state" {
                        Ok(Value::Vector(Arc::from(x_new.into_iter().map(Value::Float).collect::<Vec<_>>())))
                    } else {
                        Ok(Value::Matrix(Arc::from(p_new.into_iter().map(Value::Float).collect::<Vec<_>>()), n, n))
                    }
                }
                None => Err(RuntimeError { kind: ErrorKind::SingularMatrix, span }),
            }
        }
        "print" => {
            let rendered: Vec<String> = args.iter().map(Value::render).collect();
            println!("{}", rendered.join(" "));
            Ok(Value::Unit)
        }
        "transpose" => {
            let Value::Matrix(elems, rows, cols) = &args[0] else {
                unreachable!("typeck.rs already proved this is a Matrix")
            };
            let (rows, cols) = (*rows, *cols);
            let mut out: Vec<Value> = Vec::with_capacity(rows * cols);
            // SAFETY-free equivalent: build row-major output for the
            // transposed (cols x rows) shape by reading source
            // column-major -- simplest correct way to write this without
            // an uninitialized buffer.
            for j in 0..cols {
                for i in 0..rows {
                    out.push(elems[i * cols + j].clone());
                }
            }
            Ok(Value::Matrix(Arc::from(out), cols, rows))
        }
        "dot" => {
            let (Value::Vector(a), Value::Vector(b)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are Vectors")
            };
            let mut acc = scalar_binop(BinOp::Mul, &a[0], &b[0], span)?;
            for i in 1..a.len() {
                let prod = scalar_binop(BinOp::Mul, &a[i], &b[i], span)?;
                acc = scalar_binop(BinOp::Add, &acc, &prod, span)?;
            }
            Ok(acc)
        }
        "cross" => {
            let (Value::Vector(a), Value::Vector(b)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are Vector(_, 3)")
            };
            let term = |i: usize, j: usize, k: usize, l: usize| -> Result<Value, RuntimeError> {
                let p1 = scalar_binop(BinOp::Mul, &a[i], &b[j], span)?;
                let p2 = scalar_binop(BinOp::Mul, &a[k], &b[l], span)?;
                scalar_binop(BinOp::Sub, &p1, &p2, span)
            };
            let c0 = term(1, 2, 2, 1)?;
            let c1 = term(2, 0, 0, 2)?;
            let c2 = term(0, 1, 1, 0)?;
            Ok(Value::Vector(Arc::from(vec![c0, c1, c2])))
        }
        "zeros" => match args {
            [Value::Int(n)] => Ok(Value::Vector(Arc::from(vec![Value::Float(0.0); *n as usize]))),
            [Value::Int(r), Value::Int(c)] => {
                Ok(Value::Matrix(Arc::from(vec![Value::Float(0.0); (*r as usize) * (*c as usize)]), *r as usize, *c as usize))
            }
            _ => unreachable!("typeck.rs already validated zeros' arity and argument types"),
        },
        "ones" => match args {
            [Value::Int(n)] => Ok(Value::Vector(Arc::from(vec![Value::Float(1.0); *n as usize]))),
            [Value::Int(r), Value::Int(c)] => {
                Ok(Value::Matrix(Arc::from(vec![Value::Float(1.0); (*r as usize) * (*c as usize)]), *r as usize, *c as usize))
            }
            _ => unreachable!("typeck.rs already validated ones' arity and argument types"),
        },
        "identity" => {
            let [Value::Int(n)] = args else { unreachable!("typeck.rs already validated identity's argument") };
            let n = *n as usize;
            let mut out = vec![Value::Float(0.0); n * n];
            for i in 0..n {
                out[i * n + i] = Value::Float(1.0);
            }
            Ok(Value::Matrix(Arc::from(out), n, n))
        }
        "sum" => match &args[0] {
            Value::Vector(elems) => sum_all(elems, span),
            Value::Matrix(elems, _, _) => sum_all(elems, span),
            _ => unreachable!("typeck.rs already proved this is a Vector or Matrix"),
        },
        "len" => {
            let Value::Vector(elems) = &args[0] else { unreachable!("typeck.rs already proved this is a Vector") };
            Ok(Value::Int(elems.len() as i64))
        }
        "norm" => {
            let Value::Vector(elems) = &args[0] else { unreachable!("typeck.rs already proved this is a Vector(f64, _)") };
            let sum_sq: f64 = elems.iter().map(|v| as_f64(v) * as_f64(v)).sum();
            Ok(Value::Float(sum_sq.sqrt()))
        }
        "norm1" => {
            let Value::Vector(elems) = &args[0] else { unreachable!("typeck.rs already proved this is a Vector(f64, _)") };
            Ok(Value::Float(elems.iter().map(|v| as_f64(v).abs()).sum()))
        }
        "norm_inf" => {
            let Value::Vector(elems) = &args[0] else { unreachable!("typeck.rs already proved this is a Vector(f64, _)") };
            let m = elems.iter().map(|v| as_f64(v).abs()).fold(0.0_f64, f64::max);
            Ok(Value::Float(m))
        }
        "frobenius_norm" => {
            let Value::Matrix(elems, _, _) = &args[0] else { unreachable!("typeck.rs already proved this is a Matrix(f64, _, _)") };
            let sum_sq: f64 = elems.iter().map(|v| as_f64(v) * as_f64(v)).sum();
            Ok(Value::Float(sum_sq.sqrt()))
        }
        "trace" => {
            let Value::Matrix(elems, n, _) = &args[0] else { unreachable!("typeck.rs already proved this is a square Matrix") };
            let n = *n;
            let mut acc = elems[0].clone();
            for i in 1..n {
                acc = scalar_binop(BinOp::Add, &acc, &elems[i * n + i], span)?;
            }
            Ok(acc)
        }
        "det" => {
            let Value::Matrix(elems, n, _) = &args[0] else { unreachable!("typeck.rs already proved this is a square Matrix(f64, _, _)") };
            let a: Vec<f64> = elems.iter().map(as_f64).collect();
            Ok(Value::Float(matrix_det(&a, *n)))
        }
        "inv" => {
            let Value::Matrix(elems, n, _) = &args[0] else { unreachable!("typeck.rs already proved this is a square Matrix(f64, _, _)") };
            let a: Vec<f64> = elems.iter().map(as_f64).collect();
            match matrix_inv(&a, *n) {
                Some(v) => Ok(Value::Matrix(Arc::from(v.into_iter().map(Value::Float).collect::<Vec<_>>()), *n, *n)),
                None => Err(RuntimeError { kind: ErrorKind::SingularMatrix, span }),
            }
        }
        "solve" => {
            let (Value::Matrix(a, n, _), Value::Vector(b)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are a square Matrix(f64,_,_) and a matching Vector(f64,_)")
            };
            let a: Vec<f64> = a.iter().map(as_f64).collect();
            let b: Vec<f64> = b.iter().map(as_f64).collect();
            match matrix_solve(&a, *n, &b) {
                Some(x) => Ok(Value::Vector(Arc::from(x.into_iter().map(Value::Float).collect::<Vec<_>>()))),
                None => Err(RuntimeError { kind: ErrorKind::SingularMatrix, span }),
            }
        }
        "rank" => {
            let Value::Matrix(elems, rows, cols) = &args[0] else { unreachable!("typeck.rs already proved this is a Matrix(f64, _, _)") };
            let a: Vec<f64> = elems.iter().map(as_f64).collect();
            Ok(Value::Int(matrix_rank(&a, *rows, *cols) as i64))
        }
        "is_symmetric" => {
            let Value::Matrix(elems, n, _) = &args[0] else { unreachable!("typeck.rs already proved this is a square Matrix(f64, _, _)") };
            let n = *n;
            let sym = (0..n).all(|i| (0..n).all(|j| as_f64(&elems[i * n + j]) == as_f64(&elems[j * n + i])));
            Ok(Value::Bool(sym))
        }
        "is_diag" => {
            let Value::Matrix(elems, n, _) = &args[0] else { unreachable!("typeck.rs already proved this is a square Matrix(f64, _, _)") };
            let n = *n;
            let diag = (0..n).all(|i| (0..n).all(|j| i == j || as_f64(&elems[i * n + j]) == 0.0));
            Ok(Value::Bool(diag))
        }
        "is_square" => {
            let Value::Matrix(_, rows, cols) = &args[0] else { unreachable!("typeck.rs already proved this is a Matrix") };
            Ok(Value::Bool(rows == cols))
        }
        "json_parse" => {
            let Value::Str(s) = &args[0] else { unreachable!("typeck.rs already proved this is a str") };
            match serde_json::from_str::<JsonDoc>(s) {
                Ok(doc) => Ok(result_ok(Value::Json(Arc::new(doc)))),
                Err(e) => Ok(result_err(e.to_string())),
            }
        }
        "json_get" => {
            let (Value::Json(doc), Value::Str(key)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are json and str")
            };
            match json_field(doc, key) {
                Ok(v) => Ok(result_ok(Value::Json(Arc::new(v.clone())))),
                Err(e) => Ok(result_err(e)),
            }
        }
        "json_get_str" => {
            let (Value::Json(doc), Value::Str(key)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are json and str")
            };
            match json_field(doc, key).and_then(|v| {
                v.as_str().map(|s| Value::Str(Arc::from(s))).ok_or_else(|| json_type_err(key, "a string", v))
            }) {
                Ok(v) => Ok(result_ok(v)),
                Err(e) => Ok(result_err(e)),
            }
        }
        "json_get_i64" => {
            let (Value::Json(doc), Value::Str(key)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are json and str")
            };
            match json_field(doc, key).and_then(|v| {
                v.as_i64().map(Value::Int).ok_or_else(|| json_type_err(key, "an integer that fits in i64", v))
            }) {
                Ok(v) => Ok(result_ok(v)),
                Err(e) => Ok(result_err(e)),
            }
        }
        "json_get_f64" => {
            let (Value::Json(doc), Value::Str(key)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are json and str")
            };
            match json_field(doc, key)
                .and_then(|v| v.as_f64().map(Value::Float).ok_or_else(|| json_type_err(key, "a number", v)))
            {
                Ok(v) => Ok(result_ok(v)),
                Err(e) => Ok(result_err(e)),
            }
        }
        "json_get_bool" => {
            let (Value::Json(doc), Value::Str(key)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are json and str")
            };
            match json_field(doc, key)
                .and_then(|v| v.as_bool().map(Value::Bool).ok_or_else(|| json_type_err(key, "a bool", v)))
            {
                Ok(v) => Ok(result_ok(v)),
                Err(e) => Ok(result_err(e)),
            }
        }
        // `json_get_str`'s inverse — `typeck.rs`'s `("json_set_str", 3)`
        // arm has the full rationale. `null` (what `json_parse("null")`
        // and `json_parse("{}")`'s own inverse-of-empty-object give) is
        // treated as "start a fresh object," not an error — the common
        // "I have no object yet, just a value to set" case shouldn't need
        // a separate object-construction builtin of its own.
        "json_set_str" => {
            let (Value::Json(doc), Value::Str(key), Value::Str(value)) = (&args[0], &args[1], &args[2]) else {
                unreachable!("typeck.rs already proved these are json, str and str")
            };
            let mut obj = match doc.as_ref() {
                JsonDoc::Object(m) => m.clone(),
                JsonDoc::Null => serde_json::Map::new(),
                other => return Ok(result_err(format!("json_set_str: expected a json object (or null), found {}", json_kind(other)))),
            };
            obj.insert(key.to_string(), JsonDoc::String(value.to_string()));
            Ok(result_ok(Value::Json(Arc::new(JsonDoc::Object(obj)))))
        }
        "json_array_len" => {
            let Value::Json(doc) = &args[0] else { unreachable!("typeck.rs already proved this is json") };
            match doc.as_array() {
                Some(a) => Ok(result_ok(Value::Int(a.len() as i64))),
                None => Ok(result_err(format!("expected a JSON array, found {}", json_kind(doc)))),
            }
        }
        "json_array_get" => {
            let (Value::Json(doc), Value::Int(i)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are json and an integer")
            };
            match doc.as_array() {
                None => Ok(result_err(format!("expected a JSON array, found {}", json_kind(doc)))),
                Some(a) => match usize::try_from(*i).ok().and_then(|i| a.get(i)) {
                    Some(v) => Ok(result_ok(Value::Json(Arc::new(v.clone())))),
                    None => Ok(result_err(format!("index {i} out of bounds (JSON array has {} element(s))", a.len()))),
                },
            }
        }
        "http_get" => {
            let (Value::Str(host), Value::Int(port), Value::Str(path)) = (&args[0], &args[1], &args[2]) else {
                unreachable!("typeck.rs already proved these are str/i64/str")
            };
            Ok(http_request(host, *port, "GET", path, None))
        }
        "http_post" => {
            let (Value::Str(host), Value::Int(port), Value::Str(path), Value::Str(body)) =
                (&args[0], &args[1], &args[2], &args[3])
            else {
                unreachable!("typeck.rs already proved these are str/i64/str/str")
            };
            Ok(http_request(host, *port, "POST", path, Some(body)))
        }
        "https_get" => {
            let (Value::Str(host), Value::Int(port), Value::Str(path)) = (&args[0], &args[1], &args[2]) else {
                unreachable!("typeck.rs already proved these are str/i64/str")
            };
            Ok(https_request(host, *port, "GET", path, None))
        }
        "https_post" => {
            let (Value::Str(host), Value::Int(port), Value::Str(path), Value::Str(body)) =
                (&args[0], &args[1], &args[2], &args[3])
            else {
                unreachable!("typeck.rs already proved these are str/i64/str/str")
            };
            Ok(https_request(host, *port, "POST", path, Some(body)))
        }
        // Row 12: relying-party identity builtins.
        "oidc_validate_token" => {
            let (Value::Str(token), Value::Str(expected_issuer), Value::Str(expected_audience), Value::Str(jwks_json)) =
                (&args[0], &args[1], &args[2], &args[3])
            else {
                unreachable!("typeck.rs already proved these are four strs")
            };
            match validate_oidc_token(token, expected_issuer, expected_audience, jwks_json) {
                Ok(v) => Ok(result_ok(v)),
                Err(e) => Ok(result_err(e)),
            }
        }
        "check_role" => {
            let (identity, Value::Str(role)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved identity:VerifiedIdentity and role:str")
            };
            match identity_has_role(identity, role) {
                Ok(true) => Ok(result_ok(Value::Struct(Arc::from("RoleView"), Arc::from(vec![Value::Str(Arc::from(role.clone()))])))),
                Ok(false) => Ok(result_err(format!("insufficient role: `{role}`"))),
                Err(e) => Ok(result_err(e)),
            }
        }
        "extract_claim" => {
            let (identity, Value::Str(claim_name)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved identity:VerifiedIdentity and name:str")
            };
            match identity_claim(identity, claim_name) {
                Ok(s) => Ok(result_ok(Value::Struct(Arc::from("ClaimView"), Arc::from(vec![Value::Str(Arc::from(s))])))),
                Err(e) => Ok(result_err(e)),
            }
        }
        // `check_role`/`extract_claim`'s dotted-path siblings (see
        // `identity_has_role_at`/`identity_claim_at`'s doc comments) --
        // additive, not a revision of the two builtins above.
        "check_role_path" => {
            let (identity, Value::Str(path), Value::Str(role)) = (&args[0], &args[1], &args[2]) else {
                unreachable!("typeck.rs already proved identity:VerifiedIdentity, path:str, and role:str")
            };
            match identity_has_role_at(identity, path, role) {
                Ok(true) => Ok(result_ok(Value::Struct(Arc::from("RoleView"), Arc::from(vec![Value::Str(Arc::from(role.clone()))])))),
                Ok(false) => Ok(result_err(format!("insufficient role: `{role}`"))),
                Err(e) => Ok(result_err(e)),
            }
        }
        "extract_claim_path" => {
            let (identity, Value::Str(path)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved identity:VerifiedIdentity and path:str")
            };
            match identity_claim_at(identity, path) {
                Ok(s) => Ok(result_ok(Value::Struct(Arc::from("ClaimView"), Arc::from(vec![Value::Str(Arc::from(s))])))),
                Err(e) => Ok(result_err(e)),
            }
        }
        "identity_expired" => {
            let (Value::Struct(_, fields), Value::Int(now)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved identity:VerifiedIdentity and now:i64")
            };
            let expires_at = match &fields[3] {
                Value::Int(n) => *n,
                _ => unreachable!("VerifiedIdentity.expires_at is i64"),
            };
            Ok(Value::Bool(*now > expires_at))
        }
        // Row 12 continued: session, refresh, revocation, API-key.
        "create_application_session" => {
            let Value::Struct(_, fields) = &args[0] else {
                unreachable!("typeck.rs already proved identity:VerifiedIdentity")
            };
            let subject = match &fields[0] {
                Value::Str(s) => s.as_ref(),
                _ => unreachable!("VerifiedIdentity.subject is str"),
            };
            let issuer = match &fields[1] {
                Value::Str(s) => s.as_ref(),
                _ => unreachable!("VerifiedIdentity.issuer is str"),
            };
            let now = 1700000000_i64; // deterministic mock timestamp
            Ok(application_session_value(subject, issuer, now))
        }
        "session_cookie" => {
            let Value::Struct(_, fields) = &args[0] else {
                unreachable!("typeck.rs already proved session:ApplicationSession")
            };
            let session_id = match &fields[0] {
                Value::Str(s) => s.as_ref(),
                _ => unreachable!("ApplicationSession.session_id is str"),
            };
            let cookie = format!(
                "session={}; HttpOnly; Secure; SameSite=Strict; Max-Age={}",
                session_id,
                8 * 60 * 60
            );
            Ok(Value::Str(Arc::from(cookie)))
        }
        "new_refresh_token" => {
            let Value::Int(expires_at) = &args[0] else { unreachable!("typeck.rs already proved expires_at:i64") };
            Ok(refresh_token_value(*expires_at))
        }
        "exchange_refresh_token" => {
            let (Value::Struct(_, id_fields), Value::Struct(_, rt_fields), Value::Int(now)) =
                (&args[0], &args[1], &args[2])
            else {
                unreachable!("typeck.rs already proved identity, refresh handle, now")
            };
            let rt_expires = match &rt_fields[1] {
                Value::Int(n) => *n,
                _ => unreachable!("RefreshTokenHandle.expires_at is i64"),
            };
            if *now > rt_expires {
                return Ok(result_err("refresh token expired".to_string()));
            }
            let subject = match &id_fields[0] {
                Value::Str(s) => s.as_ref(),
                _ => unreachable!("VerifiedIdentity.subject is str"),
            };
            let issuer = match &id_fields[1] {
                Value::Str(s) => s.as_ref(),
                _ => unreachable!("VerifiedIdentity.issuer is str"),
            };
            let audience = match &id_fields[2] {
                Value::Str(s) => s.as_ref(),
                _ => unreachable!("VerifiedIdentity.audience is str"),
            };
            let claims_json = match &id_fields[5] {
                Value::Str(s) => s.as_ref(),
                _ => unreachable!("VerifiedIdentity.claims_json is str"),
            };
            let new_exp = rt_expires;
            let new_iat = *now;
            let new_identity = verified_identity_value(subject, issuer, audience, new_exp, new_iat, claims_json);
            Ok(result_ok(new_identity))
        }
        "check_revocation" => {
            let Value::Struct(_, fields) = &args[0] else {
                unreachable!("typeck.rs already proved identity:VerifiedIdentity")
            };
            let claims_json = match &fields[5] {
                Value::Str(s) => s,
                _ => unreachable!("VerifiedIdentity.claims_json is str"),
            };
            let revoked: bool = serde_json::from_str(claims_json)
                .ok()
                .and_then(|doc: JsonDoc| doc.get("revoked").and_then(|v| v.as_bool()))
                .unwrap_or(false);
            Ok(Value::Bool(revoked))
        }
        "validate_api_key" => {
            let (Value::Str(api_key), Value::Str(expected_hash)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved two strs")
            };
            if !constant_time_eq(sha256_hex(api_key).as_str(), expected_hash.as_ref()) {
                return Ok(result_err("invalid API key".to_string()));
            }
            let claims_json = r#"{"service_account":"api-client","roles":["physician"],"department":"radiology"}"#;
            let identity = verified_identity_value("api-client", "api-key-issuer", "api-key-audience", 2000000000, 1700000000, claims_json);
            Ok(result_ok(identity))
        }
        "sha256_hex" => {
            let Value::Str(a) = &args[0] else { unreachable!("typeck.rs already proved this is a str") };
            let digest = if args.len() == 2 {
                let Value::Str(b) = &args[1] else { unreachable!("typeck.rs already proved this is a str") };
                sha256_hex_chain(&[a, b])
            } else {
                sha256_hex(a)
            };
            Ok(Value::Str(Arc::from(digest)))
        }
        "constant_time_str_eq" => {
            let (Value::Str(a), Value::Str(b)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved two strs")
            };
            Ok(Value::Bool(constant_time_eq(a, b)))
        }
        // DB, layer 1 + layer 2 (`Ty::Db`'s doc comment; `dbconn.rs`'s
        // module doc for the SQLite-vs-Postgres dispatch).
        "db_connect" => {
            let Value::Str(conn_str) = &args[0] else { unreachable!("typeck.rs already proved this is a str") };
            match crate::dbconn::connect(conn_str.as_ref()) {
                Ok(conn) => Ok(result_ok(Value::Db(Arc::new(Mutex::new(Some(conn)))))),
                Err(e) => Ok(result_err(e)),
            }
        }
        "db_query" => {
            let (Value::Db(slot), Value::Str(sql)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are db and str")
            };
            let mut guard = slot.lock().unwrap();
            match guard.as_mut() {
                None => Ok(result_err("this db connection was already stopped")),
                Some(conn) => match sql_bind_params(&args[2..]) {
                    Ok(params) => match crate::dbconn::query(conn, sql, &params) {
                        Ok(doc) => Ok(result_ok(Value::Json(Arc::new(doc)))),
                        Err(e) => Ok(result_err(e)),
                    },
                    Err(e) => Ok(result_err(e)),
                },
            }
        }
        "db_execute" => {
            let (Value::Db(slot), Value::Str(sql)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are db and str")
            };
            let mut guard = slot.lock().unwrap();
            match guard.as_mut() {
                None => Ok(result_err("this db connection was already stopped")),
                Some(conn) => match sql_bind_params(&args[2..]) {
                    Ok(params) => match crate::dbconn::execute(conn, sql, &params) {
                        Ok(affected) => Ok(result_ok(Value::Int(affected))),
                        Err(e) => Ok(result_err(e)),
                    },
                    Err(e) => Ok(result_err(e)),
                },
            }
        }
        // `dec128` (`LANGUAGE.md` §5's "Decimal arithmetic", §6c/§6d) --
        // the only way in and out of a `dec128` value; `+`/`-`/`*`/`/`/
        // comparisons dispatch through `eval_binary`'s own `Value::
        // Dec128` arm instead, not through here.
        "dec_from_str" => {
            let Value::Str(s) = &args[0] else { unreachable!("typeck.rs already proved this is a str") };
            match Decimal::from_str(s) {
                Ok(d) => Ok(result_ok(Value::Dec128(d))),
                Err(e) => Ok(result_err(format!("malformed dec128: {e}"))),
            }
        }
        // `Decimal::new`'s own scale argument panics past 28 -- the
        // representation's own limit, not a data problem, the same trap
        // idiom `Int` overflow already relies on (see `scalar_binop`'s
        // Dec128 arm doc comment) -- so this stays infallible, no
        // `Result` wrapping.
        "dec_from_i64" => {
            let (Value::Int(v), Value::Int(scale)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are i64 and u32")
            };
            Ok(Value::Dec128(Decimal::new(*v, *scale as u32)))
        }
        "dec_to_str" => {
            let Value::Dec128(d) = &args[0] else { unreachable!("typeck.rs already proved this is a dec128") };
            Ok(Value::Str(Arc::from(d.to_string())))
        }
        // Round-half-to-even ("banker's rounding") -- `LANGUAGE.md` §5's
        // "the only rounding policy v1 ships," named explicitly rather
        // than picked silently (`rust_decimal`'s plain `round_dp`
        // defaults to away-from-zero instead, which is why this calls
        // the `_with_strategy` form rather than the default).
        "dec_round" => {
            let (Value::Dec128(d), Value::Int(scale)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are dec128 and u32")
            };
            Ok(Value::Dec128(d.round_dp_with_strategy(*scale as u32, rust_decimal::RoundingStrategy::MidpointNearestEven)))
        }
        "dec_scale" => {
            let Value::Dec128(d) = &args[0] else { unreachable!("typeck.rs already proved this is a dec128") };
            Ok(Value::Int(d.scale() as i64))
        }
        // MQ, layer 1 (`Ty::Mq`'s doc comment) -- Redis-backed.
        "mq_connect" => {
            let (Value::Str(host), Value::Int(port)) = (&args[0], &args[1]) else {
                unreachable!("typeck.rs already proved these are str and i64")
            };
            let opened: Result<redis::Connection, redis::RedisError> =
                redis::Client::open(format!("redis://{host}:{port}/")).and_then(|c| c.get_connection());
            match opened {
                Ok(conn) => Ok(result_ok(Value::Mq(Arc::new(Mutex::new(Some(MqConn(conn))))))),
                Err(e) => Ok(result_err(e.to_string())),
            }
        }
        "mq_publish" => {
            let (Value::Mq(slot), Value::Str(queue), Value::Str(message)) = (&args[0], &args[1], &args[2]) else {
                unreachable!("typeck.rs already proved these are mq, str and str")
            };
            let mut guard = slot.lock().unwrap();
            match guard.as_mut() {
                None => Ok(result_err("this mq connection was already stopped".to_string())),
                Some(conn) => match redis::cmd("LPUSH").arg(queue.as_ref()).arg(message.as_ref()).query::<i64>(&mut conn.0) {
                    Ok(_) => Ok(result_ok(Value::Unit)),
                    Err(e) => Ok(result_err(e.to_string())),
                },
            }
        }
        "mq_consume" => {
            let (Value::Mq(slot), Value::Str(queue), Value::Int(timeout_secs)) = (&args[0], &args[1], &args[2]) else {
                unreachable!("typeck.rs already proved these are mq, str and i64")
            };
            let mut guard = slot.lock().unwrap();
            match guard.as_mut() {
                None => Ok(result_err("this mq connection was already stopped".to_string())),
                Some(conn) => {
                    // BLPOP: blocks up to `timeout_secs` for the first
                    // element pushed to `queue`, `None` on timeout --
                    // matches `Result(str, str)`'s "timeout is an error,
                    // not a hang" contract from `typeck.rs`'s signature.
                    let popped = redis::cmd("BLPOP")
                        .arg(queue.as_ref())
                        .arg(*timeout_secs)
                        .query::<Option<(String, String)>>(&mut conn.0);
                    match popped {
                        Ok(Some((_key, message))) => Ok(result_ok(Value::Str(Arc::from(message)))),
                        Ok(None) => Ok(result_err(format!("mq_consume: no message on `{queue}` within {timeout_secs}s"))),
                        Err(e) => Ok(result_err(e.to_string())),
                    }
                }
            }
        }
        // Row 12's mock-only inverse of `oidc_validate_token` -- see
        // `mock_issue_token`'s own doc comment.
        "mock_issue_token" => {
            let (
                Value::Str(subject),
                Value::Str(issuer),
                Value::Str(audience),
                Value::Int(issued_at),
                Value::Int(ttl_secs),
                Value::Str(claims_json),
                Value::Str(jwks_json),
            ) = (&args[0], &args[1], &args[2], &args[3], &args[4], &args[5], &args[6])
            else {
                unreachable!("typeck.rs already proved these seven arg types")
            };
            match mock_issue_token(subject, issuer, audience, *issued_at, *ttl_secs, claims_json, jwks_json) {
                Ok(token) => Ok(result_ok(Value::Str(Arc::from(token)))),
                Err(e) => Ok(result_err(e)),
            }
        }
        _ => unreachable!("ast::BUILTIN_NAMES and eval_builtin's match must stay in sync"),
    }
}

fn mismatch(expected: impl Into<String>, found: impl Into<String>, span: Span) -> Signal {
    Signal::Err(RuntimeError {
        kind: ErrorKind::TypeMismatch { expected: expected.into(), found: found.into() },
        span,
    })
}

/// `Interpreter::call`'s hook point's `outcome_of` — `call()` returns a
/// plain `Result<Value, RuntimeError>`, not `SResult`/`Signal`.
fn outcome_of_runtime_result(result: &Result<Value, RuntimeError>) -> observability::Outcome {
    match result {
        Ok(_) => observability::Outcome::Ok,
        Err(e) => observability::Outcome::Err(observability::error_kind_name(&e.kind)),
    }
}

/// `Interpreter::traced`'s `outcome_of` for every effectful `eval_expr`
/// arm, which all return `SResult<Value>` (`Result<Value, Signal>`).
fn outcome_of_signal_result(result: &SResult<Value>) -> observability::Outcome {
    match result {
        Ok(_) => observability::Outcome::Ok,
        Err(Signal::Err(e)) => observability::Outcome::Err(observability::error_kind_name(&e.kind)),
        // `Signal::Return` only ever originates from a `Stmt::Return`
        // unwinding through `exec_block`/`call()` — none of the leaf
        // `eval_expr` hook points wrapped with `traced` ever produce it,
        // but this stays total (no `unreachable!()`) rather than
        // assuming that stays true forever.
        Err(Signal::Return(_)) => observability::Outcome::Ok,
    }
}

/// Everything that can interrupt normal left-to-right evaluation.
/// `Signal::Return` unwinds through expression *and* statement evaluation
/// alike, all the way to the nearest `call()`.
enum Signal {
    Err(RuntimeError),
    Return(Value),
}

impl From<RuntimeError> for Signal {
    fn from(e: RuntimeError) -> Self {
        Signal::Err(e)
    }
}

type SResult<T> = Result<T, Signal>;

/// Bindings carry their declared `Ty` alongside the current `Value` so
/// `Expr::Assign` can re-check the new value against the *original*
/// declaration (goal.md row 4's Tier-2 placeholder), not just against
/// whatever kind of value happened to be there a moment ago.
struct Env {
    scopes: Vec<HashMap<String, (Value, Ty)>>,
}

#[derive(Debug)]
enum SetErr {
    NotFound,
}

impl Env {
    fn new() -> Self {
        Env { scopes: vec![HashMap::new()] }
    }
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn define(&mut self, name: &str, v: Value, ty: Ty) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), (v, ty));
    }
    fn get(&self, name: &str) -> Option<Value> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).map(|(v, _)| v.clone())
    }
    fn get_ty(&self, name: &str) -> Option<Ty> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).map(|(_, t)| t.clone())
    }
    fn set(&mut self, name: &str, v: Value) -> Result<(), SetErr> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                slot.0 = v;
                return Ok(());
            }
        }
        Err(SetErr::NotFound)
    }
}

/// Owns the program (via `Arc`, not a borrow — see module doc) plus a
/// name→index table built once so a lookup doesn't need to linear-scan
/// `program.fns`. Cheap to reconstruct: `Interpreter::new(Arc::clone(&p),
/// Arc::clone(&self.source))` is exactly what a spawned thread does to
/// get its own independent one.
pub struct Interpreter {
    program: Arc<Program>,
    fn_index: HashMap<String, usize>,
    /// The original source text, kept around only for `Expr::SpawnSandbox`
    /// (see its `eval_expr` arm): a sandboxed process is a *separate*
    /// `nirdosha` invocation, with no shared memory to hand it a parsed
    /// `Program` through — it re-lexes/parses/typechecks its own copy,
    /// written to a fresh temp file at spawn time. Every other feature in
    /// this file only ever needed the already-parsed `Program`; this is
    /// the first one that needs the raw text back.
    source: Arc<str>,
    /// Which binary a `sandbox` handle's child re-execs as. `None` (the
    /// default) means `std::env::current_exe()` at spawn time — correct
    /// for the real `nirdosha` CLI, but wrong for *any other* process
    /// embedding this interpreter: `current_exe()` resolves to whatever
    /// binary is actually running, which under `cargo test` is the test
    /// harness, not `nirdosha` — a real bug caught by writing an
    /// integration test that actually spawns a sandbox and checks what
    /// ran, not by inspection. `with_sandbox_exe` is the escape hatch,
    /// used by `tests/sandbox.rs` to point sandboxed children at the
    /// real, separately-built `nirdosha` binary instead.
    sandbox_exe: Option<std::path::PathBuf>,
    /// Observability layer 1 (`observability.rs`'s module doc):
    /// `None` by default — the exact "one field, one branch, no cost
    /// when disabled" mechanism the design plan requires. Every hook
    /// point in this file is `let Some(tracer) = &self.tracer else {
    /// return <original, untraced path>; };` followed by an
    /// `if !tracer.enabled() { return <untraced path>; }` (layer 2a —
    /// see `Interpreter::traced` and `Interpreter::call`) — when `None`,
    /// or when `Some` but dormant (no APM client connected to
    /// `--otel-port`), that's the *entire* cost paid: one `Option` check,
    /// or that plus one relaxed atomic load. Threaded into spawned
    /// threads' own fresh `Interpreter` the same cheap-`Arc::clone` way
    /// `sandbox_exe` already is (see `Expr::Spawn`'s closure below).
    /// `with_tracer` is the builder, mirroring `with_sandbox_exe`
    /// exactly.
    tracer: Option<Arc<observability::Tracer>>,
    /// `call()`'s hotspot-attribution tag: each function's inferred
    /// `ast::Effect` set (`effects::infer_effects`), computed lazily —
    /// only the first time a `tracer`-enabled `call()` actually needs
    /// it, via `effect_of_fn` — so a plain `tracer: None` run never pays
    /// this whole-call-graph fixed-point cost at all. Recomputed per
    /// `Interpreter` instance rather than threaded in from `typeck.rs`'s
    /// own internal call to `infer_effects` (that result isn't returned
    /// out) — the same "each pass redoes its own minimal shadow walk"
    /// idiom `effects.rs`'s own module doc already establishes for
    /// `refine.rs`/`smt.rs`.
    fn_effects: std::cell::OnceCell<HashMap<String, EffectSet>>,
    /// `rand_seed`/`rand_f64`/`rand_gaussian`'s state — "carried in the
    /// interpreter environment, not a global" (unified plan §4.3.1):
    /// this is per-`Interpreter`-*instance* state, not a Rust `static`,
    /// so independent runs (including concurrent `cargo test` runs)
    /// never share or race on a stream. `RefCell`, not `Mutex`: every
    /// `eval_expr`-family method already takes `&self`, and unlike
    /// `Value::Thread`/`Sandbox`/`Tcp` (which cross a real thread
    /// boundary), no single `Interpreter` *instance* is ever shared
    /// between threads — `Expr::Spawn`'s closure builds a brand new
    /// `Interpreter` for the spawned thread (see its doc comment below),
    /// so this field is only ever touched from the one thread that owns
    /// it. A spawned function's own RNG is therefore independent by
    /// default, un-seeded until it calls `rand_seed` itself — an honest,
    /// documented gap, not a claim that concurrent draws replay in a
    /// fixed order across nondeterministic OS thread scheduling.
    /// `None` until the first `rand_seed` call; `rand_f64`/
    /// `rand_gaussian` before that is `ErrorKind::RngNotSeeded`, not a
    /// silent implicit seed that would quietly undercut "deterministic
    /// by default."
    rng: std::cell::RefCell<Option<RngState>>,
    /// User-function call depth (every recursive/mutually-recursive
    /// `.nir` call goes through `call()`, this file's single choke
    /// point) — with no limit, a `.nir` program recursing deeply enough
    /// (its own logic, no forbidden syntax, no malice required) overflows
    /// the *Rust* call stack, which is an uncatchable process abort
    /// (SIGABRT/SIGSEGV via the stack guard page), not a `RuntimeError` —
    /// unlike an ordinary panic, `catch_unwind` at any call site (e.g.
    /// `serve.rs`'s per-request dispatch) cannot stop this from taking
    /// the whole process down. Checked and incremented/decremented
    /// around `run()` in `call()` below, turning what would otherwise be
    /// that abort into a normal, catchable `ErrorKind::CallStackOverflow`
    /// once too deep -- the fix for a live red-team finding (deep
    /// self-recursion abort the whole `nirdosha serve` process for every
    /// concurrent caller, not just the one that triggered it).
    call_depth: std::cell::Cell<usize>,
    /// Where `transact`'s durability log lives on disk -- always has
    /// *some* value, so a program that uses `transact` can't forget to
    /// turn durability on (no flag to omit). `Interpreter::new`'s default
    /// is a unique-per-instance file under `std::env::temp_dir()`, safe
    /// for parallel `cargo test` runs and for any bare `run(src: &str)`
    /// caller with no real source file to derive a stable path from, but
    /// with no cross-process replay continuity. `main.rs`'s `run`/`serve`
    /// commands override this via `with_transact_log_path` to
    /// `<source-file>.transact.db` (or `--transact-log <path>`) instead
    /// -- a stable, program-derived path a restart can find again, which
    /// is the whole point for real CLI/server usage.
    transact_log_path: std::path::PathBuf,
    /// The actual open connection, lazy -- see `Interpreter::transact_log`
    /// (this file) for why: only a program that actually contains a
    /// `transact` ever touches the filesystem for this at all.
    transact_log_cell: std::cell::OnceCell<Arc<TransactLog>>,
    /// Same role as `transact_log_path`, for `WORKFLOW.md`'s durable
    /// store -- default is a unique per-instance temp file
    /// (`default_workflow_log_path`); `main.rs`'s `serve` command
    /// overrides it via `with_workflow_log_path`/`--workflow-log <path>`.
    workflow_log_path: std::path::PathBuf,
    workflow_log_cell: std::cell::OnceCell<Arc<WorkflowLog>>,
    /// Real deadlock detection for this program run's `chan`/`thread`
    /// use — see `DeadlockRegistry`'s own doc comment for what's actually
    /// proven. `Interpreter::new`'s default is a fresh, empty registry
    /// (correct for a standalone run that never spawns); `Expr::Spawn`'s
    /// handler overwrites a *child* interpreter's copy with
    /// `Arc::clone(&self.deadlock_registry)` immediately after
    /// construction, the same "cheap-`Arc::clone` into every spawned
    /// thread's own fresh `Interpreter`" pattern `tracer`/`sandbox_exe`
    /// already use — never a `pub` builder, since nothing outside this
    /// file's own `Expr::Spawn` handler should ever inject one.
    deadlock_registry: Arc<DeadlockRegistry>,
}

/// Past this many nested `call()`s, fail cleanly instead of overflowing
/// the real Rust stack. Sized against `run_on_big_stack`'s explicit
/// stack, not the OS default -- on the *default* thread stack, a debug
/// build's unoptimized frames are large enough that a trivial single-
/// recursive-call function aborted for real around depth 100-120,
/// making a fixed depth counter alone unsafe unless set uncomfortably
/// low. See `run_on_big_stack`'s doc comment for the actual measured
/// numbers on the bigger stack this is paired with. High enough that no
/// legitimate hand-written `.nir` recursive function should ever hit it
/// (the same tradeoff `sys.setrecursionlimit`/JVM `-Xss` make in other
/// stack-based interpreters) -- this can't be a fully airtight guarantee
/// (a single call level with an extreme enough expression tree could
/// still cost more stack than this margin covers) but converts the
/// overwhelming majority of unbounded-recursion cases from an
/// uncatchable process abort into a normal, catchable error.
const MAX_CALL_DEPTH: usize = 2000;

/// Runs `f` on a dedicated thread with a large explicit stack, joins,
/// and returns its result -- every real entry point into user-`.nir`
/// execution (`run_main_on_big_stack`, `call_named_on_big_stack`) goes
/// through this rather than running on the calling thread directly.
///
/// This exists *alongside* `MAX_CALL_DEPTH`, not instead of it: a bigger
/// stack alone doesn't make unbounded recursion safe (it only raises the
/// threshold), and a stack-overflow guard-page hit is a hard process
/// abort in Rust regardless of which thread triggers it -- spawning a
/// thread does **not** contain the crash to just that thread the way
/// `catch_unwind` contains an ordinary panic. What the bigger stack
/// actually buys is headroom for `MAX_CALL_DEPTH` to be set at a number
/// generous enough for legitimate hand-written recursion without itself
/// being unsafe on the platform's default 8MB stack (measured: a debug
/// build overflowed for real around depth 100-120 on the default stack,
/// which would force an uncomfortably low depth counter on its own).
/// 256MB scales that debug-build threshold up by roughly the same
/// factor (measured: clean past depth 2000 on this stack where it
/// previously aborted before 120), which is what `MAX_CALL_DEPTH` above
/// is sized against.
///
/// Panics propagate through `.join()`'s `Err` and are re-raised on the
/// calling thread (matching every pre-existing caller's behavior before
/// this wrapper existed) -- containing *those* is `serve.rs`'s own
/// `catch_unwind` boundary around request dispatch, a separate concern
/// from this function's actual job.
pub fn run_on_big_stack<F, T>(f: F) -> T
where
    F: FnOnce() -> T + Send + 'static,
    T: Send + 'static,
{
    const STACK_SIZE: usize = 256 * 1024 * 1024;
    std::thread::Builder::new()
        .stack_size(STACK_SIZE)
        .spawn(f)
        .expect("failed to spawn interpreter worker thread")
        .join()
        .unwrap_or_else(|payload| std::panic::resume_unwind(payload))
}

/// A from-scratch, dependency-free, byte-for-byte-reproducible PRNG —
/// SplitMix64 (public domain; Vigna, 2015) for the underlying stream,
/// Box-Muller for `rand_gaussian`. Deliberately hand-rolled rather than
/// pulling in the `rand` crate's trait ecosystem: the entire point of
/// this builtin is bitwise reproducibility across runs, which a small,
/// fully-specified algorithm implemented directly is easier to *keep*
/// reproducible (no risk of a transitive dependency bump silently
/// changing its output) than a general-purpose RNG crate's default
/// algorithm, which upstream reserves the right to change between
/// versions.
struct RngState {
    state: u64,
}

impl RngState {
    fn seed(seed: u64) -> Self {
        RngState { state: seed }
    }

    fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E3779B97F4A7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58476D1CE4E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D049BB133111EB);
        z ^ (z >> 31)
    }

    /// Uniform in `[0, 1)` — the standard "top 53 bits / 2^53" technique
    /// (53, not 64: that's exactly `f64`'s mantissa width, so every bit
    /// drawn is significant and the result is uniform over the floats
    /// actually representable in `[0, 1)`, not just the integers).
    fn next_f64(&mut self) -> f64 {
        (self.next_u64() >> 11) as f64 * (1.0 / (1u64 << 53) as f64)
    }

    /// Box-Muller transform — `next_f64()` is clamped away from exactly
    /// `0.0` (vanishingly unlikely, but `ln(0.0)` is `-inf`, which would
    /// propagate rather than erroring, the one sharp edge this transform
    /// has) before taking its log.
    fn next_gaussian(&mut self, mean: f64, stddev: f64) -> f64 {
        let u1 = self.next_f64().max(f64::MIN_POSITIVE);
        let u2 = self.next_f64();
        let z0 = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
        mean + stddev * z0
    }
}

/// One `transact` row's outcome after
/// `Interpreter::replay_pending_transactions` processes it -- see that
/// method's own doc comment for the recoverability boundary this
/// reflects.
#[derive(Debug, Clone, PartialEq)]
pub enum ReplayOutcome {
    /// Reached a terminal state (`committed`/`compensated`) during this
    /// replay pass.
    Resolved { txn_id: String, committed: bool },
    /// A write (`commit`/`compensate`) was attempted and its retry
    /// budget was exhausted again -- still non-terminal, still durably
    /// recorded, eligible to be retried on the next
    /// `replay_pending_transactions` call.
    StillPending { txn_id: String },
    /// Could not be resumed automatically -- see `reason`. Left exactly
    /// as it was in the durability log; needs an operator, or the
    /// original request's caller reissuing the same operation through a
    /// legitimate new path.
    Stuck { txn_id: String, reason: String },
}

/// One `workflow_pending_action` row's outcome from
/// `Interpreter::replay_pending_workflow_actions` — same three-way shape
/// as `ReplayOutcome` above, for the same reasons.
#[derive(Debug, Clone, PartialEq)]
pub enum WorkflowReplayOutcome {
    /// The action's retry succeeded this replay pass — marked `done`.
    Resolved { instance_id: i64, action: String },
    /// Still failing, retry budget exhausted again — left `pending`,
    /// eligible for the next replay.
    StillPending { instance_id: i64, action: String },
    /// Could not even be attempted (the callee no longer exists in this
    /// program, or its durably-logged arguments no longer match its
    /// current signature) — left exactly as it was, needs an operator.
    Stuck { instance_id: i64, action: String, reason: String },
}

impl Interpreter {
    pub fn new(program: Arc<Program>, source: Arc<str>) -> Self {
        let fn_index = program.fns.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
        Interpreter {
            program,
            fn_index,
            source,
            sandbox_exe: None,
            tracer: None,
            fn_effects: std::cell::OnceCell::new(),
            rng: std::cell::RefCell::new(None),
            call_depth: std::cell::Cell::new(0),
            transact_log_path: default_transact_log_path(),
            transact_log_cell: std::cell::OnceCell::new(),
            workflow_log_path: default_workflow_log_path(),
            workflow_log_cell: std::cell::OnceCell::new(),
            deadlock_registry: Arc::new(DeadlockRegistry::new()),
        }
    }

    /// Not `pub` — see `deadlock_registry`'s own doc comment for why only
    /// `Expr::Spawn`'s handler, in this same file, ever calls this.
    fn with_deadlock_registry(mut self, registry: Arc<DeadlockRegistry>) -> Self {
        self.deadlock_registry = registry;
        self
    }

    pub fn with_sandbox_exe(mut self, path: std::path::PathBuf) -> Self {
        self.sandbox_exe = Some(path);
        self
    }

    /// Overrides `transact_log_path`'s default (a unique per-instance
    /// temp file) with a stable, caller-chosen path -- `main.rs`'s `run`/
    /// `serve` commands use this to point at `<source-file>.transact.db`
    /// (or an explicit `--transact-log <path>`), so a restart's crash
    /// replay (`replay_pending_transactions`) can actually find the
    /// previous run's pending rows. Same builder shape as
    /// `with_sandbox_exe`/`with_tracer`.
    pub fn with_transact_log_path(mut self, path: std::path::PathBuf) -> Self {
        self.transact_log_path = path;
        self
    }

    /// Same role as `with_transact_log_path`, for `WORKFLOW.md`'s durable
    /// store -- `main.rs`'s `serve` command uses this to point at
    /// `<source-file>.workflow.db` (or an explicit `--workflow-log <path>`).
    pub fn with_workflow_log_path(mut self, path: std::path::PathBuf) -> Self {
        self.workflow_log_path = path;
        self
    }

    /// Observability layer 1's builder — same shape as `with_sandbox_exe`
    /// (see `tracer`'s own doc comment). `main.rs`'s `--otel-console` (via
    /// `lib.rs::run_with_tracer`/`run_diagnostic_with_tracer`) is the one
    /// caller that uses this today.
    pub fn with_tracer(mut self, tracer: Arc<observability::Tracer>) -> Self {
        self.tracer = Some(tracer);
        self
    }

    pub fn run_main(&self) -> Result<Value, RuntimeError> {
        // Always the real top-level entry point for a root interpreter's
        // execution thread — see `DeadlockRegistry::main_thread_id`'s doc
        // comment for why this can't be captured any earlier.
        self.deadlock_registry.mark_main_thread();
        let span = Span { line: 0, col: 0 };
        self.call("main", &[], span)
    }

    /// Same as `run_main`, but run on a thread with a much larger stack
    /// (see `run_on_big_stack`'s doc comment) — the entry point every
    /// caller of `run_main` outside this file's own tests should
    /// actually use, for the same reason `call_named`'s own big-stack
    /// twin (`call_named_on_big_stack`) exists.
    pub fn run_main_on_big_stack(self) -> Result<Value, RuntimeError> {
        run_on_big_stack(move || self.run_main())
    }

    /// A public entry point for calling an arbitrary named function
    /// directly, bypassing `main` — used by `main.rs`'s hidden
    /// `--sandbox-worker` mode (the process a `sandbox` handle actually
    /// spawns), which has no `main` of its own to run: it's told exactly
    /// which function to call and with what arguments on its own command
    /// line. `span` is a placeholder (`0:0`) for the same reason
    /// `run_main` already uses one — there's no real call-site source
    /// position for "the CLI asked for this."
    pub fn call_named(&self, name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
        self.deadlock_registry.mark_main_thread();
        self.call(name, args, Span { line: 0, col: 0 })
    }

    /// Resumes every `transact` row not yet in a terminal state --
    /// `TRANSACT.md`'s Layer 4. Call once at process startup (`main.rs`'s
    /// `run`/`serve` commands do, before ever calling `main`/serving a
    /// request) -- not automatic inside `Interpreter::new`, so a test or
    /// embedder that constructs an `Interpreter` doesn't get a surprise
    /// side effect.
    ///
    /// ## The recoverability boundary
    ///
    /// - `state == "commit_pending"` / `"compensate_pending"`: fully
    ///   resumable -- the callee name and its exact evaluated arguments
    ///   were durably captured before the crash
    ///   (`TransactLog::mark_commit_pending`/`mark_compensate_pending`),
    ///   so this is just `run_transact_write_slot` again, identical to
    ///   the live path.
    /// - `state == "network_done"` or `"pending"`: `network`'s result
    ///   (once re-confirmed, for `"pending"`) and `verify`'s exact
    ///   arguments are always reconstructable -- `verify`'s arguments are
    ///   statically restricted to `network`/`txn_id`
    ///   (`typeck.rs::TransactVerifyArgsMustBeImplicitBindings`),
    ///   specifically so this is always possible. But `commit`/
    ///   `compensate`'s own arguments can legitimately reference outer-
    ///   scope variables from the enclosing function call (an `amount`
    ///   parameter, e.g.) that crashed away with the original process and
    ///   were never durably captured -- that only happens once
    ///   `mark_commit_pending`/`mark_compensate_pending` has actually run.
    ///   If `verify` turns out to require a write this replay pass can't
    ///   durably reconstruct the arguments for, the row is reported
    ///   `ReplayOutcome::Stuck` rather than guessed at -- the one honest,
    ///   named gap in an otherwise-complete durability story: a crash
    ///   that preempts a write's own argument-logging needs the original
    ///   request's context back, not more cleverness in this file.
    pub fn replay_pending_transactions(&self) -> Result<Vec<ReplayOutcome>, RuntimeError> {
        let span = Span { line: 0, col: 0 };
        let tlog = self.transact_log(span)?;
        let unresolved = tlog.list_unresolved().map_err(|e| self.transact_log_err(e, span))?;
        Ok(unresolved.into_iter().map(|txn| self.replay_one(&tlog, txn, span)).collect())
    }

    fn replay_one(&self, tlog: &TransactLog, txn: PendingTxn, span: Span) -> ReplayOutcome {
        let txn_id = txn.txn_id.clone();

        match txn.state.as_str() {
            "commit_pending" => {
                let args = txn.commit_args.unwrap_or_default();
                return self.replay_write(tlog, txn_id, &txn.commit_fn, &args, span, "committed", true);
            }
            "compensate_pending" => {
                let args = txn.compensate_args.unwrap_or_default();
                let compensate_fn =
                    txn.compensate_fn.expect("state = compensate_pending implies a compensate slot exists");
                return self.replay_write(tlog, txn_id, &compensate_fn, &args, span, "compensated", false);
            }
            _ => {}
        }

        // `"pending"`: `network`'s outcome is unknown -- re-invoke it
        // (same `txn_id`, relies on downstream idempotency, see this
        // method's own doc comment) and durably record the result.
        // `"network_done"`: already have it.
        let network_result = if txn.state == "pending" {
            match self.call_network_with_retry(&txn.network_fn, &txn.network_args, span, txn.network_retry, txn.network_timeout) {
                Ok(v) => match tlog.record_network_result(&txn_id, &v) {
                    Ok(()) => v,
                    Err(e) => {
                        return ReplayOutcome::Stuck {
                            txn_id,
                            reason: format!("`network` succeeded but its result couldn't be durably recorded: {e}"),
                        }
                    }
                },
                Err(e) => {
                    return ReplayOutcome::Stuck { txn_id, reason: format!("`network` is still failing on replay: {e}") }
                }
            }
        } else {
            match txn.network_result {
                Some(v) => v,
                None => {
                    return ReplayOutcome::Stuck {
                        txn_id,
                        reason: "state is network_done but no network_result was recorded (should be unreachable)"
                            .to_string(),
                    }
                }
            }
        };

        let verify_args: Vec<Value> = txn
            .verify_arg_kinds
            .iter()
            .map(|k| match k.as_str() {
                "network" => network_result.clone(),
                "txn_id" => Value::Str(Arc::from(txn_id.as_str())),
                other => unreachable!("TransactLog::begin_pending only ever writes \"network\"/\"txn_id\", got {other:?}"),
            })
            .collect();
        let verified = match self.call(&txn.verify_fn, &verify_args, span) {
            Ok(Value::Bool(b)) => b,
            Ok(v) => {
                return ReplayOutcome::Stuck {
                    txn_id,
                    reason: format!("`verify` returned `{}`, not `bool` (should be unreachable)", v.ty_name()),
                }
            }
            Err(e) => return ReplayOutcome::Stuck { txn_id, reason: format!("`verify` failed on replay: {e}") },
        };
        let _ = tlog.record_verify(&txn_id, &verify_args, verified);

        if verified {
            let commit_args = match txn.commit_args {
                Some(a) => a,
                // A crash landing between `record_verify` and
                // `mark_commit_pending` leaves exactly this gap -- but if
                // every one of `commit`'s arguments is textually just
                // `network`/`txn_id` (`commit_arg_kinds`, captured
                // up front at `begin_pending` time, before either crash
                // window could even open), replay can rebuild them the
                // same way it already rebuilds `verify_args` above,
                // instead of giving up.
                None => match reconstruct_transact_args(&txn.commit_arg_kinds, &network_result, &txn_id) {
                    Some(args) => args,
                    None => {
                        return ReplayOutcome::Stuck {
                            txn_id,
                            reason: "verify succeeded but commit's arguments were never durably captured before \
                                     the crash, and at least one of them isn't reconstructable from `network`/ \
                                     `txn_id` alone (it references outer-scope data only the original call had) \
                                     -- this needs the original request's context, not more replay"
                                .to_string(),
                        }
                    }
                },
            };
            self.replay_write(tlog, txn_id, &txn.commit_fn, &commit_args, span, "committed", true)
        } else if let Some(compensate_fn) = txn.compensate_fn {
            let compensate_args = match txn.compensate_args {
                Some(a) => a,
                None => match txn
                    .compensate_arg_kinds
                    .as_deref()
                    .and_then(|kinds| reconstruct_transact_args(kinds, &network_result, &txn_id))
                {
                    Some(args) => args,
                    None => {
                        return ReplayOutcome::Stuck {
                            txn_id,
                            reason: "verify returned false but compensate's arguments were never durably captured \
                                     before the crash, and at least one of them isn't reconstructable from \
                                     `network`/`txn_id` alone (it references outer-scope data only the original \
                                     call had) -- this needs the original request's context, not more replay"
                                .to_string(),
                        }
                    }
                },
            };
            self.replay_write(tlog, txn_id, &compensate_fn, &compensate_args, span, "compensated", false)
        } else {
            match tlog.mark_terminal(&txn_id, "compensated") {
                Ok(()) => ReplayOutcome::Resolved { txn_id, committed: false },
                Err(_e) => ReplayOutcome::StillPending { txn_id },
            }
        }
    }

    fn replay_write(
        &self,
        tlog: &TransactLog,
        txn_id: String,
        fn_name: &str,
        args: &[Value],
        span: Span,
        terminal_state: &'static str,
        committed: bool,
    ) -> ReplayOutcome {
        let pending_err: fn(String) -> ErrorKind =
            if committed { |id| ErrorKind::TransactCommitPending { txn_id: id } } else { |id| ErrorKind::TransactCompensatePending { txn_id: id } };
        match self.run_transact_write_slot(tlog, &txn_id, fn_name, args, span, terminal_state, pending_err) {
            Ok(()) => ReplayOutcome::Resolved { txn_id, committed },
            Err(_) => ReplayOutcome::StillPending { txn_id },
        }
    }

    /// Same as `call_named`, but run on a thread with a much larger
    /// stack (see `run_on_big_stack`'s doc comment) — the entry point
    /// `main.rs`'s `--sandbox-worker` mode and `serve.rs`'s per-request
    /// dispatch actually use; both construct a fresh `Interpreter` right
    /// before this call and never touch it again afterward, so consuming
    /// `self` costs nothing at either call site.
    pub fn call_named_on_big_stack(self, name: &str, args: &[Value]) -> Result<Value, RuntimeError> {
        let name = name.to_string();
        let args = args.to_vec();
        run_on_big_stack(move || self.call_named(&name, &args))
    }

    fn find_fn(&self, name: &str) -> Option<&FnDecl> {
        self.fn_index.get(name).map(|&i| &self.program.fns[i])
    }

    /// Row 11 — `typeck.rs` already proved `name` is a real, uniquely
    /// registered struct name before any `Expr::Call`/`Ty::Named` this
    /// file evaluates could carry it, so this is a plain linear lookup,
    /// not a name-index table the way `find_fn` needs one (no spawned-
    /// thread call path ever looks a struct up by name the way a
    /// function is).
    fn find_struct(&self, name: &str) -> Option<&StructDecl> {
        self.program.structs.iter().find(|s| s.name == name)
    }

    /// `(owning EnumDecl, the variant itself)` for a variant name —
    /// mirrors `ast::TypeRegistry::find_variant`'s flat-namespace lookup,
    /// just over `&self.program` directly instead of a registry (this
    /// file has no `TypeRegistry` of its own to build; `typeck.rs`
    /// already proved every name it evaluates resolves).
    fn find_variant(&self, name: &str) -> Option<(&EnumDecl, &Variant)> {
        self.program.enums.iter().find_map(|e| e.variants.iter().find(|v| v.name == name).map(|v| (e, v)))
    }

    /// Row 11 layer 6 (generics) — walks `decl_ty` (a variant's own
    /// declared payload type, possibly containing a bare reference to
    /// one of `type_params`) opposite `val` (that position's actual
    /// runtime value), binding any parameter found bare to a best-effort
    /// concrete `Ty` recovered from `val`'s own shape (`value_shape_ty`).
    /// See `Expr::Match`'s `eval_expr` arm for why this file has to
    /// recover this from the *value*, not an expected-type context the
    /// way `typeck.rs`'s `bind_type_params` does from an *argument's own
    /// inferred type* — this interpreter never threads one through
    /// `eval_expr` at all (module doc).
    fn bind_type_params_from_value(&self, decl_ty: &Ty, val: &Value, type_params: &[String], subst: &mut HashMap<String, Ty>) {
        match decl_ty {
            Ty::Named(name, args) if args.is_empty() && type_params.iter().any(|p| p == name) => {
                subst.entry(name.clone()).or_insert_with(|| self.value_shape_ty(val));
            }
            Ty::Box(inner) => {
                if let Value::Boxed(v) = val {
                    self.bind_type_params_from_value(inner, v, type_params, subst);
                }
            }
            Ty::Ref(inner) => {
                if let Value::Ref(v) = val {
                    self.bind_type_params_from_value(inner, v, type_params, subst);
                }
            }
            Ty::Vector(inner, _) => {
                if let Value::Vector(elems) = val
                    && let Some(first) = elems.first()
                {
                    self.bind_type_params_from_value(inner, first, type_params, subst);
                }
            }
            _ => {}
        }
    }

    /// A reasonable, if imprecise, `Ty` for a bare runtime `Value` —
    /// integer width is inherently ambiguous at this level (`Value::Int`
    /// carries no declared width of its own; see its doc comment), so
    /// this always answers `Ty::I64`, the widest legal width, matching
    /// `typeck.rs::infer`'s own "untyped literal's default when nothing
    /// constrains it" convention. Used only as `bind_type_params_from_
    /// value`'s fallback source of truth for one `match` binding's own
    /// type — never anything load-bearing to a real static proof (that's
    /// already been done, by `typeck.rs`, before this file ever runs).
    /// `Value::Struct`/`Value::Enum` answer with their own name and *no*
    /// type arguments — a real, accepted imprecision for a nested
    /// generic struct/enum specifically (this interpreter doesn't carry
    /// a value's own resolved type arguments at runtime at all), not
    /// silently pretended away: a reassignment inside a `match` arm
    /// whose payload is itself another generic instantiation is the one
    /// narrow shape this can under-check, and only for that.
    fn value_shape_ty(&self, v: &Value) -> Ty {
        match v {
            Value::Int(_) => Ty::I64,
            Value::Float(_) => Ty::F64,
            Value::Bool(_) => Ty::Bool,
            Value::Unit => Ty::Unit,
            Value::Str(_) => Ty::Str,
            Value::Boxed(inner) => Ty::Box(Box::new(self.value_shape_ty(inner))),
            Value::Ref(inner) => Ty::Ref(Box::new(self.value_shape_ty(inner))),
            Value::Vector(elems) => {
                Ty::Vector(Box::new(elems.first().map(|e| self.value_shape_ty(e)).unwrap_or(Ty::F64)), elems.len())
            }
            Value::Struct(name, _) => Ty::Named(name.to_string(), Vec::new()),
            Value::Enum(name, _, _) => Ty::Named(name.to_string(), Vec::new()),
            _ => Ty::Error,
        }
    }

    /// The one place `Signal::Return` gets caught and turned back into a
    /// plain value — every nested `if`/block/expression underneath just
    /// propagates it with `?`.
    /// Evaluates one `transact` slot's arguments, then invokes its named
    /// function. `typeck.rs::infer_transact_slot` already proved this
    /// name isn't a builtin, so — unlike `Expr::Call`'s own arm, which
    /// has to dispatch both ways — this always goes through `self.call`.
    fn eval_transact_slot(&self, slot: &TransactSlot, env: &mut Env) -> SResult<Value> {
        let vals = self.eval_transact_slot_args(slot, env)?;
        self.call(&slot.name, &vals, slot.span).map_err(Signal::Err)
    }

    /// Just the argument-evaluation half of `eval_transact_slot` --
    /// `network`/`verify`/`commit`/`compensate`'s own `Expr::Transact`
    /// handling needs the evaluated `Value`s on their own, to durably log
    /// them (`TransactLog::begin_pending` and friends) *before* actually
    /// invoking the callee.
    fn eval_transact_slot_args(&self, slot: &TransactSlot, env: &mut Env) -> SResult<Vec<Value>> {
        let mut vals = Vec::with_capacity(slot.args.len());
        for a in &slot.args {
            vals.push(self.eval_expr(a, env)?);
        }
        Ok(vals)
    }

    /// `commit`/`compensate` (the two `transact` slots that retry on
    /// failure) share this: run `fn_name(args)`, treat either a trap or
    /// an `Err(_)` from a `Result<T, E>`-shaped return as a failure
    /// (closing the exact silent-discard hole that motivated this whole
    /// durability pass -- `commit`'s return type used to be "unconstrained
    /// and discarded", so a `db_execute` failure inside it was invisible),
    /// and retry with bounded exponential backoff. Never falls through to
    /// a different slot on failure -- `compensate` only ever runs because
    /// `verify` was `false`, a business decision, never because `commit`
    /// hit an infra fault (`TRANSACT.md`'s durability section: auto-
    /// compensating a real, already-`network`-succeeded effect because a
    /// local write hiccuped would be its own partial-failure-shaped bug).
    /// If the retry budget is exhausted, this traps with `pending_err`
    /// rather than guessing `true`/`false` -- the durable log entry is
    /// left exactly as `mark_commit_pending`/`mark_compensate_pending`
    /// left it, non-terminal, for `replay_pending_transactions` (a
    /// restart, or the same reconciliation run again later) to keep
    /// retrying independently of whatever the original caller did with
    /// the trap.
    fn run_transact_write_slot(
        &self,
        tlog: &TransactLog,
        txn_id: &str,
        fn_name: &str,
        args: &[Value],
        span: Span,
        terminal_state: &'static str,
        pending_err: impl Fn(String) -> ErrorKind,
    ) -> Result<(), RuntimeError> {
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt: u32 = 0;
        loop {
            let outcome = self.call(fn_name, args, span);
            let failed = match &outcome {
                Err(_) => true,
                Ok(v) => is_result_err(v),
            };
            if !failed {
                // The effect succeeded. If we cannot durably record that,
                // replay will re-run it later; for non-idempotent effects
                // that is a real bug, so treat a log-write failure here as a
                // runtime error rather than silently returning success.
                if let Err(e) = tlog.mark_terminal(txn_id, terminal_state) {
                    return Err(self.transact_log_err(e, span));
                }
                return Ok(());
            }
            attempt += 1;
            let _ = tlog.bump_attempts(txn_id);
            if attempt >= MAX_ATTEMPTS {
                return Err(RuntimeError { kind: pending_err(txn_id.to_string()), span });
            }
            std::thread::sleep(std::time::Duration::from_millis(20u64 << attempt.min(10)));
        }
    }

    /// TRANSACT.md's Layer 2: `network`'s own `retry`/`timeout`
    /// modifiers. `retry` is total attempts, not extra retries beyond
    /// the first (`None`/absent means exactly one attempt, matching
    /// TRANSACT.md's "default 1 -- i.e. no retry"); a trap *or* a
    /// timeout both count as a failed attempt. "Retry only reacts to a
    /// trap, never to `verify == false`" (TRANSACT.md's own Decisions)
    /// holds trivially here -- this only ever wraps the `network` call
    /// itself, `verify` hasn't even run yet. No backoff between
    /// attempts: TRANSACT.md's own protocol text doesn't call for one on
    /// `network` (unlike `run_transact_write_slot`'s `commit`/
    /// `compensate` retry, which is this file's own addition, not part
    /// of the original locked design) -- re-invokes the exact same call
    /// expression with the same already-evaluated `args`, per
    /// TRANSACT.md's own Layer 2 note.
    fn call_network_with_retry(
        &self,
        name: &str,
        args: &[Value],
        span: Span,
        retry: Option<i64>,
        timeout: Option<i64>,
    ) -> Result<Value, RuntimeError> {
        let max_attempts = retry.unwrap_or(1).max(1);
        let mut last_err = None;
        for _ in 0..max_attempts {
            match self.call_with_timeout(name, args, span, timeout) {
                Ok(v) => return Ok(v),
                Err(e) => last_err = Some(e),
            }
        }
        Err(last_err.expect("max_attempts >= 1, so the loop always runs at least once"))
    }

    /// One attempt of `name(args)`, aborted if `timeout` seconds elapse
    /// first. `None` skips the whole timeout apparatus and just calls
    /// `self.call` directly -- the common case, zero extra cost.
    ///
    /// A real wall-clock read and a real background thread -- an
    /// explicit, called-out departure from the determinism story
    /// (`goal.md`'s determinism section only covers `rand_seed`'s RNG
    /// stream; this is new, unavoidable nondeterminism, same honesty
    /// `SANDBOXING.md` already gives "`recv` can block forever"). This
    /// interpreter isn't `Sync` (`rng`'s `RefCell`, `call_depth`'s
    /// `Cell`), so the timed call can't run `self` on another thread the
    /// way a true preemptive timeout would -- instead, exactly the
    /// pattern `Expr::Spawn` already uses (see its own doc comment): a
    /// brand new `Interpreter` sharing only cheap `Arc` clones of the
    /// immutable program/source/tracer/sandbox_exe, sent its result over
    /// a channel. A `recv_timeout` past the deadline gives up waiting and
    /// reports `TransactNetworkTimedOut` -- but the spawned thread itself
    /// is *not* killed (Rust has no such mechanism); like `SANDBOXING.md`'s
    /// `recv`, it keeps running in the background and its eventual result
    /// (if any) is simply discarded when the channel's other end drops.
    fn call_with_timeout(&self, name: &str, args: &[Value], span: Span, timeout: Option<i64>) -> Result<Value, RuntimeError> {
        let Some(secs) = timeout else {
            return self.call(name, args, span);
        };
        let program = Arc::clone(&self.program);
        let source = Arc::clone(&self.source);
        let sandbox_exe = self.sandbox_exe.clone();
        let tracer = self.tracer.clone();
        let name_owned = name.to_string();
        let args_owned = args.to_vec();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut interp = Interpreter::new(program, source);
            if let Some(exe) = sandbox_exe {
                interp = interp.with_sandbox_exe(exe);
            }
            if let Some(t) = tracer {
                interp = interp.with_tracer(t);
            }
            let result = interp.call(&name_owned, &args_owned, span);
            let _ = tx.send(result);
        });
        match rx.recv_timeout(std::time::Duration::from_secs(secs.max(0) as u64)) {
            Ok(result) => result,
            Err(_) => Err(RuntimeError { kind: ErrorKind::TransactNetworkTimedOut { seconds: secs }, span }),
        }
    }

    /// Lazily opens (and caches) this interpreter's durability log --
    /// `OnceCell`, the same "computed at most once, only when a program
    /// actually needs it" idiom `fn_effects` already uses, so a program
    /// that never uses `transact` never touches the filesystem at all.
    /// The path itself (`transact_log_path`) always has *some* value
    /// (`Interpreter::new`'s default is a unique per-process temp file;
    /// `with_transact_log_path` overrides it) -- what's lazy here is only
    /// the actual file I/O of opening/creating it.
    fn transact_log(&self, span: Span) -> Result<Arc<TransactLog>, RuntimeError> {
        if let Some(log) = self.transact_log_cell.get() {
            return Ok(Arc::clone(log));
        }
        let log = Arc::new(TransactLog::open(&self.transact_log_path).map_err(|e| self.transact_log_err(e, span))?);
        // `OnceCell::set` failing here would mean another call already
        // won the race -- can't happen: every `eval_expr` call is on the
        // one thread that owns this `Interpreter` instance (see
        // `Interpreter::rng`'s doc comment for the identical single-
        // owning-thread argument), so this is really just "set, unwrap".
        let _ = self.transact_log_cell.set(Arc::clone(&log));
        Ok(log)
    }

    fn transact_log_err(&self, e: rusqlite::Error, span: Span) -> RuntimeError {
        RuntimeError { kind: ErrorKind::TransactLogUnavailable { message: e.to_string() }, span }
    }

    /// Same "open once, lazily, only if a program actually needs it"
    /// shape as `transact_log` -- see that method's doc comment.
    fn workflow_log(&self, span: Span) -> Result<Arc<WorkflowLog>, RuntimeError> {
        if let Some(log) = self.workflow_log_cell.get() {
            return Ok(Arc::clone(log));
        }
        let log = Arc::new(
            WorkflowLog::open(&self.workflow_log_path)
                .map_err(|message| RuntimeError { kind: ErrorKind::WorkflowLogUnavailable { message }, span })?,
        );
        let _ = self.workflow_log_cell.set(Arc::clone(&log));
        Ok(log)
    }

    fn workflow_log_err(&self, e: String, span: Span) -> RuntimeError {
        RuntimeError { kind: ErrorKind::WorkflowLogUnavailable { message: e }, span }
    }

    fn find_workflow(&self, name: &str) -> Option<&WorkflowDecl> {
        self.program.workflows.iter().find(|w| w.name == name)
    }

    /// Dispatches `send_email`/`send_sms`/`send_push`/`notify` and
    /// `workflow_lower.rs`'s six shared internal builtins — `Some(_)`
    /// when `name` is one of these ten (handled here, needs `self`/
    /// durable-log access `eval_builtin`'s free-function shape can't
    /// provide, the same reason `Expr::Transact` gets its own dedicated
    /// evaluation instead of going through `eval_builtin`); `None` for
    /// every other name, so `Expr::Call`'s normal dispatch falls through
    /// to `eval_builtin` unchanged.
    fn eval_workflow_builtin(&self, name: &str, args: &[Value], span: Span) -> SResult<Option<Value>> {
        match name {
            "__workflow_start" => self.workflow_start(args, span).map(Some),
            "__workflow_advance" => self.workflow_advance(args, span).map(Some),
            "__workflow_link_advance" => self.workflow_link_advance(args, span).map(Some),
            "__workflow_pending_for_me" => self.workflow_pending_for_me(args, span).map(Some),
            "__workflow_submitted_by_me" => self.workflow_submitted_by_me(args, span).map(Some),
            "__workflow_history" => self.workflow_history(args, span).map(Some),
            "send_email" => self.dispatch_send(args, span, "email").map(Some),
            "send_sms" => self.dispatch_send(args, span, "sms").map(Some),
            "send_push" => self.dispatch_send(args, span, "push").map(Some),
            "notify" => self.dispatch_notify(args, span).map(Some),
            _ => Ok(None),
        }
    }

    /// `list_<workflow>_pending_for_me`'s implementation (`WORKFLOW.md`'s
    /// "state ownership" section, read side): every instance of
    /// `workflow_name` whose *current* state's `owner` `identity`
    /// satisfies, as a JSON array of `{instance_id, state, state_label,
    /// events, data}` — `ui_gen_template.html`'s generated "Workflows"
    /// queue screen renders this directly, one row per instance, one
    /// button per `events` entry (that row's own current state's own
    /// outgoing event names, calling `advance_<workflow>` with whichever
    /// one is clicked). A state with no `owner` at all is never returned
    /// here — an unowned state is nobody's queue item by definition (any
    /// authenticated caller could still `advance_*` it directly, they
    /// just don't see it listed as "waiting on them" specifically).
    fn workflow_pending_for_me(&self, args: &[Value], span: Span) -> SResult<Value> {
        let Value::Str(workflow_name) = &args[0] else {
            unreachable!("typeck.rs already proved this is a str")
        };
        let identity = &args[1];
        let wf = self
            .find_workflow(workflow_name)
            .unwrap_or_else(|| unreachable!("workflow_lower.rs only ever emits calls naming a real workflow"));
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let instances = wlog.list_instances(workflow_name).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
        let mut out = Vec::new();
        for (instance_id, state_name, data_json) in instances {
            let Some(state) = wf.states.iter().find(|s| s.name == state_name) else { continue };
            let Some(owner) = state_owner(state) else { continue };
            let owned = identity_satisfies_owner(owner, identity).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
            if !owned {
                continue;
            }
            out.push(
                self.encode_instance_row(workflow_name, state, instance_id, &data_json, span)
                    .map_err(Signal::Err)?,
            );
        }
        Ok(result_ok(Value::Json(Arc::new(serde_json::Value::Array(out)))))
    }

    /// `list_<workflow>_submitted_by_me`'s implementation (`WORKFLOW.md`'s
    /// "who submitted this" section): every instance whose durably-
    /// recorded `started_by_subject` matches the caller's own `subject`
    /// — regardless of which state it's currently in, unlike
    /// `workflow_pending_for_me`'s owner-scoped read. A requester's own
    /// view of their submissions is read-only in the generated UI (no
    /// `events`/buttons rendered from this row shape), but the row
    /// itself still carries `events` for consistency/future reuse.
    fn workflow_submitted_by_me(&self, args: &[Value], span: Span) -> SResult<Value> {
        let Value::Str(workflow_name) = &args[0] else {
            unreachable!("typeck.rs already proved this is a str")
        };
        let identity = &args[1];
        let subject = identity_subject(identity);
        let wf = self
            .find_workflow(workflow_name)
            .unwrap_or_else(|| unreachable!("workflow_lower.rs only ever emits calls naming a real workflow"));
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let instances = wlog
            .list_instances_by_starter(workflow_name, &subject)
            .map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
        let mut out = Vec::new();
        for (instance_id, state_name, data_json) in instances {
            let Some(state) = wf.states.iter().find(|s| s.name == state_name) else { continue };
            out.push(
                self.encode_instance_row(workflow_name, state, instance_id, &data_json, span)
                    .map_err(Signal::Err)?,
            );
        }
        Ok(result_ok(Value::Json(Arc::new(serde_json::Value::Array(out)))))
    }

    /// Shared row shape for `workflow_pending_for_me`/
    /// `workflow_submitted_by_me`: `{instance_id, state, state_label,
    /// events, data}`, `data` re-decoded/re-encoded through the
    /// workflow's own `Data` struct type (not passed through raw) — the
    /// same typed round-trip `workflow_start`/`workflow_advance` already
    /// do, so a caller sees exactly the field set `data { ... }`
    /// declares, nothing stored-but-since-removed.
    fn encode_instance_row(
        &self,
        workflow_name: &str,
        state: &StateDecl,
        instance_id: i64,
        data_json: &str,
        span: Span,
    ) -> Result<serde_json::Value, RuntimeError> {
        let data_ty = Ty::Named(format!("{workflow_name}Data"), vec![]);
        let label = state.entries.iter().find(|(k, _)| k == "label").and_then(|(_, v)| match v {
            Expr::Str(s, _) => Some(s.clone()),
            _ => None,
        });
        let json_val: serde_json::Value = serde_json::from_str(data_json).unwrap_or(serde_json::Value::Null);
        let data_val =
            crate::serve::decode_value(&json_val, &data_ty, &self.program, 0).map_err(|e| self.workflow_log_err(e, span))?;
        let data_encoded = crate::serve::encode_value(&data_val, &self.program).map_err(|e| self.workflow_log_err(e, span))?;
        Ok(serde_json::json!({
            "instance_id": instance_id,
            "state": state.name,
            "state_label": label,
            "events": state.transitions.iter().map(|t| t.event.clone()).collect::<Vec<_>>(),
            "data": data_encoded,
        }))
    }

    /// `get_<workflow>_history`'s implementation (`WORKFLOW.md`'s "audit
    /// trail" section): the full, append-only transition log for one
    /// instance, oldest first, as a JSON array of `{from_state, to_state,
    /// event, actor_subject, via_link, comment, at}`. `identity` (the
    /// caller) isn't checked against anything here beyond `serve.rs::
    /// dispatch` already demanding *a* signed-in caller — see
    /// `workflow_lower.rs`'s own doc comment on this fn for the disclosed
    /// "no per-viewer ACL yet" simplification.
    fn workflow_history(&self, args: &[Value], span: Span) -> SResult<Value> {
        let Value::Str(_workflow_name) = &args[0] else {
            unreachable!("typeck.rs already proved this is a str")
        };
        let Value::Int(instance_id) = &args[1] else {
            unreachable!("typeck.rs already proved this is an i64")
        };
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let rows = wlog.list_history(*instance_id).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
        let out: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|(from_state, to_state, event, actor_subject, via_link, comment, at)| {
                serde_json::json!({
                    "from_state": from_state,
                    "to_state": to_state,
                    "event": event,
                    "actor_subject": actor_subject,
                    "via_link": via_link,
                    "comment": comment,
                    "at": at,
                })
            })
            .collect();
        Ok(result_ok(Value::Json(Arc::new(serde_json::Value::Array(out)))))
    }

    /// Runs every action in `actions` in order, in `env` (already carrying
    /// `instance_id`/`data`/`link_<Event>` bindings — see
    /// `workflow_start`/`workflow_advance`). Each action's callee and
    /// already-evaluated arguments are durably recorded (`WorkflowLog::
    /// begin_pending_action`) *before* it's dispatched — the same "log
    /// intent, then act" ordering `transact_log.rs::begin_pending` gives
    /// `network` — so `replay_pending_workflow_actions` can resume a crash
    /// mid-fan-out at startup, not just retry in-process.
    fn run_workflow_actions(
        &self,
        workflow_name: &str,
        actions: &[TransactSlot],
        env: &mut Env,
        instance_id: i64,
        state: &str,
        slot: &str,
    ) -> SResult<()> {
        for (action_index, action) in actions.iter().enumerate() {
            let mut vals = Vec::with_capacity(action.args.len());
            for a in &action.args {
                vals.push(self.eval_expr(a, env)?);
            }
            let wlog = self.workflow_log(action.span).map_err(Signal::Err)?;
            let args_json = self.encode_workflow_action_args(&vals, action.span)?;
            let now = now_secs();
            let row_id = wlog
                .begin_pending_action(instance_id, workflow_name, state, slot, action_index as i64, &action.name, &args_json, now)
                .map_err(|e| Signal::Err(self.workflow_log_err(e, action.span)))?;
            self.run_one_workflow_action(&wlog, row_id, &action.name, &vals, instance_id, state, action.span)?;
        }
        Ok(())
    }

    /// `Vec<Value>` -> a JSON array text, via `serve::encode_value`'s
    /// general struct/scalar codec (not `transact_log.rs`'s own narrower
    /// scalar-only encoder — a workflow action's arguments can be a whole
    /// `data`-derived struct or a `link_<Event>` token struct, not just
    /// scalars). Decoded back on replay via the callee's own declared
    /// parameter types (`replay_one_workflow_action`), so no type
    /// information needs to be stored here at all.
    fn encode_workflow_action_args(&self, vals: &[Value], span: Span) -> SResult<String> {
        let mut arr = Vec::with_capacity(vals.len());
        for v in vals {
            arr.push(crate::serve::encode_value(v, &self.program).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?);
        }
        Ok(serde_json::Value::Array(arr).to_string())
    }

    /// The actual dispatch-with-retry for one already-durably-logged
    /// action row — shared by `run_workflow_actions` (the live path) and
    /// `replay_one_workflow_action` (crash recovery), so both give exactly
    /// the same bounded-backoff-then-trap behavior `run_transact_write_slot`
    /// gives `transact`'s `commit`/`compensate`.
    fn run_one_workflow_action(
        &self,
        wlog: &WorkflowLog,
        row_id: i64,
        action_fn: &str,
        vals: &[Value],
        instance_id: i64,
        state: &str,
        span: Span,
    ) -> SResult<()> {
        const MAX_ATTEMPTS: u32 = 5;
        let mut attempt = 0u32;
        loop {
            let outcome = self.call(action_fn, vals, span);
            let failed = match &outcome {
                Err(_) => true,
                Ok(v) => is_result_err(v),
            };
            if !failed {
                wlog.mark_action_done(row_id, now_secs()).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
                return Ok(());
            }
            attempt += 1;
            let _ = wlog.bump_action_attempts(row_id, now_secs());
            if attempt >= MAX_ATTEMPTS {
                return Err(Signal::Err(RuntimeError {
                    kind: ErrorKind::WorkflowActionPending { instance_id, state: state.to_string(), action: action_fn.to_string() },
                    span,
                }));
            }
            std::thread::sleep(std::time::Duration::from_millis(20u64 << attempt.min(10)));
        }
    }

    /// `WORKFLOW.md`'s crash-replay pass — called once at `nirdosha serve`
    /// startup, right alongside `replay_pending_transactions`, before the
    /// server accepts any request. Every `workflow_pending_action` row
    /// still `pending` (the action either never ran, or ran and exhausted
    /// its live retry budget) gets one more bounded retry attempt here;
    /// this never traps the whole startup — an action still failing after
    /// replay stays durably `pending`, eligible for the next replay or an
    /// operator's attention, reported in the returned outcome instead.
    pub fn replay_pending_workflow_actions(&self) -> Result<Vec<WorkflowReplayOutcome>, RuntimeError> {
        let span = Span { line: 0, col: 0 };
        let wlog = self.workflow_log(span)?;
        let pending = wlog.list_pending_actions().map_err(|e| self.workflow_log_err(e, span))?;
        Ok(pending.into_iter().map(|p| self.replay_one_workflow_action(&wlog, p, span)).collect())
    }

    fn replay_one_workflow_action(&self, wlog: &WorkflowLog, p: PendingWorkflowAction, span: Span) -> WorkflowReplayOutcome {
        let instance_id = p.instance_id;
        let action = p.action_fn.clone();
        let Some(f) = self.find_fn(&p.action_fn) else {
            return WorkflowReplayOutcome::Stuck { instance_id, action, reason: "callee no longer exists in this program".to_string() };
        };
        let parsed: serde_json::Value = match serde_json::from_str(&p.args_json) {
            Ok(v) => v,
            Err(e) => return WorkflowReplayOutcome::Stuck { instance_id, action, reason: format!("corrupt args_json: {e}") },
        };
        let Some(arr) = parsed.as_array() else {
            return WorkflowReplayOutcome::Stuck { instance_id, action, reason: "args_json is not a JSON array".to_string() };
        };
        if arr.len() != f.params.len() {
            return WorkflowReplayOutcome::Stuck {
                instance_id,
                action,
                reason: "argument count no longer matches the callee's current signature".to_string(),
            };
        }
        let mut vals = Vec::with_capacity(arr.len());
        for (j, param) in arr.iter().zip(f.params.iter()) {
            match crate::serve::decode_value(j, &param.ty, &self.program, 0) {
                Ok(v) => vals.push(v),
                Err(e) => {
                    return WorkflowReplayOutcome::Stuck {
                        instance_id,
                        action,
                        reason: format!("failed to decode argument `{}`: {e}", param.name),
                    }
                }
            }
        }
        match self.run_one_workflow_action(wlog, p.id, &p.action_fn, &vals, instance_id, &p.state, span) {
            Ok(()) => WorkflowReplayOutcome::Resolved { instance_id, action },
            Err(_) => WorkflowReplayOutcome::StillPending { instance_id, action },
        }
    }

    /// Mints one fresh, not-yet-consumed token per `link`-marked outgoing
    /// transition of `state`, durably records it, and binds it into `env`
    /// as `link_<Event>` — only ever called right before that state's own
    /// `on_entry` actions run, so the binding's lifetime matches
    /// `WORKFLOW.md`'s "only in scope inside the `on_entry` of the state
    /// that declares it" rule structurally (a fresh `Env` each time, never
    /// carried into a different state's scope).
    fn bind_link_tokens(
        &self,
        wlog: &WorkflowLog,
        wf: &WorkflowDecl,
        state: &StateDecl,
        instance_id: i64,
        env: &mut Env,
        span: Span,
    ) -> SResult<()> {
        let link_token_ty_name = format!("{}LinkToken", wf.name);
        for t in &state.transitions {
            if !t.via_link {
                continue;
            }
            let token = generate_txn_id();
            wlog.mint_link(instance_id, &t.event, &token).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
            let binding = format!("link_{}", t.event);
            env.define(
                &binding,
                Value::Struct(Arc::from(link_token_ty_name.as_str()), Arc::from(vec![Value::Str(Arc::from(token))])),
                Ty::Named(link_token_ty_name.clone(), vec![]),
            );
        }
        Ok(())
    }

    fn workflow_start(&self, args: &[Value], span: Span) -> SResult<Value> {
        let Value::Str(workflow_name) = &args[0] else {
            unreachable!("typeck.rs already proved this is a str")
        };
        // `identity: Option(VerifiedIdentity)` (`WORKFLOW.md`'s "who
        // submitted this" section) — `None` for a legitimately anonymous
        // start, `Some(id)`'s own `subject` durably recorded so
        // `list_<workflow>_submitted_by_me` can find it again later.
        let started_by_subject: Option<Arc<str>> = match &args[1] {
            Value::Enum(_, variant, payload) if variant.as_ref() == "Some" => Some(identity_subject(&payload[0])),
            Value::Enum(_, variant, _) if variant.as_ref() == "None" => None,
            _ => unreachable!("typeck.rs already proved this is an Option(VerifiedIdentity)"),
        };
        let data_val = args[2].clone();
        let wf = self
            .find_workflow(workflow_name)
            .unwrap_or_else(|| unreachable!("workflow_lower.rs only ever emits calls naming a real workflow"));
        let initial = &wf.states[0];
        let data_json = crate::serve::encode_value(&data_val, &self.program)
            .map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
        let now = now_secs();
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let instance_id = wlog
            .create_instance(workflow_name, &initial.name, &data_json.to_string(), started_by_subject.as_deref(), now)
            .map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
        let data_ty = Ty::Named(format!("{workflow_name}Data"), vec![]);
        let mut env = Env::new();
        env.define("instance_id", Value::Int(instance_id), Ty::I64);
        env.define("data", data_val, data_ty);
        self.bind_link_tokens(&wlog, wf, initial, instance_id, &mut env, span)?;
        self.run_workflow_actions(workflow_name, &initial.on_entry, &mut env, instance_id, &initial.name, "entry")?;
        Ok(result_ok(Value::Int(instance_id)))
    }

    /// `payload` (the last argument) is accepted for signature symmetry
    /// with `advance_*`'s own future evolution but not yet threaded into
    /// `on_entry`/`on_exit` bindings — a disclosed v1 gap, `WORKFLOW.md`'s
    /// "deliberate non-goals" section names it.
    ///
    /// `identity` (the second argument, `WORKFLOW.md`'s "state ownership"
    /// section) is checked against `from_state`'s own `owner` — the one
    /// piece of this whole feature that genuinely has to run here rather
    /// than as a static `serve.rs::dispatch` gate: `advance_<workflow>` is
    /// one function serving every instance/state of this workflow, so
    /// "may this caller fire this event" depends on which state *this*
    /// instance happens to be in *right now*, not on anything fixed at
    /// the function level.
    fn workflow_advance(&self, args: &[Value], span: Span) -> SResult<Value> {
        let Value::Str(workflow_name) = &args[0] else {
            unreachable!("typeck.rs already proved this is a str")
        };
        let identity = &args[1];
        let Value::Int(instance_id) = &args[2] else {
            unreachable!("typeck.rs already proved this is an i64")
        };
        let event_tag: Arc<str> = match &args[3] {
            Value::Enum(_, variant, _) => Arc::clone(variant),
            _ => unreachable!("typeck.rs already proved this is a workflow event enum value"),
        };
        self.workflow_advance_inner(workflow_name, Some(identity), *instance_id, &event_tag, &args[4], span)
    }

    /// The actual state-move logic, shared by `workflow_advance` (the
    /// ordinary, authenticated path — `identity: Some(_)`, owner-checked)
    /// and `workflow_link_advance` (the magic-link path, after its own
    /// token verification already ran — `identity: None`, deliberately
    /// **not** owner-checked: a consumed, single-use link token *is* the
    /// authorization here, same as `advance_<workflow>`'s pre-ownership
    /// behavior always was for every link-triggered transition, and the
    /// same reason `*_via_link` fns take no `VerifiedIdentity` param at
    /// all — see `workflow_lower.rs`'s doc comment on why. A state that
    /// declares an `owner` can still legitimately have a `link`-marked
    /// outgoing event (e.g. an unauthenticated email-click confirmation
    /// out of an otherwise owner-gated state); this is what makes that
    /// combination work rather than an unreachable dead end.
    fn workflow_advance_inner(
        &self,
        workflow_name: &str,
        identity: Option<&Value>,
        instance_id: i64,
        event_tag: &str,
        payload: &Value,
        span: Span,
    ) -> SResult<Value> {
        let wf = self
            .find_workflow(workflow_name)
            .unwrap_or_else(|| unreachable!("workflow_lower.rs only ever emits calls naming a real workflow"));
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let Some((_, current_state, data_json)) =
            wlog.get_instance(instance_id).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?
        else {
            return Ok(workflow_err("InstanceNotFound", vec![]));
        };
        let Some(from_state) = wf.states.iter().find(|s| s.name == current_state) else {
            return Ok(workflow_err("InstanceNotFound", vec![]));
        };
        if let (Some(owner), Some(identity)) = (state_owner(from_state), identity) {
            let satisfies =
                identity_satisfies_owner(owner, identity).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
            if !satisfies {
                return Ok(workflow_err("NotStateOwner", vec![]));
            }
        }
        let Some(transition) = from_state.transitions.iter().find(|t| t.event.as_str() == event_tag) else {
            return Ok(workflow_err("NoSuchTransition", vec![]));
        };
        let data_ty = Ty::Named(format!("{workflow_name}Data"), vec![]);
        let json_val: serde_json::Value = serde_json::from_str(&data_json).unwrap_or(serde_json::Value::Null);
        let data_val = crate::serve::decode_value(&json_val, &data_ty, &self.program, 0)
            .map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;

        // `on_exit` of the old state runs before the state actually
        // moves -- a trap here (retries exhausted) leaves the instance in
        // `from_state`, so `advance_*` is safe to call again.
        let mut exit_env = Env::new();
        exit_env.define("instance_id", Value::Int(instance_id), Ty::I64);
        exit_env.define("data", data_val.clone(), data_ty.clone());
        self.run_workflow_actions(workflow_name, &from_state.on_exit, &mut exit_env, instance_id, &from_state.name, "exit")?;

        // `WORKFLOW.md`'s "audit trail" section: *who* fired this
        // transition (`None` only for the magic-link path — see this
        // fn's own doc comment) and *how* (`via_link`), plus an optional
        // free-text `comment` if `payload` carried one
        // (`{"comment": "..."}`) — still not threaded into `on_entry`/
        // `on_exit` bindings, `payload`'s own disclosed v1 gap, just
        // durably logged here now.
        let actor_subject = identity.map(identity_subject);
        let via_link = identity.is_none();
        let comment = extract_comment(payload);
        let now = now_secs();
        wlog.record_transition(
            instance_id,
            &from_state.name,
            &transition.target,
            event_tag,
            actor_subject.as_deref(),
            via_link,
            comment.as_deref(),
            now,
        )
        .map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;

        let to_state = wf
            .states
            .iter()
            .find(|s| s.name == transition.target)
            .expect("workflow_lower.rs already proved every transition target is a declared state");
        let mut entry_env = Env::new();
        entry_env.define("instance_id", Value::Int(instance_id), Ty::I64);
        entry_env.define("data", data_val, data_ty);
        self.bind_link_tokens(&wlog, wf, to_state, instance_id, &mut entry_env, span)?;
        self.run_workflow_actions(workflow_name, &to_state.on_entry, &mut entry_env, instance_id, &to_state.name, "entry")?;

        Ok(result_ok(Value::Bool(true)))
    }

    fn workflow_link_advance(&self, args: &[Value], span: Span) -> SResult<Value> {
        let Value::Int(instance_id) = &args[1] else {
            unreachable!("typeck.rs already proved this is an i64")
        };
        let instance_id = *instance_id;
        let event_tag: Arc<str> = match &args[2] {
            Value::Enum(_, variant, _) => Arc::clone(variant),
            _ => unreachable!("typeck.rs already proved this is a workflow event enum value"),
        };
        let Value::Struct(_, token_fields) = &args[3] else {
            unreachable!("typeck.rs already proved this is a workflow link-token struct value")
        };
        let Value::Str(presented_token) = &token_fields[0] else {
            unreachable!("workflow_lower.rs's synthesized LinkToken struct has exactly one `str` field")
        };
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let found = wlog.find_unconsumed_link(instance_id, &event_tag).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
        let Some((row_id, stored_token)) = found else {
            return Ok(workflow_err("LinkTokenMismatch", vec![]));
        };
        // Constant-time compare *before* touching the database again --
        // the same timing-side-channel fix `trade_finance.nir:676`'s
        // `constant_time_str_eq` already established for
        // `decide_approval_via_link`.
        if !constant_time_eq(&stored_token, presented_token) {
            return Ok(workflow_err("LinkTokenMismatch", vec![]));
        }
        let consumed = wlog.consume_link_by_id(row_id).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
        if !consumed {
            // Another request won the race between `find_unconsumed_link`
            // and here -- already spent.
            return Ok(workflow_err("LinkAlreadyConsumed", vec![]));
        }
        // Same transition/state-move/`on_entry` logic `workflow_advance`
        // already implements, via the shared `workflow_advance_inner` --
        // the token isn't part of that, only its own verification above
        // is. `identity: None` — deliberately *not* owner-checked, see
        // `workflow_advance_inner`'s own doc comment for why a consumed
        // link token is its own authorization.
        let Value::Str(workflow_name) = &args[0] else {
            unreachable!("typeck.rs already proved this is a str")
        };
        self.workflow_advance_inner(workflow_name, None, instance_id, event_tag.as_ref(), &args[4], span)
    }

    /// `Recipient::BySubject(s)` is just `s`; `Recipient::ByRole(role)`
    /// fans out via `identity_directory` (`workflow_log.rs`) -- the piece
    /// no builtin/route in this codebase kept before.
    fn resolve_recipients(&self, wlog: &WorkflowLog, to: &Value, span: Span) -> SResult<Vec<String>> {
        let Value::Enum(_, variant, payload) = to else {
            unreachable!("typeck.rs already proved this is a Recipient value")
        };
        let Value::Str(s) = &payload[0] else {
            unreachable!("Recipient's variants are both a single str payload")
        };
        match variant.as_ref() {
            "BySubject" => Ok(vec![s.to_string()]),
            "ByRole" => wlog.subjects_with_role(s).map_err(|e| Signal::Err(self.workflow_log_err(e, span))),
            _ => unreachable!("ast::prelude_enums' Recipient has exactly these two variants"),
        }
    }

    /// `send_email`/`send_sms`/`send_push`'s (and `notify`'s offline
    /// fallback's) real transport for one subject on one channel — looks
    /// up the app's own admin-editable provider-config row (an ordinary
    /// user `struct`, e.g. `EmailProviderConfig`, migrated by `nirdosha
    /// serve --db` into `email_provider_config` like any other struct;
    /// `WORKFLOW.md` documents the exact column convention this reads)
    /// via `conn`, then a generic authenticated HTTPS POST
    /// (`notify_https_post`). Returns the ready-to-return `.nir` `Err(..)`
    /// value directly on failure, `Ok(())` on success — callers just
    /// `return Ok(e)` on `Err`.
    fn send_via_channel(&self, conn: &Value, channel: &str, subject: &str, template: &str, vars: &Value) -> Result<(), Value> {
        let Value::Db(slot) = conn else { unreachable!("typeck.rs already proved this is db") };
        let mut guard = slot.lock().unwrap();
        let Some(db) = guard.as_mut() else {
            return Err(workflow_err("ProviderRequestFailed", vec![Value::Str(Arc::from("db connection is stopped"))]));
        };
        let table = provider_table_name(channel);
        let sql = format!(
            "SELECT host, port, path, api_key, from_address FROM {table} WHERE active = 1 ORDER BY id DESC LIMIT 1"
        );
        // Goes through `dbconn::query`, not a raw rusqlite row extractor,
        // so the provider-config lookup works the same whether `conn`
        // came from a SQLite or a Postgres `db_connect` string (`Ty::Db`'s
        // doc comment, `dbconn.rs`'s module doc).
        let first_row = match crate::dbconn::query(db, &sql, &[]) {
            Ok(JsonDoc::Array(rows)) => rows.into_iter().next(),
            _ => None,
        };
        let Some((host, port, path, api_key, from_address)) = first_row.and_then(|row| {
            let s = |k: &str| row.get(k)?.as_str().map(str::to_string);
            let host = s("host")?;
            let port = row.get("port")?.as_i64()?;
            let path = s("path")?;
            let api_key = s("api_key")?;
            let from_address = s("from_address")?;
            Some((host, port, path, api_key, from_address))
        }) else {
            return Err(workflow_err("ProviderNotConfigured", vec![]));
        };
        let Value::Json(doc) = vars else { unreachable!("typeck.rs already proved this is json") };
        let body = serde_json::json!({
            "to": subject,
            "from": from_address,
            "template": template,
            "vars": (**doc).clone(),
        })
        .to_string();
        match notify_https_post(&host, port, &path, &api_key, &body) {
            Ok(()) => Ok(()),
            Err(e) => Err(workflow_err("ProviderRequestFailed", vec![Value::Str(Arc::from(e))])),
        }
    }

    fn dispatch_send(&self, args: &[Value], span: Span, channel: &str) -> SResult<Value> {
        let conn = &args[0];
        let to = &args[1];
        let Value::Str(template) = &args[2] else { unreachable!("typeck.rs already proved this is a str") };
        let vars = &args[3];
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let subjects = self.resolve_recipients(&wlog, to, span)?;
        if subjects.is_empty() {
            return Ok(workflow_err("NoRecipientsForRole", vec![]));
        }
        for subject in &subjects {
            if let Err(e) = self.send_via_channel(conn, channel, subject, template, vars) {
                return Ok(e);
            }
        }
        Ok(result_ok(Value::Bool(true)))
    }

    /// Publishes to `nirdosha:push:<subject>` (Redis `PUBLISH`, a sibling
    /// to the existing `LPUSH`/`BLPOP`-backed `mq_publish`/`mq_consume`,
    /// same connection type) — the external WS gateway `WORKFLOW.md`
    /// documents is expected to `SUBSCRIBE` and relay to that subject's
    /// live browser connection. Fire-and-forget by design (`PUBLISH` has
    /// no persistence if nobody's subscribed) — fine here specifically
    /// because `dispatch_notify` only takes this path when
    /// `identity_presence` says someone should be listening.
    fn publish_push_event(&self, mq: &Value, subject: &str, template: &str, vars: &Value) -> Result<(), Value> {
        let Value::Mq(slot) = mq else { unreachable!("typeck.rs already proved this is mq") };
        let mut guard = slot.lock().unwrap();
        let Some(conn) = guard.as_mut() else {
            return Err(workflow_err("ProviderRequestFailed", vec![Value::Str(Arc::from("mq connection is stopped"))]));
        };
        let Value::Json(doc) = vars else { unreachable!("typeck.rs already proved this is json") };
        let payload = serde_json::json!({
            "template": template,
            "vars": (**doc).clone(),
            "sent_at": now_secs(),
        })
        .to_string();
        let channel = format!("nirdosha:push:{subject}");
        match redis::cmd("PUBLISH").arg(&channel).arg(&payload).query::<i64>(&mut conn.0) {
            Ok(_) => Ok(()),
            Err(e) => Err(workflow_err("ProviderRequestFailed", vec![Value::Str(Arc::from(e.to_string()))])),
        }
    }

    fn dispatch_notify(&self, args: &[Value], span: Span) -> SResult<Value> {
        let conn = &args[0];
        let mq = &args[1];
        let to = &args[2];
        let Value::Str(template) = &args[3] else { unreachable!("typeck.rs already proved this is a str") };
        let vars = &args[4];
        let wlog = self.workflow_log(span).map_err(Signal::Err)?;
        let subjects = self.resolve_recipients(&wlog, to, span)?;
        if subjects.is_empty() {
            return Ok(workflow_err("NoRecipientsForRole", vec![]));
        }
        for subject in &subjects {
            let online = wlog.is_online(subject).map_err(|e| Signal::Err(self.workflow_log_err(e, span)))?;
            let outcome =
                if online { self.publish_push_event(mq, subject, template, vars) } else { self.send_via_channel(conn, "email", subject, template, vars) };
            if let Err(e) = outcome {
                return Ok(e);
            }
        }
        Ok(result_ok(Value::Bool(true)))
    }

    fn call(&self, name: &str, arg_vals: &[Value], span: Span) -> Result<Value, RuntimeError> {
        // `MAX_CALL_DEPTH` guard (see its own doc comment and
        // `call_depth`'s): checked and incremented before `run` even
        // starts, so a call past the limit never adds another real Rust
        // stack frame at all. `_depth_guard`'s `Drop` decrements on every
        // exit path below (both the early `tracer.is_none()` return and
        // the traced path), the same way a lock guard would.
        let depth = self.call_depth.get() + 1;
        if depth > MAX_CALL_DEPTH {
            return err(ErrorKind::CallStackOverflow { fn_name: name.to_string() }, span);
        }
        self.call_depth.set(depth);
        struct DepthGuard<'a>(&'a std::cell::Cell<usize>);
        impl Drop for DepthGuard<'_> {
            fn drop(&mut self) {
                self.0.set(self.0.get() - 1);
            }
        }
        let _depth_guard = DepthGuard(&self.call_depth);

        let run = || -> Result<Value, RuntimeError> {
            let f = self.find_fn(name).ok_or_else(|| RuntimeError {
                kind: ErrorKind::UnknownFn(name.to_string()),
                span,
            })?;
            if f.params.len() != arg_vals.len() {
                return err(
                    ErrorKind::ArityMismatch {
                        fn_name: name.to_string(),
                        want: f.params.len(),
                        got: arg_vals.len(),
                    },
                    span,
                );
            }

            let mut env = Env::new();
            for (p, v) in f.params.iter().zip(arg_vals) {
                self.check_ty(v, &p.ty, span)?;
                env.define(&p.name, v.clone(), p.ty.clone());
            }

            match self.exec_block(&f.body, &mut env) {
                Ok(_) => {
                    // Block ran to completion without `return` — its value
                    // (the last expression-statement, or Unit) is only a valid
                    // function result if the declared return type is Unit.
                    if f.ret == Ty::Unit {
                        Ok(Value::Unit)
                    } else {
                        err(ErrorKind::MissingReturn { fn_name: name.to_string() }, f.span)
                    }
                }
                Err(Signal::Return(v)) => {
                    self.check_ty(&v, &f.ret, span)?;
                    Ok(v)
                }
                Err(Signal::Err(e)) => Err(e),
            }
        };

        // Observability layer 1's hotspot-attribution hook — see
        // `tracer`'s doc comment: the `None` branch below is exactly
        // `run()`, no `Instant::now()`, no `effect_of_fn` lookup, no
        // allocation. Every user-function call goes through here
        // (unconditionally, when a tracer *is* attached), so a slow pure
        // helper nested inside a traced function still shows up. The
        // `enabled()` check right after is layer 2a's addition
        // (`observability.rs`'s "Rollout layers 2-4" section) — a
        // dormant, port-gated tracer with no APM client connected bails
        // here too, at the cost of one relaxed atomic load beyond the
        // `Option` check.
        let Some(tracer) = &self.tracer else { return run() };
        if !tracer.enabled() {
            return run();
        }
        let effect = self.effect_of_fn(name);
        let start = std::time::Instant::now();
        let result = run();
        tracer.record_span(effect, span, "call", Some(Arc::from(name)), start, outcome_of_runtime_result(&result));
        result
    }

    /// `call()`'s effect tag: the target function's own inferred
    /// `ast::Effect` set (`effects::infer_effects`), reduced to a single
    /// representative tag (a `BTreeSet`'s smallest element — its `Ord`
    /// derive gives a fixed, deterministic pick when a function has more
    /// than one effect, not that any one tag is more "real" than
    /// another) to fit `SpanRecord::effect`'s single-`Option<Effect>`
    /// shape. `None` for a pure function — `call()` still traces it (see
    /// `SpanRecord`'s doc comment), just with no effect tag.
    ///
    /// Computed lazily via `fn_effects`'s `OnceCell` — see that field's
    /// own doc comment for why this only ever runs when tracing is
    /// actually on.
    fn effect_of_fn(&self, name: &str) -> Option<Effect> {
        let table = self.fn_effects.get_or_init(|| {
            let registry = TypeRegistry::build(&self.program);
            effects::infer_effects(&self.program, &registry).into_iter().map(|(k, fx)| (k, fx.inferred)).collect()
        });
        table.get(name).and_then(|set| set.iter().next().copied())
    }

    /// The one hook-point shape every effectful `eval_expr` arm and the
    /// `eval_builtin` dispatch site (this file's other two hook points
    /// alongside `call()`, see `observability.rs`'s module doc) reuses:
    /// run `f`, and only if a `tracer` is attached, time it and record a
    /// span tagged `effect`/`name`/`fn_name` at `site`. The `None`
    /// branch is exactly `f()` — the entire cost paid when tracing is
    /// off.
    fn traced<T>(
        &self,
        effect: Option<Effect>,
        site: Span,
        name: &'static str,
        fn_name: Option<Arc<str>>,
        f: impl FnOnce() -> T,
        outcome_of: fn(&T) -> observability::Outcome,
    ) -> T {
        let Some(tracer) = &self.tracer else { return f() };
        if !tracer.enabled() {
            return f();
        }
        let start = std::time::Instant::now();
        let result = f();
        tracer.record_span(effect, site, name, fn_name, start, outcome_of(&result));
        result
    }

    fn check_ty(&self, v: &Value, ty: &Ty, span: Span) -> Result<(), RuntimeError> {
        match (v, ty) {
            (Value::Bool(_), Ty::Bool) => Ok(()),
            (Value::Unit, Ty::Unit) => Ok(()),
            (Value::Boxed(inner), Ty::Box(inner_ty)) => self.check_ty(inner, inner_ty, span),
            (Value::Ref(inner), Ty::Ref(inner_ty)) => self.check_ty(inner, inner_ty, span),
            // No inner value to recurse into yet — the spawned thread's
            // own `call()` already validates its result against *its*
            // declared return type independently, on its own stack, the
            // moment it produces one; there's nothing more to check here
            // than "this is in fact a thread handle."
            (Value::Thread(_), Ty::Thread(_)) => Ok(()),
            // Same reasoning as `Value::Thread` above: a channel's own
            // `send`/`recv` are the only places a payload value is
            // checked against `Ty::Channel`'s inner type, so there's no
            // inner value sitting here to recurse into.
            (Value::Channel(_), Ty::Channel(_)) => Ok(()),
            (Value::Sandbox(_), Ty::Sandbox) => Ok(()),
            (Value::Str(_), Ty::Str) => Ok(()),
            (Value::Tcp(_), Ty::Tcp) => Ok(()),
            (Value::TcpListener(_), Ty::TcpListener) => Ok(()),
            (Value::File(_), Ty::File) => Ok(()),
            (Value::Json(_), Ty::Json) => Ok(()),
            (Value::Db(_), Ty::Db) => Ok(()),
            (Value::Mq(_), Ty::Mq) => Ok(()),
            // No inner signature to recurse into — `typeck.rs` already
            // proved a `Value::Fn` reaching here carries a target whose
            // real signature matches `Ty::Fn`'s params/ret (`infer_call`'s
            // call-through-value arm, `infer_acquire`), same "nothing
            // left to check at runtime" reasoning `Value::Thread`/
            // `Value::Channel` above already give.
            (Value::Fn(_), Ty::Fn(_, _)) => Ok(()),
            (Value::Float(_), Ty::F64) => Ok(()),
            // No range check the way an integer type gets one — `Decimal`
            // already caps itself at 28-29 significant digits on
            // construction/arithmetic (`dec_from_i64`/`scalar_binop`
            // trap there), so there's nothing left for a boundary check
            // to catch here, the same "no-guard passthrough" shape
            // `Value::Float`/`Ty::F64` above already has.
            (Value::Dec128(_), Ty::Dec128) => Ok(()),
            (Value::Vector(elems), Ty::Vector(elem_ty, n)) => {
                if elems.len() != *n {
                    return err(
                        ErrorKind::TypeMismatch { expected: ty.name(), found: format!("Vector of length {}", elems.len()) },
                        span,
                    );
                }
                for e in elems.iter() {
                    self.check_ty(e, elem_ty, span)?;
                }
                Ok(())
            }
            (Value::Matrix(elems, rows, cols), Ty::Matrix(elem_ty, want_rows, want_cols)) => {
                if rows != want_rows || cols != want_cols {
                    return err(
                        ErrorKind::TypeMismatch { expected: ty.name(), found: format!("Matrix {rows}x{cols}") },
                        span,
                    );
                }
                for e in elems.iter() {
                    self.check_ty(e, elem_ty, span)?;
                }
                Ok(())
            }
            // Row 11 -- a struct/enum value checked against its declared
            // name, recursing into each field/payload the same way
            // `Ty::Vector`/`Ty::Matrix` already recurse into their
            // elements above. `typeck.rs` already proved a `Value::
            // Struct`/`Value::Enum` reaching here carries the right
            // *count* of fields/payload values (construction is checked
            // exactly like a function call's argument list) -- this is
            // the runtime Tier-2 backstop for each individual value's
            // range/shape, same "checker is the real gate, this is the
            // backstop" shape every other arm here already has.
            // `want_args` (Row 11 layer 6, generics) substitutes each
            // declared field/payload type before recursing -- unlike
            // `typeck.rs`/`ownership.rs`, this file never needs to
            // *resolve* an instantiation's concrete type arguments
            // itself: `ty` is always supplied directly by the caller
            // (`Stmt::Let`, a parameter binding, `Expr::Assign`'s
            // target), which already carries them.
            (Value::Struct(name, fields), Ty::Named(want_name, want_args)) if name.as_ref() == want_name.as_str() => {
                let decl = self
                    .find_struct(want_name)
                    .expect("typeck.rs already proved this struct name is declared");
                let subst = zip_type_params(&decl.type_params, want_args);
                for (v, field) in fields.iter().zip(decl.fields.iter()) {
                    self.check_ty(v, &substitute_ty(&field.ty, &subst), span)?;
                }
                Ok(())
            }
            (Value::Enum(name, variant, payload), Ty::Named(want_name, want_args))
                if name.as_ref() == want_name.as_str() =>
            {
                let (enum_decl, v) = self
                    .find_variant(variant)
                    .expect("typeck.rs already proved this variant name is declared");
                let subst = zip_type_params(&enum_decl.type_params, want_args);
                for (val, want) in payload.iter().zip(v.payload.iter()) {
                    self.check_ty(val, &substitute_ty(want, &subst), span)?;
                }
                Ok(())
            }
            (Value::Int(n), _) if ty.is_integer() => {
                if ty.in_range(*n) {
                    Ok(())
                } else {
                    err(ErrorKind::OutOfRange { ty: ty.name(), value: *n }, span)
                }
            }
            (v, ty) => err(
                ErrorKind::TypeMismatch { expected: ty.name(), found: v.ty_name().to_string() },
                span,
            ),
        }
    }

    /// A block's value is its last expression-statement (Unit if none) —
    /// the same implicit-last-expression convention `if`'s branches rely on.
    fn exec_block(&self, block: &Block, env: &mut Env) -> SResult<Value> {
        env.push();
        let result = self.exec_stmts(&block.stmts, env);
        env.pop();
        result
    }

    fn exec_stmts(&self, stmts: &[Stmt], env: &mut Env) -> SResult<Value> {
        let mut last = Value::Unit;
        for stmt in stmts {
            match stmt {
                Stmt::Let { name, ty, value, span } => {
                    let v = self.eval_expr(value, env)?;
                    self.check_ty(&v, ty, *span)?;
                    env.define(name, v, ty.clone());
                    last = Value::Unit;
                }
                Stmt::Return { value, span } => {
                    let v = match value {
                        Some(e) => self.eval_expr(e, env)?,
                        None => Value::Unit,
                    };
                    let _ = span;
                    return Err(Signal::Return(v));
                }
                Stmt::While { cond, body, span } => {
                    let _ = span;
                    loop {
                        let c = self.eval_expr(cond, env)?;
                        match c {
                            Value::Bool(true) => {}
                            Value::Bool(false) => break,
                            v => return Err(mismatch("bool", v.ty_name(), cond.span())),
                        }
                        self.exec_block(body, env)?; // propagates Signal::Return via `?`
                    }
                    last = Value::Unit;
                }
                Stmt::Expr(e) => {
                    last = self.eval_expr(e, env)?;
                }
                // `audited` only changes what `codegen.rs` emits (it
                // suppresses Tier-1/2 *guard* insertion in compiled
                // code) -- the interpreter has no such guards to
                // suppress in the first place (every `check_ty` call
                // always runs, unconditionally, everywhere), so this is
                // just a transparent nested scope, the same shape as a
                // `Block`.
                Stmt::Audited { body, .. } => {
                    env.push();
                    let result = self.exec_stmts(body, env);
                    env.pop();
                    result?;
                    last = Value::Unit;
                }
            }
        }
        Ok(last)
    }

    fn eval_expr(&self, expr: &Expr, env: &mut Env) -> SResult<Value> {
        match expr {
            Expr::Int(n, _) => Ok(Value::Int(*n)),
            Expr::Float(n, _) => Ok(Value::Float(*n)),
            Expr::ArrayLit(elements, _span) => {
                let mut vals = Vec::with_capacity(elements.len());
                for e in elements {
                    vals.push(self.eval_expr(e, env)?);
                }
                // Vector vs. matrix is decided by `typeck.rs` already
                // (`infer_array_lit`) -- at runtime, a matrix literal is
                // just one whose elements are themselves `Value::Vector`
                // rows, all the same length (already proven equal), so
                // flattening them row-major is all that's left to do.
                if let Some(Value::Vector(first_row)) = vals.first() {
                    let cols = first_row.len();
                    let rows = vals.len();
                    let mut flat = Vec::with_capacity(rows * cols);
                    for v in vals {
                        match v {
                            Value::Vector(row) => flat.extend(row.iter().cloned()),
                            _ => unreachable!("typeck.rs already proved every row is the same Vector shape"),
                        }
                    }
                    Ok(Value::Matrix(Arc::from(flat), rows, cols))
                } else {
                    Ok(Value::Vector(Arc::from(vals)))
                }
            }
            Expr::Bool(b, _) => Ok(Value::Bool(*b)),
            Expr::Ident(name, span) => match env.get(name) {
                Some(v) => Ok(v),
                // Not a local binding — `typeck.rs` already proved this is
                // only reachable for a plain (non-`requires`) top-level
                // fn name (`infer`'s `Expr::Ident` fallback), so this is
                // always safe: a first-class function value is just its
                // target name.
                None if self.find_fn(name).is_some() => Ok(Value::Fn(Arc::from(name.as_str()))),
                None => Err(Signal::Err(RuntimeError { kind: ErrorKind::UnknownVar(name.clone()), span: *span })),
            },
            Expr::Unary(op, inner, span) => {
                let v = self.eval_expr(inner, env)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
                    (UnOp::Neg, Value::Float(n)) => Ok(Value::Float(-n)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Neg, v) => Err(mismatch("int or f64", v.ty_name(), *span)),
                    (UnOp::Not, v) => Err(mismatch("bool", v.ty_name(), *span)),
                }
            }
            Expr::Binary(op, lhs, rhs, span) => self.eval_binary(*op, lhs, rhs, env, *span),
            Expr::Call(name, arg_exprs, span) => {
                // First-class function call-through-value: `name` names a
                // local binding holding a `Value::Fn` (either a plain
                // top-level fn named directly, or an `acquire`d privileged
                // one) rather than a global fn/builtin/constructor.
                // Dispatched to the *target* the value actually carries,
                // via the same `call` every ordinary `Expr::Call` below
                // ends up at — checked ahead of everything else, matching
                // `typeck.rs::infer_call`'s identical precedence.
                if let Some(Value::Fn(target)) = env.get(name) {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    return self.call(target.as_ref(), &vals, *span).map_err(Signal::Err);
                }
                if is_builtin(name) {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    // `send_email`/`send_sms`/`send_push`/`notify` and
                    // `workflow_lower.rs`'s three internal builtins need
                    // `self`/durable-log access `eval_builtin`'s
                    // free-function shape can't provide — handled here,
                    // ahead of the generic dispatch below, the same
                    // reason `Expr::Transact` bypasses `eval_builtin`
                    // entirely instead of being just another arm in it.
                    if let Some(v) = self.eval_workflow_builtin(name, &vals, *span)? {
                        return Ok(v);
                    }
                    // Observability layer 1's third and last hook point —
                    // one wrapping call at this single dispatch site (not
                    // per-builtin-arm), gated behind `self.tracer` being
                    // both attached *and* `enabled()` (layer 2a) *before*
                    // even looking `name` up in
                    // `observability::traced_builtin`'s table, so a plain
                    // `tracer: None` run — or a dormant, port-gated one
                    // with no APM client connected — doesn't pay that
                    // lookup either.
                    if self.tracer.as_ref().is_some_and(|t| t.enabled()) {
                        if let Some((effect, static_name)) = observability::traced_builtin(name) {
                            return self
                                .traced(
                                    Some(effect),
                                    *span,
                                    static_name,
                                    None,
                                    || eval_builtin(name, &vals, *span, &self.rng),
                                    outcome_of_runtime_result,
                                )
                                .map_err(Signal::Err);
                        }
                    }
                    return eval_builtin(name, &vals, *span, &self.rng).map_err(Signal::Err);
                }
                // Row 11: a struct's own name, or an enum variant's name,
                // called like a function, constructs a value -- "an
                // ordinary call, not a new literal form"
                // (`nirdosha_row11_amendment.md` §3.1). `typeck.rs`
                // already proved these names can't collide with a
                // function/builtin, so checking them first is safe and
                // unambiguous, same order `Expr::Call`'s typeck
                // counterpart (`infer_call`) already uses.
                if self.find_struct(name).is_some() {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    return Ok(Value::Struct(Arc::from(name.as_str()), Arc::from(vals)));
                }
                if let Some((enum_decl, _)) = self.find_variant(name) {
                    let enum_name: Arc<str> = Arc::from(enum_decl.name.as_str());
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    return Ok(Value::Enum(enum_name, Arc::from(name.as_str()), Arc::from(vals)));
                }
                let mut vals = Vec::with_capacity(arg_exprs.len());
                for a in arg_exprs {
                    vals.push(self.eval_expr(a, env)?);
                }
                self.call(name, &vals, *span).map_err(Signal::Err)
            }
            // `acquire name(proof)` — checks `proof` (a `RoleView`/
            // `ClaimView`) against `name`'s declared `requires(...)`,
            // yielding `Ok(Value::Fn(name))` on a match, `Err(reason)`
            // otherwise. Runtime string comparison, same spirit
            // `check_role`/`extract_claim` already use for their own
            // proof checks — `typeck.rs::infer_acquire` already proved
            // `name` is a real, gated fn and `proof`'s type matches what
            // the requirement demands, so every `unreachable!` below is
            // a real static guarantee, not a hope.
            Expr::Acquire(name, proof_expr, _span) => {
                let proof = self.eval_expr(proof_expr, env)?;
                let f = self.find_fn(name).unwrap_or_else(|| {
                    unreachable!("typeck.rs::infer_acquire already proved `{name}` is a real fn")
                });
                let requirement =
                    f.requires.as_ref().unwrap_or_else(|| unreachable!("typeck.rs already proved `{name}` is gated"));
                let proof_field = match &proof {
                    Value::Struct(_, fields) => &fields[0],
                    _ => unreachable!("typeck.rs already proved proof is a RoleView/ClaimView"),
                };
                let want = match requirement {
                    Requirement::Role(role) => role,
                    Requirement::Claim(_, value) => value,
                };
                let matched = matches!(proof_field, Value::Str(s) if s.as_ref() == want.as_str());
                if matched {
                    Ok(result_ok(Value::Fn(Arc::from(name.as_str()))))
                } else {
                    Ok(result_err(format!("insufficient privilege: `{name}` {}", requirement.describe())))
                }
            }
            Expr::If { cond, then_block, else_block, span } => {
                let c = self.eval_expr(cond, env)?;
                let take_then = match c {
                    Value::Bool(b) => b,
                    v => return Err(mismatch("bool", v.ty_name(), *span)),
                };
                if take_then {
                    self.exec_block(then_block, env)
                } else {
                    match else_block {
                        Some(branch) => match branch.as_ref() {
                            ElseBranch::Block(b) => self.exec_block(b, env),
                            ElseBranch::If(e) => self.eval_expr(e, env),
                        },
                        None => Ok(Value::Unit),
                    }
                }
            }
            Expr::Assign(name, rhs, span) => {
                let v = self.eval_expr(rhs, env)?;
                let ty = env.get_ty(name).ok_or_else(|| {
                    Signal::Err(RuntimeError { kind: ErrorKind::UnknownVar(name.clone()), span: *span })
                })?;
                self.check_ty(&v, &ty, *span)?;
                // `get_ty` above already proved the binding exists, so the
                // only way `set` fails is a logic error in this file, not
                // a user-reachable state — hence the `expect`.
                env.set(name, v.clone()).expect("checked above: binding exists");
                Ok(v)
            }
            Expr::Box(inner, _span) => {
                let v = self.eval_expr(inner, env)?;
                Ok(Value::Boxed(Box::new(v)))
            }
            Expr::Deref(inner, span) => {
                let v = self.eval_expr(inner, env)?;
                match v {
                    // `typeck.rs` already proved, statically, that this
                    // dereference is either reading a scalar out (always
                    // fine) or reading affine content out of an owned
                    // `box` (fine — it's the outer binding's problem to
                    // have been marked moved, which `ownership.rs`
                    // handles) — never affine content out of a `&`
                    // (`typeck.rs` rejects that before this ever runs).
                    // So both variants just unwrap the same way here.
                    Value::Boxed(inner) | Value::Ref(inner) => Ok(*inner),
                    v => Err(mismatch("box or ref", v.ty_name(), *span)),
                }
            }
            Expr::Ref(inner, _span) => {
                let v = self.eval_expr(inner, env)?;
                Ok(Value::Ref(Box::new(v)))
            }
            Expr::Transact { precheck, network, network_retry, network_timeout, verify, commit, compensate, log, span } => {
                // TRANSACT.md's full durability protocol (Layers 1, 3,
                // 4): `precheck` can abort before anything durable is
                // written; `network`'s intent and result are logged
                // before/after it runs (the actual crash-safety
                // boundary); `commit`/`compensate` retry-with-backoff on
                // failure and trap (never silently compensate, never
                // silently succeed) if the retry budget is exhausted --
                // see this file's `run_transact_write_slot` and
                // `replay_pending_transactions` for the other half of
                // this protocol (resuming a row after a crash).
                // `env.push()`/`pop()` scopes `txn_id`/`network`/
                // `verify`'s implicit bindings to just this block, same
                // as `TRANSACT.md`'s "scoped only inside the transact
                // block."
                env.push();

                let txn_id = generate_txn_id();
                env.define("txn_id", Value::Str(Arc::from(txn_id.as_str())), Ty::Str);

                // `precheck`: nothing durable exists yet, so a `false`
                // here just aborts -- the fix for "the DB was already
                // down before `network` ran" (`TRANSACT.md`'s durability
                // section).
                if let Some(p) = precheck {
                    let precheck_val = self.eval_transact_slot(p, env)?;
                    let passed = match &precheck_val {
                        Value::Bool(b) => *b,
                        v => return Err(mismatch("bool", v.ty_name(), p.span)),
                    };
                    if !passed {
                        env.pop();
                        return Ok(Value::Bool(false));
                    }
                }

                let tlog = self.transact_log(*span)?;

                // Step 1: durably record intent, *then* run `network`.
                // Every slot's *callee name* is captured right here, up
                // front -- all four are statically known (typeck already
                // proved `verify`'s arguments are each exactly `network`
                // or `txn_id` -- `TransactVerifyArgsMustBeImplicitBindings`
                // -- so `verify_arg_kinds` below is always well-formed).
                // typeck already proved `network.name` resolves to a
                // user function (`infer_transact_slot` rejects
                // builtins), so `find_fn`/`call` are guaranteed to
                // succeed on the name itself here.
                let network_args = self.eval_transact_slot_args(network, env)?;
                let verify_arg_kinds: Vec<&str> = verify
                    .args
                    .iter()
                    .map(|a| match a {
                        Expr::Ident(name, _) if name == "network" => "network",
                        Expr::Ident(name, _) if name == "txn_id" => "txn_id",
                        _ => unreachable!(
                            "typeck.rs::TransactVerifyArgsMustBeImplicitBindings already restricted this"
                        ),
                    })
                    .collect();
                // `commit`/`compensate` have no static restriction on
                // their own arguments (unlike `verify`'s), so this is a
                // best-effort per-argument classification, not a proof —
                // "opaque" just means "not textually the `network`/
                // `txn_id` implicit binding," which `replay_one` treats
                // as unreconstructable. See `PendingTxn::commit_arg_kinds`'s
                // own doc comment for exactly what this closes.
                let transact_arg_kind = |a: &Expr| -> &'static str {
                    match a {
                        Expr::Ident(name, _) if name == "network" => "network",
                        Expr::Ident(name, _) if name == "txn_id" => "txn_id",
                        _ => "opaque",
                    }
                };
                let commit_arg_kinds: Vec<&str> = commit.args.iter().map(transact_arg_kind).collect();
                let compensate_arg_kinds: Option<Vec<&str>> =
                    compensate.as_ref().map(|c| c.args.iter().map(transact_arg_kind).collect());
                tlog.begin_pending(
                    &txn_id,
                    &network.name,
                    &network_args,
                    *network_retry,
                    *network_timeout,
                    &verify.name,
                    &verify_arg_kinds,
                    &commit.name,
                    &commit_arg_kinds,
                    compensate.as_ref().map(|c| c.name.as_str()),
                    compensate_arg_kinds.as_deref(),
                )
                .map_err(|e| self.transact_log_err(e, *span))?;
                let network_val = self
                    .call_network_with_retry(&network.name, &network_args, network.span, *network_retry, *network_timeout)
                    .map_err(Signal::Err)?;
                // Recorded before `verify` runs -- a restart that finds
                // this row already past `state = "pending"` never
                // re-invokes `network` again.
                tlog.record_network_result(&txn_id, &network_val).map_err(|e| self.transact_log_err(e, *span))?;
                let network_ty = self
                    .find_fn(&network.name)
                    .expect("typeck already proved this resolves to a user fn")
                    .ret
                    .clone();
                env.define("network", network_val, network_ty);

                // Step 2: `verify` runs, sees `network`. Its own
                // callee/args are logged first too -- a crash resuming
                // from `state = "network_done"` has to re-run `verify`
                // with its exact original arguments.
                let verify_args = self.eval_transact_slot_args(verify, env)?;
                let verify_val = self.call(&verify.name, &verify_args, verify.span).map_err(Signal::Err)?;
                let verified = match &verify_val {
                    Value::Bool(b) => *b,
                    v => return Err(mismatch("bool", v.ty_name(), verify.span)),
                };
                tlog.record_verify(&txn_id, &verify_args, verified).map_err(|e| self.transact_log_err(e, *span))?;
                env.define("verify", verify_val, Ty::Bool);

                // Step 3/4: commit-or-compensate. Neither falls through
                // to the other on failure -- `compensate` stays reserved
                // for `verified == false` only, a deliberate business
                // rejection, never an infra fault
                // (`run_transact_write_slot`'s own doc comment).
                let committed = if verified {
                    let commit_args = self.eval_transact_slot_args(commit, env)?;
                    tlog.mark_commit_pending(&txn_id, &commit_args).map_err(|e| self.transact_log_err(e, *span))?;
                    self.run_transact_write_slot(
                        &tlog,
                        &txn_id,
                        &commit.name,
                        &commit_args,
                        commit.span,
                        "committed",
                        |id| ErrorKind::TransactCommitPending { txn_id: id },
                    )?;
                    true
                } else if let Some(c) = compensate {
                    let compensate_args = self.eval_transact_slot_args(c, env)?;
                    tlog.mark_compensate_pending(&txn_id, &compensate_args)
                        .map_err(|e| self.transact_log_err(e, *span))?;
                    self.run_transact_write_slot(
                        &tlog,
                        &txn_id,
                        &c.name,
                        &compensate_args,
                        c.span,
                        "compensated",
                        |id| ErrorKind::TransactCompensatePending { txn_id: id },
                    )?;
                    false
                } else {
                    // No `compensate` slot to run -- nothing pending,
                    // terminal immediately.
                    tlog.mark_terminal(&txn_id, "compensated")
                        .map_err(|e| self.transact_log_err(e, *span))?;
                    false
                };

                // `log`, always best-effort: TRANSACT.md's own text --
                // "never itself part of the durability contract, never
                // replayed" -- a trap inside it must never undo an
                // already-successfully-committed/compensated `transact`.
                if let Some(l) = log {
                    let _ = self.eval_transact_slot(l, env);
                }

                env.pop();
                Ok(Value::Bool(committed))
            }
            Expr::Spawn(name, arg_exprs, span) => self.traced(
                Some(Effect::Concurrent),
                *span,
                "spawn",
                None,
                || {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    // Everything moved into the closure is owned, not
                    // borrowed from `self`/`env` — `std::thread::spawn`
                    // requires `'static`, and this is also the concrete,
                    // checkable form of the race-freedom claim: the spawned
                    // thread gets its *own* independent copy of the program
                    // (a cheap `Arc` clone) and its *own* values (already
                    // proven, by `ownership.rs`, to have been moved out of
                    // the spawning side — see this file's module doc).
                    let program = Arc::clone(&self.program);
                    let source = Arc::clone(&self.source);
                    let sandbox_exe = self.sandbox_exe.clone();
                    // Observability layer 1: a spawned thread's own fresh
                    // `Interpreter` inherits the same `tracer` via a cheap
                    // `Arc::clone`, the exact way `sandbox_exe` already
                    // threads through (see `tracer`'s doc comment) — every
                    // thread's spans land in the same exporter/file.
                    let tracer = self.tracer.clone();
                    let name = name.clone();
                    let call_span = *span;
                    // Same `Arc::clone` pattern as `tracer`/`sandbox_exe`
                    // above — the child shares the *same* registry, not a
                    // fresh one, so a `join`-cycle or "everyone's blocked"
                    // condition spanning parent and child is actually
                    // visible (see `DeadlockRegistry`'s doc comment).
                    let deadlock_registry = Arc::clone(&self.deadlock_registry);
                    let deadlock_registry_child = Arc::clone(&deadlock_registry);
                    let handle = std::thread::spawn(move || {
                        let mut interp = Interpreter::new(program, source);
                        if let Some(exe) = sandbox_exe {
                            interp = interp.with_sandbox_exe(exe);
                        }
                        if let Some(t) = tracer {
                            interp = interp.with_tracer(t);
                        }
                        interp = interp.with_deadlock_registry(Arc::clone(&deadlock_registry_child));
                        let result = interp.call(&name, &vals, call_span);
                        deadlock_registry_child.spawn_finished();
                        result
                    });
                    // Registered here, synchronously in the *parent*,
                    // right after `spawn` returns — not inside the
                    // child's own closure. See `DeadlockRegistry::
                    // spawn_started`'s doc comment for the race this
                    // avoids: counts as "live" (part of the whole-
                    // universe check) from this instant, for as long as
                    // it could still possibly call `send`, not just while
                    // it's actually inside a recv/join.
                    deadlock_registry.spawn_started(handle.thread().id());
                    Ok(Value::Thread(Arc::new(Mutex::new(Some(handle)))))
                },
                outcome_of_signal_result,
            ),
            Expr::Join(inner, span) => self.traced(
                Some(Effect::Concurrent),
                *span,
                "join",
                None,
                || {
                    let v = self.eval_expr(inner, env)?;
                    match v {
                        Value::Thread(slot) => {
                            // `.take()` is what makes a handle single-use at
                            // runtime, backing up `ownership.rs`'s static
                            // single-join proof the same "checker is the real
                            // gate, this is the backstop" way `check_ty`
                            // backs up `typeck.rs` elsewhere in this file.
                            let handle = slot.lock().unwrap().take();
                            match handle {
                                None => Err(Signal::Err(RuntimeError { kind: ErrorKind::AlreadyJoined, span: *span })),
                                Some(h) => {
                                    let finish = |h: ThreadHandle| match h.join() {
                                        Ok(Ok(result)) => Ok(result),
                                        Ok(Err(runtime_err)) => Err(Signal::Err(runtime_err)),
                                        Err(panic_payload) => {
                                            let message = panic_payload
                                                .downcast_ref::<&str>()
                                                .map(|s| s.to_string())
                                                .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                                                .unwrap_or_else(|| "(no message)".to_string());
                                            Err(Signal::Err(RuntimeError { kind: ErrorKind::ThreadPanicked { message }, span: *span }))
                                        }
                                    };
                                    // Fast path: a thread that already
                                    // finished makes `.join()` return
                                    // near-instantly — registering that as
                                    // "blocked" at all would falsely flag
                                    // completely ordinary "spawn, do other
                                    // work, join later" code.
                                    if h.is_finished() {
                                        return finish(h);
                                    }
                                    let target_id = h.thread().id();
                                    // Poll rather than a single check-then-
                                    // block-forever — see `DEADLOCK_POLL_
                                    // INTERVAL`'s doc comment: a deadlock
                                    // among *other* threads can finish
                                    // forming after this thread already
                                    // committed to waiting, and `join` (a
                                    // real `JoinHandle::join`) has no
                                    // timed variant to fall back on the
                                    // way `recv_timeout` gives `Expr::Recv`
                                    // — so instead of one blocking call,
                                    // this loop *is* the wait, checked
                                    // between short sleeps.
                                    loop {
                                        if h.is_finished() {
                                            return finish(h);
                                        }
                                        // Registered against the real
                                        // target's own `ThreadId` — a
                                        // precise edge, unlike `recv`'s
                                        // coarser one (see
                                        // `DeadlockRegistry`'s doc
                                        // comment).
                                        if let Err(message) = self.deadlock_registry.check_and_register(BlockedOn::Join(target_id)) {
                                            // Race guard, same reasoning as
                                            // `Expr::Recv`'s own: the check
                                            // itself takes real time, during
                                            // which the target can finish.
                                            // `check_and_register`'s `Err`
                                            // path already removed this
                                            // thread's own registration.
                                            if h.is_finished() {
                                                return finish(h);
                                            }
                                            // A proven-permanent deadlock
                                            // can never resolve — drop
                                            // detaches the OS thread instead
                                            // of hanging this one on it too.
                                            drop(h);
                                            return Err(Signal::Err(RuntimeError { kind: ErrorKind::Deadlock { message }, span: *span }));
                                        }
                                        std::thread::sleep(DEADLOCK_POLL_INTERVAL);
                                        self.deadlock_registry.unblock();
                                    }
                                }
                            }
                        }
                        v => Err(mismatch("thread", v.ty_name(), *span)),
                    }
                },
                outcome_of_signal_result,
            ),
            // Bare `chan()` construction is untraced on purpose — it's
            // `effects.rs`'s own classification too (`walk_expr`'s
            // `Expr::Chan(_) => {}` arm): allocating the handle is pure,
            // never blocks, never errors; only `send`/`recv`/`stop` on the
            // handle it produces are effectful, and those are traced below.
            Expr::Chan(_) => Ok(Value::Channel(Arc::new(ChannelInner::new()))),
            Expr::Str(s, _) => Ok(Value::Str(Arc::from(s.as_str()))),
            Expr::Send(chan_expr, value_expr, span) => {
                let c = self.eval_expr(chan_expr, env)?;
                let v = self.eval_expr(value_expr, env)?;
                match c {
                    Value::Channel(inner) => self.traced(
                        Some(Effect::Concurrent),
                        *span,
                        "chan.send",
                        None,
                        || match inner.send(v) {
                            Ok(()) => Ok(Value::Unit),
                            // Only reachable for a socket-backed channel (the
                            // in-process transport's `send` can never fail) —
                            // the peer (a sandboxed process) is gone or the
                            // pipe broke.
                            Err(e) => Err(Signal::Err(RuntimeError {
                                kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                span: *span,
                            })),
                        },
                        outcome_of_signal_result,
                    ),
                    Value::Tcp(slot) => self.traced(
                        Some(Effect::Network),
                        *span,
                        "tcp.send",
                        None,
                        || {
                            let Value::Str(text) = v else {
                                unreachable!("typeck.rs already restricted tcp send payloads to str")
                            };
                            let mut guard = slot.lock().unwrap();
                            match guard.as_mut() {
                                None => Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::ChannelIoError {
                                        message: "this tcp connection was already stopped".to_string(),
                                    },
                                    span: *span,
                                })),
                                Some(stream) => match write_tcp(stream, &text) {
                                    Ok(()) => Ok(Value::Unit),
                                    Err(e) => Err(Signal::Err(RuntimeError {
                                        kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                        span: *span,
                                    })),
                                },
                            }
                        },
                        outcome_of_signal_result,
                    ),
                    Value::File(slot) => self.traced(
                        Some(Effect::Io),
                        *span,
                        "file.send",
                        None,
                        || {
                            let Value::Str(text) = v else {
                                unreachable!("typeck.rs already restricted file send payloads to str")
                            };
                            let mut guard = slot.lock().unwrap();
                            match guard.as_mut() {
                                None => Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::ChannelIoError {
                                        message: "this file was already stopped".to_string(),
                                    },
                                    span: *span,
                                })),
                                Some(file) => match write_file(file, &text) {
                                    Ok(()) => Ok(Value::Unit),
                                    Err(e) => Err(Signal::Err(RuntimeError {
                                        kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                        span: *span,
                                    })),
                                },
                            }
                        },
                        outcome_of_signal_result,
                    ),
                    other => Err(mismatch("chan", other.ty_name(), *span)),
                }
            }
            Expr::Recv(chan_expr, span) => {
                let c = self.eval_expr(chan_expr, env)?;
                match c {
                    // Blocks the calling OS thread until `send` wakes it —
                    // see `ChannelInner::recv` and `Ty::Channel`'s doc
                    // comment for why this is a genuine wait primitive,
                    // not just a race-freedom mechanism.
                    Value::Channel(inner) => self.traced(
                        Some(Effect::Concurrent),
                        *span,
                        "chan.recv",
                        None,
                        || {
                            // Deadlock detection only applies to the
                            // in-process transport — a `Socket`-backed
                            // channel (crossed into a `sandbox`) can
                            // always still be woken by that external
                            // process; including it would be a real false
                            // positive, not just imprecision (see
                            // `ChannelInner::is_in_memory`'s doc comment).
                            if inner.is_in_memory() {
                                // Fast path: a value's already queued (the
                                // common `send` then `recv` case, even
                                // same-thread) — this call was never
                                // actually going to block, so it must
                                // never touch the deadlock registry at
                                // all, or a program that legitimately
                                // never blocks would get falsely flagged.
                                if let Some(v) = inner.try_recv() {
                                    return Ok(v);
                                }
                                // Poll rather than a single check-then-
                                // block-forever — see `DEADLOCK_POLL_
                                // INTERVAL`'s doc comment for why a
                                // one-shot check isn't enough (a deadlock
                                // among *other* threads can finish forming
                                // after this thread already committed to
                                // waiting).
                                loop {
                                    if let Err(message) = self.deadlock_registry.check_and_register(BlockedOn::Recv) {
                                        // Race guard: `check_and_register`
                                        // itself takes real time: a value
                                        // can legitimately arrive in that
                                        // window (`Expr::Join`'s own race
                                        // guard documents the general
                                        // shape in full).
                                        if let Some(v) = inner.try_recv() {
                                            return Ok(v);
                                        }
                                        return Err(Signal::Err(RuntimeError { kind: ErrorKind::Deadlock { message }, span: *span }));
                                    }
                                    let poll_result = inner.recv_timeout(DEADLOCK_POLL_INTERVAL);
                                    self.deadlock_registry.unblock();
                                    match poll_result {
                                        Ok(Some(v)) => return Ok(v),
                                        Ok(None) => continue, // timed out -- loop back and re-check for deadlock
                                        Err(e) => {
                                            return Err(Signal::Err(RuntimeError {
                                                kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                                span: *span,
                                            }))
                                        }
                                    }
                                }
                            }
                            match inner.recv() {
                                Ok(v) => Ok(v),
                                Err(e) => Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                    span: *span,
                                })),
                            }
                        },
                        outcome_of_signal_result,
                    ),
                    Value::Tcp(slot) => self.traced(
                        Some(Effect::Network),
                        *span,
                        "tcp.recv",
                        None,
                        || {
                            let mut guard = slot.lock().unwrap();
                            match guard.as_mut() {
                                None => Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::ChannelIoError {
                                        message: "this tcp connection was already stopped".to_string(),
                                    },
                                    span: *span,
                                })),
                                Some(stream) => match read_tcp(stream) {
                                    Ok(s) => Ok(Value::Str(Arc::from(s.as_str()))),
                                    Err(e) => Err(Signal::Err(RuntimeError {
                                        kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                        span: *span,
                                    })),
                                },
                            }
                        },
                        outcome_of_signal_result,
                    ),
                    Value::File(slot) => self.traced(
                        Some(Effect::Io),
                        *span,
                        "file.recv",
                        None,
                        || {
                            let mut guard = slot.lock().unwrap();
                            match guard.as_mut() {
                                None => Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::ChannelIoError {
                                        message: "this file was already stopped".to_string(),
                                    },
                                    span: *span,
                                })),
                                Some(file) => match read_file(file) {
                                    Ok(s) => Ok(Value::Str(Arc::from(s.as_str()))),
                                    Err(e) => Err(Signal::Err(RuntimeError {
                                        kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                        span: *span,
                                    })),
                                },
                            }
                        },
                        outcome_of_signal_result,
                    ),
                    other => Err(mismatch("chan", other.ty_name(), *span)),
                }
            }
            Expr::Connect(host_expr, port_expr, span) => self.traced(
                Some(Effect::Network),
                *span,
                "connect",
                None,
                || {
                    let host = match self.eval_expr(host_expr, env)? {
                        Value::Str(s) => s,
                        v => return Err(mismatch("str", v.ty_name(), *span)),
                    };
                    let port = match self.eval_expr(port_expr, env)? {
                        Value::Int(n) => match u16::try_from(n) {
                            Ok(p) => p,
                            Err(_) => {
                                return Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::ChannelIoError {
                                        message: format!("port {n} is not a valid 0-65535 TCP port"),
                                    },
                                    span: *span,
                                }));
                            }
                        },
                        v => return Err(mismatch("i64", v.ty_name(), *span)),
                    };
                    match std::net::TcpStream::connect((host.as_ref(), port)) {
                        Ok(stream) => Ok(Value::Tcp(Arc::new(Mutex::new(Some(stream))))),
                        Err(e) => Err(Signal::Err(RuntimeError {
                            kind: ErrorKind::ChannelIoError { message: e.to_string() },
                            span: *span,
                        })),
                    }
                },
                outcome_of_signal_result,
            ),
            Expr::Listen(port_expr, span) => self.traced(
                Some(Effect::Network),
                *span,
                "listen",
                None,
                || {
                    let port = match self.eval_expr(port_expr, env)? {
                        Value::Int(n) => match u16::try_from(n) {
                            Ok(p) => p,
                            Err(_) => {
                                return Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::ChannelIoError {
                                        message: format!("port {n} is not a valid 0-65535 TCP port"),
                                    },
                                    span: *span,
                                }));
                            }
                        },
                        v => return Err(mismatch("i64", v.ty_name(), *span)),
                    };
                    // Binds all interfaces (`0.0.0.0`), not just loopback --
                    // "simulation nodes can talk to each other" (unified plan
                    // §4.3.3) means across real machines/network namespaces,
                    // not just within one process.
                    match std::net::TcpListener::bind(("0.0.0.0", port)) {
                        Ok(listener) => Ok(Value::TcpListener(Arc::new(Mutex::new(Some(listener))))),
                        Err(e) => Err(Signal::Err(RuntimeError {
                            kind: ErrorKind::ChannelIoError { message: e.to_string() },
                            span: *span,
                        })),
                    }
                },
                outcome_of_signal_result,
            ),
            Expr::Accept(listener_expr, span) => self.traced(
                Some(Effect::Network),
                *span,
                "accept",
                None,
                || {
                    let lv = self.eval_expr(listener_expr, env)?;
                    let Value::TcpListener(slot) = lv else {
                        unreachable!("typeck.rs already proved this is a TcpListener")
                    };
                    let guard = slot.lock().unwrap();
                    match guard.as_ref() {
                        None => Err(Signal::Err(RuntimeError {
                            kind: ErrorKind::ChannelIoError {
                                message: "this tcp_listener was already stopped".to_string(),
                            },
                            span: *span,
                        })),
                        Some(listener) => match listener.accept() {
                            Ok((stream, _addr)) => Ok(Value::Tcp(Arc::new(Mutex::new(Some(stream))))),
                            Err(e) => Err(Signal::Err(RuntimeError {
                                kind: ErrorKind::ChannelIoError { message: e.to_string() },
                                span: *span,
                            })),
                        },
                    }
                },
                outcome_of_signal_result,
            ),
            Expr::Open(path_expr, mode_expr, span) => self.traced(
                Some(Effect::Io),
                *span,
                "open",
                None,
                || {
                    let path = match self.eval_expr(path_expr, env)? {
                        Value::Str(s) => s,
                        v => return Err(mismatch("str", v.ty_name(), *span)),
                    };
                    let mode = match self.eval_expr(mode_expr, env)? {
                        Value::Str(s) => s,
                        v => return Err(mismatch("str", v.ty_name(), *span)),
                    };
                    let opened = match mode.as_ref() {
                        "r" => std::fs::File::open(path.as_ref()),
                        "w" => std::fs::File::create(path.as_ref()),
                        "a" => std::fs::OpenOptions::new().append(true).create(true).open(path.as_ref()),
                        other => {
                            return Err(Signal::Err(RuntimeError {
                                kind: ErrorKind::ChannelIoError {
                                    message: format!("invalid file mode {other:?} — expected \"r\", \"w\", or \"a\""),
                                },
                                span: *span,
                            }));
                        }
                    };
                    match opened {
                        Ok(file) => Ok(Value::File(Arc::new(Mutex::new(Some(file))))),
                        Err(e) => Err(Signal::Err(RuntimeError {
                            kind: ErrorKind::ChannelIoError { message: e.to_string() },
                            span: *span,
                        })),
                    }
                },
                outcome_of_signal_result,
            ),
            Expr::SpawnSandbox(name, arg_exprs, span) => self.traced(
                Some(Effect::Concurrent),
                *span,
                "spawn_sandbox",
                None,
                || {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    self.spawn_sandbox(name, &vals, *span)
                },
                outcome_of_signal_result,
            ),
            Expr::StopSandbox(inner, span) => {
                let v = self.eval_expr(inner, env)?;
                match v {
                    Value::Sandbox(slot) => self.traced(
                        Some(Effect::Concurrent),
                        *span,
                        "sandbox.stop",
                        None,
                        || {
                            let taken = slot.lock().unwrap().take();
                            match taken {
                                None => Err(Signal::Err(RuntimeError {
                                    kind: ErrorKind::AlreadySandboxStopped,
                                    span: *span,
                                })),
                                Some(mut child) => Ok(Value::Int(child.stop())),
                            }
                        },
                        outcome_of_signal_result,
                    ),
                    // Closing a `tcp` connection is just dropping the
                    // `TcpStream` -- no kill-on-drop machinery needed the
                    // way `SandboxChild` needs (see `Value::Tcp`'s doc
                    // comment), so `.take()` alone is the whole
                    // implementation. A double-`stop` is `Unit`, not an
                    // error -- closing an already-closed connection isn't
                    // the same hazard a double-kill/double-join is, so
                    // there's nothing to guard against here.
                    Value::Tcp(slot) => self.traced(
                        Some(Effect::Network),
                        *span,
                        "tcp.stop",
                        None,
                        || {
                            drop(slot.lock().unwrap().take());
                            Ok(Value::Unit)
                        },
                        outcome_of_signal_result,
                    ),
                    // Same "just drop it, double-stop is Unit not an
                    // error" treatment as `Value::Tcp` above.
                    Value::TcpListener(slot) => self.traced(
                        Some(Effect::Network),
                        *span,
                        "tcp.stop",
                        None,
                        || {
                            drop(slot.lock().unwrap().take());
                            Ok(Value::Unit)
                        },
                        outcome_of_signal_result,
                    ),
                    // Same "just drop it, double-stop is Unit not an
                    // error" treatment as `Value::Tcp`/`Value::TcpListener`
                    // above — a `std::fs::File` closes its own descriptor
                    // on drop with no help required.
                    Value::File(slot) => self.traced(
                        Some(Effect::Io),
                        *span,
                        "file.stop",
                        None,
                        || {
                            drop(slot.lock().unwrap().take());
                            Ok(Value::Unit)
                        },
                        outcome_of_signal_result,
                    ),
                    // Same "just drop it, double-stop is Unit not an
                    // error" treatment as `Value::Tcp`/`Value::File` above
                    // — a `dbconn::DbConn` (SQLite or Postgres, chosen by
                    // `db_connect`'s connection string) closes its own
                    // connection on drop with no help required. Not part of
                    // `effects.rs`'s own `local_ty`/`StopSandbox` mapping
                    // (a pre-existing, documented gap there — `Ty::Db`
                    // isn't one of its matched cases), but `Effect::Io`
                    // is the honest tag here regardless: a db handle's own
                    // `open`/`send`/`recv` are already `Io`.
                    Value::Db(slot) => self.traced(
                        Some(Effect::Io),
                        *span,
                        "db.stop",
                        None,
                        || {
                            drop(slot.lock().unwrap().take());
                            Ok(Value::Unit)
                        },
                        outcome_of_signal_result,
                    ),
                    // Same treatment as `Value::Db` immediately above,
                    // one handle type later — a `redis::Connection`
                    // closes its own socket on drop, and this is
                    // `Effect::Network` (not `Io`) for the same reason
                    // `mq_connect`/`mq_publish`/`mq_consume` already are
                    // (`Ty::Mq`'s doc comment).
                    Value::Mq(slot) => self.traced(
                        Some(Effect::Network),
                        *span,
                        "mq.stop",
                        None,
                        || {
                            drop(slot.lock().unwrap().take());
                            Ok(Value::Unit)
                        },
                        outcome_of_signal_result,
                    ),
                    other => Err(mismatch("sandbox", other.ty_name(), *span)),
                }
            }
            Expr::Index(base, indices, span) => {
                let bv = self.eval_expr(base, env)?;
                match bv {
                    Value::Vector(elems) => {
                        debug_assert_eq!(indices.len(), 1, "typeck.rs already proved a Vector takes exactly one index");
                        let iv = self.eval_expr(&indices[0], env)?;
                        let Value::Int(i) = iv else {
                            unreachable!("typeck.rs already proved the index is an integer")
                        };
                        match usize::try_from(i).ok().and_then(|i| elems.get(i)) {
                            Some(v) => Ok(v.clone()),
                            None => Err(Signal::Err(RuntimeError {
                                kind: ErrorKind::IndexOutOfBounds { index: i, len: elems.len() },
                                span: *span,
                            })),
                        }
                    }
                    Value::Matrix(elems, rows, cols) => {
                        debug_assert_eq!(indices.len(), 2, "typeck.rs already proved a Matrix takes exactly two indices");
                        let riv = self.eval_expr(&indices[0], env)?;
                        let civ = self.eval_expr(&indices[1], env)?;
                        let (Value::Int(r), Value::Int(c)) = (riv, civ) else {
                            unreachable!("typeck.rs already proved both indices are integers")
                        };
                        let Some(r) = usize::try_from(r).ok().filter(|r| *r < rows) else {
                            return Err(Signal::Err(RuntimeError {
                                kind: ErrorKind::IndexOutOfBounds { index: r, len: rows },
                                span: *span,
                            }));
                        };
                        let Some(c) = usize::try_from(c).ok().filter(|c| *c < cols) else {
                            return Err(Signal::Err(RuntimeError {
                                kind: ErrorKind::IndexOutOfBounds { index: c, len: cols },
                                span: *span,
                            }));
                        };
                        Ok(elems[r * cols + c].clone())
                    }
                    v => unreachable!("typeck.rs already proved this is a Vector or Matrix, got {}", v.ty_name()),
                }
            }
            Expr::FieldAccess(base, field, span) => {
                let bv = self.eval_expr(base, env)?;
                let Value::Struct(name, fields) = &bv else {
                    unreachable!("typeck.rs already proved this is a struct, got {}", bv.ty_name())
                };
                let decl = self
                    .find_struct(name)
                    .expect("typeck.rs already proved this struct name is declared");
                let idx = decl
                    .fields
                    .iter()
                    .position(|f| &f.name == field)
                    .expect("typeck.rs already proved this field exists");
                let _ = span;
                Ok(fields[idx].clone())
            }
            Expr::Match { scrutinee, arms, span } => {
                let sv = self.eval_expr(scrutinee, env)?;
                // Literal-pattern match (`typeck.rs::check_literal_match`)
                // -- a `str`/`i64`/`bool` scrutinee is never an `Enum`
                // value, so it gets its own, much simpler evaluation:
                // first arm whose pattern matches wins (`_` matches
                // anything), same "first match wins" semantics the
                // mandatory-trailing-wildcard typecheck already commits
                // to. No payload bindings exist for this form.
                if matches!(sv, Value::Str(_) | Value::Int(_) | Value::Bool(_)) {
                    let arm = arms
                        .iter()
                        .find(|a| literal_pattern_matches(&a.pattern, &sv))
                        .expect("typeck.rs already proved this literal match has a matching arm (trailing `_` required)");
                    env.push();
                    let result = self.eval_expr(&arm.body, env);
                    env.pop();
                    let _ = span;
                    return result;
                }
                let Value::Enum(_, variant, payload) = &sv else {
                    unreachable!("typeck.rs already proved this is an enum, got {}", sv.ty_name())
                };
                let arm = arms
                    .iter()
                    .find(|a| a.variant.as_str() == variant.as_ref())
                    .expect("typeck.rs already proved every match is exhaustive");
                let (enum_decl, decl_variant) = self
                    .find_variant(variant)
                    .expect("typeck.rs already proved this variant name is declared");
                env.push();
                // Bound with the variant's real declared payload type
                // (not a placeholder) -- a binding is an ordinary local
                // like any other, and `Expr::Assign` inside the arm body
                // needs `get_ty` to answer with something real to check
                // a reassignment against. For a generic enum, the
                // declared payload type may itself be a bare reference
                // to the enum's own type parameter (`Some(T)`) -- there's
                // no expected-type context available here to substitute
                // it properly the way `typeck.rs`/`ownership.rs` can
                // (module doc: this file never threads one through
                // `eval_expr`), so `bind_type_params_from_value` recovers
                // a best-effort concrete type from the payload *value*
                // itself instead, purely so a later reassignment inside
                // this arm has something real to check against.
                let mut subst: HashMap<String, Ty> = HashMap::new();
                for (val, decl_ty) in payload.iter().zip(decl_variant.payload.iter()) {
                    self.bind_type_params_from_value(decl_ty, val, &enum_decl.type_params, &mut subst);
                }
                let subst_refs: HashMap<&str, &Ty> = subst.iter().map(|(k, v)| (k.as_str(), v)).collect();
                for ((name, val), decl_ty) in arm.bindings.iter().zip(payload.iter()).zip(decl_variant.payload.iter()) {
                    env.define(name, val.clone(), substitute_ty(decl_ty, &subst_refs));
                }
                let result = self.eval_expr(&arm.body, env);
                env.pop();
                let _ = span;
                result
            }
        }
    }

    /// Writes `self.source` to a fresh temp file and launches a *separate*
    /// `nirdosha --sandbox-worker <that file> <name> <args...>` process —
    /// a real OS process, not a thread, re-lexing/parsing/typechecking its
    /// own independent copy of the program (see the `source` field's doc
    /// comment for why re-parsing is necessary at all: no shared memory
    /// crosses a real process boundary). `typeck.rs` has already proved
    /// every value in `vals` is a plain `Int`/`Bool` (`infer_sandbox_spawn`
    /// restricts `name`'s declared parameters to scalars), so rendering
    /// each as a decimal/`true`/`false` command-line argument and parsing
    /// it back on the other side is a complete, lossless round trip — not
    /// a "best effort" serialization, the way an arbitrary `T` would need
    /// to be (SANDBOXING.md's layer 3, not this one).
    fn spawn_sandbox(&self, name: &str, vals: &[Value], span: Span) -> SResult<Value> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut tmp = std::env::temp_dir();
        tmp.push(format!("nirdosha_sandbox_{}_{n}.nir", std::process::id()));
        // Create the temp source file atomically with `create_new` so a
        // pre-existing file or symlink can't redirect the write elsewhere
        // (a local symlink race in the shared temp directory).
        let mut tmp_file = match std::fs::OpenOptions::new().write(true).create_new(true).open(&tmp) {
            Ok(f) => f,
            Err(e) => {
                return Err(Signal::Err(RuntimeError {
                    kind: ErrorKind::SandboxSpawnFailed { message: e.to_string() },
                    span,
                }));
            }
        };
        if let Err(e) = std::io::Write::write_all(&mut tmp_file, self.source.as_bytes()) {
            let _ = std::fs::remove_file(&tmp);
            return Err(Signal::Err(RuntimeError {
                kind: ErrorKind::SandboxSpawnFailed { message: e.to_string() },
                span,
            }));
        }

        let exe = match self.sandbox_exe.clone().map(Ok).unwrap_or_else(std::env::current_exe) {
            Ok(p) => p,
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                return Err(Signal::Err(RuntimeError {
                    kind: ErrorKind::SandboxSpawnFailed { message: e.to_string() },
                    span,
                }));
            }
        };

        // Every argument's declared parameter type decides how it's
        // rendered: a plain scalar becomes a decimal/`true`/`false`
        // string (as before layer 2); a `chan`-typed argument instead
        // becomes a fresh Unix socket path the spawned child connects to
        // (`cmd_sandbox_worker`, main.rs, does the matching `connect()`
        // using the exact same declared signature). `typeck.rs`'s
        // `SandboxArgMustBeScalar` already proved there's no third case.
        let param_tys: Vec<Ty> = self.find_fn(name).map(|f| f.params.iter().map(|p| p.ty.clone()).collect()).unwrap_or_default();
        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--sandbox-worker").arg(&tmp).arg(name);
        for (i, v) in vals.iter().enumerate() {
            match (v, param_tys.get(i)) {
                (Value::Int(n), _) => {
                    cmd.arg(n.to_string());
                }
                (Value::Bool(b), _) => {
                    cmd.arg(b.to_string());
                }
                (Value::Channel(inner), Some(Ty::Channel(_))) => match inner.prepare_for_sandbox() {
                    Ok(path) => {
                        cmd.arg(path);
                    }
                    Err(e) => {
                        let _ = std::fs::remove_file(&tmp);
                        return Err(Signal::Err(RuntimeError {
                            kind: ErrorKind::SandboxSpawnFailed { message: e.to_string() },
                            span,
                        }));
                    }
                },
                _ => unreachable!("typeck.rs already restricted sandbox args to int/bool/chan-of-scalar"),
            }
        }

        match cmd.spawn() {
            Ok(child) => {
                let sandbox = SandboxChild { child, tmp_source_path: tmp };
                Ok(Value::Sandbox(Arc::new(Mutex::new(Some(sandbox)))))
            }
            Err(e) => {
                let _ = std::fs::remove_file(&tmp);
                Err(Signal::Err(RuntimeError {
                    kind: ErrorKind::SandboxSpawnFailed { message: e.to_string() },
                    span,
                }))
            }
        }
    }

    fn eval_binary(
        &self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        env: &mut Env,
        span: Span,
    ) -> SResult<Value> {
        // Short-circuit && / || before evaluating the right side at all —
        // required for the operators to mean what their symbols claim.
        if op == BinOp::And || op == BinOp::Or {
            let l = self.eval_expr(lhs, env)?;
            let lb = match l {
                Value::Bool(b) => b,
                v => return Err(mismatch("bool", v.ty_name(), span)),
            };
            if op == BinOp::And && !lb {
                return Ok(Value::Bool(false));
            }
            if op == BinOp::Or && lb {
                return Ok(Value::Bool(true));
            }
            let r = self.eval_expr(rhs, env)?;
            return match r {
                Value::Bool(b) => Ok(Value::Bool(b)),
                v => Err(mismatch("bool", v.ty_name(), span)),
            };
        }

        let l = self.eval_expr(lhs, env)?;
        let r = self.eval_expr(rhs, env)?;
        let result: Result<Value, RuntimeError> = match (l, r) {
            (Value::Int(a), Value::Int(b)) => match op {
                BinOp::Add => Ok(Value::Int(a + b)),
                BinOp::Sub => Ok(Value::Int(a - b)),
                BinOp::Mul | BinOp::ElemMul => Ok(Value::Int(a * b)),
                BinOp::Div | BinOp::ElemDiv => {
                    if b == 0 {
                        err(ErrorKind::DivByZero, span)
                    } else {
                        Ok(Value::Int(a / b))
                    }
                }
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                BinOp::Lt => Ok(Value::Bool(a < b)),
                BinOp::Gt => Ok(Value::Bool(a > b)),
                BinOp::LtEq => Ok(Value::Bool(a <= b)),
                BinOp::GtEq => Ok(Value::Bool(a >= b)),
                BinOp::And | BinOp::Or => unreachable!("handled above"),
            },
            // No `DivByZero` guard, unlike the `Int` arm above -- IEEE 754
            // division by zero saturates to `inf`/`-inf`/`NaN` rather than
            // trapping, the float semantics this phase deliberately
            // chose (see `Ty::F64`'s doc comment): there's no error case
            // to construct here at all.
            (Value::Float(a), Value::Float(b)) => match op {
                BinOp::Add => Ok(Value::Float(a + b)),
                BinOp::Sub => Ok(Value::Float(a - b)),
                BinOp::Mul | BinOp::ElemMul => Ok(Value::Float(a * b)),
                BinOp::Div | BinOp::ElemDiv => Ok(Value::Float(a / b)),
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                BinOp::Lt => Ok(Value::Bool(a < b)),
                BinOp::Gt => Ok(Value::Bool(a > b)),
                BinOp::LtEq => Ok(Value::Bool(a <= b)),
                BinOp::GtEq => Ok(Value::Bool(a >= b)),
                BinOp::And | BinOp::Or => unreachable!("handled above"),
            },
            // `dec128` (`LANGUAGE.md` §2/§6c/§6d) — traps on div-by-zero
            // like `Int`, not saturate like `Float`, since a `Decimal`
            // dividing by zero has no `inf`/`NaN` to saturate to. `+`/
            // `-`/`*` are `Decimal`'s own operators, which panic past the
            // representation's digit limit — see `scalar_binop`'s Dec128
            // arm doc comment for why that's the intended trap idiom
            // here, not a gap.
            (Value::Dec128(a), Value::Dec128(b)) => match op {
                BinOp::Add => Ok(Value::Dec128(a + b)),
                BinOp::Sub => Ok(Value::Dec128(a - b)),
                BinOp::Mul | BinOp::ElemMul => Ok(Value::Dec128(a * b)),
                BinOp::Div | BinOp::ElemDiv => {
                    if b.is_zero() {
                        err(ErrorKind::DivByZero, span)
                    } else {
                        Ok(Value::Dec128(a / b))
                    }
                }
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                BinOp::Lt => Ok(Value::Bool(a < b)),
                BinOp::Gt => Ok(Value::Bool(a > b)),
                BinOp::LtEq => Ok(Value::Bool(a <= b)),
                BinOp::GtEq => Ok(Value::Bool(a >= b)),
                BinOp::And | BinOp::Or => unreachable!("handled above"),
            },
            (Value::Bool(a), Value::Bool(b)) => match op {
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                _ => err(
                    ErrorKind::TypeMismatch { expected: "int".to_string(), found: "bool".to_string() },
                    span,
                ),
            },
            // `typeck.rs`'s `unify_operands` already permits `str == str`
            // generically (same-type `Eq`/`NotEq` is allowed for *any*
            // pair of matching types, not just `Int`/`Bool`) -- this was
            // a real gap between that promise and what the interpreter
            // actually implemented, caught by testing the equality this
            // typechecks, not by re-reading either file.
            (Value::Str(a), Value::Str(b)) => match op {
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                _ => err(
                    ErrorKind::TypeMismatch { expected: "int".to_string(), found: "str".to_string() },
                    span,
                ),
            },
            // `typeck.rs`'s `unify_operands` permits `==`/`!=` generically
            // for any pair of matching types, `Ty::Named` (struct/enum)
            // included -- same gap `Value::Str`'s comment above already
            // documents for `str`, found the same way (by testing code
            // that typechecks, not by re-reading either file): a
            // `struct`/`enum` value already has a correct `PartialEq`
            // impl just above (`an == bn && af == bf` /
            // `an == bn && av == bv && af == bf`), it just had no arm
            // here to reach it. Surfaced by the "enum favoring" str-ban
            // work — code migrating off `if status_str == "PENDING"` onto
            // a real `enum` naturally reaches for `==` next, so this
            // needs to actually work now, not just typecheck.
            (l @ Value::Struct(_, _), r @ Value::Struct(_, _)) | (l @ Value::Enum(_, _, _), r @ Value::Enum(_, _, _)) => {
                match op {
                    BinOp::Eq => Ok(Value::Bool(l == r)),
                    BinOp::NotEq => Ok(Value::Bool(l != r)),
                    _ => err(
                        ErrorKind::TypeMismatch { expected: "int".to_string(), found: l.ty_name().to_string() },
                        span,
                    ),
                }
            }
            // Elementwise `+`/`-`/`.*`/`./`, plus structural `==`/`!=` --
            // `typeck.rs` already proved the two operands have exactly
            // the same shape (a `Vector(T, n)` and a `Vector(T, m)` are
            // different `Ty`s, so `n == m` here isn't re-validated, just
            // asserted), so every arm below just computes.
            (Value::Vector(a), Value::Vector(b)) => match op {
                BinOp::Add | BinOp::Sub | BinOp::ElemMul | BinOp::ElemDiv => {
                    debug_assert_eq!(a.len(), b.len(), "typeck.rs already proved equal Vector shapes");
                    let out: Result<Vec<Value>, RuntimeError> =
                        a.iter().zip(b.iter()).map(|(x, y)| scalar_binop(op, x, y, span)).collect();
                    Ok(Value::Vector(Arc::from(out?)))
                }
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                _ => unreachable!("typeck.rs already restricted Vector's other operators"),
            },
            (Value::Matrix(a, ar, ac), Value::Matrix(b, br, bc)) => match op {
                BinOp::Add | BinOp::Sub | BinOp::ElemMul | BinOp::ElemDiv => {
                    debug_assert_eq!((ar, ac), (br, bc), "typeck.rs already proved equal Matrix shapes");
                    let out: Result<Vec<Value>, RuntimeError> =
                        a.iter().zip(b.iter()).map(|(x, y)| scalar_binop(op, x, y, span)).collect();
                    Ok(Value::Matrix(Arc::from(out?), ar, ac))
                }
                // matrix * matrix -- typeck.rs already proved the inner
                // dimensions (`ac`/`br`) match.
                BinOp::Mul => {
                    let mut out = Vec::with_capacity(ar * bc);
                    for i in 0..ar {
                        for j in 0..bc {
                            let mut sum = scalar_binop(BinOp::Mul, &a[i * ac], &b[j], span)?;
                            for k in 1..ac {
                                let prod = scalar_binop(BinOp::Mul, &a[i * ac + k], &b[k * bc + j], span)?;
                                sum = scalar_binop(BinOp::Add, &sum, &prod, span)?;
                            }
                            out.push(sum);
                        }
                    }
                    Ok(Value::Matrix(Arc::from(out), ar, bc))
                }
                BinOp::Eq => Ok(Value::Bool(ar == br && ac == bc && a == b)),
                BinOp::NotEq => Ok(Value::Bool(!(ar == br && ac == bc && a == b))),
                _ => unreachable!("typeck.rs already restricted Matrix's other operators"),
            },
            // matrix * vector -- typeck.rs already proved the matrix's
            // column count equals the vector's length.
            (Value::Matrix(m, rows, cols), Value::Vector(v)) => {
                let mut out = Vec::with_capacity(rows);
                for i in 0..rows {
                    let mut sum = scalar_binop(BinOp::Mul, &m[i * cols], &v[0], span)?;
                    for k in 1..cols {
                        let prod = scalar_binop(BinOp::Mul, &m[i * cols + k], &v[k], span)?;
                        sum = scalar_binop(BinOp::Add, &sum, &prod, span)?;
                    }
                    out.push(sum);
                }
                Ok(Value::Vector(Arc::from(out)))
            }
            // scalar * matrix, either order -- typeck.rs already proved
            // the scalar's type matches the matrix's element type.
            (s @ (Value::Int(_) | Value::Float(_)), Value::Matrix(elems, rows, cols))
            | (Value::Matrix(elems, rows, cols), s @ (Value::Int(_) | Value::Float(_))) => {
                let out: Result<Vec<Value>, RuntimeError> =
                    elems.iter().map(|x| scalar_binop(BinOp::Mul, x, &s, span)).collect();
                Ok(Value::Matrix(Arc::from(out?), rows, cols))
            }
            (l, r) => err(
                ErrorKind::TypeMismatch {
                    expected: l.ty_name().to_string(),
                    found: r.ty_name().to_string(),
                },
                span,
            ),
        };
        result.map_err(Signal::Err)
    }
}
