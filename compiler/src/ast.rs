//! The AST. Each node type is a fixed, closed set — deliberately, since row 8
//! (goal.md) asks for a semantics where the meaning of a compound expression
//! is a function only of the meanings of its parts (`⟦compose(a,b)⟧ =
//! F(⟦a⟧,⟦b⟧)`), never a special case. Concretely: `interpreter.rs` has
//! exactly one `eval_expr` match arm per `Expr` variant here, and no variant
//! reaches back into a sibling's internals — composition, not entanglement.

use std::collections::HashMap;

use crate::token::Span;

/// Not `Copy` — deliberately. `Ty::Box` owns a nested `Ty`, and giving the
/// whole enum a free bitwise-copy would say every type is trivially
/// duplicable, which is exactly the property `Ty::Box` exists to *not*
/// have (see `is_affine`, and the real move-checker in `ownership.rs`).
/// `Clone` stays cheap for everything else since only `Box` recurses.
///
/// `Serialize`/`Deserialize` (not just `Serialize`) exist from this phase
/// on, ahead of the full AST-export work row 9 asks for (Phase 2): every
/// `TypeErrorKind`/`OwnershipErrorKind` field that names a type embeds a
/// `Ty` directly, so a diagnostic can't serialize at all — let alone
/// round-trip — without this. Phase 2 doesn't have to add anything here;
/// it just adds the same derive to `Expr`/`Stmt`/`Program`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// IEEE 754 double precision. Unlike the integer types, there is only
    /// one float width — no `f32` — so there's no literal-flexibility
    /// story to design (contrast `is_integer()`'s declared-width-only
    /// widening): a float literal is just `F64`, the same way a string
    /// literal is just `Str`. Not affine, not in `bounds()`'s domain
    /// (`is_integer()` is false for this on purpose — see `is_numeric()`).
    F64,
    /// A fixed-length, dense, homogeneous 1-D array — `Vector(f64, 3)`.
    /// "Sized by Default" (the unified plan's §2): the length is part of
    /// the type, so `Vector(f64, 3)` and `Vector(f64, 4)` are different
    /// types and a shape mismatch is a compile-time `TypeMismatch` for
    /// free, the same way two different integer widths already are — no
    /// separate `ShapeMismatch` machinery needed for `+`/`-`/literal
    /// construction, only for `*`'s inner-dimension check (see
    /// `typeck.rs::infer_mul`), where the two operand types are
    /// legitimately different `Ty`s by design.
    Vector(Box<Ty>, usize),
    /// A fixed-shape, dense, homogeneous 2-D array, row-major —
    /// `Matrix(f64, 3, 3)`. Same "shape is part of the type" reasoning as
    /// `Vector`, two dimensions instead of one.
    Matrix(Box<Ty>, usize, usize),
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
    /// A handle to a real, listening TCP server socket (`listen(port)` —
    /// see `Expr::Listen`). Affine, same reason as `Ty::Tcp`: exactly
    /// one owner, and `stop` (reused again, same "one-time consuming
    /// close" shape) is how it's torn down. `accept` (`Expr::Accept`)
    /// doesn't consume it — a listener accepts many connections over its
    /// lifetime, only the listener *handle itself* is single-owner.
    /// SANDBOXING.md/the unified plan's §4.3.3: "same `Channel<T>`
    /// semantics over TCP as over Unix sockets/VSOCK" — `accept` returns
    /// an ordinary `Ty::Tcp`, so every `send`/`recv`/`stop` a client
    /// connection already supports works identically on the server side,
    /// no separate server-connection type needed.
    TcpListener,
    /// A handle to a real, local file (`open(path, mode)` — see
    /// `Expr::Open`). Affine, same reason as `Ty::Tcp`: a file has
    /// exactly one owner, and `stop`ping it (reusing `Expr::StopSandbox`'s
    /// keyword — same one-time consuming close every other affine handle
    /// here shares) is how it's closed. Payloads read/written through a
    /// `File` are `str` only, same "smallest thing that's actually
    /// needed" scope `Ty::Tcp`'s payload restriction already set — this
    /// language has no `bytes` type to carry anything richer yet.
    File,
    /// A handle to an already-parsed JSON document (`json_parse(s)` — see
    /// `Expr::Call`'s `"json_parse"` arm). **Not** affine: like `Ty::Str`,
    /// meant to be read freely — `Value::Json` wraps a `serde_json::Value`
    /// in an `Arc`, the same cheap-clone-on-read reasoning `Value::Str`'s
    /// `Arc<str>` already documents. Navigation (`json_get`, and the
    /// per-target-type leaf accessors `json_get_str`/`_i64`/`_f64`/
    /// `_bool`) reads the *already-parsed* tree, not the original text —
    /// a genuinely lazy, zero-copy-from-source-text design (never
    /// materializing anything until a field is actually asked for, no
    /// index built up front) would need real byte-range string slicing
    /// this language doesn't have yet; parsing once with the well-tested
    /// `serde_json` crate (already a dependency, for AST export — see
    /// `lib.rs::Diagnostic`) and navigating the resulting tree is the
    /// honest, narrower version that ships today. Every fallible
    /// accessor returns `Ty::Named("Result", [_, Ty::Str])` — a real
    /// Nirdosha value, not a Rust-level trap.
    Json,
    /// A handle to a real, open database connection (`db_connect(path)` —
    /// see `Expr::Call`'s `"db_connect"` arm). Affine, for the same reason
    /// `Ty::Tcp`/`Ty::File` are: a connection has exactly one owner, and
    /// `stop`ping it (reusing `Expr::StopSandbox`'s keyword — see its doc
    /// comment) is a one-time consuming close, the fourth type to join
    /// that reuse after `sandbox`/`tcp`/`file`. `db_query`/`db_execute`
    /// read through the handle the same way `send`/`recv` read through a
    /// `tcp`/`file` one — the handle itself isn't consumed by an
    /// individual query, only by `stop`. Layer 1 targets SQLite
    /// specifically (`rusqlite`, "bundled" — statically linked, no system
    /// `libsqlite3` dependency): embedded, no server process, so every
    /// test that needs one can be fully self-contained, the same
    /// discipline every other affine-handle feature's tests already
    /// follow. A different backend (Postgres, named explicitly as a
    /// future layer) would be a new `db_connect`-recognized connection
    /// *string* scheme, not a new `Ty` — the Nirdosha-facing surface
    /// (`db_connect`/`db_query`/`db_execute`/`stop`, results always
    /// `Ty::Json`) stays the same regardless of which real driver crate
    /// answers it underneath.
    Db,
    /// A handle to a real, open message-queue connection (`mq_connect(host,
    /// port)` — see `Expr::Call`'s `"mq_connect"` arm). Affine, same
    /// reason and same shape as `Ty::Db`: one owner, `stop`-closed once.
    /// Unlike `Db` (SQLite, a local file — `Effect::Io`), a queue is a
    /// real network service, so `mq_*` builtins carry `Effect::Network`
    /// instead (see `effects.rs`). Layer 1 targets Redis specifically
    /// (`redis` crate; `LPUSH`/`BLPOP` back `mq_publish`/`mq_consume`) —
    /// same "the Nirdosha-facing surface stays the same regardless of the
    /// driver underneath" stance `Ty::Db`'s doc comment already takes.
    Mq,
    /// A first-class function value — `fn(T1, T2) -> R`. Just a name
    /// under the hood (`Value::Fn` in `interpreter.rs`); this language has
    /// no closures, so there's no captured environment to carry, which is
    /// what keeps this a plain, freely-copyable reference rather than
    /// something `ownership.rs` has to treat specially — see `is_affine`.
    /// Produced two ways: naming a plain top-level `fn` directly where a
    /// `Ty::Fn` is expected, or `acquire`ing a `requires`-gated one after
    /// its proof checks out (`Expr::Acquire`, `ast::Requirement`) — a
    /// `requires`-gated function's name is rejected everywhere *except*
    /// `acquire`'s callee position (`TypeErrorKind::PrivilegedFnNotAcquired`),
    /// so this is the only way to end up holding one.
    Fn(Vec<Ty>, Box<Ty>),
    /// A user-declared `struct`/`enum` name, plus its concrete type
    /// arguments (`nirdosha_row11_amendment.md` — Row 11, layer 6:
    /// generics). `Point` is `Named("Point", [])`; `Pair(i64, str)` is
    /// `Named("Pair", [I64, Str])` — two, fully concrete, structurally
    /// distinct `Ty`s, the same "structural-per-instantiation, no
    /// erasure" identity `Vector(f64,3)`/`Vector(f64,4)` already have
    /// (§3.3: "this means 'monomorphization' isn't a separate pass... it
    /// falls out of a type equality rule Nirdosha already has"). Carries
    /// just the name and args, not the declaration's fields/variants:
    /// what a given name actually means (struct vs. enum, its field/
    /// payload types, its own type-parameter names) lives in
    /// `Program::structs`/`Program::enums`, looked up through a
    /// `TypeRegistry` built once per checking pass, the same "each pass
    /// redoes its own minimal shadow walk" idiom `refine.rs`/`smt.rs`/
    /// `effects.rs` already establish — not carried inline here, since a
    /// `Ty` has no access to the declaration table that would let it
    /// resolve itself.
    Named(String, Vec<Ty>),
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
            "f64" => Ty::F64,
            "bool" => Ty::Bool,
            "unit" => Ty::Unit,
            "str" => Ty::Str,
            "tcp" => Ty::Tcp,
            "tcp_listener" => Ty::TcpListener,
            "file" => Ty::File,
            "json" => Ty::Json,
            "db" => Ty::Db,
            "mq" => Ty::Mq,
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
            Ty::F64 => "f64".to_string(),
            Ty::Vector(inner, n) => format!("Vector({}, {n})", inner.name()),
            Ty::Matrix(inner, r, c) => format!("Matrix({}, {r}, {c})", inner.name()),
            Ty::Bool => "bool".to_string(),
            Ty::Unit => "unit".to_string(),
            Ty::Box(inner) => format!("box {}", inner.name()),
            Ty::Ref(inner) => format!("&{}", inner.name()),
            Ty::Thread(inner) => format!("thread {}", inner.name()),
            Ty::Channel(inner) => format!("chan {}", inner.name()),
            Ty::Sandbox => "sandbox".to_string(),
            Ty::Str => "str".to_string(),
            Ty::Tcp => "tcp".to_string(),
            Ty::TcpListener => "tcp_listener".to_string(),
            Ty::File => "file".to_string(),
            Ty::Json => "json".to_string(),
            Ty::Db => "db".to_string(),
            Ty::Mq => "mq".to_string(),
            Ty::Fn(params, ret) => {
                format!("fn({}) -> {}", params.iter().map(Ty::name).collect::<Vec<_>>().join(", "), ret.name())
            }
            Ty::Named(name, args) if args.is_empty() => name.clone(),
            Ty::Named(name, args) => {
                format!("{name}({})", args.iter().map(Ty::name).collect::<Vec<_>>().join(", "))
            }
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
                | Ty::TcpListener
                | Ty::File
                | Ty::Json
                | Ty::Db
                | Ty::Mq
                | Ty::Fn(_, _)
                | Ty::Named(_, _)
                | Ty::F64
                | Ty::Vector(_, _)
                | Ty::Matrix(_, _, _)
        )
    }

    /// `is_integer() || self == F64` — the gate every arithmetic operator
    /// (`typeck.rs::infer_binary`) now checks instead of `is_integer()`
    /// alone. Kept as its own named predicate, not inlined at each call
    /// site, because "numeric" and "integer" mean different things from
    /// this phase on: literal-widening (`unify_operands`'s `in_range`
    /// check) is still integer-only — there's no second float width to
    /// widen *to* — so code that needs "can this hold a bare int literal"
    /// still has to ask `is_integer()` specifically, not this.
    pub fn is_numeric(&self) -> bool {
        self.is_integer() || *self == Ty::F64
    }

    /// True for the two fixed-shape aggregate types (`Vector`/`Matrix`) --
    /// every other `Ty` in this language is a single scalar value that
    /// lives in one SSA register. `codegen.rs` uses this to decide, at
    /// every call site that produces or consumes a value, whether to use
    /// `Codegen::expr` (returns a bare SSA value) or `Codegen::expr_ptr`
    /// (returns a pointer to a stack-allocated array) -- see that module's
    /// doc comment for why a `Vector`/`Matrix` genuinely can't live in one
    /// register the way everything else here does.
    pub fn is_aggregate(&self) -> bool {
        matches!(self, Ty::Vector(_, _) | Ty::Matrix(_, _, _))
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
    /// **Deliberately blind to `Ty::Named`'s real affinity** — a `struct`/
    /// `enum` is affine iff one of its fields/payload types is
    /// (`nirdosha_row11_amendment.md` §3.5), which this method has no way
    /// to know (no declaration table to consult, see `Ty::Named`'s doc
    /// comment) — so it just falls through the `matches!` below to
    /// `false`, same as every other pattern this list doesn't name. The
    /// three call sites that actually need a struct/enum's real affinity
    /// (`typeck.rs`'s `Expr::Deref` arm, `ownership.rs`'s `Expr::Deref`
    /// and `touch_ident`) go through `TypeRegistry::is_affine` instead,
    /// which delegates to this method for every other `Ty` and only
    /// special-cases `Ty::Named`.
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
        matches!(self, Ty::Box(_) | Ty::Thread(_) | Ty::Sandbox | Ty::Tcp | Ty::TcpListener | Ty::File | Ty::Db | Ty::Mq)
        // `Ty::Fn` deliberately excluded — see its own doc comment.
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
            | Ty::TcpListener
            | Ty::File
            | Ty::Json
            | Ty::Db
            | Ty::Mq
            | Ty::Fn(_, _)
            | Ty::Named(_, _)
            | Ty::F64
            | Ty::Vector(_, _)
            | Ty::Matrix(_, _, _)
            | Ty::Error => (i64::MIN, i64::MAX),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
}

