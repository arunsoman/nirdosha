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
}

/// A spawned computation's raw join handle. Its own alias, not just inline
/// in `Value::Thread`, purely to keep clippy's `type_complexity` lint (and
/// human readers) from tripping over four levels of generic nesting.
type ThreadHandle = std::thread::JoinHandle<Result<Value, RuntimeError>>;

/// The shared state behind a `Value::Channel`: a plain `Mutex`-guarded
/// FIFO queue plus a `Condvar` to let `recv` block until `send` wakes it,
/// rather than spin-polling. `send` never blocks (the queue is unbounded)
/// — the only blocking operation is `recv` on an empty queue, which is
/// also the reason row 3's "no deadlocks" claim stays narrower than full
/// proof-by-construction for channels specifically; see `Ty::Channel`'s
/// doc comment.
#[derive(Debug)]
pub struct ChannelInner {
    queue: Mutex<VecDeque<Value>>,
    not_empty: Condvar,
}

impl ChannelInner {
    fn new() -> Self {
        ChannelInner { queue: Mutex::new(VecDeque::new()), not_empty: Condvar::new() }
    }

    fn send(&self, v: Value) {
        self.queue.lock().unwrap().push_back(v);
        self.not_empty.notify_one();
    }

    fn recv(&self) -> Value {
        let mut q = self.queue.lock().unwrap();
        while q.is_empty() {
            q = self.not_empty.wait(q).unwrap();
        }
        q.pop_front().expect("just proved non-empty under the same lock")
    }
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
            Expr::Send(chan_expr, value_expr, span) => {
                let c = self.eval_expr(chan_expr, env)?;
                let v = self.eval_expr(value_expr, env)?;
                match c {
                    Value::Channel(inner) => {
                        inner.send(v);
                        Ok(Value::Unit)
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
                    Value::Channel(inner) => Ok(inner.recv()),
                    other => Err(mismatch("chan", other.ty_name(), *span)),
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

        let mut cmd = std::process::Command::new(exe);
        cmd.arg("--sandbox-worker").arg(&tmp).arg(name);
        for v in vals {
            let arg = match v {
                Value::Int(n) => n.to_string(),
                Value::Bool(b) => b.to_string(),
                _ => unreachable!("typeck.rs already restricted sandbox args to int/bool"),
            };
            cmd.arg(arg);
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
