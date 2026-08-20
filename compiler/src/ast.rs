//! The AST. Each node type is a fixed, closed set — deliberately, since row 8
//! (goal.md) asks for a semantics where the meaning of a compound expression
//! is a function only of the meanings of its parts (`⟦compose(a,b)⟧ =
//! F(⟦a⟧,⟦b⟧)`), never a special case. Concretely: `interpreter.rs` has
//! exactly one `eval_expr` match arm per `Expr` variant here, and no variant
//! reaches back into a sibling's internals — composition, not entanglement.

use crate::token::Span;

/// Not `Copy` — deliberately. `Ty::Box` owns a nested `Ty`, and giving the
/// whole enum a free bitwise-copy would say every type is trivially
/// duplicable, which is exactly the property `Ty::Box` exists to *not*
/// have (see `is_affine`, and the real move-checker in `ownership.rs`).
/// `Clone` stays cheap for everything else since only `Box` recurses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    Bool,
    Unit,
    /// A heap-allocated single-value cell — `box i64`, `box bool`, even
    /// `box box i64`. The minimal vehicle for goal.md row 1 (no GC, no
    /// manual `free()`): every other type in this language is freely
    /// copyable, so there was nothing for ownership to say anything about
    /// until this existed. See `ownership.rs` for what actually enforces
    /// single ownership; this type just makes it meaningful to ask.
    Box(Box<Ty>),
    /// A shared, read-only borrow — `&i64`, `&box i64`. Unlike `Ty::Box`,
    /// this is **not** affine: unlimited simultaneous `&T`s are always
    /// sound (many readers, no writers), the same as Rust. There is no
    /// `&mut` yet — exclusive/mutable borrows need real liveness tracking
    /// to enforce "aliasing xor mutability" and are deliberately deferred;
    /// see `ownership.rs`'s module doc.
    Ref(Box<Ty>),
    /// A handle to a spawned concurrent computation (`spawn f(x)` — see
    /// `Expr::Spawn`) that will eventually produce a `Ty`-typed result.
    /// Affine, like `Ty::Box`: a handle has exactly one owner, and
    /// `join`ing it (`Expr::Join`) consumes it — you can't join the same
    /// spawned computation twice, the same "own it or don't touch it"
    /// discipline `box` already has. goal.md rows 2–3 (no data races, no
    /// deadlocks): a spawned computation only ever receives values moved
    /// to it (this reuses the exact move-checking a normal function call
    /// argument already gets — nothing new in `ownership.rs` beyond
    /// treating `Thread` as affine), so no two concurrent computations
    /// can ever alias the same `box`-typed data. That's the actual
    /// content of the race-freedom claim, not a separate mechanism.
    Thread(Box<Ty>),
    /// A handle to an unbounded, multi-producer multi-consumer message
    /// queue — `chan i64`, `chan box i64`. **Not** affine, unlike
    /// `Ty::Thread`: a channel is meant to be held by more than one
    /// concurrent computation at once (whoever sends, whoever receives),
    /// so every read of a `chan T`-typed binding is a free, cheap-clone
    /// copy of the same underlying queue (see `Value::Channel` in
    /// `interpreter.rs`), the same "freely copyable" treatment `Ty::Ref`
    /// already gets. Race-freedom for a *sent* value still comes from
    /// ownership, though: `Expr::Send`'s payload is a normal moving use
    /// (see `ownership.rs`), so an affine value handed to `send` can't
    /// still be touched by the sender afterward — sending is how
    /// ownership actually crosses a channel. goal.md row 3 (no
    /// deadlocks): channels remove the shared-memory lock primitive that
    /// causes classic lock-order deadlocks (there is no `mutex`/`lock` in
    /// this language) — but `recv` is a genuine blocking wait, so this is
    /// *not* full Pony-style proof-by-construction yet; see PHASE0.md's
    /// row 3 entry for the honest scope of that claim.
    Channel(Box<Ty>),
    /// A handle to a real, separate OS process (`sandbox worker(x)` — see
    /// `Expr::SpawnSandbox`). No inner type parameter, unlike `Thread`/
    /// `Channel`: this first slice has no typed result channel at all
    /// (SANDBOXING.md's "layer 1" — an affine handle and deterministic
    /// teardown, nothing else yet), so there's no `T` to name. Affine,
    /// for the same reason `Thread` is: a spawned process has exactly one
    /// owner, and `stop`ping it (`Expr::StopSandbox`) is a one-time
    /// consuming operation. Unlike every other affine type here, this one
    /// backs its cleanup with a real Rust `Drop` impl on the interpreter
    /// value (`SandboxChild` in `interpreter.rs`) that actually kills the
    /// child process — not just Rust's ordinary memory reclamation — so
    /// "deterministic teardown" is a real guarantee even if a Nirdosha
    /// program never calls `stop` at all, the same way `box`'s memory is
    /// freed by Rust's own `Drop` regardless of whether `ownership.rs`'s
    /// static proof is the thing a reader trusts.
    Sandbox,
    /// A UTF-8 text value — `str`. Just enough to name things (a Docker
    /// image, a hostname) that can't be spelled any other way; not a
    /// general string-processing type yet (no concatenation, no slicing,
    /// no indexing — see `is_sandbox_safe` and the parser for the exact
    /// literal grammar). **Not** affine: like `Ty::Channel`, a string
    /// value is meant to be read freely (`Value::Str` is an `Arc<str>` in
    /// `interpreter.rs` for exactly this reason — cheap to clone, same
    /// trick `Value::Channel` already uses).
    Str,
    /// A handle to a real TCP connection (`connect(host, port)` — see
    /// `Expr::Connect`). Affine, for the same reason `Ty::Sandbox` is: a
    /// connection has exactly one owner, and `stop`ping it (reusing
    /// `Expr::StopSandbox`'s keyword — see its doc comment) is a one-time
    /// consuming close. Payloads crossing a `Tcp` connection are `str`
    /// only, the same "smallest thing that's actually needed" scope
    /// `chan`'s sandbox-crossing payload restriction already set.
    Tcp,
    /// Poison type: stands in for an expression that already produced a
    /// type error, so the checker (`typeck.rs`) doesn't cascade a dozen
    /// follow-on diagnostics from one root cause. Never produced by the
    /// parser or lexer — source code has no way to spell this — and never
    /// reaches the interpreter, since `run()` refuses to execute a program
    /// that failed type checking.
    Error,
}