/// Row 11 (`nirdosha_row11_amendment.md` §3.1) — one `name: type` field of
/// a `struct` declaration. Also doubles as an enum variant's payload
/// element carries its own `Ty` directly (`Variant::payload: Vec<Ty>`),
/// not `Vec<Field>` — a variant's payload is positional-only, same as a
/// struct's constructor call (§3.1's "positional-only... no named-field
/// construction in v1"), so there's no field *name* to carry there.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Field {
    pub name: String,
    pub ty: Ty,
}

/// `struct Point { x: f64, y: f64 }` — Row 11's product type. `name` is
/// registered both as a type name (`Ty::Named(name, _)`) and, via
/// `Expr::Call`, as a constructor (§3.1: "construction is an ordinary
/// call, not a new literal form"). `type_params` (layer 6, generics —
/// §3.3) is empty for a fully concrete struct like `Point`; for `struct
/// Pair(A, B) { first: A, second: B }` it's `["A", "B"]`, and `fields`'
/// own `Ty`s are free to reference those names directly (a bare
/// `Ty::Named("A", [])`, indistinguishable at the `Ty` level from a
/// genuine zero-arg struct/enum reference named `A` — `typeck.rs`'s
/// `validate_ty` is what tells the two apart, by checking the *current*
/// declaration's own `type_params` first). No constraint syntax
/// (`nirdosha_row11_amendment.md` §2.2/§3.3): whatever a field's own type
/// does with a parameter is checked normally at each concrete
/// instantiation, the same way `Vector(T,N)`'s `T` already works.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StructDecl {
    pub name: String,
    pub type_params: Vec<String>,
    pub fields: Vec<Field>,
    pub span: Span,
}

