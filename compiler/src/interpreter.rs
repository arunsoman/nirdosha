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

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Condvar, Mutex};

use crate::ast::*;
use crate::token::Span;

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
#[derive(Debug, Clone)]
pub enum Value {
    Int(i64),
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

#[derive(Debug)]
enum TransportState {
    InMemory(VecDeque<Value>),
    PendingListener(std::os::unix::net::UnixListener, std::path::PathBuf),
    Socket(std::os::unix::net::UnixStream),
}

impl ChannelInner {
    fn new() -> Self {
        ChannelInner { state: Mutex::new(TransportState::InMemory(VecDeque::new())), not_empty: Condvar::new() }
    }

    /// The child side's constructor: a `Value::Channel` built directly
    /// from an already-connected socket, no `InMemory`/`PendingListener`
    /// detour — the child never creates a channel via `chan`, it's always
    /// handed one that's already meant to cross a process boundary.
    pub fn from_socket(stream: std::os::unix::net::UnixStream) -> Self {
        ChannelInner { state: Mutex::new(TransportState::Socket(stream)), not_empty: Condvar::new() }
    }

    /// Binds a fresh Unix domain socket and transitions this channel from
    /// `InMemory` to `PendingListener`, returning the socket's path (for
    /// the caller to pass to the spawned child via argv, the same way the
    /// source file's temp path already is). Doesn't `accept()` — that's
    /// deferred to first use (see the type's doc comment) — so this never
    /// blocks.
    fn prepare_for_sandbox(&self) -> std::io::Result<std::path::PathBuf> {
        static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let mut path = std::env::temp_dir();
        path.push(format!("nirdosha_sandbox_chan_{}_{n}.sock", std::process::id()));

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
        let listener = std::os::unix::net::UnixListener::bind(&path)?;
        *state = TransportState::PendingListener(listener, path.clone());
        Ok(path)
    }

    /// If still `PendingListener`, blocks until the child connects, then
    /// transitions to `Socket` — the one place this file's "sandbox
    /// spawning never blocks, only send/recv do" rule gets enforced for
    /// the channel transport specifically. Unlinks the socket's path
    /// immediately on a successful accept: once connected, a Unix domain
    /// socket doesn't need its filesystem path anymore, and nothing else
    /// will ever connect to it (exactly one child, exactly one accept).
    fn ensure_connected(state: &mut TransportState) -> std::io::Result<()> {
        if let TransportState::PendingListener(listener, path) = state {
            let (stream, _addr) = listener.accept()?;
            let _ = std::fs::remove_file(path);
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
}

impl Drop for ChannelInner {
    /// The only cleanup a channel ever needs beyond Rust's own (closing
    /// the socket fd, which `UnixListener`/`UnixStream`'s own `Drop`
    /// already does): a *bound but never-accepted* listener's socket file
    /// would otherwise leak on disk — `ensure_connected` already unlinks
    /// it on the success path, so this only ever fires for a channel that
    /// was prepared for a sandbox but never actually used.
    fn drop(&mut self) {
        let state = match self.state.get_mut() {
            Ok(s) => s,
            Err(poisoned) => poisoned.into_inner(),
        };
        if let TransportState::PendingListener(_, path) = state {
            let _ = std::fs::remove_file(path);
        }
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
fn write_value(stream: &mut std::os::unix::net::UnixStream, v: &Value) -> std::io::Result<()> {
    use std::io::Write;
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

fn read_value(stream: &mut std::os::unix::net::UnixStream) -> std::io::Result<Value> {
    use std::io::Read;
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
    /// its temp source file. Returns the OS exit code, or `-1` if it
    /// exited via a signal (including this same call's own `kill()` —
    /// SIGKILL termination has no exit code to report) or if the wait
    /// itself failed.
    fn stop(&mut self) -> i64 {
        if self.child.try_wait().ok().flatten().is_none() {
            let _ = self.child.kill();
        }
        let code = match self.child.wait() {
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
            (Value::Bool(a), Value::Bool(b)) => a == b,
            (Value::Unit, Value::Unit) => true,
            (Value::Boxed(a), Value::Boxed(b)) => a == b,
            (Value::Ref(a), Value::Ref(b)) => a == b,
            (Value::Thread(a), Value::Thread(b)) => Arc::ptr_eq(a, b),
            (Value::Channel(a), Value::Channel(b)) => Arc::ptr_eq(a, b),
            (Value::Sandbox(a), Value::Sandbox(b)) => Arc::ptr_eq(a, b),
            (Value::Str(a), Value::Str(b)) => a == b,
            (Value::Tcp(a), Value::Tcp(b)) => Arc::ptr_eq(a, b),
            _ => false,
        }
    }
}

impl Value {
    fn ty_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Bool(_) => "bool",
            Value::Unit => "unit",
            Value::Boxed(_) => "box",
            Value::Ref(_) => "ref",
            Value::Thread(_) => "thread",
            Value::Channel(_) => "chan",
            Value::Sandbox(_) => "sandbox",
            Value::Str(_) => "str",
            Value::Tcp(_) => "tcp",
        }
    }

    fn render(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Unit => "()".to_string(),
            Value::Boxed(inner) => format!("box({})", inner.render()),
            Value::Ref(inner) => format!("&{}", inner.render()),
            Value::Thread(_) => "thread(..)".to_string(),
            Value::Channel(_) => "chan(..)".to_string(),
            Value::Sandbox(_) => "sandbox(..)".to_string(),
            Value::Str(s) => s.to_string(),
            Value::Tcp(_) => "tcp(..)".to_string(),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
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
}

#[derive(Debug, Clone)]
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
        }
    }
}

fn err<T>(kind: ErrorKind, span: Span) -> Result<T, RuntimeError> {
    Err(RuntimeError { kind, span })
}

fn mismatch(expected: impl Into<String>, found: impl Into<String>, span: Span) -> Signal {
    Signal::Err(RuntimeError {
        kind: ErrorKind::TypeMismatch { expected: expected.into(), found: found.into() },
        span,
    })
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
}

impl Interpreter {
    pub fn new(program: Arc<Program>, source: Arc<str>) -> Self {
        let fn_index = program.fns.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
        Interpreter { program, fn_index, source, sandbox_exe: None }
    }