impl Ty {
    pub fn from_name(name: &str) -> Option<Ty> {
        Some(match name {
            "i8" => Ty::I8,
            "i16" => Ty::I16,
            "i32" => Ty::I32,
            "i64" => Ty::I64,
            "u8" => Ty::U8,
            "u16" => Ty::U16,
            "u32" => Ty::U32,
            "u64" => Ty::U64,
            "usize" => Ty::Usize,
            "bool" => Ty::Bool,
            "unit" => Ty::Unit,
            "str" => Ty::Str,
            "tcp" => Ty::Tcp,
            _ => return None,
        })
    }

    /// Owned, not `&'static str`, because `Ty::Box` has to render its
    /// inner type recursively (`"box i64"`, `"box box bool"`, ...).
    pub fn name(&self) -> String {
        match self {
            Ty::I8 => "i8".to_string(),
            Ty::I16 => "i16".to_string(),
            Ty::I32 => "i32".to_string(),
            Ty::I64 => "i64".to_string(),
            Ty::U8 => "u8".to_string(),
            Ty::U16 => "u16".to_string(),
            Ty::U32 => "u32".to_string(),
            Ty::U64 => "u64".to_string(),
            Ty::Usize => "usize".to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Unit => "unit".to_string(),
            Ty::Box(inner) => format!("box {}", inner.name()),
            Ty::Ref(inner) => format!("&{}", inner.name()),
            Ty::Thread(inner) => format!("thread {}", inner.name()),
            Ty::Channel(inner) => format!("chan {}", inner.name()),
            Ty::Sandbox => "sandbox".to_string(),
            Ty::Str => "str".to_string(),
            Ty::Tcp => "tcp".to_string(),
            Ty::Error => "<error>".to_string(),
        }
    }