/// One `enum` variant — `Some(T)`, `None`, `Circle(f64)`. `payload` is
/// empty for a zero-payload variant (`None`); construction is still
/// `Expr::Call("None", [], _)`, never a bare identifier (§3.2: "a
/// zero-payload variant still takes `()`, no bare-identifier special
/// case needed").
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Variant {
    pub name: String,
    pub payload: Vec<Ty>,
    pub span: Span,
}

/// `enum Option(T) { Some(T), None }` — Row 11's sum type. Same
/// `type_params` shape and meaning as `StructDecl`'s, shared across every
/// variant (there's no per-variant parameter list — `T` in `Some(T)`
/// names the *enum's* own type parameter). Unlike a struct, an enum's own
/// name is never itself callable — only its variants are; `TypeRegistry`
/// and `typeck.rs`'s registration pass both keep this distinction (a
/// struct name doubles as its own constructor, an enum name never does).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct EnumDecl {
    pub name: String,
    pub type_params: Vec<String>,
    pub variants: Vec<Variant>,
    pub span: Span,
}

/// `Option(T)`/`Result(T, E)` — Row 11 layer 7's prelude, injected into
/// every `Program.enums` at parse time (`Parser::parse_program`) as if
/// the user had written them, exactly the way `nirdosha_row11_amendment.md`
/// §3.6's own step 7 frames it: "ordinary user `enum`s... no
/// special-casing, which is itself the proof that the mechanism is
/// general enough to earn its place in `std`." No module/import system
/// exists yet (`LANGUAGE.md` §6) for a real prelude mechanism to hook
/// into, so this is the honest, narrow stand-in: two literal `EnumDecl`
/// values prepended to whatever the user's own source declares, going
/// through the exact same registration/collision-checking
/// (`typeck.rs::typecheck`) as any user-written `enum` — a program that
/// tries to redeclare `Option`/`Result`, or reuse `Some`/`None`/`Ok`/`Err`
/// as a function/struct/other-variant name, gets the same
/// `DuplicateType`/`DuplicateConstructor` error it would for colliding
/// with any other real declaration, with no special-casing anywhere else
/// in the checker.
pub fn prelude_enums() -> Vec<EnumDecl> {
    let span = Span { line: 0, col: 0 };
    vec![
        EnumDecl {
            name: "Option".to_string(),
            type_params: vec!["T".to_string()],
            variants: vec![
                Variant { name: "Some".to_string(), payload: vec![Ty::Named("T".to_string(), vec![])], span },
                Variant { name: "None".to_string(), payload: vec![], span },
            ],
            span,
        },
        EnumDecl {
            name: "Result".to_string(),
            type_params: vec!["T".to_string(), "E".to_string()],
            variants: vec![
                Variant { name: "Ok".to_string(), payload: vec![Ty::Named("T".to_string(), vec![])], span },
                Variant { name: "Err".to_string(), payload: vec![Ty::Named("E".to_string(), vec![])], span },
            ],
            span,
        },
    ]
}

/// `struct HttpResponse { status: i64, body: str }` — `http_get`/
/// `http_post`'s result shape, injected into every `Program.structs` at
/// parse time the same way `prelude_enums` injects `Option`/`Result`
/// (see that function's doc comment — same mechanism, same "no
/// special-casing" reasoning, just a `struct` instead of an `enum`).
/// Field order (`status` then `body`) is load-bearing: `interpreter.rs`'s
/// `http_response_value` constructs the matching `Value::Struct`
/// positionally, in this exact order.
pub fn prelude_structs() -> Vec<StructDecl> {
    let span = Span { line: 0, col: 0 };
    vec![
        StructDecl {
            name: "HttpResponse".to_string(),
            type_params: vec![],
            fields: vec![
                Field { name: "status".to_string(), ty: Ty::I64 },
                Field { name: "body".to_string(), ty: Ty::Str },
            ],
            span,
        },
        // Row 12: first-class identity consumption. VerifiedIdentity is the
        // common abstraction produced by protocol adapters; it is freely
        // copyable (no affine fields). RoleView/ClaimView are runtime proofs
        // that a role check or claim extraction was performed.
        StructDecl {
            name: "VerifiedIdentity".to_string(),
            type_params: vec![],
            fields: vec![
                Field { name: "subject".to_string(), ty: Ty::Str },
                Field { name: "issuer".to_string(), ty: Ty::Str },
                Field { name: "audience".to_string(), ty: Ty::Str },
                Field { name: "expires_at".to_string(), ty: Ty::I64 },
                Field { name: "issued_at".to_string(), ty: Ty::I64 },
                Field { name: "claims_json".to_string(), ty: Ty::Str },
            ],
            span,
        },
        StructDecl {
            name: "RoleView".to_string(),
            type_params: vec![],
            fields: vec![Field { name: "role".to_string(), ty: Ty::Str }],
            span,
        },
        StructDecl {
            name: "ClaimView".to_string(),
            type_params: vec![],
            fields: vec![Field { name: "value".to_string(), ty: Ty::Str }],
            span,
        },
        // Row 12 continued: application session and refresh-token lifecycle.
        // ApplicationSession is separate from VerifiedIdentity and manages
        // the browser/session state.
        StructDecl {
            name: "ApplicationSession".to_string(),
            type_params: vec![],
            fields: vec![
                Field { name: "session_id".to_string(), ty: Ty::Str },
                Field { name: "identity_subject".to_string(), ty: Ty::Str },
                Field { name: "identity_issuer".to_string(), ty: Ty::Str },
                Field { name: "created_at".to_string(), ty: Ty::I64 },
                Field { name: "expires_at".to_string(), ty: Ty::I64 },
                Field { name: "last_accessed_at".to_string(), ty: Ty::I64 },
            ],
            span,
        },
        // RefreshTokenHandle is affine (box i64 field) — represents a
        // runtime-managed refresh token slot.
        StructDecl {
            name: "RefreshTokenHandle".to_string(),
            type_params: vec![],
            fields: vec![
                Field { name: "handle".to_string(), ty: Ty::Box(Box::new(Ty::I64)) },
                Field { name: "expires_at".to_string(), ty: Ty::I64 },
            ],
            span,
        },
        // Generic pair for returning two values together (used by refresh
        // token exchange).
        StructDecl {
            name: "Pair".to_string(),
            type_params: vec!["A".to_string(), "B".to_string()],
            fields: vec![
                Field { name: "first".to_string(), ty: Ty::Named("A".to_string(), vec![]) },
                Field { name: "second".to_string(), ty: Ty::Named("B".to_string(), vec![]) },
            ],
            span,
        },
    ]
}

