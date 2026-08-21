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

use serde::{Deserialize, Serialize};

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
            (Value::Float(a), Value::Float(b)) => a == b,
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
            (Value::Struct(an, af), Value::Struct(bn, bf)) => an == bn && af == bf,
            (Value::Enum(an, av, af), Value::Enum(bn, bv, bf)) => an == bn && av == bv && af == bf,
            _ => false,
        }
    }
}

impl Value {
    fn ty_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Float(_) => "f64",
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
            // A generic tag, not the real declared name — every real call
            // site that needs the *actual* struct/enum name already has
            // the `Ty::Named`/`Value::Struct`/`Value::Enum` name field
            // directly at hand and doesn't need to go through this
            // generic method; this exists only so `ty_name` stays total.
            Value::Struct(..) => "struct",
            Value::Enum(..) => "enum",
        }
    }

    fn render(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Float(n) => n.to_string(),
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
            Value::Struct(name, fields) => {
                format!("{name}({})", fields.iter().map(Value::render).collect::<Vec<_>>().join(", "))
            }
            Value::Enum(_, variant, payload) => {
                format!("{variant}({})", payload.iter().map(Value::render).collect::<Vec<_>>().join(", "))
            }
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
        _ => unreachable!("typeck.rs already proved matching, numeric-scalar element types"),
    }
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
        _ => unreachable!("ast::BUILTIN_NAMES and eval_builtin's match must stay in sync"),
    }
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

impl Interpreter {
    pub fn new(program: Arc<Program>, source: Arc<str>) -> Self {
        let fn_index = program.fns.iter().enumerate().map(|(i, f)| (f.name.clone(), i)).collect();
        Interpreter { program, fn_index, source, sandbox_exe: None, rng: std::cell::RefCell::new(None) }
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

    /// The one place `Signal::Return` gets caught and turned back into a
    /// plain value — every nested `if`/block/expression underneath just
    /// propagates it with `?`.
    /// Evaluates one `transact` slot's arguments, then invokes its named
    /// function. `typeck.rs::infer_transact_slot` already proved this
    /// name isn't a builtin, so — unlike `Expr::Call`'s own arm, which
    /// has to dispatch both ways — this always goes through `self.call`.
    fn eval_transact_slot(&self, slot: &TransactSlot, env: &mut Env) -> SResult<Value> {
        let mut vals = Vec::with_capacity(slot.args.len());
        for a in &slot.args {
            vals.push(self.eval_expr(a, env)?);
        }
        self.call(&slot.name, &vals, slot.span).map_err(Signal::Err)
    }

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
            (Value::TcpListener(_), Ty::TcpListener) => Ok(()),
            (Value::File(_), Ty::File) => Ok(()),
            (Value::Float(_), Ty::F64) => Ok(()),
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
            (Value::Struct(name, fields), Ty::Named(want_name)) if name.as_ref() == want_name.as_str() => {
                let decl = self
                    .find_struct(want_name)
                    .expect("typeck.rs already proved this struct name is declared");
                for (v, field) in fields.iter().zip(decl.fields.iter()) {
                    self.check_ty(v, &field.ty, span)?;
                }
                Ok(())
            }
            (Value::Enum(name, variant, payload), Ty::Named(want_name)) if name.as_ref() == want_name.as_str() => {
                let (_, v) = self
                    .find_variant(variant)
                    .expect("typeck.rs already proved this variant name is declared");
                for (val, want) in payload.iter().zip(v.payload.iter()) {
                    self.check_ty(val, want, span)?;
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
            Expr::Ident(name, span) => env.get(name).ok_or_else(|| {
                Signal::Err(RuntimeError { kind: ErrorKind::UnknownVar(name.clone()), span: *span })
            }),
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
                if is_builtin(name) {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
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
            Expr::Transact { network, verify, commit, compensate, log, .. } => {
                // TRANSACT.md's five-step protocol, Layer 1 slice: no
                // durability log, no retry/timeout — `network` runs
                // exactly once. `env.push()`/`pop()` scopes `network`/
                // `verify`'s implicit bindings to just this block, same
                // as `TRANSACT.md`'s "scoped only inside the transact
                // block."
                env.push();

                // Steps 1-2: `network` runs, its result becomes an
                // implicit local binding visible to every slot after it.
                // typeck already proved `network.name` resolves to a
                // user function (`infer_transact_slot` rejects builtins),
                // so `find_fn` is guaranteed to succeed here.
                let network_val = self.eval_transact_slot(network, env)?;
                let network_ty = self
                    .find_fn(&network.name)
                    .expect("typeck already proved this resolves to a user fn")
                    .ret
                    .clone();
                env.define("network", network_val, network_ty);

                // Step 3: `verify` runs, sees `network`.
                let verify_val = self.eval_transact_slot(verify, env)?;
                let verified = match &verify_val {
                    Value::Bool(b) => *b,
                    v => return Err(mismatch("bool", v.ty_name(), verify.span)),
                };
                env.define("verify", verify_val, Ty::Bool);

                // Step 4/5: commit-or-compensate, then a best-effort
                // `log` (never itself part of the durability contract —
                // there is no durability log yet in Layer 1).
                let committed = if verified {
                    self.eval_transact_slot(commit, env)?;
                    true
                } else {
                    if let Some(c) = compensate {
                        self.eval_transact_slot(c, env)?;
                    }
                    false
                };
                if let Some(l) = log {
                    self.eval_transact_slot(l, env)?;
                }

                env.pop();
                Ok(Value::Bool(committed))
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
                    Value::File(slot) => {
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
                    Value::File(slot) => {
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
            Expr::Listen(port_expr, span) => {
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
            }
            Expr::Accept(listener_expr, span) => {
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
            }
            Expr::Open(path_expr, mode_expr, span) => {
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
                    // Same "just drop it, double-stop is Unit not an
                    // error" treatment as `Value::Tcp` above.
                    Value::TcpListener(slot) => {
                        drop(slot.lock().unwrap().take());
                        Ok(Value::Unit)
                    }
                    // Same "just drop it, double-stop is Unit not an
                    // error" treatment as `Value::Tcp`/`Value::TcpListener`
                    // above — a `std::fs::File` closes its own descriptor
                    // on drop with no help required.
                    Value::File(slot) => {
                        drop(slot.lock().unwrap().take());
                        Ok(Value::Unit)
                    }
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
                let Value::Enum(_, variant, payload) = &sv else {
                    unreachable!("typeck.rs already proved this is an enum, got {}", sv.ty_name())
                };
                let arm = arms
                    .iter()
                    .find(|a| a.variant.as_str() == variant.as_ref())
                    .expect("typeck.rs already proved every match is exhaustive");
                let (_, decl_variant) = self
                    .find_variant(variant)
                    .expect("typeck.rs already proved this variant name is declared");
                env.push();
                // Bound with the variant's real declared payload type
                // (not a placeholder) -- a binding is an ordinary local
                // like any other, and `Expr::Assign` inside the arm body
                // needs `get_ty` to answer with something real to check
                // a reassignment against.
                for ((name, val), ty) in arm.bindings.iter().zip(payload.iter()).zip(decl_variant.payload.iter()) {
                    env.define(name, val.clone(), ty.clone());
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