    pub fn is_integer(&self) -> bool {
        !matches!(
            self,
            Ty::Bool
                | Ty::Unit
                | Ty::Error
                | Ty::Box(_)
                | Ty::Ref(_)
                | Ty::Thread(_)
                | Ty::Channel(_)
                | Ty::Sandbox
                | Ty::Str
                | Ty::Tcp
        )
    }

    /// The property that makes ownership meaningful: an affine value has
    /// exactly one owner at a time, and using it (other than reading
    /// *through* it — see `Expr::Deref`) transfers that ownership rather
    /// than copying it. Every scalar type here is the opposite —
    /// unrestricted, Rust's `Copy` — because duplicating an `i32` has no
    /// observable cost or hazard; duplicating a `box i32` would mean two
    /// owners believing they alone are responsible for freeing the same
    /// allocation, which is exactly the class of bug row 1 exists to rule
    /// out statically.
    pub fn is_affine(&self) -> bool {
        // `Ty::Ref` is deliberately excluded: a shared borrow is always
        // freely copyable (unlimited simultaneous readers is sound), even
        // though it can point at affine content. That's what actually
        // makes borrowing useful — see `Ty::Ref`'s doc comment. `Ty::Thread`
        // *is* affine, for the same reason `Ty::Box` is: a spawned
        // computation has exactly one owner, and joining it is a one-time
        // consuming operation, the same shape as freeing a box. `Ty::Channel`
        // is deliberately excluded too, for the same reason as `Ty::Ref`:
        // a channel handle is meant to be held by more than one concurrent
        // computation at once, so it has to be freely copyable — see its
        // own doc comment for where the actual ownership-transfer happens
        // instead (a channel's *payload*, at `send`, not the handle).
        matches!(self, Ty::Box(_) | Ty::Thread(_) | Ty::Sandbox | Ty::Tcp)
    }

    /// goal.md row 4's runtime fallback (Tier 2, §4): `interpreter.rs`
    /// checks every `let`/assign/return/call boundary against this. Two
    /// *static* passes now also exist and prove some of these checks
    /// unnecessary ahead of time (Tier 1) — `refine.rs` (interval
    /// analysis) and `smt.rs` (real Z3, where available) — but this
    /// runtime check stays regardless of what either proves, since
    /// there's no backend yet to spend a Tier-1 proof's performance
    /// payoff on removing it. See both modules' doc comments.
    pub fn in_range(&self, v: i64) -> bool {
        if !self.is_integer() {
            return false;
        }
        let (lo, hi) = self.bounds();
        (lo..=hi).contains(&v)
    }