/// `goal.md`'s "Effects" synthesis layer (row 4, 9) — `PROTOLANG_PORT.md`'s
/// "Locked design 1". A Koka-style **set**, not ProtoLang's own total
/// order (`pure < mutates < io < network`): Nirdosha's real effectful
/// builtins don't nest the way that lattice assumes (`network` isn't a
/// superset of `concurrent`), so membership in this set is the only
/// ordering fact, other than `pure` (the empty set) being a subset of
/// every other set — see `effects.rs`'s module doc for the full
/// classification. Deliberately just four tags, no `mutates`/`io`
/// distinction ProtoLang draws: Nirdosha's actual effectful operations
/// sort cleanly into these four, and adding a tag with no builtin that
/// produces it yet would be notation with nothing to check.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, serde::Serialize, serde::Deserialize)]
pub enum Effect {
    /// `rand_seed`/`rand_f64`/`rand_gaussian` — deterministic given a
    /// seed (`LANGUAGE.md` §9), but mutates the interpreter's own RNG
    /// stream, a different fact from "talks to the outside world" and
    /// so kept out of `Io` on purpose.
    Rng,
    /// `print`, and `file`'s `open`/`send`/`recv`/`stop`.
    Io,
    /// `spawn`/`join`, `chan`'s `send`/`recv`, `sandbox`/`stop`.
    Concurrent,
    /// `connect`/`listen`/`accept`, `tcp`'s `send`/`recv`/`stop`.
    Network,
}

impl Effect {
    pub fn name(&self) -> &'static str {
        match self {
            Effect::Rng => "rng",
            Effect::Io => "io",
            Effect::Concurrent => "concurrent",
            Effect::Network => "network",
        }
    }
}

/// One `name(args)` call — a `transact { ... }` slot (`TRANSACT.md`).
/// Restricted to a bare call, never an arbitrary expression: the same
/// "parse normally, then validate what came out" restriction `Expr::Spawn`/
/// `Expr::SpawnSandbox` already enforce (see `parser.rs::parse_transact_slot`)
/// — a slot that needs a real network/process effect wraps it in a named
/// function, the same as every worked example in `TRANSACT.md` already does.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct TransactSlot {
    pub name: String,
    pub args: Vec<Expr>,
    pub span: Span,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Block,
    pub span: Span,
    /// `effect(pure)` / `effect(io, network)` / ... — `None` when the
    /// declaration has no `effect(...)` annotation at all (the common
    /// case: fully inferred, nothing checked against, zero notational
    /// cost — `goal.md` row 6/7). `Some(set)` — including `Some({})` for
    /// a bare `effect(pure)` — means `effects::infer_effects`'s result
    /// for this function must be a *subset* of `set` (`typeck::typecheck`
    /// enforces this); an effect this function actually performs but
    /// didn't declare is `TypeErrorKind::EffectNotDeclared`. A declared
    /// effect the body never actually uses is not an error — the same
    /// generosity ProtoLang's own effect-subsumption rule gives.
    pub declared_effects: Option<std::collections::BTreeSet<Effect>>,
    /// `requires(role: "admin")` / `requires(claim: "department", "cardiology")`
    /// — `None` when the declaration has no `requires(...)` at all (the
    /// common case: an ordinary function, callable directly and freely
    /// usable as a `Ty::Fn` value). `Some(req)` gates *both* direct calls
    /// and bare value-references to this name — `typeck.rs` rejects each
    /// with `TypeErrorKind::PrivilegedFnNotAcquired` — leaving `acquire`
    /// (`Expr::Acquire`) as the only path to a callable value, gated on
    /// presenting a matching `RoleView`/`ClaimView` proof. See
    /// `Requirement`'s own doc comment.
    pub requires: Option<Requirement>,
}

/// What `acquire` (`Expr::Acquire`) demands proof of before a `requires`-
/// gated function (`FnDecl::requires`) becomes a callable `Ty::Fn` value.
/// Deliberately reuses the identity feature's own proof types
/// (`RoleView`/`ClaimView`, produced by `check_role`/`extract_claim`)
/// rather than inventing a parallel credential type — "user management
/// happens in an external system" already means an outside IdP is the
/// root of trust (`oidc_validate_token`); this is just the missing piece
/// that makes holding one of those proofs the *only* way to obtain a
/// callable value for a gated function, checked once at acquisition
/// instead of re-checked (or forgotten) at every call site.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum Requirement {
    /// `requires(role: "admin")` — `acquire` needs a `RoleView` whose
    /// `role` field equals this string (i.e. `check_role(identity,
    /// "admin")` must have already succeeded).
    Role(String),
    /// `requires(claim: "department", "cardiology")` — `acquire` needs a
    /// `ClaimView` whose `value` field equals the second string (i.e.
    /// `extract_claim(identity, "department")` must have succeeded and
    /// yielded exactly `"cardiology"`).
    Claim(String, String),
}

impl Requirement {
    /// The proof type `acquire` demands for this requirement — always one
    /// of the identity feature's own prelude structs (`prelude_structs`),
    /// never a new type: `check_role`/`extract_claim` already produce
    /// exactly these.
    pub fn proof_ty(&self) -> Ty {
        match self {
            Requirement::Role(_) => Ty::Named("RoleView".to_string(), vec![]),
            Requirement::Claim(..) => Ty::Named("ClaimView".to_string(), vec![]),
        }
    }