    pub fn with_sandbox_exe(mut self, path: std::path::PathBuf) -> Self {
        self.sandbox_exe = Some(path);
        self
    }

    pub fn run_main(&self) -> Result<Value, RuntimeError> {
        let span = Span { line: 0, col: 0 };
        self.call("main", &[], span)
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
        self.call(name, args, Span { line: 0, col: 0 })
    }

    fn find_fn(&self, name: &str) -> Option<&FnDecl> {
        self.fn_index.get(name).map(|&i| &self.program.fns[i])
    }

    /// The one place `Signal::Return` gets caught and turned back into a
    /// plain value — every nested `if`/block/expression underneath just
    /// propagates it with `?`.
    fn call(&self, name: &str, arg_vals: &[Value], span: Span) -> Result<Value, RuntimeError> {
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
            }
        }
        Ok(last)
    }

    fn eval_expr(&self, expr: &Expr, env: &mut Env) -> SResult<Value> {
        match expr {
            Expr::Int(n, _) => Ok(Value::Int(*n)),
            Expr::Bool(b, _) => Ok(Value::Bool(*b)),
            Expr::Ident(name, span) => env.get(name).ok_or_else(|| {
                Signal::Err(RuntimeError { kind: ErrorKind::UnknownVar(name.clone()), span: *span })
            }),
            Expr::Unary(op, inner, span) => {
                let v = self.eval_expr(inner, env)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Neg, v) => Err(mismatch("int", v.ty_name(), *span)),
                    (UnOp::Not, v) => Err(mismatch("bool", v.ty_name(), *span)),
                }
            }
            Expr::Binary(op, lhs, rhs, span) => self.eval_binary(*op, lhs, rhs, env, *span),
            Expr::Call(name, arg_exprs, span) => {
                if name == "print" {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    let rendered: Vec<String> = vals.iter().map(Value::render).collect();
                    println!("{}", rendered.join(" "));
                    return Ok(Value::Unit);
                }
                let mut vals = Vec::with_capacity(arg_exprs.len());
                for a in arg_exprs {
                    vals.push(self.eval_expr(a, env)?);
                }
                self.call(name, &vals, *span).map_err(Signal::Err)
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
            Expr::Spawn(name, arg_exprs, span) => {
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
                let name = name.clone();
                let call_span = *span;
                let handle = std::thread::spawn(move || {
                    let mut interp = Interpreter::new(program, source);
                    if let Some(exe) = sandbox_exe {
                        interp = interp.with_sandbox_exe(exe);
                    }
                    interp.call(&name, &vals, call_span)
                });
                Ok(Value::Thread(Arc::new(Mutex::new(Some(handle)))))
            }
            Expr::Join(inner, span) => {
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
                            Some(h) => match h.join() {
                                Ok(Ok(result)) => Ok(result),
                                Ok(Err(runtime_err)) => Err(Signal::Err(runtime_err)),
                                Err(panic_payload) => {
                                    let message = panic_payload
                                        .downcast_ref::<&str>()
                                        .map(|s| s.to_string())
                                        .or_else(|| panic_payload.downcast_ref::<String>().cloned())
                                        .unwrap_or_else(|| "(no message)".to_string());
                                    Err(Signal::Err(RuntimeError {
                                        kind: ErrorKind::ThreadPanicked { message },
                                        span: *span,
                                    }))
                                }
                            },
                        }
                    }
                    v => Err(mismatch("thread", v.ty_name(), *span)),
                }
            }
            Expr::Chan(_) => Ok(Value::Channel(Arc::new(ChannelInner::new()))),
            Expr::Str(s, _) => Ok(Value::Str(Arc::from(s.as_str()))),
            Expr::Send(chan_expr, value_expr, span) => {
                let c = self.eval_expr(chan_expr, env)?;
                let v = self.eval_expr(value_expr, env)?;
                match c {
                    Value::Channel(inner) => match inner.send(v) {
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
                    Value::Tcp(slot) => {
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
                    }
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
                    Value::Channel(inner) => match inner.recv() {
                        Ok(v) => Ok(v),
                        Err(e) => Err(Signal::Err(RuntimeError {
                            kind: ErrorKind::ChannelIoError { message: e.to_string() },
                            span: *span,
                        })),
                    },
                    Value::Tcp(slot) => {
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
                    }
                    other => Err(mismatch("chan", other.ty_name(), *span)),
                }
            }
            Expr::Connect(host_expr, port_expr, span) => {
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
            }
            Expr::SpawnSandbox(name, arg_exprs, span) => {
                let mut vals = Vec::with_capacity(arg_exprs.len());
                for a in arg_exprs {
                    vals.push(self.eval_expr(a, env)?);
                }
                self.spawn_sandbox(name, &vals, *span)
            }
            Expr::StopSandbox(inner, span) => {
                let v = self.eval_expr(inner, env)?;
                match v {
                    Value::Sandbox(slot) => {
                        let taken = slot.lock().unwrap().take();
                        match taken {
                            None => Err(Signal::Err(RuntimeError {
                                kind: ErrorKind::AlreadySandboxStopped,
                                span: *span,
                            })),
                            Some(mut child) => Ok(Value::Int(child.stop())),
                        }
                    }
                    // Closing a `tcp` connection is just dropping the
                    // `TcpStream` -- no kill-on-drop machinery needed the
                    // way `SandboxChild` needs (see `Value::Tcp`'s doc
                    // comment), so `.take()` alone is the whole
                    // implementation. A double-`stop` is `Unit`, not an
                    // error -- closing an already-closed connection isn't
                    // the same hazard a double-kill/double-join is, so
                    // there's nothing to guard against here.
                    Value::Tcp(slot) => {
                        drop(slot.lock().unwrap().take());
                        Ok(Value::Unit)
                    }
                    other => Err(mismatch("sandbox", other.ty_name(), *span)),
                }
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
        if let Err(e) = std::fs::write(&tmp, self.source.as_bytes()) {
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
                BinOp::Mul => Ok(Value::Int(a * b)),
                BinOp::Div => {
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