    /// A type's legal values as bare `(min, max)` bounds — the same
    /// information `in_range` uses, exposed directly for `refine.rs` and
    /// `smt.rs`, both of which need the endpoints themselves (to build an
    /// `Interval` or a pair of SMT assertions), not just a yes/no
    /// predicate. Defined once, here, so the two static passes and the
    /// runtime check can never silently disagree about what a type's
    /// range actually is — that kind of three-way drift is exactly the
    /// class of bug that would be invisible until it wasn't.
    pub fn bounds(&self) -> (i64, i64) {
        match self {
            Ty::I8 => (i8::MIN as i64, i8::MAX as i64),
            Ty::I16 => (i16::MIN as i64, i16::MAX as i64),
            Ty::I32 => (i32::MIN as i64, i32::MAX as i64),
            Ty::I64 => (i64::MIN, i64::MAX),
            Ty::U8 => (0, u8::MAX as i64),
            Ty::U16 => (0, u16::MAX as i64),
            Ty::U32 => (0, u32::MAX as i64),
            // u64's true max doesn't fit in i64 — every value this
            // language can actually hold is backed by i64 anyway (see
            // interpreter.rs's `Value::Int(i64)`), so `i64::MAX` is the
            // real ceiling regardless of the declared type.
            Ty::U64 | Ty::Usize => (0, i64::MAX),
            // Never legitimately queried (`is_integer()` is false for
            // all of these, and `in_range` above already short-circuits
            // on that) — full range is the harmless, safe default if
            // something calls this directly anyway.
            Ty::Bool
            | Ty::Unit
            | Ty::Box(_)
            | Ty::Ref(_)
            | Ty::Thread(_)
            | Ty::Channel(_)
            | Ty::Sandbox
            | Ty::Str
            | Ty::Tcp
            | Ty::Error => (i64::MIN, i64::MAX),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, ty: Ty, value: Expr, span: Span },
    Return { value: Option<Expr>, span: Span },
    While { cond: Expr, body: Block, span: Span },
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64, Span),
    Str(String, Span),
    Bool(bool, Span),
    Ident(String, Span),
    Unary(UnOp, Box<Expr>, Span),
    Binary(BinOp, Box<Expr>, Box<Expr>, Span),
    Call(String, Vec<Expr>, Span),
    If { cond: Box<Expr>, then_block: Block, else_block: Option<Box<ElseBranch>>, span: Span },
    /// `x = expr` — reassigns an existing binding. Ownership-checked as of
    /// `ownership.rs`: reassigning clears the LHS's moved status (it's
    /// holding a fresh value now); the RHS, if it names an affine binding
    /// directly, is a move of that binding, same as any other value use.
    Assign(String, Box<Expr>, Span),
    /// `box expr` — heap-allocate `expr`'s value. The only expression form
    /// that produces an affine (`Ty::Box`) value.
    Box(Box<Expr>, Span),
    /// `*expr` — read the value out of a box or through a reference. Does
    /// **not** move the box/reference itself when what comes out is
    /// freely copyable; see `ownership.rs`'s doc comment for the type-
    /// directed rule this actually needs (it's more subtle than "derefs
    /// never move" once nested boxes and references both exist).
    Deref(Box<Expr>, Span),
    /// `&expr` — borrow a binding without taking ownership of it. The
    /// operand is restricted to a plain `Expr::Ident` — parsed and
    /// enforced in `parser.rs`, the same way `Expr::Assign`'s left side
    /// is — because "borrow an arbitrary temporary expression" drags in
    /// temporary-lifetime rules this language doesn't have a story for
    /// yet.
    Ref(Box<Expr>, Span),
    /// `spawn name(args)` — runs `name` as a new concurrent computation
    /// and immediately returns a `Ty::Thread` handle to it, without
    /// waiting. Shaped exactly like `Expr::Call` (a name plus arguments,
    /// not an arbitrary expression) so it can reuse the same signature
    /// lookup and argument-move-checking a normal call already gets —
    /// see `Ty::Thread`'s doc comment for why that reuse is the actual
    /// content of the race-freedom claim, not incidental.
    Spawn(String, Vec<Expr>, Span),
    /// `join expr` — blocks until the spawned computation `expr` (which
    /// must be `Ty::Thread`-typed) finishes, consumes the handle (it's
    /// affine — a single join, like a single free), and yields its
    /// result.
    Join(Box<Expr>, Span),
    /// `chan` — creates a fresh, empty channel. Unlike `box expr` or `spawn
    /// name(args)`, there's no sub-expression to infer the payload type
    /// from, so `typeck.rs` only accepts this where an expected `chan T`
    /// type is already known (a `let` with an explicit type annotation) —
    /// see `TypeErrorKind::ChannelNeedsExplicitType`.
    Chan(Span),
    /// `send(chan_expr, value_expr)` — pushes a value onto a channel and
    /// returns immediately (never blocks; unbounded queue). The payload is
    /// a normal moving use, checked exactly like a call argument (see
    /// `ownership.rs`) — that's what makes it sound for an affine value to
    /// cross a channel: the sender loses it the moment it's sent.
    Send(Box<Expr>, Box<Expr>, Span),
    /// `recv(chan_expr)` — blocks until a value is available, then removes
    /// and returns it. A genuine blocking wait — see `Ty::Channel`'s doc
    /// comment for why that keeps row 3's "no deadlocks" claim narrower
    /// than full proof-by-construction for now.
    Recv(Box<Expr>, Span),
    /// `sandbox name(args)` — runs `name` as a **real, separate OS
    /// process** (not a thread — a fresh invocation of the `nirdosha`
    /// binary itself, re-interpreting the same source) and returns a
    /// `Ty::Sandbox` handle. Shaped like `Expr::Spawn` (a name plus
    /// arguments, reusing the same signature lookup and argument-move-
    /// checking machinery) for the same reason: no new ownership logic
    /// needed. `typeck.rs` additionally restricts `name`'s parameters and
    /// return type to plain scalars — see `TypeErrorKind::
    /// SandboxArgMustBeScalar`/`SandboxFnMustReturnUnit` — since crossing
    /// a real process boundary has no typed serialization story yet
    /// (SANDBOXING.md's layer 3, not built here).
    SpawnSandbox(String, Vec<Expr>, Span),
    /// `stop expr` — terminates the sandboxed process (killing it if
    /// still running), consumes the handle (affine, single stop like a
    /// single join), and yields its OS exit code as an `i64` (`-1` if it
    /// was terminated by a signal, including by this same `stop`). Also
    /// the consuming close for a `Ty::Tcp` connection (`Expr::Connect`)
    /// — same keyword, same affine "own it or don't touch it" shape, so
    /// it was reused rather than inventing a second word that would mean
    /// the same thing.
    StopSandbox(Box<Expr>, Span),
    /// `connect(host, port)` — opens a real TCP connection and returns a
    /// `Ty::Tcp` handle. `host`/`port` are `str`/`i64` expressions, not
    /// restricted to identifiers or literals — same "an ordinary
    /// expression, not a special grammar position" treatment `send`'s
    /// operands already get.
    Connect(Box<Expr>, Box<Expr>, Span),
}