    /// Human-readable fragment for diagnostics — always reads naturally
    /// after a function name, e.g. "`transfer_funds` requires role
    /// \"admin\"".
    pub fn describe(&self) -> String {
        match self {
            Requirement::Role(role) => format!("requires role \"{role}\""),
            Requirement::Claim(name, value) => format!("requires claim \"{name}\" = \"{value}\""),
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Stmt {
    Let { name: String, ty: Ty, value: Expr, span: Span },
    Return { value: Option<Expr>, span: Span },
    While { cond: Expr, body: Block, span: Span },
    Expr(Expr),
    /// `audited "<justification>" { <stmts> }` — goal.md §4's Tier-3
    /// escape hatch: inside `body`, `codegen.rs` suppresses Tier-1/2
    /// guard emission (`guard_in_range`, the division-by-zero trap) it
    /// would otherwise insert. `justification` must be non-empty
    /// (`typeck.rs` checks this) — the compiler enforces syntax and
    /// non-emptiness only; judging the justification's *content*, or
    /// gating who may write one, is a CI/review-process rule, not
    /// compiler code (unified plan §4.3.4). Every escape hatch stays
    /// greppable: `grep -rn "audited"` finds all of them. `body` is a
    /// bare `Vec<Stmt>`, not a `Block` — this is purely a statement-level
    /// construct with no value of its own to produce, unlike a real
    /// block used in expression position (an `if`'s branches).
    Audited { justification: String, body: Vec<Stmt>, span: Span },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
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
    /// `.*` — Hadamard (elementwise) multiply. Distinct from `*`, which
    /// means linear-algebra product for `Vector`/`Matrix` operands (see
    /// `typeck.rs::infer_mul`) — `.{*,/}` exist specifically because
    /// plain `*`/`/` already mean something else for those types, the
    /// same reason Julia keeps the two families separate. No `.+`/`.-`:
    /// elementwise is already the *only* sensible meaning of `+`/`-` for
    /// matching-shape operands, so a dotted form would just be a
    /// redundant second spelling of the same operation.
    ElemMul,
    /// `./` — Hadamard (elementwise) divide.
    ElemDiv,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum Expr {
    Int(i64, Span),
    /// A float literal (`3.14`). Not covered by `literal_value` — unlike
    /// `Expr::Int`, there is only one float type to flex against (see
    /// `Ty::F64`'s doc comment), so there's nothing for width-flexibility
    /// to do here; this always types as `Ty::F64`, full stop.
    Float(f64, Span),
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
    /// `acquire name(proof)` — the only way to obtain a callable
    /// `Ty::Fn` value for a `requires`-gated function (`FnDecl::requires`,
    /// `Requirement`). Shaped exactly like `Expr::Spawn` (a name plus a
    /// single argument, not an arbitrary expression) for the same reuse
    /// reason: `name` is looked up as a top-level fn, its `requires`
    /// checked against `proof`'s type (`RoleView` for a role requirement,
    /// `ClaimView` for a claim one). Evaluates to `Ty::Named("Result",
    /// [Ty::Fn(params, ret), Ty::Str])` — `Ok(f)` if `proof`'s field
    /// matches the requirement, `Err(reason)` otherwise. `name` must not
    /// be a local variable — this always resolves a global function, the
    /// same restriction `Expr::Spawn`'s callee already has.
    Acquire(String, Box<Expr>, Span),
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
    /// `listen(port)` — binds a real TCP listening socket on all
    /// interfaces and returns a `Ty::TcpListener` handle. `port` is an
    /// ordinary `i64` expression, same treatment `connect`'s operands
    /// get.
    Listen(Box<Expr>, Span),
    /// `accept(listener)` — blocks until a client connects, then returns
    /// a `Ty::Tcp` handle for that connection. Does not consume the
    /// listener (unlike `stop`) — a listener accepts many connections
    /// over its lifetime.
    Accept(Box<Expr>, Span),
    /// `open(path, mode)` — opens a real local file and returns a
    /// `Ty::File` handle. `path`/`mode` are `str` expressions (`mode` is
    /// one of `"r"`/`"w"`/`"a"`, checked at runtime, not by the grammar —
    /// see `PROTOLANG_PORT.md`'s file I/O design for why a string tag
    /// rather than a real enum: this language has no closed-choice type
    /// former yet). Same "an ordinary expression, not a special grammar
    /// position" treatment `connect`'s operands already get.
    Open(Box<Expr>, Box<Expr>, Span),
    /// `v[i]` or `m[i, j]` — one bracket group, one or more comma-
    /// separated index expressions, never chained (`m[i][j]` isn't this;
    /// see `parser.rs`'s `parse_postfix`). `v[i]` needs exactly one index,
    /// `m[i, j]` exactly two (`TypeErrorKind::WrongIndexArity` otherwise);
    /// indexing anything else at all is `TypeErrorKind::NotIndexable`.
    Index(Box<Expr>, Vec<Expr>, Span),
    /// `[e1, e2, ...]` — a vector literal (`[1.0, 2.0, 3.0]`) if every
    /// element is a plain scalar, or a matrix literal, row-major
    /// (`[[1.0, 2.0], [3.0, 4.0]]`), if every element is itself a
    /// same-shaped vector — `typeck.rs::infer_array_lit` is what decides
    /// which. Always at least one element: the parser requires a real
    /// `expr` inside `[` `]` (see `parser.rs::parse_primary`), so an
    /// empty `[]` is a parse error, not a value this variant can hold —
    /// there would be no way to infer an element type for it.
    ArrayLit(Vec<Expr>, Span),
    /// `transact { network: call verify: call commit: call (compensate:
    /// call)? (log: call)? }` — `TRANSACT.md`'s durable-effect construct.
    /// **Layer 1 only** (this repo's staged rollout, `TRANSACT.md`'s own
    /// "layers, not a syntax spec" section): in-process, no durability
    /// log, no `retry`/`timeout` on `network` — those are Layer 2+.
    /// `network` and `verify` become implicit local bindings (typed from
    /// each call's own declared return type), visible to every slot after
    /// them; this is an `Expr`, not a `Stmt`, specifically so `return
    /// transact { ... }` works exactly like `return if c {..} else {..}`
    /// already does — "the same block-value convention `if` already
    /// uses," per `TRANSACT.md`. Evaluates to `true` if `commit` ran
    /// (`verify` was `true`), `false` if `compensate` ran or would have
    /// (`verify` was `false`, whether or not a `compensate` slot exists)
    /// — see `interpreter.rs`'s `Expr::Transact` arm for the exact
    /// five-step protocol, and `typeck.rs::infer_transact_slot` for why
    /// every slot's callee is restricted to a user-defined function
    /// (never a builtin).
    Transact {
        network: TransactSlot,
        verify: TransactSlot,
        commit: TransactSlot,
        compensate: Option<TransactSlot>,
        log: Option<TransactSlot>,
        span: Span,
    },
    /// `expr.field` — Row 11's one genuinely new expression form
    /// (`nirdosha_row11_amendment.md` §3.1: "no field access exists
    /// today"). `expr` must type to a `Ty::Named` struct; construction
    /// itself reuses `Expr::Call`, so there's no dedicated "struct
    /// literal" node to pair this with.
    FieldAccess(Box<Expr>, String, Span),
    /// `match scrutinee { variant(bindings) => body, ... }`
    /// (`nirdosha_row11_amendment.md` §3.4), extended (post-v1) to also
    /// accept a `str`/`i64`/`bool` scrutinee matched against literal-
    /// value arms plus a mandatory trailing `_` — see `MatchArm::pattern`.
    /// An `expr`, not a `stmt`, for the same reason `if`/`transact`
    /// already are: `return match o { ... }` has to work. Exhaustiveness
    /// is `typeck.rs`'s job, not this type's: every variant of the
    /// scrutinee's specific enum exactly once for the original enum
    /// form, or every literal arm plus exactly one trailing `_` for the
    /// literal form (a literal domain like `str`/`i64` isn't closed the
    /// way an enum's variant set is, so there's no way to prove
    /// coverage without a wildcard).
    Match { scrutinee: Box<Expr>, arms: Vec<MatchArm>, span: Span },
}

/// One `variant(bindings) => body` arm of a `match`. `bindings` names the
/// variant's payload positionally (empty for a zero-payload variant) —
/// same positional-only convention `StructDecl`'s fields and `Variant`'s
/// payload already use, no named-field binding in v1.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatchArm {
    /// The enum-variant form's payload-binding name, kept exactly as
    /// before when `pattern` is `None`. When `pattern` is `Some(_)`
    /// (a literal or `_` arm), this instead holds a human-readable
    /// display form of that pattern (`"admin"`/`5`/`true`/`_`) purely
    /// for error messages and duplicate-arm detection — deliberately
    /// *not* a real variant name, so `TypeRegistry::find_variant` (used
    /// by `ownership.rs`/`effects.rs`) safely returns `None` for it and
    /// those passes' existing "no such variant" fallback already does
    /// the right thing with zero changes to either file.
    pub variant: String,
    pub bindings: Vec<String>,
    /// `None` — this arm names an enum variant (today's only v1 shape).
    /// `Some(_)` — this arm is a literal-value or `_` pattern; `bindings`
    /// is always empty in that case (the parser never populates it for
    /// these, there's no payload to bind).
    pub pattern: Option<LiteralPattern>,
    pub body: Box<Expr>,
    pub span: Span,
}

/// A literal-value `match` arm's pattern — the non-enum sibling
/// `nirdosha_row11_amendment.md` §3.4 named as a disclosed v1 gap
/// ("no wildcard/binding-only catch-all patterns"). Only `str`/`i64`/
/// `bool` scrutinees get this form (no `f64`: floating-point pattern
/// equality is a real footgun `match`'s int/str/bool literal form
/// doesn't need to inherit). `Wildcard` (`_`) is required exactly once,
/// last, whenever any arm uses this form — see `typeck.rs::check_literal_match`.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum LiteralPattern {
    Str(String),
    Int(i64),
    Bool(bool),
    Wildcard,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub enum ElseBranch {
    Block(Block),
    If(Expr),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s)
            | Expr::Float(_, s)
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
            | Expr::Acquire(_, _, s)
            | Expr::Join(_, s)
            | Expr::Chan(s)
            | Expr::Send(_, _, s)
            | Expr::Recv(_, s)
            | Expr::SpawnSandbox(_, _, s)
            | Expr::StopSandbox(_, s)
            | Expr::Connect(_, _, s)
            | Expr::Listen(_, s)
            | Expr::Accept(_, s)
            | Expr::Open(_, _, s)
            | Expr::Index(_, _, s)
            | Expr::ArrayLit(_, s)
            | Expr::Transact { span: s, .. }
            | Expr::FieldAccess(_, _, s)
            | Expr::Match { span: s, .. } => *s,
        }
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct Program {
    pub fns: Vec<FnDecl>,
    pub structs: Vec<StructDecl>,
    pub enums: Vec<EnumDecl>,
    /// Declared `screen <Struct> { ... }` blocks (Row 12 UI DSL). Layered
    /// on top of `ui_gen.rs`'s pure convention-based inference, never a
    /// replacement for it — a struct with no matching `ScreenDecl` here
    /// keeps today's full inferred UI unchanged; see `ui_gen::build_screens`.
    pub screens: Vec<ScreenDecl>,
    /// At most one `dashboard { ... }` block — supplements (doesn't
    /// replace) `stat_`/`chart_` naming-convention inference.
    pub dashboard: Option<DashboardDecl>,
}

/// One `key: value` slot inside a `screen`/`dashboard`/`field`/`action`
/// body — `title: "Catalog"`, `page_size: 25`, `view: role("admin")`.
/// The value is an ordinary `Expr` (the parser reuses `parse_expr()`
/// unchanged); typeck narrows each key to its expected shape rather than
/// the parser inventing a second value grammar. See `ast.rs` module docs
/// / the UI DSL design doc for the full key vocabulary.
pub type KvEntry = (String, Expr);

/// `field <name> { ... }` inside a `screen` block — overrides/supplements
/// one field's label, formatting, searchability, sortability, and
/// view/edit visibility for the struct's inferred UI. Every entry is a
/// plain `KvEntry`; typeck (not the parser) enforces which keys are
/// recognized and what shape their values must have.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct FieldOverride {
    pub field_name: String,
    pub entries: Vec<KvEntry>,
    pub span: Span,
}

/// `action "<label>" -> <target_fn> { ... }` inside a `screen` block — a
/// custom row/toolbar action beyond the inferred create/update/delete
/// set, e.g. `action "Restock" -> restock_product { style: "outlined" }`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ActionDecl {
    pub label: String,
    pub target_fn: String,
    pub entries: Vec<KvEntry>,
    pub span: Span,
}

/// `screen <StructName> { ... }` — a top-level declaration overriding/
/// supplementing the inferred UI for one struct. `entries` holds the
/// screen-level `key: value` slots (`title: "..."`, `list: list_product`,
/// `paginate { ... }` folds its own entries in under the `"paginate"`
/// key as a nested-entries convenience — see `parse_screen_decl`).
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct ScreenDecl {
    pub struct_name: String,
    pub entries: Vec<KvEntry>,
    pub fields: Vec<FieldOverride>,
    pub actions: Vec<ActionDecl>,
    pub span: Span,
}

/// `tile "<label>" -> <fn>` or `chart "<label>" -> <fn>` inside a
/// `dashboard` block — same shape for both, distinguished by which
/// `Vec` in `DashboardDecl` it lands in.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MetricRef {
    pub label: String,
    pub target_fn: String,
    pub span: Span,
}

/// `dashboard { tile "..." -> stat_fn  chart "..." -> chart_fn }` — a
/// single top-level block supplementing `stat_`/`chart_` naming-
/// convention inference in `ui_gen.rs`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct DashboardDecl {
    pub tiles: Vec<MetricRef>,
    pub charts: Vec<MetricRef>,
    pub span: Span,
}

/// Resolves `Ty::Named` references against a program's own `struct`/
/// `enum` declarations — Row 11's registry, built once per checking pass
/// (`typeck.rs`, `ownership.rs`, `effects.rs` each build their own over
/// the same `&Program`, the same "each pass redoes its own minimal shadow
/// walk" idiom `refine.rs`/`smt.rs` already establish). Doesn't validate
/// anything itself (a name colliding with a function, or a struct/enum
/// declared twice, is `typeck.rs`'s job — see its `TypeErrorKind::
/// DuplicateType`/`DuplicateConstructor`) — this just answers "what
/// fields does this struct have" / "what payload does this variant
/// carry."
pub struct TypeRegistry<'a> {
    structs: HashMap<&'a str, &'a StructDecl>,
    enums: HashMap<&'a str, &'a EnumDecl>,
    /// `(owning enum's name, the variant itself)`, keyed by variant name
    /// — variant names live in the same flat callable namespace as
    /// functions/struct constructors (`nirdosha_row11_amendment.md` §3.2:
    /// "a variant is just a constructor, registered by name exactly like
    /// a struct's"), so a bare variant name alone is enough to look this
    /// up without also knowing which enum it belongs to ahead of time.
    variant_owner: HashMap<&'a str, (&'a str, &'a Variant)>,
}

impl<'a> TypeRegistry<'a> {
    /// No struct/enum declarations at all — every `HashMap` here is
    /// empty, so this needs no real `&Program` to borrow from and works
    /// at any lifetime (`typeck::validate_fragment` uses this: it checks
    /// one isolated `Expr` fragment with no surrounding `Program`, so
    /// there's no declaration table to build from a real one).
    pub fn empty() -> Self {
        TypeRegistry { structs: HashMap::new(), enums: HashMap::new(), variant_owner: HashMap::new() }
    }

    pub fn build(program: &'a Program) -> Self {
        let mut structs = HashMap::new();
        let mut enums = HashMap::new();
        let mut variant_owner = HashMap::new();
        for s in &program.structs {
            structs.insert(s.name.as_str(), s);
        }
        for e in &program.enums {
            enums.insert(e.name.as_str(), e);
            for v in &e.variants {
                variant_owner.insert(v.name.as_str(), (e.name.as_str(), v));
            }
        }
        TypeRegistry { structs, enums, variant_owner }
    }