#[derive(Debug, Clone)]
pub enum ElseBranch {
    Block(Block),
    If(Expr),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s)
            | Expr::Str(_, s)
            | Expr::Bool(_, s)
            | Expr::Ident(_, s)
            | Expr::Unary(_, _, s)
            | Expr::Binary(_, _, _, s)
            | Expr::Call(_, _, s)
            | Expr::If { span: s, .. }
            | Expr::Assign(_, _, s)
            | Expr::Box(_, s)
            | Expr::Deref(_, s)
            | Expr::Ref(_, s)
            | Expr::Spawn(_, _, s)
            | Expr::Join(_, s)
            | Expr::Chan(s)
            | Expr::Send(_, _, s)
            | Expr::Recv(_, s)
            | Expr::SpawnSandbox(_, _, s)
            | Expr::StopSandbox(_, s)
            | Expr::Connect(_, _, s) => *s,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Program {
    pub fns: Vec<FnDecl>,
}

/// Returns `Some(n)` if `e` is, syntactically, an integer literal or a
/// unary-negated one (`-5`) — including through the transparent
/// parenthesization the parser already collapses away. Anything else,
/// including a variable that merely *holds* a literal-looking value, is
/// `None`: literal flexibility is a syntactic property of the expression,
/// not a value-flow analysis.
///
/// Shared, not duplicated per-module: `typeck.rs` uses this to decide
/// which expressions get flexible-width treatment against a declared
/// target type (goal.md §3's "no implicit conversions" rule, with
/// literals as the deliberate exception), and `codegen.rs` has to agree
/// with that decision *exactly* — a literal typeck allowed to fit a
/// narrower type has to be emitted by codegen at that same narrower
/// width, not a second, independently-written recognizer that might
/// drift out of sync with what typeck actually decided.
pub fn literal_value(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n, _) => Some(*n),
        Expr::Unary(UnOp::Neg, inner, _) => literal_value(inner).map(|n| -n),
        _ => None,
    }
}