    pub fn struct_decl(&self, name: &str) -> Option<&'a StructDecl> {
        self.structs.get(name).copied()
    }

    pub fn enum_decl(&self, name: &str) -> Option<&'a EnumDecl> {
        self.enums.get(name).copied()
    }

    pub fn struct_fields(&self, name: &str) -> Option<&'a [Field]> {
        self.structs.get(name).map(|s| s.fields.as_slice())
    }

    pub fn enum_variants(&self, name: &str) -> Option<&'a [Variant]> {
        self.enums.get(name).map(|e| e.variants.as_slice())
    }

    pub fn struct_type_params(&self, name: &str) -> Option<&'a [String]> {
        self.structs.get(name).map(|s| s.type_params.as_slice())
    }

    pub fn enum_type_params(&self, name: &str) -> Option<&'a [String]> {
        self.enums.get(name).map(|e| e.type_params.as_slice())
    }

    pub fn is_struct(&self, name: &str) -> bool {
        self.structs.contains_key(name)
    }

    pub fn is_enum(&self, name: &str) -> bool {
        self.enums.contains_key(name)
    }

    pub fn find_variant(&self, variant_name: &str) -> Option<(&'a str, &'a Variant)> {
        self.variant_owner.get(variant_name).copied()
    }

    /// Registry-aware affinity — `Ty::is_affine`'s doc comment explains
    /// why the plain method can't do this itself. For a generic
    /// instantiation (`Ty::Named(name, args)` with `args` non-empty), the
    /// declaration's own fields/payloads are substituted against `args`
    /// first (`substitute_ty`) — `Wrapper(box i64)` is affine even though
    /// `struct Wrapper(T) { v: T }`'s own declared field type, `T`, is
    /// not (it's not a real type at all outside a substitution context).
    /// Recurses "one level through" a field/payload at a time otherwise,
    /// exactly as `nirdosha_row11_amendment.md` §3.5 describes.
    ///
    /// **Known, deliberate non-issue, not a guarded-against case:** a
    /// struct/enum that's directly self-referential with no `box`/`chan`/
    /// etc. indirection (`struct A { b: B } struct B { a: A }`) would
    /// recurse here forever — not defended against, because no program
    /// could ever construct a value of such a type in the first place
    /// (the constructor call itself could never terminate), so no
    /// well-typed program can ever reach this path with one.
    pub fn is_affine(&self, ty: &Ty) -> bool {
        match ty {
            Ty::Named(name, args) => {
                if let Some(decl) = self.structs.get(name.as_str()) {
                    let subst = zip_type_params(&decl.type_params, args);
                    decl.fields.iter().any(|f| self.is_affine(&substitute_ty(&f.ty, &subst)))
                } else if let Some(decl) = self.enums.get(name.as_str()) {
                    let subst = zip_type_params(&decl.type_params, args);
                    decl.variants.iter().any(|v| v.payload.iter().any(|t| self.is_affine(&substitute_ty(t, &subst))))
                } else {
                    // Unknown name -- `typeck.rs` reports this separately
                    // (`TypeErrorKind::UnknownType`); nothing sound to say
                    // here, so the safe default (not affine) stands.
                    false
                }
            }
            _ => ty.is_affine(),
        }
    }
}

/// `type_params.iter().zip(args)`, as a lookup map — the shared shape
/// every generic-substitution call site needs (`TypeRegistry::is_affine`,
/// `typeck.rs`, `ownership.rs`, `interpreter.rs`). Silently ignores a
/// length mismatch (zips to the shorter of the two) rather than panicking
/// — `typeck.rs`'s own arity check (`TypeErrorKind::WrongTypeArity`) is
/// what rejects that case; every other caller here just needs a sound
/// (if partial) substitution to keep walking without crashing.
/// `Ty::Named("Result", [ok, Str])` — every JSON builtin's return type
/// shape (see e.g. `json_get_str`'s doc comment in `BUILTIN_NAMES`),
/// spelled once so every caller agrees on it exactly: `typeck.rs`'s own
/// `infer_builtin_call` arms, and `ownership.rs`'s `builtin_return_ty`
/// (which needs the same shape *without* re-deriving it through real
/// type inference — see that function's own doc comment for why).
/// Reuses the real prelude `Result` (layer 7), with `str` as the fixed
/// error type — the same flat, stringly-typed error convention
/// `ErrorKind::ChannelIoError` already established for `chan`/`tcp`/
/// `file` (`PROTOLANG_PORT.md`'s std_io §14 entry), not a bespoke
/// `JsonError` type invented in parallel.
pub fn result_of(ok: Ty) -> Ty {
    Ty::Named("Result".to_string(), vec![ok, Ty::Str])
}

pub fn zip_type_params<'a, 'b>(type_params: &'a [String], args: &'b [Ty]) -> HashMap<&'a str, &'b Ty> {
    type_params.iter().map(String::as_str).zip(args.iter()).collect()
}

/// Replaces every bare `Ty::Named(p, [])` in `ty` where `p` is a key of
/// `subst` with its bound concrete type, recursing through every
/// type-former this language has (`Box`/`Ref`/`Thread`/`Channel`/
/// `Vector`/`Matrix`'s inner type, and a nested `Ty::Named`'s own type
/// arguments) — Row 11 layer 6's actual substitution mechanism, used
/// identically by every pass that needs to turn a generic declaration's
/// field/payload types into a specific instantiation's concrete ones. A
/// `Ty::Named(p, args)` where `args` is non-empty is never itself treated
/// as a substitutable parameter reference (a bare type-parameter name is
/// never itself further parameterized — nothing in this language's
/// grammar can write `A(B)` as a *use* of type parameter `A`), only
/// recursed into for its own args.
pub fn substitute_ty(ty: &Ty, subst: &HashMap<&str, &Ty>) -> Ty {
    match ty {
        Ty::Named(name, args) if args.is_empty() => {
            subst.get(name.as_str()).map(|t| (*t).clone()).unwrap_or_else(|| ty.clone())
        }
        Ty::Named(name, args) => {
            Ty::Named(name.clone(), args.iter().map(|a| substitute_ty(a, subst)).collect())
        }
        Ty::Box(inner) => Ty::Box(Box::new(substitute_ty(inner, subst))),
        Ty::Ref(inner) => Ty::Ref(Box::new(substitute_ty(inner, subst))),
        Ty::Thread(inner) => Ty::Thread(Box::new(substitute_ty(inner, subst))),
        Ty::Channel(inner) => Ty::Channel(Box::new(substitute_ty(inner, subst))),
        Ty::Vector(inner, n) => Ty::Vector(Box::new(substitute_ty(inner, subst)), *n),
        Ty::Matrix(inner, r, c) => Ty::Matrix(Box::new(substitute_ty(inner, subst)), *r, *c),
        // Every other `Ty` is a plain scalar/handle with nothing inside it
        // to substitute into.
        _ => ty.clone(),
    }
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

/// The builtin-function name registry ("Builtins Are Native Rust" — §2 of
/// the unified development plan): the single source of truth for "is
/// this name a builtin." Before this existed, `if name == "print"` was
/// independently special-cased six times across three files (`typeck.rs`,
/// `interpreter.rs`, `codegen.rs` twice) — six separately-typed string
/// literals that could silently drift. Every one of those sites now asks
/// `is_builtin` instead of comparing to a literal directly.
///
/// Deliberately just a name list, not a shared type-check/eval table the
/// way an earlier revision of this tried (`print` alone made a uniform
/// `fn(&[Ty]) -> Ty` signature look sufficient). Phase 2's dense linear
/// algebra builtins broke that assumption two different ways: `zeros`/
/// `ones`/`identity`'s result *shape* comes from an argument's literal
/// *value* (`zeros(3)` needs to see the `3`, not just "an `i64`"), which
/// a type-only signature can't see; and `Ty`-producing logic
/// (`typeck.rs`) and `Value`-producing logic (`interpreter.rs`) live in
/// modules with a one-way dependency (`interpreter` on `typeck` would
/// create a cycle back through here). Each of those two files now owns
/// its own per-builtin dispatch (`typeck.rs::infer_builtin_call`,
/// `interpreter.rs`'s `Expr::Call` arm) — a smaller, honest duplication
/// (one name list, two independent bodies of *logic* that were always
/// going to differ anyway) rather than a forced abstraction leaking
/// implementation details across the module boundary it exists to keep
/// clean.
pub const BUILTIN_NAMES: &[&str] = &[
    "print",
    "transpose",
    "dot",
    "cross",
    "zeros",
    "ones",
    "identity",
    "sum",
    "len",
    "norm",
    "norm1",
    "norm_inf",
    "frobenius_norm",
    "trace",
    "det",
    "inv",
    "solve",
    "rank",
    "is_symmetric",
    "is_diag",
    "is_square",
    "rand_seed",
    "rand_f64",
    "rand_gaussian",
    "distance",
    "bearing",
    "lla_to_ecef",
    "ecef_to_lla",
    "ecef_to_enu",
    "enu_to_ecef",
    "kf_predict_state",
    "kf_predict_cov",
    "kf_update_state",
    "kf_update_cov",
    // JSON (lazy-navigation design over an already-parsed `serde_json::
    // Value` tree -- see `Ty::Json`'s doc comment): concrete, per-target-
    // type accessors rather than one generic `json_get<T>`, the same
    // "narrow, concrete, cheap to extend later" discipline every other
    // builtin here already follows -- there is no generic-return-type
    // resolution mechanism for plain builtin calls (unlike struct/variant
    // *construction*, which Row 11 layer 6 does have one for). Every
    // fallible one returns `Result(_, str)`, the same flat, stringly-
    // typed error convention `ErrorKind::ChannelIoError` already
    // established for `chan`/`tcp`/`file` (`PROTOLANG_PORT.md`'s std_io
    // §14 entry) -- a real Nirdosha *value* now, not a Rust-level trap,
    // since `Result` exists.
    "json_parse",
    "json_get",
    "json_get_str",
    "json_get_i64",
    "json_get_f64",
    "json_get_bool",
    "json_array_len",
    "json_array_get",
    // HTTP (plain, client-only, layer 1 -- HTTPS/TLS is a real, deliberate
    // gap: PROTOLANG_PORT.md's std_io §12 already says TLS should be a
    // vetted library binding, not hand-rolled, and no such binding has
    // been added yet). Built on the exact same `std::net::TcpStream`
    // `connect` already uses. Every request sends `Connection: close`
    // and reads to EOF -- no `Content-Length`/chunked-transfer-encoding
    // parsing needed for a first cut, since the server closing the
    // socket *is* the end-of-body signal. Returns `Result(HttpResponse,
    // str)` (`ast::prelude_structs`) -- a network failure, a malformed
    // status line, or a non-UTF-8 body are all `Err`, not a trap.
    "http_get",
    "http_post",
    // HTTPS -- same request/response handling as http_get/http_post,
    // over a `native_tls::TlsStream` wrapping the same real TCP socket.
    // `TlsConnector::new()`'s defaults do the actual security-critical
    // work (certificate-chain and hostname verification against the
    // platform's trust store) -- a vetted library binding, not
    // hand-rolled, per PROTOLANG_PORT.md's std_io §12 stance.
    "https_get",
    "https_post",
    // Row 12: identity as a relying party. The runtime validates tokens
    // issued by external IdPs and returns an unforgeable-in-the-type-system
    // VerifiedIdentity. `oidc_validate_token` performs mock OIDC/JWT
    // signature validation against a supplied JWKS JSON string; `check_role`
    // and `extract_claim` derive views from the verified claims.
    "oidc_validate_token",
    "check_role",
    "extract_claim",
    // `check_role`/`extract_claim`'s dotted-path siblings -- additive,
    // for IdPs that nest the role array/claim under a path instead of a
    // flat top-level field (e.g. Keycloak's `realm_access.roles`).
    // `check_role`/`extract_claim` are unchanged and still the right
    // tool for a flat claim, including one whose own name contains a
    // literal dot (Auth0-style namespaced claims) -- see
    // `interpreter.rs::json_field_path`'s doc comment.
    "check_role_path",
    "extract_claim_path",
    "identity_expired",
    // Row 12 continued: session management, refresh tokens, revocation,
    // and an API-key adapter.
    "create_application_session",
    "session_cookie",
    "new_refresh_token",
    "exchange_refresh_token",
    "check_revocation",
    "validate_api_key",
    // A plain SHA-256 hex digest, infallible (any `str` hashes to
    // something) -- exposes the same hasher `validate_api_key` already
    // uses internally, as a general-purpose builtin. `sha256_hex(s)`
    // hashes one string; `sha256_hex(a, b)` hashes both in sequence
    // (no concatenation operator exists to combine them first) -- the
    // one thing a real hash-chained audit log
    // (`hash = sha256_hex(prev_hash, payload)`) needs that wasn't
    // otherwise reachable from Nirdosha source.
    "sha256_hex",
    // A red-team finding, fixed: `.nir` code comparing two secret-derived
    // strings (a decision-link token against its stored value, say) had
    // no way to do it other than `==`, which short-circuits on the first
    // differing byte -- a timing side channel on whatever secret
    // comparison the program was trying to do. Backed by the same
    // constant-time comparison `interpreter.rs`'s own JWT signature
    // check now uses internally (also a fixed red-team finding).
    "constant_time_str_eq",
    // DB, layer 1: SQLite only (`rusqlite`, "bundled" -- statically
    // linked, no system `libsqlite3`). `Ty::Db`'s doc comment has the
    // full design; the short version: one connection handle, `stop`-able
    // like `tcp`/`file`/`sandbox`, and results always `Ty::Json` (no
    // growable-collection type needed for a query's row count -- the
    // same reasoning that already shaped JSON's own design applies here
    // for free). `db_connect(path)` -- `":memory:"` is a real SQLite path
    // meaning "in-memory," not special-cased here. `db_query` is for
    // row-returning statements (`SELECT`); `db_execute` is for
    // everything else (`INSERT`/`UPDATE`/`DELETE`/DDL), returning the
    // affected-row count. Every fallible one returns `Result(_, str)`,
    // same convention as JSON/HTTP.
    "db_connect",
    "db_query",
    "db_execute",
    // `Ty::Mq`'s doc comment has the full design: one connection handle,
    // `stop`-able like `db`, backed by Redis (`LPUSH`/`BLPOP`).
    // `mq_connect(host, port)`, `mq_publish(conn, queue, message)`,
    // `mq_consume(conn, queue, timeout_secs)` -- every fallible one
    // returns `Result(_, str)`, same convention as `db`/JSON/HTTP.
    "mq_connect",
    "mq_publish",
    "mq_consume",
    // Row 12's deliberate mock-only exception to "the runtime never
    // mints tokens" -- see its own doc comment at the builtin's typeck
    // signature (`typeck.rs`) for why the `mock_` prefix is load-bearing.
    "mock_issue_token",
];

pub fn is_builtin(name: &str) -> bool {
    BUILTIN_NAMES.contains(&name)
}
