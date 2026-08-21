//! LLVM codegen — goal.md row 5 ("native, hardware-speed codegen"),
//! started here for the first time. Emits **textual LLVM IR** and shells
//! out to the system `clang` to assemble/link a real native binary,
//! rather than binding to the LLVM C API from Rust (`inkwell`/`llvm-sys`)
//! — this environment has LLVM 22, recent enough that a Rust binding
//! crate's supported-version list might not cover it yet, and textual IR
//! sidesteps that entirely: it's a stable, documented format, and `clang`
//! on this system is *the same* LLVM 22, so there's no version-skew risk
//! between what's emitted and what assembles it. Several real compilers
//! (plenty of small production and hobby ones) use exactly this strategy.
//!
//! **Scoped to what's honestly supported, not silently narrowed.**
//! `check_supported` rejects, with a specific reason, anything outside:
//! signed integers (`i8`/`i16`/`i32`/`i64`), `bool`, `unit` — no
//! `u8`..`usize` (deferred: LLVM's `icmp`/`div` need a signed-vs-unsigned
//! instruction choice this pass doesn't make yet), no `box`/`&`/`*`
//! (compiling real heap allocation and move semantics to native code is
//! a separate, larger undertaking — ownership.rs's proof exists, but
//! nothing yet *executes* on it, matching that module's own "not yet
//! load-bearing" framing), and `print` only for integer-typed arguments
//! (every existing example only ever prints integers — printing `bool`
//! is a real, narrow, documented gap, not an oversight).
//!
//! **Tier 1 vs Tier 2 finally means something.** `refine.rs` and
//! `smt.rs` both proved things and both said, explicitly, "not wired to
//! elide the runtime check — there's no backend to spend the payoff on
//! yet." There is now. A `let`/assignment whose span is in the passed-in
//! `SmtReport::proven_in_range` gets no runtime bounds check emitted at
//! all (Tier 1, silent, exactly as goal.md §4 describes); one that
//! isn't gets an explicit compare-and-trap sequence in the compiled
//! binary (Tier 2, a real cost, visible in the generated IR). Same
//! distinction for division and `proven_nonzero_divisor`. This is the
//! first place in the whole codebase where a static proof actually
//! changes what runs, not just what's reported.
//!
//! **Codegen strategy: alloca everywhere, correctness over cleverness.**
//! Every parameter and every `let` gets its own stack slot
//! (`alloca`/`store`/`load`), the same strategy `clang -O0` itself uses
//! and every "toy compiler to LLVM" tutorial teaches — it's simple to
//! get right, and LLVM's own optimizer (not run here; nothing asks for
//! `-O2`) would promote these to registers anyway if it were. Allocas
//! are emitted at the point of each `let`, not hoisted to the entry
//! block: Nirdosha's scoping rules mean a name is only ever referenced
//! somewhere its `let` already dominates (you can't read a variable
//! before its declaration or from a sibling branch), so this is valid
//! LLVM IR without the hoisting pass a stricter backend might do. `&&`/
//! `||` are lowered to real conditional branches, not eager bitwise
//! `and`/`or` — short-circuit evaluation is a tested behavior
//! (`tests/basic.rs`'s short-circuit tests), and this backend has to
//! preserve it, not just the interpreter.

use std::collections::HashMap;
use std::fmt::Write as _;

use crate::ast::*;
use crate::ownership::{self, FreeMap};
use crate::smt::SmtReport;
use crate::token::Span;

#[derive(Debug, Clone, PartialEq)]
pub struct CodegenError {
    pub message: String,
}

impl std::fmt::Display for CodegenError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

fn unsupported<T>(msg: impl Into<String>) -> Result<T, CodegenError> {
    Err(CodegenError { message: msg.into() })
}

/// Escapes raw bytes into LLVM's `c"..."` string-constant syntax:
/// printable ASCII passes through unchanged except `"`/`\` (which would
/// otherwise terminate/escape the constant early), everything else
/// becomes a `\XX` two-hex-digit byte escape — the same scheme LLVM's own
/// IR parser expects, used here since a `str` literal's already-escape-
/// resolved bytes (the lexer/parser already turned `\n`/`\t`/etc. into
/// real bytes — see `Expr::Str`'s doc) can contain anything, not just the
/// hand-picked printable text `@.int_fmt`/`@.float_fmt` use.
fn llvm_escape_bytes(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len());
    for &b in bytes {
        match b {
            b'"' => out.push_str("\\22"),
            b'\\' => out.push_str("\\5C"),
            0x20..=0x7E => out.push(b as char),
            _ => out.push_str(&format!("\\{b:02X}")),
        }
    }
    out
}

/// Builtins with codegen support as of Phase 4 of the Vector/Matrix
/// codegen plan — every one of these has loop trip counts that depend
/// only on compile-time-known shape (never on runtime data values), so
/// each fully unrolls into straight-line IR (module doc / design decision
/// 3). `det`/`inv`/`solve`/`rank`/`kf_update_state`/`kf_update_cov` have
/// genuine data-dependent control flow (partial-pivot search) and are
/// deliberately excluded — they land via a linked runtime call in a later
/// phase, not unrolled IR. `rand_seed`/`rand_f64`/`rand_gaussian` are also
/// excluded (unrelated to Vector/Matrix; no RNG state exists in generated
/// code yet).
const PHASE4_BUILTINS: &[&str] = &[
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
    "is_symmetric",
    "is_diag",
    "is_square",
    "distance",
    "bearing",
    "lla_to_ecef",
    "ecef_to_lla",
    "ecef_to_enu",
    "enu_to_ecef",
    "kf_predict_state",
    "kf_predict_cov",
];

/// Phase 5's builtins — genuine data-dependent control flow (partial-pivot
/// row selection), so these go through a linked native `call` into
/// `runtime_kernels.rs`'s staticlib (see `call_builtin_scalar`/
/// `call_builtin_agg`'s dispatch and `build()`'s embedded-lib linking)
/// rather than unrolled IR the way every `PHASE4_BUILTINS` name is.
const PHASE5_BUILTINS: &[&str] = &["det", "inv", "solve", "rank", "kf_update_state", "kf_update_cov"];

/// WGS84 ellipsoid constants — mirrors `interpreter.rs`'s own
/// `WGS84_A`/`WGS84_F`/`wgs84_e2()` exactly (same values, same derived
/// `e2` formula), needed independently here since codegen computes these
/// geometry builtins as inline IR rather than calling back into
/// `interpreter.rs`'s Rust functions.
const WGS84_A: f64 = 6_378_137.0;
const WGS84_F: f64 = 1.0 / 298.257_223_563;
const WGS84_E2: f64 = WGS84_F * (2.0 - WGS84_F);

/// Every signed integer/bool/unit type maps to a fixed LLVM type name;
/// `Vector`/`Matrix` map to a flat array type (`[N x double]`, or
/// `[R*C x double]` row-major for a `Matrix` — matching
/// `interpreter.rs`'s `Value::Matrix` storage exactly); everything else
/// is rejected — see module doc for exactly what and why. Returns an
/// owned `String`, not `&'static str`, because an aggregate's type
/// string depends on its compile-time-known but not statically-fixed
/// length.
fn llvm_ty(ty: &Ty) -> Result<String, CodegenError> {
    match ty {
        Ty::I8 => Ok("i8".to_string()),
        Ty::I16 => Ok("i16".to_string()),
        Ty::I32 => Ok("i32".to_string()),
        Ty::I64 => Ok("i64".to_string()),
        Ty::Bool => Ok("i1".to_string()),
        Ty::Unit => Ok("void".to_string()),
        Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => unsupported(format!(
            "codegen doesn't support unsigned types yet (`{}`) — LLVM needs a signed-vs-\
             unsigned instruction choice (icmp/div) this pass doesn't make",
            ty.name()
        )),
        // A single heap (`Box`) or borrowed (`Ref`) pointer — one word,
        // passed by value exactly like `Ty::Str`'s `{ptr, i64}` above,
        // just narrower. `Ref` needs no allocation of its own (it's
        // always just an existing binding's own storage address —
        // `Codegen::expr`'s `Expr::Ref` arm), and `Box` needs one heap
        // allocation at construction (`nir_alloc`) but no `free` logic
        // yet in this phase (see `Expr::Box`'s doc comment — a later
        // phase threads `ownership.rs`'s already-proven move data into
        // codegen to free at last-use; this phase deliberately leaks,
        // stated here rather than silently).
        Ty::Box(_) | Ty::Ref(_) => Ok("ptr".to_string()),
        Ty::Thread(_) => unsupported(format!(
            "codegen doesn't support `{}` yet — spawn/join are interpreter-only for now \
             (real OS threads, not yet compiled to native code)",
            ty.name()
        )),
        Ty::Channel(_) => unsupported(format!(
            "codegen doesn't support `{}` yet — chan/send/recv are interpreter-only for now",
            ty.name()
        )),
        Ty::Sandbox => unsupported(
            "codegen doesn't support `sandbox` yet — sandbox/stop are interpreter-only for now",
        ),
        Ty::File => unsupported(
            "codegen doesn't support `file` yet — open/send/recv/stop on file are interpreter-only for now",
        ),
        // A fixed-size, two-word value — pointer to the byte data plus an
        // explicit `i64` length, never NUL-terminated-only (a `str`'s
        // bytes are whatever the source literal's escapes resolved to,
        // and a future `tcp` payload needs to carry arbitrary bytes that
        // could contain an embedded NUL). Passed by value in registers,
        // like `f64`/`bool` — NOT `is_aggregate()` (that's the
        // sret/pointer convention Vector/Matrix need because their size
        // varies per-type; a `str` value is always these same two words,
        // so it needs no allocation of its own to pass around).
        Ty::Str => Ok("{ptr, i64}".to_string()),
        // Phase 4: `f64` maps directly to LLVM's `double`. No width
        // story the way integers have one (`is_integer()` is false for
        // `F64`, so `guard_in_range`/`narrow_from_i64`/`widen_to_i64`
        // all already treat it as a no-guard, no-narrow passthrough --
        // see each of their doc comments), and no range check either:
        // IEEE 754 saturates instead of trapping, the same semantics
        // the interpreter already committed to (`Value::Float`'s doc
        // comment).
        Ty::F64 => Ok("double".to_string()),
        // `Vector`/`Matrix` land as of this phase (Phase 0+1 of the
        // Vector/Matrix codegen plan): a flat array type, never a single
        // SSA register the way every other `Ty` here is -- see the
        // module doc and `Codegen::expr_ptr`'s doc comment for the
        // pointer-based codegen strategy this actually requires. Element
        // count is `n` for `Vector`, `r*c` (row-major) for `Matrix` --
        // `agg_elem_and_len` is the single place that flattening rule
        // lives, reused by every aggregate codegen path so it can never
        // silently disagree with `interpreter.rs`'s own flattening.
        Ty::Vector(_, _) | Ty::Matrix(_, _, _) => {
            let (elem, len) = agg_elem_and_len(ty);
            let elem_llty = llvm_ty(elem)?;
            Ok(format!("[{len} x {elem_llty}]"))
        }
        // Phase B1 (str/tcp codegen plan): a `tcp`/`tcp_listener` handle
        // is just a raw OS file descriptor — the kernel already tracks
        // everything a "handle" needs, so no separate handle table is
        // needed the way `Value::Tcp`'s `Arc<Mutex<Option<..>>>` wrapper
        // gives the interpreter. Both lower to the same `i64` regardless
        // of which one they are (`nir_tcp_stop` closes either uniformly).
        Ty::Tcp | Ty::TcpListener => Ok("i64".to_string()),
        Ty::Named(name, _) => unsupported(format!(
            "codegen doesn't support `{name}` yet — struct/enum/match (Row 11) are interpreter-only \
             for now, see nirdosha_row11_amendment.md"
        )),
        Ty::Error => unreachable!("a program with a type error is never handed to codegen"),
    }
}

/// Element type and total flat element count for an aggregate `Ty` --
/// `Matrix(T,R,C)` flattens to `R*C` elements, row-major, matching
/// `interpreter.rs`'s `Value::Matrix` storage exactly. Only ever called
/// where `Ty::is_aggregate()` is already known true.
fn agg_elem_and_len(ty: &Ty) -> (&Ty, usize) {
    match ty {
        Ty::Vector(elem, n) => (elem, *n),
        Ty::Matrix(elem, r, c) => (elem, r * c),
        _ => unreachable!("only called on Ty::is_aggregate() types"),
    }
}

/// A scalar element type's in-memory size, for `llvm.memcpy` byte counts.
/// No literal or builtin in this language can ever *construct* a value
/// whose `Vector`/`Matrix` element type is itself `Vector`/`Matrix` --
/// `typeck::infer_array_lit` collapses any 2-level literal nesting
/// straight into `Ty::Matrix`, and 3+ levels is a static
/// `ArrayLiteralTooDeep` error — even though `Vector(Vector(f64,3), 2)`
/// is syntactically parseable as a bare type annotation
/// (`parser.rs::expect_type` recurses generically on the element type).
/// So this never actually runs on a nested-aggregate element for any
/// value that reaches codegen, even though the type system doesn't rule
/// the annotation itself out.
fn elem_byte_size(ty: &Ty) -> u64 {
    match ty {
        Ty::I8 | Ty::U8 | Ty::Bool => 1,
        Ty::I16 | Ty::U16 => 2,
        Ty::I32 | Ty::U32 => 4,
        Ty::I64 | Ty::U64 | Ty::Usize | Ty::F64 => 8,
        _ => unreachable!(
            "Vector/Matrix element types are always plain scalars for any constructible \
             value -- see this fn's doc comment"
        ),
    }
}

/// The heap-allocation size (in bytes) `box e` needs for a value of type
/// `ty` — unlike `elem_byte_size` (scalars only, for aggregate elements)
/// or `agg_layout` (aggregates only), this covers every `Ty` `box` can
/// legally wrap (ast.rs: "any type, recursively" — `box i64`, `box
/// box i64`, `box Vector(f64,3)`, `box "..."`'s type, etc.), since `box`
/// itself has no such restriction. Matches `llvm_ty`'s own type-to-size
/// story exactly: `Vector`/`Matrix` use their flat byte layout,
/// `Ty::Str` is the two-word `{ptr, i64}` value (16 bytes), every
/// pointer-shaped handle (`Box`, `Ref`, and — once later phases compile
/// them — `Thread`/`Channel`/`Sandbox`/`Tcp`/`TcpListener`) is one
/// pointer-word (8 bytes on every platform this backend targets), and
/// everything else is a plain scalar via `elem_byte_size`.
fn ty_byte_size(ty: &Ty) -> u64 {
    match ty {
        Ty::Vector(_, _) | Ty::Matrix(_, _, _) => agg_layout(ty).2,
        Ty::Str => 16,
        Ty::Box(_) | Ty::Ref(_) | Ty::Thread(_) | Ty::Channel(_) | Ty::Sandbox | Ty::Tcp | Ty::TcpListener | Ty::File => 8,
        Ty::Unit => 0,
        _ => elem_byte_size(ty),
    }
}

/// `(element Ty, flat element count, total byte size)` for an aggregate
/// type — everything `llvm.memcpy`-based aggregate codegen (function
/// prologue copy-in, `let`/assignment copies, literal row copies) needs
/// in one call.
fn agg_layout(ty: &Ty) -> (&Ty, usize, u64) {
    let (elem, len) = agg_elem_and_len(ty);
    let bytes = len as u64 * elem_byte_size(elem);
    (elem, len, bytes)
}

/// Structural pre-check: walks the whole program and rejects, with a
/// specific reason, anything `llvm_ty` or the `print`-argument rule
/// would reject — run once, up front, so codegen itself can assume every
/// type/expression it encounters is in the supported subset.
pub fn check_supported(program: &Program) -> Result<(), CodegenError> {
    // Row 11 (struct/enum/match) is interpreter-only, full stop — but
    // `program.enums` is never actually empty any more (`Option`/
    // `Result` are injected into *every* program at parse time —
    // `ast::prelude_enums`'s doc comment), so a blanket "any struct/enum
    // declared at all" check would reject every program unconditionally,
    // not just ones that actually use Row 11. This instead rejects a
    // program only if it actually *uses* a struct/enum: `llvm_ty`
    // already rejects any `Ty::Named` reaching a param/let/return type,
    // and `check_expr`'s own `Expr::FieldAccess`/`Expr::Match` arms
    // already reject those forms directly — `ctor_names` (struct names
    // and every enum's variant names, prelude included) is the one
    // remaining gap: a struct/variant constructor call is syntactically
    // just `Expr::Call`, indistinguishable from an ordinary function
    // call without a declaration table to check the callee's name
    // against, which `check_expr` doesn't otherwise have access to.
    let mut ctor_names: std::collections::HashSet<&str> = program.structs.iter().map(|s| s.name.as_str()).collect();
    for e in &program.enums {
        ctor_names.extend(e.variants.iter().map(|v| v.name.as_str()));
    }
    for f in &program.fns {
        for p in &f.params {
            llvm_ty(&p.ty)?;
        }
        llvm_ty(&f.ret)?;
        check_stmts(&f.body.stmts, &ctor_names)?;
    }
    Ok(())
}

fn check_stmts(stmts: &[Stmt], ctor_names: &std::collections::HashSet<&str>) -> Result<(), CodegenError> {
    for s in stmts {
        check_stmt(s, ctor_names)?;
    }
    Ok(())
}

fn check_stmt(s: &Stmt, ctor_names: &std::collections::HashSet<&str>) -> Result<(), CodegenError> {
    match s {
        Stmt::Let { ty, value, .. } => {
            llvm_ty(ty)?;
            check_expr(value, ctor_names)
        }
        Stmt::Return { value: Some(e), .. } => check_expr(e, ctor_names),
        Stmt::Return { value: None, .. } => Ok(()),
        Stmt::While { cond, body, .. } => {
            check_expr(cond, ctor_names)?;
            check_stmts(&body.stmts, ctor_names)
        }
        Stmt::Expr(e) => check_expr(e, ctor_names),
        // `audited` only suppresses guard *emission* (`Codegen::audited`,
        // checked inside `guard_in_range`/the division trap) -- every
        // statement inside still has to be otherwise codegen-supported,
        // so this walks in exactly like `While`'s body.
        Stmt::Audited { body, .. } => check_stmts(body, ctor_names),
    }
}

fn check_expr(e: &Expr, ctor_names: &std::collections::HashSet<&str>) -> Result<(), CodegenError> {
    match e {
        Expr::Int(_, _) | Expr::Bool(_, _) | Expr::Ident(_, _) | Expr::Float(_, _) | Expr::Str(_, _) => Ok(()),
        Expr::Unary(_, inner, _) => check_expr(inner, ctor_names),
        Expr::Binary(_, l, r, _) => {
            check_expr(l, ctor_names)?;
            check_expr(r, ctor_names)
        }
        Expr::Call(name, args, _) => {
            if ctor_names.contains(name.as_str()) {
                return unsupported(format!(
                    "codegen doesn't support `{name}` yet — struct/enum construction (Row 11) is \
                     interpreter-only for now, see nirdosha_row11_amendment.md"
                ));
            }
            if name == "print" {
                for a in args {
                    if !is_printable_expr(a) {
                        return unsupported(
                            "codegen only supports `print` on integer/f64-typed arguments so far \
                             — no existing example needs bool/unit printing, so it wasn't built",
                        );
                    }
                }
            } else if is_builtin(name)
                && !PHASE4_BUILTINS.contains(&name.as_str())
                && !PHASE5_BUILTINS.contains(&name.as_str())
            {
                // Every dense linear algebra builtin not in
                // `PHASE4_BUILTINS` (unrolled IR) or `PHASE5_BUILTINS`
                // (linked runtime call, for genuine data-dependent
                // control flow) is rejected here with a specific reason
                // rather than falling through to `check_expr`'s
                // per-argument walk, which would report a less specific
                // one. `rand_seed`/`rand_f64`/`rand_gaussian` stay
                // rejected (no RNG state exists in generated code).
                return unsupported(format!(
                    "codegen doesn't support `{name}` yet — this builtin is interpreter-only \
                     for now (numeric codegen lands in a later phase)"
                ));
            }
            for a in args {
                check_expr(a, ctor_names)?;
            }
            Ok(())
        }
        Expr::If { cond, then_block, else_block, .. } => {
            check_expr(cond, ctor_names)?;
            check_stmts(&then_block.stmts, ctor_names)?;
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => check_stmts(&b.stmts, ctor_names),
                Some(ElseBranch::If(e2)) => check_expr(e2, ctor_names),
                None => Ok(()),
            }
        }
        Expr::Assign(_, rhs, _) => check_expr(rhs, ctor_names),
        // Row 11's other two unsupported forms, rejected directly —
        // neither can type-check without a real struct/enum somewhere
        // (`typeck.rs`), but unlike a bare constructor call, both are
        // their own dedicated `Expr` variants, easy to reject by shape
        // alone with no `ctor_names` lookup needed.
        Expr::FieldAccess(..) | Expr::Match { .. } => unsupported(
            "codegen doesn't support struct/enum/match yet (Row 11) — interpreter-only for now",
        ),
        // `box`/`*`/`&` land as of this phase — see `llvm_ty`'s
        // `Ty::Box`/`Ty::Ref` arm and `Codegen::expr`'s real `Expr::Box`/
        // `Expr::Deref`/`Expr::Ref` arms for the actual codegen. This
        // structural pre-pass has no type info (that's `local_ty_of`'s
        // job, at real IR-gen time), so it just recurses into whatever's
        // inside — same "walk, don't reject" treatment every other
        // already-supported unary-ish construct gets.
        Expr::Box(inner, _) | Expr::Deref(inner, _) | Expr::Ref(inner, _) => check_expr(inner, ctor_names),
        Expr::Spawn(_, _, _) | Expr::Join(_, _) => {
            unsupported("codegen doesn't support `spawn`/`join` yet — interpreter-only for now")
        }
        // `chan` construction itself is still unsupported (Phase D2) —
        // but `send`/`recv` are the same two AST nodes `tcp`'s I/O reuses
        // (typeck.rs's `Expr::Send`/`Expr::Recv` arms dispatch on the
        // operand's *type*, `Ty::Channel` vs `Ty::Tcp`), and this
        // structural pre-pass has no type info to tell those apart (same
        // "type-oblivious pre-pass, real check happens at IR-gen time"
        // precedent `print`'s aggregate rejection already established —
        // module doc). So both recurse here; `Codegen::expr`'s real
        // `Expr::Send`/`Expr::Recv` arms are the ones that reject a
        // `Ty::Channel` operand specifically, gracefully, once they can
        // actually see its type via `local_ty_of`.
        Expr::Chan(_) => unsupported("codegen doesn't support `chan` yet — interpreter-only for now"),
        Expr::Send(chan, value, _) => {
            check_expr(chan, ctor_names)?;
            check_expr(value, ctor_names)
        }
        Expr::Recv(chan, _) => check_expr(chan, ctor_names),
        // Same reasoning as `send`/`recv` above: `stop` is one AST node
        // (`Expr::StopSandbox`) shared by `sandbox`/`tcp`/`tcp_listener`,
        // dispatched on the operand's type — `sandbox` itself stays
        // rejected (unsupported below), `stop` recurses structurally so
        // `Codegen::expr` can accept it for a `Ty::Tcp`/`Ty::TcpListener`
        // operand and reject it for `Ty::Sandbox` with real type info.
        Expr::SpawnSandbox(_, _, _) => {
            unsupported("codegen doesn't support `sandbox` yet — interpreter-only for now")
        }
        Expr::StopSandbox(inner, _) => check_expr(inner, ctor_names),
        // `open`/file I/O is interpreter-only for now, same treatment
        // `Expr::Chan`/`Expr::SpawnSandbox` above get — no type info at
        // this structural pre-pass, so straight rejection rather than
        // recursing into a program that's unsupported regardless.
        Expr::Open(_, _, _) => {
            unsupported("codegen doesn't support `open`/file I/O yet — interpreter-only for now")
        }
        Expr::Connect(host, port, _) => {
            check_expr(host, ctor_names)?;
            check_expr(port, ctor_names)
        }
        Expr::Listen(port, _) => check_expr(port, ctor_names),
        Expr::Accept(listener, _) => check_expr(listener, ctor_names),
        // Vector/Matrix indexing lands as of this phase — the base and
        // every index expression are walked the same way `ArrayLit`'s
        // elements are, so a still-unsupported construct nested inside
        // either (e.g. a `.*` index expression) keeps its own specific
        // rejection reason instead of being silently accepted because
        // `Expr::Index` itself is now fine.
        Expr::Index(base, indices, _) => {
            check_expr(base, ctor_names)?;
            for idx in indices {
                check_expr(idx, ctor_names)?;
            }
            Ok(())
        }
        // Vector/Matrix literals land as of this phase — walk each
        // element the same way `Expr::Call`'s arguments are walked, so a
        // still-unsupported construct nested inside a literal (e.g. a
        // `.*` element expression) is still caught with its own specific
        // reason rather than silently accepted because the outer
        // `ArrayLit` shape itself is now fine.
        Expr::ArrayLit(elements, _) => {
            for e in elements {
                check_expr(e, ctor_names)?;
            }
            Ok(())
        }
        // `TRANSACT.md`'s own decision: "Compiled backend (codegen.rs) is
        // out of scope until the interpreter version is proven" — same
        // "reject, don't mis-compile" treatment every other unimplemented
        // construct gets (`box`, `thread`, `chan`, `sandbox`, `tcp` per
        // `LANGUAGE.md` §10). `transact` joins that list, not an
        // exception to it.
        Expr::Transact { .. } => {
            unsupported("codegen doesn't support `transact` yet — interpreter-only for now")
        }
    }
}

/// A conservative, codegen-local "is this definitely not a `bool`"
/// check — not a real type inference pass (typeck.rs already is one;
/// re-deriving its full precision here isn't worth it for one narrow
/// question). Good enough to catch the common cases (`print(true)`,
/// `print(some_bool_var)`) without needing a declared-type table
/// threaded all the way into `check_expr`. Named for what it actually
/// accepts as of Phase 4 (integers *and* `f64`), not just integers.
fn is_printable_expr(e: &Expr) -> bool {
    !matches!(e, Expr::Bool(_, _))
}

/// One binding's declared type plus the LLVM register holding a
/// *pointer* to its stack slot (an `alloca` result) — reads go through a
/// `load`, writes through a `store`, exactly like `clang -O0`'s output.
struct Scopes(Vec<HashMap<String, (Ty, String)>>);

impl Scopes {
    fn new() -> Self {
        Scopes(vec![HashMap::new()])
    }
    fn push(&mut self) {
        self.0.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.0.pop();
    }
    fn define(&mut self, name: &str, ty: Ty, ptr_reg: String) {
        self.0.last_mut().unwrap().insert(name.to_string(), (ty, ptr_reg));
    }
    fn get(&self, name: &str) -> Option<(Ty, String)> {
        self.0.iter().rev().find_map(|s| s.get(name)).cloned()
    }
}

/// A function's declared signature — codegen's own copy, built once up
/// front (mirroring `typeck::FnSig`, which is private to that module and
/// not reusable here). `call()` needs this for two things LLVM requires
/// to get exactly right at every call site: the call instruction's
/// return type must match the callee's `define` exactly, and every
/// argument's type annotation must match the corresponding declared
/// parameter type exactly — guessing either from the argument
/// *expression's* own shape (an earlier draft's approach) is wrong
/// whenever a literal argument's "natural" type doesn't match a narrower
/// declared parameter (see the "found by testing" note in the module
/// doc / PHASE0.md's write-up of this milestone).
struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
}

struct Codegen<'a> {
    out: String,
    /// Every `alloca` this function emits, collected here instead of
    /// written inline to `self.out` at the point of use, then spliced
    /// into `self.out` right after `entry:` once `function()` finishes
    /// generating the body. Reset per-function. Necessary because an
    /// aggregate (`Vector`/`Matrix`) alloca's address is always taken
    /// (passed to `memcpy`/GEP/a `call`), so LLVM's `mem2reg` can never
    /// promote it away — an alloca emitted inline inside a loop body
    /// would allocate fresh, unreclaimed stack space on every iteration
    /// instead of the one real stack slot a loop actually needs. Scalar
    /// allocas are hoisted the same way for uniformity and safety (it
    /// costs nothing — `mem2reg` eliminates them regardless of which
    /// block they start in), not just the aggregate ones that need it.
    entry_allocas: String,
    /// Every `str` literal's backing global constant, collected here for
    /// the same structural reason `entry_allocas` exists: a global
    /// definition (`@.str.N = ...`) is only valid LLVM IR at module
    /// scope, never written mid-function the way `self.out` is being
    /// built — a literal can appear anywhere inside any function body.
    /// Module-scoped, not per-function (unlike `entry_allocas`): never
    /// reset, appended to `self.out` once at the very end of
    /// `emit_llvm_ir`. LLVM doesn't care about textual definition order
    /// for a global referenced by name, so appending at the end rather
    /// than the true point of first use is fine.
    string_globals: String,
    tmp: usize,
    label: usize,
    smt_report: &'a SmtReport,
    /// Where to insert `nir_free` for still-owned `box`-typed bindings —
    /// computed once, up front, by `ownership.rs`'s own move-tracking
    /// pass (see `FreeMap`'s doc) rather than a second, codegen-side
    /// liveness analysis. Consulted at every scope-closing point
    /// (`Stmt::Return`, a loop body's end-of-iteration, an `if`/`audited`
    /// block's own close, and a function's implicit fall-off-the-end).
    free_map: FreeMap,
    sigs: HashMap<String, FnSig>,
    /// The function currently being generated code for — `Stmt::Return`
    /// needs its declared return type to guard/narrow against, and
    /// there's no other way to reach it from inside `stmt()` without
    /// threading it through every call.
    current_fn_ret: Ty,
    /// This function's own name — `free_map.at_fn_end` is keyed by it,
    /// consulted only at the implicit fall-off-the-end return point.
    current_fn_name: String,
    /// `Some("%sret.ret")` while generating a function whose return type
    /// is `Ty::is_aggregate()` — the implicit out-pointer parameter
    /// `Stmt::Return` memcpys its result into, instead of emitting a
    /// `ret <ty> <val>`. `None` for every scalar-returning function
    /// (including `unit`), which still just `ret`s normally.
    current_fn_sret: Option<String>,
    /// Once a block's been given a terminator (`br`/`ret`), any further
    /// statements in the same source block are unreachable — this stops
    /// codegen from emitting a second terminator into an already-closed
    /// block, which would be invalid IR.
    terminated: bool,
    /// Inside a `Stmt::Audited` body — `guard_in_range` and the
    /// division-by-zero trap both check this first and skip emitting
    /// their guard entirely when it's set (goal.md §4's Tier-3 escape
    /// hatch). A plain `bool`, not a depth counter: nested `audited`
    /// blocks don't need their own count, only "is at least one
    /// enclosing scope audited" — restored to its prior value (not
    /// unconditionally cleared) on exit so a nested `audited` inside a
    /// non-audited function correctly re-enables guards afterward, and
    /// one written (redundantly) inside an already-audited block doesn't
    /// prematurely turn guards back on when *it* exits.
    audited: bool,
}

pub fn emit_llvm_ir(program: &Program, smt_report: &SmtReport) -> Result<String, CodegenError> {
    check_supported(program)?;
    let sigs = program
        .fns
        .iter()
        .map(|f| (f.name.clone(), FnSig { params: f.params.iter().map(|p| p.ty.clone()).collect(), ret: f.ret.clone() }))
        .collect();
    // Trusts the program already passed `ownership::check_ownership` (the
    // caller's job, same as `typecheck_and_own`'s existing precedent) —
    // this recomputes the same move-tracking traversal for its own
    // FreeMap side data, not to re-validate.
    let free_map = ownership::compute_free_map(program);
    let mut cg =
        Codegen {
            out: String::new(),
            entry_allocas: String::new(),
            string_globals: String::new(),
            tmp: 0,
            label: 0,
            smt_report,
            free_map,
            sigs,
            current_fn_ret: Ty::Unit,
            current_fn_name: String::new(),
            current_fn_sret: None,
            terminated: false,
            audited: false,
        };

    writeln!(cg.out, "declare i32 @printf(ptr, ...)").unwrap();
    writeln!(cg.out, "declare void @abort() noreturn").unwrap();
    // Every aggregate (`Vector`/`Matrix`) copy — function-prologue
    // copy-in, a `let`/assignment's value copy, a matrix literal's
    // per-row copy — goes through this one intrinsic rather than a
    // hand-unrolled load/store loop; `clang -O2` lowers a small
    // constant-size `memcpy` to inline loads/stores itself, so this
    // costs nothing extra at the optimized output (module doc's own
    // "correctness over cleverness" call, extended to aggregates).
    writeln!(
        cg.out,
        "declare void @llvm.memcpy.p0.p0.i64(ptr noalias writeonly, ptr noalias readonly, i64, i1 immarg)"
    )
    .unwrap();
    // Phase 4's geometry/norm builtins (`lla_to_ecef`, `bearing`, `norm`,
    // ...) need real transcendental functions this backend has no plain
    // instruction for. `sqrt`/`sin`/`cos`/`fabs`/`maxnum` are standard
    // LLVM intrinsics (recognized by name, no special attributes needed
    // on the `declare` itself); `atan2` has no LLVM intrinsic form, so
    // it's declared as the plain libm C function instead — `build()`
    // links `-lm` for it (see that function's own note).
    writeln!(cg.out, "declare double @llvm.sqrt.f64(double)").unwrap();
    writeln!(cg.out, "declare double @llvm.sin.f64(double)").unwrap();
    writeln!(cg.out, "declare double @llvm.cos.f64(double)").unwrap();
    writeln!(cg.out, "declare double @llvm.fabs.f64(double)").unwrap();
    writeln!(cg.out, "declare double @llvm.maxnum.f64(double, double)").unwrap();
    writeln!(cg.out, "declare double @atan2(double, double)").unwrap();
    // Phase 5's data-dependent-control-flow builtins (`det`/`inv`/
    // `solve`/`rank`/`kf_update_state`/`kf_update_cov`) — linked native
    // calls into `runtime_kernels.rs`'s staticlib (`build()` writes it
    // out and links it alongside this `.ll`) rather than hand-emitted
    // branchy IR for partial-pivot selection. `i32` return, not `i1`:
    // Rust's `extern "C" fn -> bool` ABI representation isn't guaranteed
    // the way a plain `i32` 0/nonzero convention is.
    writeln!(cg.out, "declare double @nir_det(ptr, i64)").unwrap();
    writeln!(cg.out, "declare i32 @nir_inv(ptr, i64, ptr)").unwrap();
    writeln!(cg.out, "declare i32 @nir_solve(ptr, i64, ptr, ptr)").unwrap();
    writeln!(cg.out, "declare i64 @nir_rank(ptr, i64, i64)").unwrap();
    writeln!(cg.out, "declare i32 @nir_kf_update_state(ptr, ptr, ptr, ptr, ptr, i64, i64, ptr)").unwrap();
    writeln!(cg.out, "declare i32 @nir_kf_update_cov(ptr, ptr, ptr, ptr, ptr, i64, i64, ptr)").unwrap();
    // `str`'s one non-trivial operation (`==`/`!=`) — a length check plus
    // a byte compare, same "reuse proven Rust code via a linked call"
    // choice as the Phase 5 builtins above, not hand-emitted IR.
    writeln!(cg.out, "declare i32 @nir_str_eq(ptr, i64, ptr, i64)").unwrap();
    // Phase B1's `tcp`/`tcp_listener` kernels — real socket syscalls,
    // wrapped in Rust inside the linked staticlib rather than hand-
    // emitted raw syscall IR, same "reuse proven Rust code via a linked
    // call" choice as every other runtime kernel here. Handles are plain
    // `i64` fds (`llvm_ty`'s note on `Ty::Tcp`/`Ty::TcpListener`).
    writeln!(cg.out, "declare i64 @nir_tcp_connect(ptr, i64, i64)").unwrap();
    writeln!(cg.out, "declare i64 @nir_tcp_listen(i64)").unwrap();
    writeln!(cg.out, "declare i64 @nir_tcp_accept(i64)").unwrap();
    writeln!(cg.out, "declare i64 @nir_tcp_send(i64, ptr, i64)").unwrap();
    writeln!(cg.out, "declare i64 @nir_tcp_recv(i64, ptr, i64)").unwrap();
    writeln!(cg.out, "declare i32 @nir_tcp_stop(i64)").unwrap();
    // `box`'s heap allocator — see `Expr::Box`'s doc comment for why
    // `nir_free` isn't called anywhere yet (this phase deliberately
    // leaks; a later phase wires the calls once ownership.rs's move data
    // is threaded into codegen). Declared now regardless, same as every
    // other runtime kernel here, so that later phase is a pure `call`-
    // site change, not a declare-list change too.
    writeln!(cg.out, "declare ptr @nir_alloc(i64)").unwrap();
    writeln!(cg.out, "declare void @nir_free(ptr)").unwrap();
    // "%lld\n\0" — 6 bytes (%, l, l, d, \n, \0), not 5; LLVM's array
    // constant size has to match the literal exactly, byte for byte.
    writeln!(cg.out, "@.int_fmt = private unnamed_addr constant [6 x i8] c\"%lld\\0A\\00\"").unwrap();
    // "%f\n\0" — 4 bytes. `%f` (not `%g`/`%e`) is a plain, standard
    // choice for a `double`; it won't byte-for-byte match the
    // interpreter's `render()` (Rust's shortest-round-trip `f64`
    // formatting), a known, honest cosmetic difference between the two
    // execution paths, not a semantic one — both print the same real
    // number.
    writeln!(cg.out, "@.float_fmt = private unnamed_addr constant [4 x i8] c\"%f\\0A\\00\"").unwrap();
    // "%.*s\n\0" — 6 bytes. `%.*s` (precision from an explicit `i32` arg,
    // not `%s`) prints exactly `len` bytes regardless of what follows
    // them in memory — load-bearing, since a `str` value's buffer isn't
    // guaranteed NUL-terminated by this design (see `Ty::Str`'s note in
    // `llvm_ty`), only the format string itself is.
    writeln!(cg.out, "@.str_fmt = private unnamed_addr constant [6 x i8] c\"%.*s\\0A\\00\"").unwrap();
    writeln!(cg.out).unwrap();

    for f in &program.fns {
        cg.function(f)?;
    }

    cg.emit_c_main(program)?;
    // See `string_globals`'s own doc — every `str` literal's backing
    // global constant, collected during function codegen since it can't
    // be written mid-function-body, appended here once at the end.
    cg.out.push_str(&cg.string_globals);
    Ok(cg.out)
}

impl Codegen<'_> {
    fn fresh_reg(&mut self, prefix: &str) -> String {
        self.tmp += 1;
        format!("%{prefix}.{}", self.tmp)
    }
    fn fresh_label(&mut self, prefix: &str) -> String {
        self.label += 1;
        format!("{prefix}.{}", self.label)
    }
    /// A fresh, module-unique global name (`@.prefix.N`) — shares the
    /// `tmp` counter `fresh_reg` uses (no collision risk: `@.` vs. `%`
    /// are disjoint sigils), used for each `str` literal's backing
    /// constant.
    fn fresh_global(&mut self, prefix: &str) -> String {
        self.tmp += 1;
        format!("@.{prefix}.{}", self.tmp)
    }

    /// Every `alloca` in the whole file must go through this, never a
    /// direct `writeln!(self.out, "... = alloca ...")` — see the
    /// `entry_allocas` field doc for why.
    fn emit_alloca(&mut self, dest: &str, ty: &str) {
        writeln!(self.entry_allocas, "  {dest} = alloca {ty}").unwrap();
    }

    fn function(&mut self, f: &FnDecl) -> Result<(), CodegenError> {
        self.current_fn_ret = f.ret.clone();
        self.current_fn_name = f.name.clone();
        // Aggregate returns use an sret-style out-pointer, passed as an
        // implicit first argument, rather than an LLVM-level aggregate
        // return value — the first by-pointer ABI convention in this
        // file (module doc / `Codegen::expr_ptr`). The caller allocas
        // its own destination and passes its address; this function's
        // `Stmt::Return` memcpys its computed result into it and `ret
        // void`s, instead of `ret <ty> <val>`.
        let is_agg_ret = f.ret.is_aggregate();
        let ret_llty: String = if is_agg_ret { "void".to_string() } else { llvm_ty(&f.ret)? };
        let name = if f.name == "main" { "nir_main" } else { f.name.as_str() };

        let mut params: Vec<String> = Vec::new();
        if is_agg_ret {
            params.push("ptr %sret.ret".to_string());
        }
        for p in &f.params {
            if p.ty.is_aggregate() {
                // Aggregate params are passed as a plain pointer, not by
                // value — see the prologue below for the copy-in that
                // makes this behave like the language's actual value
                // semantics (there's no `m[i,j] = x` lvalue syntax, so a
                // callee can never observably mutate the caller's copy
                // through this pointer; the prologue copy exists so a
                // whole-variable reassignment inside the callee doesn't
                // either).
                params.push(format!("ptr %arg.{}", p.name));
            } else {
                params.push(format!("{} %arg.{}", llvm_ty(&p.ty)?, p.name));
            }
        }
        writeln!(self.out, "define {ret_llty} @{name}({}) {{", params.join(", ")).unwrap();
        writeln!(self.out, "entry:").unwrap();
        // Every `alloca` emitted from here until this function's closing
        // brace lands in `self.entry_allocas` instead of `self.out`
        // (see that field's doc) — remember exactly where in `self.out`
        // they belong (right after `entry:`, before anything else this
        // function emits) and splice them in once the body's done.
        let alloca_splice_pos = self.out.len();
        self.entry_allocas.clear();
        self.terminated = false;
        self.current_fn_sret = if is_agg_ret { Some("%sret.ret".to_string()) } else { None };

        let mut scopes = Scopes::new();
        for p in &f.params {
            if p.ty.is_aggregate() {
                let agg_llty = llvm_ty(&p.ty)?;
                let local_ptr = format!("%{}.addr", p.name);
                self.emit_alloca(&local_ptr, &agg_llty);
                let (_, _, bytes) = agg_layout(&p.ty);
                writeln!(
                    self.out,
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {local_ptr}, ptr %arg.{}, i64 {bytes}, i1 false)",
                    p.name
                )
                .unwrap();
                scopes.define(&p.name, p.ty.clone(), local_ptr);
            } else {
                let ty = llvm_ty(&p.ty)?;
                let ptr = format!("%{}.addr", p.name);
                self.emit_alloca(&ptr, &ty);
                writeln!(self.out, "  store {ty} %arg.{}, ptr {ptr}", p.name).unwrap();
                scopes.define(&p.name, p.ty.clone(), ptr);
            }
        }

        self.stmts(&f.body.stmts, &mut scopes)?;

        // A function whose body definitely returns on every path
        // (typeck.rs already proved this for any non-`unit` return type)
        // never falls off the end reachably — but the *block* still
        // needs a terminator if the very last statement wasn't itself a
        // `return` on this specific path (e.g. a `unit`-returning
        // function that just runs off the end normally).
        if !self.terminated {
            if let Some(names) = self.free_map.at_fn_end.get(&f.name).cloned() {
                self.emit_frees_for_names(&names, &scopes);
            }
            if f.ret == Ty::Unit {
                writeln!(self.out, "  ret void").unwrap();
            } else {
                // typeck.rs's definite-return analysis already rules
                // this out for any well-typed program; unreachable is
                // the honest LLVM idiom for "provably can't happen".
                writeln!(self.out, "  unreachable").unwrap();
            }
        }
        writeln!(self.out, "}}\n").unwrap();
        // Splice every alloca this function collected into place, right
        // after `entry:` — see `entry_allocas`'s doc and the note at
        // this function's start where `alloca_splice_pos` was captured.
        self.out.insert_str(alloca_splice_pos, &self.entry_allocas);
        Ok(())
    }

    fn stmts(&mut self, stmts: &[Stmt], scopes: &mut Scopes) -> Result<(), CodegenError> {
        for s in stmts {
            if self.terminated {
                break; // dead code after a `return`/branch — not emitted
            }
            self.stmt(s, scopes)?;
        }
        Ok(())
    }

    fn stmt(&mut self, stmt: &Stmt, scopes: &mut Scopes) -> Result<(), CodegenError> {
        match stmt {
            Stmt::Let { name, ty, value, span } => {
                if ty.is_aggregate() {
                    // No integer guard/narrow pipeline applies to an
                    // aggregate — always a fresh destination alloca plus
                    // a whole-value `memcpy` from whatever `expr_ptr`
                    // produced, never a direct alias of the source
                    // pointer: `let w = v` has to give `w` its own
                    // storage, or a later `w = ...` reassignment would
                    // silently mutate `v` too (see `Codegen::expr_ptr`'s
                    // `Expr::Ident` arm, which deliberately returns the
                    // existing pointer with no copy of its own).
                    let src = self.expr_ptr(value, scopes)?;
                    let agg_llty = llvm_ty(ty)?;
                    let dest = self.fresh_reg(&format!("{name}.addr"));
                    self.emit_alloca(&dest, &agg_llty);
                    let (_, _, bytes) = agg_layout(ty);
                    writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {dest}, ptr {src}, i64 {bytes}, i1 false)").unwrap();
                    scopes.define(name, ty.clone(), dest);
                    return Ok(());
                }
                let val = self.expr(value, scopes)?; // i64 (or i1 for bool)
                let val = self.guard_in_range(&val, ty, *span)?; // checked at i64 width, before narrowing
                let val = if ty.is_integer() { self.narrow_from_i64(&val, ty)? } else { val };
                let llty = llvm_ty(ty)?;
                let ptr = self.fresh_reg(&format!("{name}.addr"));
                self.emit_alloca(&ptr, &llty);
                writeln!(self.out, "  store {llty} {val}, ptr {ptr}").unwrap();
                scopes.define(name, ty.clone(), ptr);
                Ok(())
            }
            Stmt::Return { value, span } => {
                // Captured once, up front — every arm below needs it
                // emitted immediately before its own `ret`, since nothing
                // can follow a block's terminator in valid IR. `ty`
                // still resolves at this point (before any pop), whether
                // or not `value` itself is the box being returned —
                // `ownership.rs` already excluded a directly-returned box
                // from this exact list (see `FreeMap::at_return`'s doc).
                let free_names = self.free_map.at_return.get(span).cloned().unwrap_or_default();
                match value {
                    Some(e) => {
                        let ret_ty = self.current_fn_ret.clone();
                        if ret_ty.is_aggregate() {
                            let src = self.expr_ptr(e, scopes)?;
                            let sret = self
                                .current_fn_sret
                                .clone()
                                .expect("an aggregate-returning function always sets current_fn_sret");
                            let (_, _, bytes) = agg_layout(&ret_ty);
                            writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {sret}, ptr {src}, i64 {bytes}, i1 false)").unwrap();
                            self.emit_frees_for_names(&free_names, scopes);
                            writeln!(self.out, "  ret void").unwrap();
                        } else {
                            let val = self.expr(e, scopes)?;
                            // `refine.rs`/`smt.rs` now both record a proof for
                            // `return` sites too (they gained their own
                            // `current_fn_ret` field, the same fix this file
                            // already had) — so this can be genuine Tier 1 in
                            // practice, not just in principle. Still routed
                            // through the same real guard either way, not a
                            // hardcoded always-check special case.
                            let val = self.guard_in_range(&val, &ret_ty, *span)?;
                            let val = if ret_ty.is_integer() { self.narrow_from_i64(&val, &ret_ty)? } else { val };
                            self.emit_frees_for_names(&free_names, scopes);
                            writeln!(self.out, "  ret {} {val}", llvm_ty(&ret_ty)?).unwrap();
                        }
                    }
                    None => {
                        self.emit_frees_for_names(&free_names, scopes);
                        writeln!(self.out, "  ret void").unwrap();
                    }
                }
                self.terminated = true;
                Ok(())
            }
            Stmt::While { cond, body, span } => self.while_loop(*span, cond, body, scopes),
            Stmt::Expr(e) => {
                // An aggregate-valued expression statement (e.g. a bare
                // `v = w` reassignment) has no bare SSA value to give
                // `expr()` — same fork as `Stmt::Let`/`Stmt::Return`,
                // based on `local_ty_of`'s (typeck-mirroring) guess at
                // `e`'s type rather than a full inference pass, matching
                // this function's own existing precedent.
                if self.local_ty_of(e, scopes).is_aggregate() {
                    self.expr_ptr(e, scopes)?;
                } else {
                    self.expr(e, scopes)?;
                }
                Ok(())
            }
            Stmt::Audited { body, span, .. } => {
                // Save/restore, not unconditional reset -- see
                // `Codegen::audited`'s doc comment for why (nesting).
                let was_audited = self.audited;
                self.audited = true;
                scopes.push();
                let result = self.stmts(body, scopes);
                // Only when the block falls through normally -- if it
                // already emitted a `ret`/`br` (an inner `return`), the
                // block is terminated and nothing more can follow it in
                // valid IR; `Stmt::Return`'s own free-emission already
                // covered every still-owned box on that path.
                if result.is_ok()
                    && !self.terminated
                    && let Some(names) = self.free_map.at_audited_end.get(span).cloned()
                {
                    self.emit_frees_for_names(&names, scopes);
                }
                scopes.pop();
                self.audited = was_audited;
                result
            }
        }
    }

    /// A very small, codegen-local "what LLVM type does this produce"
    /// helper — used only where a caller (`return`) needs it and typeck
    /// isn't threaded through. Trusts the program is already well-typed
    /// (typeck.rs ran first), so it doesn't need to be a full inference
    /// pass, just enough to pick the right LLVM type keyword.
    fn local_ty_of(&self, e: &Expr, scopes: &Scopes) -> Ty {
        match e {
            Expr::Int(_, _) => Ty::I64,
            Expr::Float(_, _) => Ty::F64,
            Expr::Bool(_, _) => Ty::Bool,
            Expr::Str(_, _) => Ty::Str,
            Expr::Ident(name, _) => scopes.get(name).map(|(t, _)| t).unwrap_or(Ty::I64),
            Expr::Unary(UnOp::Not, _, _) => Ty::Bool,
            Expr::Unary(UnOp::Neg, inner, _) => self.local_ty_of(inner, scopes),
            Expr::Binary(op, l, r, _) => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq | BinOp::And | BinOp::Or => {
                    Ty::Bool
                }
                // `*`'s result shape depends on *both* operands (a
                // Matrix's own left-operand type is not always the
                // answer -- `Matrix * Vector` produces a `Vector`,
                // `scalar * Matrix` produces the `Matrix`) -- every other
                // arithmetic op here is same-shape-on-both-sides
                // (typeck-guaranteed), so the left operand's type alone
                // is still correct for those.
                BinOp::Mul => self.mul_result_ty(l, r, scopes),
                _ => self.local_ty_of(l, scopes),
            },
            Expr::Assign(name, _, _) => scopes.get(name).map(|(t, _)| t).unwrap_or(Ty::I64),
            Expr::Call(name, args, _)
                if PHASE4_BUILTINS.contains(&name.as_str()) || PHASE5_BUILTINS.contains(&name.as_str()) =>
            {
                self.builtin_result_ty(name, args, scopes)
            }
            Expr::Call(name, _, _) => self.sigs.get(name).map(|s| s.ret.clone()).unwrap_or(Ty::I64),
            Expr::ArrayLit(elements, _) => self.array_lit_ty(elements, scopes),
            Expr::Index(base, _, _) => match self.local_ty_of(base, scopes) {
                Ty::Vector(elem, _) | Ty::Matrix(elem, _, _) => *elem,
                _ => Ty::I64,
            },
            // Needed so a directly-nested call (e.g. `stop(connect(...))`,
            // with no intervening `let` to record the declared type in
            // `scopes`) still reports the right `Ty` to `Expr::StopSandbox`/
            // `Expr::Send`/`Expr::Recv`'s own `local_ty_of` dispatch —
            // mirrors `typeck.rs`'s `infer` arms for these exactly.
            Expr::Connect(_, _, _) => Ty::Tcp,
            Expr::Listen(_, _) => Ty::TcpListener,
            Expr::Accept(_, _) => Ty::Tcp,
            Expr::Recv(target, _) => match self.local_ty_of(target, scopes) {
                Ty::Tcp => Ty::Str,
                Ty::Channel(inner) => *inner,
                _ => Ty::I64,
            },
            Expr::Box(inner, _) => Ty::Box(Box::new(self.local_ty_of(inner, scopes))),
            Expr::Ref(inner, _) => Ty::Ref(Box::new(self.local_ty_of(inner, scopes))),
            // `*e` unwraps exactly one pointer level — `ownership.rs`
            // already proved `e`'s type is `Box`/`Ref` for any program
            // that reaches codegen, and (per its own move-checking) that
            // unwrapping affine content out of a shared `Ref` never
            // typechecks in the first place, so this never needs to
            // reject anything itself, only report the unwrapped type.
            Expr::Deref(inner, _) => match self.local_ty_of(inner, scopes) {
                Ty::Box(t) | Ty::Ref(t) => *t,
                _ => Ty::I64,
            },
            _ => Ty::I64,
        }
    }

    /// Mirrors `typeck::infer_array_lit`'s Vector-vs-Matrix shape rule
    /// exactly (minus its diagnostic paths, irrelevant here — codegen
    /// only ever sees an already-well-typed program): element 0's own
    /// type decides everything. A plain scalar element 0 makes the whole
    /// literal a `Vector`; a same-shaped-`Vector` element 0 (of scalars,
    /// not itself nested) makes it a `Matrix`, flattened row-major.
    fn array_lit_ty(&self, elements: &[Expr], scopes: &Scopes) -> Ty {
        let t0 = self.local_ty_of(&elements[0], scopes);
        match &t0 {
            Ty::Vector(inner, n) if !matches!(inner.as_ref(), Ty::Vector(..) | Ty::Matrix(..)) => {
                Ty::Matrix(inner.clone(), elements.len(), *n)
            }
            _ => Ty::Vector(Box::new(t0), elements.len()),
        }
    }

    /// Mirrors `typeck::infer_mul`'s shape-resolution table (minus its
    /// diagnostics -- codegen only ever sees an already-well-typed
    /// program, so `Vector * Vector` and other illegal shapes never
    /// reach this): scalar × `Matrix` (either order) keeps the `Matrix`'s
    /// shape, `Matrix * Vector` produces a `Vector` sized to the
    /// `Matrix`'s row count, `Matrix * Matrix` produces a `Matrix` sized
    /// `(rows-of-left, cols-of-right)`, and two matching scalars keep
    /// their shared scalar type.
    fn mul_result_ty(&self, lhs: &Expr, rhs: &Expr, scopes: &Scopes) -> Ty {
        let lt = self.local_ty_of(lhs, scopes);
        let rt = self.local_ty_of(rhs, scopes);
        match (&lt, &rt) {
            (s, Ty::Matrix(elem, r, c)) if !s.is_aggregate() => Ty::Matrix(elem.clone(), *r, *c),
            (Ty::Matrix(elem, r, c), s) if !s.is_aggregate() => Ty::Matrix(elem.clone(), *r, *c),
            (Ty::Matrix(m_elem, r, _c), Ty::Vector(..)) => Ty::Vector(m_elem.clone(), *r),
            (Ty::Matrix(l_elem, r1, _c1), Ty::Matrix(_, _r2, c2)) => Ty::Matrix(l_elem.clone(), *r1, *c2),
            _ => lt,
        }
    }

    /// Phase 4's `local_ty_of` analog for a builtin call — mirrors each
    /// builtin's `typeck.rs` signature (minus diagnostics, same "already
    /// well-typed" trust every other `local_ty_of` arm relies on), needed
    /// because `call_ptr`/`call_builtin_agg` have to allocate a
    /// correctly-shaped destination before they know what a `let`'s own
    /// declared type annotation says (they're reached through `expr_ptr`,
    /// which doesn't see that outer context). `zeros`/`ones`/`identity`
    /// read their shape off `literal_value` (`ast.rs`), the same
    /// literal-only rule `typeck::literal_dimension` enforces.
    fn builtin_result_ty(&self, name: &str, args: &[Expr], scopes: &Scopes) -> Ty {
        match name {
            "transpose" => match self.local_ty_of(&args[0], scopes) {
                Ty::Matrix(elem, r, c) => Ty::Matrix(elem, c, r),
                _ => Ty::F64,
            },
            "dot" => match self.local_ty_of(&args[0], scopes) {
                Ty::Vector(elem, _) => *elem,
                _ => Ty::F64,
            },
            "cross" => self.local_ty_of(&args[0], scopes),
            "zeros" | "ones" => {
                if args.len() == 1 {
                    let n = literal_value(&args[0]).unwrap_or(0) as usize;
                    Ty::Vector(Box::new(Ty::F64), n)
                } else {
                    let r = literal_value(&args[0]).unwrap_or(0) as usize;
                    let c = literal_value(&args[1]).unwrap_or(0) as usize;
                    Ty::Matrix(Box::new(Ty::F64), r, c)
                }
            }
            "identity" => {
                let n = literal_value(&args[0]).unwrap_or(0) as usize;
                Ty::Matrix(Box::new(Ty::F64), n, n)
            }
            "sum" => match self.local_ty_of(&args[0], scopes) {
                Ty::Vector(elem, _) | Ty::Matrix(elem, _, _) => *elem,
                _ => Ty::F64,
            },
            "len" => Ty::I64,
            "norm" | "norm1" | "norm_inf" | "frobenius_norm" | "distance" | "bearing" => Ty::F64,
            "trace" => match self.local_ty_of(&args[0], scopes) {
                Ty::Matrix(elem, _, _) => *elem,
                _ => Ty::F64,
            },
            "is_symmetric" | "is_diag" | "is_square" => Ty::Bool,
            "lla_to_ecef" | "ecef_to_lla" | "ecef_to_enu" | "enu_to_ecef" => Ty::Vector(Box::new(Ty::F64), 3),
            "kf_predict_state" => match self.local_ty_of(&args[0], scopes) {
                Ty::Vector(elem, n) => Ty::Vector(elem, n),
                _ => Ty::F64,
            },
            "kf_predict_cov" => match self.local_ty_of(&args[0], scopes) {
                Ty::Vector(elem, n) => Ty::Matrix(elem, n, n),
                _ => Ty::F64,
            },
            // Phase 5: `inv` keeps its square-matrix operand's own shape;
            // `solve`/`kf_update_state` return a `Vector` shaped like the
            // state/RHS vector operand; `kf_update_cov` returns a square
            // `Matrix` sized off that same vector's length; `det`/`rank`
            // are scalar (handled by the catch-all fallthrough via their
            // absence here would be wrong -- listed explicitly instead).
            "det" => Ty::F64,
            "inv" => self.local_ty_of(&args[0], scopes),
            "solve" => self.local_ty_of(&args[1], scopes),
            "rank" => Ty::I64,
            "kf_update_state" => self.local_ty_of(&args[0], scopes),
            "kf_update_cov" => match self.local_ty_of(&args[0], scopes) {
                Ty::Vector(elem, n) => Ty::Matrix(elem, n, n),
                _ => Ty::F64,
            },
            _ => unreachable!("PHASE4_BUILTINS/PHASE5_BUILTINS and this match must stay in sync"),
        }
    }

    /// Every integer-typed value this backend hands around internally is
    /// `i64` — see the module doc's "why arithmetic is always computed
    /// at i64 width" note; this is the *load* side of that: a value just
    /// read out of a narrower-than-`i64` stack slot gets sign-extended
    /// immediately, before it can participate in anything else. A no-op
    /// for `bool` (stays `i1` throughout — it never enters the i64
    /// scheme) or a value that's already `i64`.
    fn widen_to_i64(&mut self, val: &str, ty: &Ty) -> String {
        if !ty.is_integer() || *ty == Ty::I64 {
            return val.to_string();
        }
        let llty = llvm_ty(ty).expect("check_supported already validated this type");
        let r = self.fresh_reg("widen");
        writeln!(self.out, "  {r} = sext {llty} {val} to i64").unwrap();
        r
    }

    /// The *store* side: narrows an `i64` value back down to `ty`'s
    /// actual declared width, right before it's written to a stack slot,
    /// passed as a call argument, or returned. Always lossless when
    /// called on a value that already passed `guard_in_range` for this
    /// same `ty` (that's the whole point of checking *before* narrowing,
    /// not after — see the module doc's "found by testing" note on why
    /// computing directly at the narrow width was wrong).
    fn narrow_from_i64(&mut self, val: &str, ty: &Ty) -> Result<String, CodegenError> {
        let llty = llvm_ty(ty)?;
        if llty == "i64" {
            return Ok(val.to_string());
        }
        let r = self.fresh_reg("narrow");
        writeln!(self.out, "  {r} = trunc i64 {val} to {llty}").unwrap();
        Ok(r)
    }

    /// Tier 1 vs Tier 2, for real: if `span` is in the SMT report's
    /// proven-safe set, `val` is used exactly as computed — no runtime
    /// check, no cost, matching goal.md §4's Tier 1. Otherwise, emits an
    /// actual compare-and-trap sequence: a value outside `ty`'s range
    /// calls `abort()` rather than silently wrapping or corrupting
    /// anything. Returns `val` unchanged either way — the check is
    /// side-effecting (branches to a trap block if it fails), not a
    /// transformation of the value itself.
    ///
    /// **Compares at `i64` width, always — this is load-bearing, not
    /// cosmetic.** An earlier draft compared at `ty`'s own (narrow)
    /// width, using LLVM's plain `add`/`sub`/`mul` computed *at that
    /// narrow width already* — which silently wraps on overflow, the
    /// same as any two's-complement machine addition. That meant the
    /// check was comparing an *already-wrapped* value against the very
    /// bounds it's supposed to detect escaping — a wrapped 8-bit value
    /// is, by construction, always representable in 8 bits, so the
    /// check could never fire. Found by actually running a deliberately
    /// overflowing test program through a compiled binary and watching
    /// it exit 0 instead of aborting — not caught by reading the code.
    /// The fix (this function, plus `widen_to_i64`/`narrow_from_i64`
    /// bracketing every arithmetic op) keeps every intermediate value at
    /// `i64` until *after* this check has run, exactly matching how
    /// `interpreter.rs`'s `Value::Int(i64)` already worked all along.
    fn guard_in_range(&mut self, val: &str, ty: &Ty, span: Span) -> Result<String, CodegenError> {
        if self.audited || self.smt_report.proven_in_range.contains(&span) || !ty.is_integer() {
            return Ok(val.to_string());
        }
        let (lo, hi) = ty.bounds();
        let ok_lo = self.fresh_reg("ge_lo");
        writeln!(self.out, "  {ok_lo} = icmp sge i64 {val}, {lo}").unwrap();
        let ok_hi = self.fresh_reg("le_hi");
        writeln!(self.out, "  {ok_hi} = icmp sle i64 {val}, {hi}").unwrap();
        let ok = self.fresh_reg("in_range");
        writeln!(self.out, "  {ok} = and i1 {ok_lo}, {ok_hi}").unwrap();
        let pass = self.fresh_label("range_ok");
        let fail = self.fresh_label("range_trap");
        writeln!(self.out, "  br i1 {ok}, label %{pass}, label %{fail}").unwrap();
        writeln!(self.out, "{fail}:").unwrap();
        writeln!(self.out, "  call void @abort()").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        writeln!(self.out, "{pass}:").unwrap();
        Ok(val.to_string())
    }

    /// Frees one still-owned `box`-typed binding, recursing into nested
    /// `box box T` layers first — `nir_free` only reclaims the one heap
    /// block `ptr_reg` itself points to; a `box box i64`'s outer
    /// allocation holds nothing but *another* heap pointer as its
    /// content, and that inner allocation would leak forever if nothing
    /// ever loads it back out and frees it too. Order doesn't matter for
    /// memory safety (the inner pointer is read out before anything is
    /// freed), only for tidiness — innermost first, matching how nested
    /// teardown reads everywhere else in this file.
    fn emit_box_free(&mut self, ptr_reg: &str, ty: &Ty) {
        let Ty::Box(inner) = ty else { return };
        if let Ty::Box(_) = inner.as_ref() {
            let inner_ptr = self.fresh_reg("nested_box_inner");
            writeln!(self.out, "  {inner_ptr} = load ptr, ptr {ptr_reg}").unwrap();
            self.emit_box_free(&inner_ptr, inner);
        }
        writeln!(self.out, "  call void @nir_free(ptr {ptr_reg})").unwrap();
    }

    /// Emits `nir_free` (recursively, via `emit_box_free`) for every
    /// named binding in a `FreeMap` entry, looking each one up in the
    /// scope it's still live in. Callers are every scope-closing point
    /// `FreeMap`'s doc comment lists — always guarded by `!self.terminated`
    /// except `Stmt::Return`'s own call, which fires unconditionally since
    /// it's what's *about* to set `terminated`, not something already past
    /// it.
    fn emit_frees_for_names(&mut self, names: &[String], scopes: &Scopes) {
        for name in names {
            let Some((ty, stack_ptr)) = scopes.get(name) else {
                // Only reachable for a name whose owning scope already
                // popped by the time this runs — doesn't happen for any
                // `FreeMap` entry as constructed (every entry is recorded
                // strictly before its own scope's pop), but a silent
                // no-op is the honest response to "nothing left to free"
                // rather than a panic over an invariant this function
                // itself doesn't own.
                continue;
            };
            // `scopes.get` hands back the binding's own stack slot (the
            // same `ptr_reg` every other scalar-shaped value uses) — it
            // holds the box's heap pointer *value*, it isn't that value
            // itself. Load the actual heap address out before freeing it;
            // freeing the stack slot's own address would be a bug (and,
            // separately, the stack slot doesn't need freeing at all —
            // it's `entry_allocas`-hoisted and dies for free at return).
            let heap_ptr = self.fresh_reg(&format!("{name}.heap_ptr"));
            writeln!(self.out, "  {heap_ptr} = load ptr, ptr {stack_ptr}").unwrap();
            self.emit_box_free(&heap_ptr, &ty);
        }
    }

    /// The array-bounds analog of `guard_in_range` — checks `0 <= idx <
    /// dim` (same two-sided AND-combine shape), elided when `span` is
    /// already covered by `SmtReport::proven_index_bounds`. That set was
    /// already populated by both `refine.rs` (interval analysis) and
    /// `smt.rs` (real Z3) before this phase — codegen simply didn't have
    /// a check to elide it against yet (this fn is that consumer, the
    /// same relationship `guard_in_range` already has with
    /// `proven_in_range`).
    fn guard_index_in_bounds(&mut self, idx: &str, dim: usize, span: Span) -> Result<(), CodegenError> {
        if self.audited || self.smt_report.proven_index_bounds.contains(&span) {
            return Ok(());
        }
        let ok_lo = self.fresh_reg("idx_ge_lo");
        writeln!(self.out, "  {ok_lo} = icmp sge i64 {idx}, 0").unwrap();
        let ok_hi = self.fresh_reg("idx_lt_dim");
        writeln!(self.out, "  {ok_hi} = icmp slt i64 {idx}, {dim}").unwrap();
        let ok = self.fresh_reg("idx_in_bounds");
        writeln!(self.out, "  {ok} = and i1 {ok_lo}, {ok_hi}").unwrap();
        let pass = self.fresh_label("idx_ok");
        let fail = self.fresh_label("idx_trap");
        writeln!(self.out, "  br i1 {ok}, label %{pass}, label %{fail}").unwrap();
        writeln!(self.out, "{fail}:").unwrap();
        writeln!(self.out, "  call void @abort()").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        writeln!(self.out, "{pass}:").unwrap();
        Ok(())
    }

    fn while_loop(&mut self, span: Span, cond: &Expr, body: &Block, scopes: &mut Scopes) -> Result<(), CodegenError> {
        let cond_label = self.fresh_label("while_cond");
        let body_label = self.fresh_label("while_body");
        let after_label = self.fresh_label("while_after");

        writeln!(self.out, "  br label %{cond_label}").unwrap();
        writeln!(self.out, "{cond_label}:").unwrap();
        self.terminated = false;
        let c = self.expr(cond, scopes)?;
        writeln!(self.out, "  br i1 {c}, label %{body_label}, label %{after_label}").unwrap();

        writeln!(self.out, "{body_label}:").unwrap();
        self.terminated = false;
        scopes.push();
        self.stmts(&body.stmts, scopes)?;
        // Once per iteration (this IR runs every time around the loop),
        // not once total — a `box` allocated fresh each iteration reuses
        // the same hoisted stack slot (`entry_allocas`) but gets a brand
        // new heap block from `nir_alloc` every time, so the *previous*
        // iteration's block has to be freed here before it's overwritten,
        // or it leaks on every iteration but the last.
        if !self.terminated
            && let Some(names) = self.free_map.at_while_end.get(&span).cloned()
        {
            self.emit_frees_for_names(&names, scopes);
        }
        scopes.pop();
        if !self.terminated {
            writeln!(self.out, "  br label %{cond_label}").unwrap();
        }

        writeln!(self.out, "{after_label}:").unwrap();
        self.terminated = false;
        Ok(())
    }

    /// Evaluates `e`, returning an LLVM value operand (a register name
    /// like `%foo.3`, or a literal like `5`/`true`) ready to drop
    /// directly into another instruction.
    fn expr(&mut self, e: &Expr, scopes: &mut Scopes) -> Result<String, CodegenError> {
        match e {
            Expr::Int(n, _) => Ok(n.to_string()),
            Expr::Bool(b, _) => Ok(if *b { "true".to_string() } else { "false".to_string() }),
            // LLVM's own hex float literal format (`0x` + 16 hex digits
            // of the IEEE 754 binary64 bit pattern) — not a plain
            // decimal like `3.14`: a decimal literal only round-trips
            // exactly if LLVM's parser and Rust's `f64` formatter agree
            // on every rounding decision, which isn't guaranteed. The
            // bit pattern is unambiguous by construction.
            Expr::Float(f, _) => Ok(format!("0x{:016X}", f.to_bits())),
            Expr::Str(s, _) => {
                let bytes = s.as_bytes();
                let global = self.fresh_global("str");
                let escaped = llvm_escape_bytes(bytes);
                writeln!(
                    self.string_globals,
                    "{global} = private unnamed_addr constant [{} x i8] c\"{escaped}\\00\"",
                    bytes.len() + 1
                )
                .unwrap();
                // `{ptr, i64}` built via `insertvalue` from `undef` — a
                // first-class LLVM struct value, exactly like any other
                // `expr()` result, not routed through `expr_ptr()`'s
                // alloca-and-pointer convention (`Ty::Str` isn't
                // `is_aggregate()` — see `llvm_ty`'s note).
                let partial = self.fresh_reg("str_val");
                writeln!(self.out, "  {partial} = insertvalue {{ptr, i64}} undef, ptr {global}, 0").unwrap();
                let full = self.fresh_reg("str_val");
                writeln!(self.out, "  {full} = insertvalue {{ptr, i64}} {partial}, i64 {}, 1", bytes.len()).unwrap();
                Ok(full)
            }
            Expr::Ident(name, _) => {
                let (ty, ptr) = scopes.get(name).expect("typeck.rs already proved this resolves");
                if ty.is_aggregate() {
                    // Every well-behaved caller checks `is_aggregate()`
                    // first and calls `expr_ptr()` instead — this is a
                    // defense-in-depth guard against a construct this
                    // phase doesn't support routing correctly yet (e.g.
                    // an aggregate value inside an `if`/`while`
                    // expression's value slot), so it fails as a clean
                    // `CodegenError`, not a silently wrong `load` of an
                    // array through a scalar-typed instruction.
                    return unsupported(format!(
                        "codegen doesn't support using `{name}` (a Vector/Matrix) in this \
                         expression position yet — bind it via `let`, or pass/return it \
                         through a function call"
                    ));
                }
                let llty = llvm_ty(&ty)?;
                let reg = self.fresh_reg(&format!("{name}.val"));
                writeln!(self.out, "  {reg} = load {llty}, ptr {ptr}").unwrap();
                Ok(self.widen_to_i64(&reg, &ty))
            }
            Expr::Unary(UnOp::Neg, inner, _) => {
                // `inner` is already i64 (every integer-typed `expr()`
                // result is) — no need to consult its declared width at
                // all anymore, which used to be `local_ty_of`'s job here.
                // `f64` has no such invariant (its `expr()` result is
                // always genuinely `double`), so it's the one case here
                // that still needs `local_ty_of` to pick the right
                // instruction.
                let v = self.expr(inner, scopes)?;
                if self.local_ty_of(inner, scopes) == Ty::F64 {
                    let r = self.fresh_reg("fneg");
                    writeln!(self.out, "  {r} = fneg double {v}").unwrap();
                    return Ok(r);
                }
                let r = self.fresh_reg("neg");
                writeln!(self.out, "  {r} = sub i64 0, {v}").unwrap();
                Ok(r)
            }
            Expr::Unary(UnOp::Not, inner, _) => {
                let v = self.expr(inner, scopes)?;
                let r = self.fresh_reg("not");
                writeln!(self.out, "  {r} = xor i1 {v}, true").unwrap();
                Ok(r)
            }
            Expr::Binary(op, lhs, rhs, span) => self.binary(*op, lhs, rhs, *span, scopes),
            Expr::Call(name, args, _) => self.call(name, args, scopes),
            Expr::If { cond, then_block, else_block, span } => self.if_expr(cond, then_block, else_block.as_deref(), *span, scopes),
            Expr::Assign(name, rhs, span) => {
                let (ty, ptr) = scopes.get(name).expect("typeck.rs already proved this resolves");
                if ty.is_aggregate() {
                    // Same defense-in-depth reasoning as `Expr::Ident`
                    // above — every real caller (`Stmt::Expr`) already
                    // forks to `expr_ptr()`'s own `Expr::Assign` arm for
                    // an aggregate target before ever reaching here.
                    return unsupported(format!(
                        "codegen doesn't support assigning to `{name}` (a Vector/Matrix) in \
                         this expression position yet"
                    ));
                }
                let val = self.expr(rhs, scopes)?; // i64 (or i1 for bool)
                let val = self.guard_in_range(&val, &ty, *span)?; // checked at i64 width
                let store_val = if ty.is_integer() { self.narrow_from_i64(&val, &ty)? } else { val.clone() };
                let llty = llvm_ty(&ty)?;
                writeln!(self.out, "  store {llty} {store_val}, ptr {ptr}").unwrap();
                // `val` (still i64/i1), not `store_val` (narrow) — every
                // other `expr()` result is i64 for an integer type, and
                // an assignment-expression's own value has to match that
                // convention so a caller combining it further doesn't
                // need to know it came from an assignment specifically.
                Ok(val)
            }
            Expr::Spawn(_, _, _)
            | Expr::Join(_, _)
            | Expr::Chan(_)
            | Expr::SpawnSandbox(_, _, _)
            | Expr::Open(_, _, _)
            | Expr::Transact { .. }
            | Expr::FieldAccess(..)
            | Expr::Match { .. } => {
                unreachable!("check_supported already rejected this program")
            }
            // `box e` — heap-allocate `e`'s type's own byte size
            // (`ty_byte_size`, covers every `Ty` a `box` can wrap, not
            // just aggregates), copy `e`'s value in, return the heap
            // pointer as this expression's own value. `Ty::Box` is a
            // single pointer word (`llvm_ty`), so — like `Ty::Str` — it's
            // an ordinary `expr()` result, never routed through
            // `expr_ptr()`'s sret convention.
            //
            // **Deliberately leaks — no `free` here.** `ownership.rs`
            // already proves single ownership and last-use (see its
            // module doc), which is what a real free-insertion pass
            // would consume; that bridge doesn't exist in codegen yet.
            // Threading it through and emitting `nir_free` at last-use
            // is a later, separate phase — staged this way so the
            // allocation/deref/borrow half is independently reviewable
            // and testable first, not because this is an oversight.
            Expr::Box(inner, _) => {
                let inner_ty = self.local_ty_of(inner, scopes);
                let size = ty_byte_size(&inner_ty);
                let heap_ptr = self.fresh_reg("box_heap");
                writeln!(self.out, "  {heap_ptr} = call ptr @nir_alloc(i64 {size})").unwrap();
                if inner_ty.is_aggregate() {
                    let src = self.expr_ptr(inner, scopes)?;
                    writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {heap_ptr}, ptr {src}, i64 {size}, i1 false)").unwrap();
                } else {
                    let v = self.expr(inner, scopes)?;
                    let llty = llvm_ty(&inner_ty)?;
                    let v = if inner_ty.is_integer() { self.narrow_from_i64(&v, &inner_ty)? } else { v };
                    writeln!(self.out, "  store {llty} {v}, ptr {heap_ptr}").unwrap();
                }
                Ok(heap_ptr)
            }
            // `&x` — parser-enforced identifier-only (`typeck.rs` asserts
            // this), so codegen never has to evaluate an arbitrary
            // expression here: `x`'s own storage pointer (its `let`/param
            // alloca, or its own heap pointer if `x: box T`) already *is*
            // the reference's value. No new allocation, no copy, no load
            // — the cheapest possible case, exactly as cheap as the
            // language's own "not affine, freely copyable" treatment of
            // `Ty::Ref` implies it should be.
            Expr::Ref(inner, _) => match inner.as_ref() {
                Expr::Ident(name, _) => {
                    let (_, ptr) = scopes.get(name).expect("typeck.rs already proved this resolves");
                    Ok(ptr)
                }
                _ => unreachable!("parser only ever produces Expr::Ref with an Ident operand"),
            },
            // `*e` — `e`'s own type is always `Box`/`Ref` here
            // (`ownership.rs` already proved this typechecks), so `e`
            // itself is a plain pointer value fetched via `expr()` (never
            // `expr_ptr()` — `Ty::Box`/`Ty::Ref` are never
            // `is_aggregate()`). What's pointed to may or may not be an
            // aggregate; if it is, this arm hands back — this exact
            // pointer, unchanged, no copy — to whichever caller wants an
            // `expr_ptr()`-shaped result instead (see `expr_ptr`'s own
            // `Expr::Deref` arm), matching the interpreter's own
            // `Value::Boxed(inner) | Value::Ref(inner) => *inner`: a
            // dereference exposes the same storage, it doesn't clone it.
            Expr::Deref(inner, span) => {
                let ptr = self.expr(inner, scopes)?;
                let result_ty = self.local_ty_of(e, scopes);
                if result_ty.is_aggregate() {
                    return unsupported(format!(
                        "an aggregate-typed `*` result at {span:?} needs `expr_ptr()`'s \
                         pointer-returning path, not `expr()` — this indicates a caller bug, \
                         not a language-level limitation"
                    ));
                }
                let llty = llvm_ty(&result_ty)?;
                let reg = self.fresh_reg("deref_val");
                writeln!(self.out, "  {reg} = load {llty}, ptr {ptr}").unwrap();
                Ok(self.widen_to_i64(&reg, &result_ty))
            }
            Expr::Connect(host, port, _span) => {
                let (host_ptr, host_len) = self.str_parts(host, scopes)?;
                let port_v = self.expr(port, scopes)?;
                let fd = self.fresh_reg("tcp_connect_fd");
                writeln!(self.out, "  {fd} = call i64 @nir_tcp_connect(ptr {host_ptr}, i64 {host_len}, i64 {port_v})").unwrap();
                self.guard_io_ok(&fd);
                Ok(fd)
            }
            Expr::Listen(port, _span) => {
                let port_v = self.expr(port, scopes)?;
                let fd = self.fresh_reg("tcp_listen_fd");
                writeln!(self.out, "  {fd} = call i64 @nir_tcp_listen(i64 {port_v})").unwrap();
                self.guard_io_ok(&fd);
                Ok(fd)
            }
            Expr::Accept(listener, _span) => {
                let listener_fd = self.expr(listener, scopes)?;
                let fd = self.fresh_reg("tcp_accept_fd");
                writeln!(self.out, "  {fd} = call i64 @nir_tcp_accept(i64 {listener_fd})").unwrap();
                self.guard_io_ok(&fd);
                Ok(fd)
            }
            // `send`/`recv` share one AST node with `chan`'s I/O — a
            // `Ty::Channel` operand is still unsupported (Phase D2); only
            // the `Ty::Tcp` case has real codegen here (Phase B1's whole
            // scope). `check_expr`'s structural pre-pass can't tell the
            // two apart (no type info), so this — the one place that can
            // see `local_ty_of` — is where the real, type-directed
            // rejection lives, same "type-oblivious pre-pass, real check
            // at IR-gen time" precedent `print`'s aggregate rejection
            // already established (module doc).
            Expr::Send(target, value, _span) => match self.local_ty_of(target, scopes) {
                Ty::Tcp => {
                    let fd = self.expr(target, scopes)?;
                    let (ptr, len) = self.str_parts(value, scopes)?;
                    let n = self.fresh_reg("tcp_send_n");
                    writeln!(self.out, "  {n} = call i64 @nir_tcp_send(i64 {fd}, ptr {ptr}, i64 {len})").unwrap();
                    self.guard_io_ok(&n);
                    Ok("0".to_string()) // send's own value is unit; never read
                }
                Ty::Channel(_) => unsupported("codegen doesn't support `chan` yet — interpreter-only for now"),
                _ => unreachable!("typeck.rs already restricted send's first operand to tcp/chan"),
            },
            Expr::Recv(target, _span) => match self.local_ty_of(target, scopes) {
                Ty::Tcp => {
                    let fd = self.expr(target, scopes)?;
                    // One read syscall into a fixed 64KiB buffer, matching
                    // `interpreter.rs`'s `read_tcp` exactly (module doc's
                    // "one chunk, not a message boundary" scope note) —
                    // entry-block-hoisted like every other alloca in this
                    // file, not heap-allocated (`box`'s allocator doesn't
                    // exist yet, and isn't needed here: the buffer's
                    // lifetime is exactly this expression's).
                    let buf = self.fresh_reg("tcp_recv_buf");
                    self.emit_alloca(&buf, "[65536 x i8]");
                    let buf_ptr = self.fresh_reg("tcp_recv_ptr");
                    writeln!(self.out, "  {buf_ptr} = getelementptr [65536 x i8], ptr {buf}, i64 0, i64 0").unwrap();
                    let n = self.fresh_reg("tcp_recv_n");
                    writeln!(self.out, "  {n} = call i64 @nir_tcp_recv(i64 {fd}, ptr {buf_ptr}, i64 65536)").unwrap();
                    // `0` (peer closed) is an error here too, exactly like
                    // `read_tcp`'s `n == 0` check — not a valid empty
                    // read, so it shares the same negative-or-zero trap as
                    // a real I/O failure (`guard_io_ok` only special-cases
                    // "negative", so recv needs its own explicit `<= 0`
                    // check instead of reusing it verbatim).
                    self.guard_recv_ok(&n);
                    let partial = self.fresh_reg("tcp_recv_str");
                    writeln!(self.out, "  {partial} = insertvalue {{ptr, i64}} undef, ptr {buf_ptr}, 0").unwrap();
                    let full = self.fresh_reg("tcp_recv_str");
                    writeln!(self.out, "  {full} = insertvalue {{ptr, i64}} {partial}, i64 {n}, 1").unwrap();
                    Ok(full)
                }
                Ty::Channel(_) => unsupported("codegen doesn't support `chan` yet — interpreter-only for now"),
                _ => unreachable!("typeck.rs already restricted recv's operand to tcp/chan"),
            },
            // `stop` shares one AST node across `sandbox`/`tcp`/
            // `tcp_listener` — `sandbox` is still unsupported (Phase E),
            // `tcp`/`tcp_listener` both close via the same `nir_tcp_stop`
            // (a plain fd close serves either uniformly).
            Expr::StopSandbox(inner, _span) => match self.local_ty_of(inner, scopes) {
                Ty::Tcp | Ty::TcpListener => {
                    let fd = self.expr(inner, scopes)?;
                    writeln!(self.out, "  call i32 @nir_tcp_stop(i64 {fd})").unwrap();
                    Ok("0".to_string()) // stop's own value is unit for tcp/tcp_listener; never read
                }
                Ty::Sandbox => unsupported("codegen doesn't support `sandbox` yet — interpreter-only for now"),
                _ => unreachable!("typeck.rs already restricted stop's operand to sandbox/tcp/tcp_listener"),
            },
            // `v[i]` / `m[i, j]` — always yields a scalar element, so this
            // belongs in `expr()`, not `expr_ptr()`, even though the base
            // is an aggregate reached via `expr_ptr()`. Indices are
            // arbitrary runtime integer expressions (`typeck.rs`'s
            // `Expr::Index` arm only checks `is_integer()`, no literal
            // restriction) — real `getelementptr` with a runtime offset,
            // not an unroll-only shortcut, matching the plan's design
            // decision 4.
            Expr::Index(base, indices, span) => {
                let base_ty = self.local_ty_of(base, scopes);
                let base_ptr = self.expr_ptr(base, scopes)?;
                let (elem, offset) = match &base_ty {
                    Ty::Vector(elem, n) => {
                        let idx = self.expr(&indices[0], scopes)?;
                        self.guard_index_in_bounds(&idx, *n, *span)?;
                        ((**elem).clone(), idx)
                    }
                    Ty::Matrix(elem, r, c) => {
                        let i = self.expr(&indices[0], scopes)?;
                        self.guard_index_in_bounds(&i, *r, *span)?;
                        let j = self.expr(&indices[1], scopes)?;
                        self.guard_index_in_bounds(&j, *c, *span)?;
                        // Row-major flat offset `i*C + j` — matching
                        // `interpreter.rs`'s `Value::Matrix` layout
                        // exactly (module doc / design decision 1).
                        let scaled = self.fresh_reg("idx_row_scaled");
                        writeln!(self.out, "  {scaled} = mul i64 {i}, {c}").unwrap();
                        let flat = self.fresh_reg("idx_flat");
                        writeln!(self.out, "  {flat} = add i64 {scaled}, {j}").unwrap();
                        ((**elem).clone(), flat)
                    }
                    _ => unreachable!("typeck.rs already proved this is indexable (Vector/Matrix only)"),
                };
                let elem_llty = llvm_ty(&elem)?;
                let gep = self.fresh_reg("idx_gep");
                writeln!(self.out, "  {gep} = getelementptr {elem_llty}, ptr {base_ptr}, i64 {offset}").unwrap();
                let loaded = self.fresh_reg("idx_val");
                writeln!(self.out, "  {loaded} = load {elem_llty}, ptr {gep}").unwrap();
                Ok(self.widen_to_i64(&loaded, &elem))
            }
            // Genuinely reachable now that `check_supported` accepts
            // `ArrayLit` (this phase) — but only ever in a scalar
            // expression context, since `array_lit_ty` always types it
            // `Vector`/`Matrix`. A real, if narrow, unsupported case
            // (e.g. as an `if` expression's value slot) rather than a
            // dead catch-all, so it has to fail cleanly, not panic.
            Expr::ArrayLit(_, _) => unsupported(
                "codegen doesn't support a Vector/Matrix literal in this expression position \
                 yet — bind it via `let`, or pass/return it through a function call",
            ),
        }
    }

    fn call(&mut self, name: &str, args: &[Expr], scopes: &mut Scopes) -> Result<String, CodegenError> {
        // `check_supported` (`check_expr`'s `Expr::Call` arm, above)
        // already rejected every builtin except `print` before this ever
        // runs -- `== "print"` here, not `is_builtin`, states that
        // invariant explicitly rather than leaning on it silently.
        if name == "print" {
            for a in args {
                // `local_ty_of` picks the right `printf` format
                // string/vararg type: genuinely `double` for `f64`,
                // `i64` for a plain integer expression (every integer-
                // typed `expr()` result is, by construction — module
                // doc) -- but a *comparison* (`x > y`) is neither: its
                // `expr()` result is `i1`, not `i64`, even though
                // `is_printable_expr` (a syntactic "not a bool literal"
                // check, not real type inference) lets it through. A
                // real, pre-existing gap this phase's own `is_float`
                // dispatch is the natural place to close alongside it --
                // `zext` the `i1` up to `i64` and print it as 0/1
                // (`interpreter.rs`'s `render()` prints "true"/"false"
                // for a real `bool` value; this is an honest, narrow
                // cosmetic difference for the one case -- a comparison
                // *result* printed directly -- no existing example
                // exercises either way).
                let arg_ty = self.local_ty_of(a, scopes);
                if arg_ty.is_aggregate() {
                    // Printing a whole Vector/Matrix isn't built yet —
                    // `is_printable_expr` (a purely syntactic "not a
                    // bool literal" check) doesn't know the arg's type,
                    // so this has to be caught here instead, with a
                    // specific reason rather than falling through to a
                    // scalar `expr()` call that would fail confusingly.
                    return unsupported(
                        "codegen doesn't support `print` on a Vector/Matrix argument yet — \
                         only integer/f64-typed arguments are supported so far",
                    );
                }
                let v = self.expr(a, scopes)?;
                if arg_ty == Ty::F64 {
                    writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.float_fmt, double {v})").unwrap();
                } else if arg_ty == Ty::Str {
                    // `%.*s`, not `%s` — the buffer isn't guaranteed
                    // NUL-terminated by this design (`Ty::Str`'s note in
                    // `llvm_ty`), so the explicit length has to drive how
                    // many bytes `printf` reads, not a NUL scan.
                    let ptr_reg = self.fresh_reg("str_print_ptr");
                    writeln!(self.out, "  {ptr_reg} = extractvalue {{ptr, i64}} {v}, 0").unwrap();
                    let len_reg = self.fresh_reg("str_print_len");
                    writeln!(self.out, "  {len_reg} = extractvalue {{ptr, i64}} {v}, 1").unwrap();
                    let len_i32 = self.fresh_reg("str_print_len_i32");
                    writeln!(self.out, "  {len_i32} = trunc i64 {len_reg} to i32").unwrap();
                    writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.str_fmt, i32 {len_i32}, ptr {ptr_reg})").unwrap();
                } else if arg_ty == Ty::Bool {
                    let widened = self.fresh_reg("bool_as_i64");
                    writeln!(self.out, "  {widened} = zext i1 {v} to i64").unwrap();
                    writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.int_fmt, i64 {widened})").unwrap();
                } else {
                    writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.int_fmt, i64 {v})").unwrap();
                }
            }
            return Ok("0".to_string()); // print's own "value" is unit; never read
        }
        if PHASE4_BUILTINS.contains(&name) || name == "det" || name == "rank" {
            return self.call_builtin_scalar(name, args, scopes);
        }
        // User-defined call. `typeck.rs` already required this to
        // resolve and every argument to either exactly match or (for a
        // literal) fit its parameter's declared type — the sigs table
        // is what lets codegen honor that at the LLVM level too, where
        // a call instruction's argument types must match the callee's
        // `define` exactly, byte for byte.
        let sig_params = self.sigs.get(name).expect("typeck.rs already resolved this call").params.clone();
        let sig_ret = self.sigs.get(name).expect("typeck.rs already resolved this call").ret.clone();
        if sig_ret.is_aggregate() {
            // Every well-behaved caller already checked the callee's
            // return type and used `expr_ptr()` (→ `call_ptr()`)
            // instead — same defense-in-depth shape as the `Expr::Ident`/
            // `Expr::Assign` guards above.
            return unsupported(format!(
                "codegen doesn't support calling `{name}` (which returns a Vector/Matrix) in \
                 this expression position yet"
            ));
        }

        let arg_vals = self.call_args(args, &sig_params, scopes)?;

        let ret_llty = llvm_ty(&sig_ret)?;
        if ret_llty == "void" {
            writeln!(self.out, "  call void @{name}({})", arg_vals.join(", ")).unwrap();
            Ok("0".to_string()) // unit result; never read by a well-typed caller
        } else {
            let r = self.fresh_reg("call_result");
            writeln!(self.out, "  {r} = call {ret_llty} @{name}({})", arg_vals.join(", ")).unwrap();
            // The call instruction itself is correctly typed at the
            // callee's *declared* return width (LLVM requires that) —
            // but every other `expr()` result for an integer type is
            // `i64` (module doc), and this one has to honor that same
            // invariant too, or a caller like `Stmt::Let`'s
            // `guard_in_range` (which always compares at `i64`) sees a
            // value narrower than it expects. Found the same way as the
            // `add i8` wraparound bug: by actually building `hello.nir`
            // after the i64-everywhere fix landed and reading clang's
            // "defined with type 'i32' but expected 'i64'" error, not by
            // re-reading the code and reasoning it through in advance.
            Ok(self.widen_to_i64(&r, &sig_ret))
        }
    }

    /// Shared argument-evaluation loop for both `call()` (scalar/void
    /// return) and `call_ptr()` (aggregate return) — every argument's
    /// handling depends only on its own declared parameter type, never
    /// on the callee's return type, so there's exactly one place this
    /// logic needs to live. Args are evaluated left to right, matching
    /// `interpreter.rs`'s evaluation order.
    fn call_args(&mut self, args: &[Expr], sig_params: &[Ty], scopes: &mut Scopes) -> Result<Vec<String>, CodegenError> {
        let mut arg_vals = Vec::with_capacity(args.len());
        for (a, want) in args.iter().zip(sig_params.iter()) {
            if want.is_aggregate() {
                // Passed by pointer — no copy at the call site itself;
                // the callee's own prologue does the copy-in (`function`'s
                // doc comment), so the caller can hand over whichever
                // pointer `expr_ptr` already has (a variable's own
                // storage, or a fresh literal's).
                let ptr = self.expr_ptr(a, scopes)?;
                arg_vals.push(format!("ptr {ptr}"));
                continue;
            }
            let llty = llvm_ty(want)?;
            let v = if let Some(n) = literal_value(a) {
                // A literal (or negated literal) that typeck already
                // proved fits `want`'s range — emit it directly at
                // `want`'s width as a bare constant, no instruction
                // needed (this is the fix for the bug found by actually
                // inspecting the first generated .ll file: an earlier
                // draft ran `-3` through a real `sub i64 0, 3` and then
                // tried to pass the resulting *i64* register where an
                // `i32` parameter was declared — a genuine LLVM type
                // mismatch, not a hypothetical one).
                n.to_string()
            } else {
                // Not a literal — typeck's exact-match rule guarantees
                // this expression's own *declared* Nirdosha type already
                // equals `want`, but `expr()` itself always hands back an
                // `i64` for any integer type (module doc), so it still
                // needs narrowing to `want`'s actual LLVM width before
                // it can be passed at a call site. Lossless: the value
                // was already proven to fit `want` when it was originally
                // bound (that's what `guard_in_range` did at its own
                // `let`/assign site) — this narrow can't newly overflow.
                let val64 = self.expr(a, scopes)?;
                if want.is_integer() {
                    self.narrow_from_i64(&val64, want)?
                } else {
                    val64
                }
            };
            arg_vals.push(format!("{llty} {v}"));
        }
        Ok(arg_vals)
    }

    /// Every Phase-4 builtin that yields a plain scalar (`f64`/`i64`/
    /// `bool`) result — reached from `call()`'s `expr()` path (aggregate-
    /// returning builtins are `call_builtin_agg`'s job instead). Each
    /// mirrors its `interpreter.rs` implementation's exact accumulation
    /// order: "first term computed directly, then each remaining term
    /// folded in left-to-right" is bit-identical to `interpreter.rs`'s
    /// own `iter().sum()`/`.fold(0.0, ...)`-based versions specifically
    /// *because* `0.0 + x == x` and `f64::max(0.0, |x|) == |x|` are both
    /// exact (no rounding) for any finite `x` — not a coincidence this
    /// phase leans on, a real IEEE 754 identity.
    fn call_builtin_scalar(&mut self, name: &str, args: &[Expr], scopes: &mut Scopes) -> Result<String, CodegenError> {
        match name {
            "dot" => {
                let a_ty = self.local_ty_of(&args[0], scopes);
                let (elem, len) = agg_elem_and_len(&a_ty);
                let (elem, len) = (elem.clone(), len);
                let elem_llty = llvm_ty(&elem)?;
                let is_float = elem == Ty::F64;
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let b_ptr = self.expr_ptr(&args[1], scopes)?;
                let a0 = self.agg_load_elem(&a_ptr, &elem_llty, &elem, 0);
                let b0 = self.agg_load_elem(&b_ptr, &elem_llty, &elem, 0);
                let mut acc = self.emit_mul(&a0, &b0, is_float);
                for i in 1..len {
                    let ai = self.agg_load_elem(&a_ptr, &elem_llty, &elem, i);
                    let bi = self.agg_load_elem(&b_ptr, &elem_llty, &elem, i);
                    let prod = self.emit_mul(&ai, &bi, is_float);
                    acc = self.emit_add(&acc, &prod, is_float);
                }
                Ok(acc)
            }
            "sum" => {
                let a_ty = self.local_ty_of(&args[0], scopes);
                let (elem, len) = agg_elem_and_len(&a_ty);
                let (elem, len) = (elem.clone(), len);
                let elem_llty = llvm_ty(&elem)?;
                let is_float = elem == Ty::F64;
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let mut acc = self.agg_load_elem(&a_ptr, &elem_llty, &elem, 0);
                for i in 1..len {
                    let v = self.agg_load_elem(&a_ptr, &elem_llty, &elem, i);
                    acc = self.emit_add(&acc, &v, is_float);
                }
                Ok(acc)
            }
            "len" => {
                let Ty::Vector(_, n) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a Vector")
                };
                // `len` is genuinely O(1): a `Vector`'s length is baked
                // into its `Ty`, known at codegen time -- no load at all,
                // unlike every other builtin here.
                Ok(n.to_string())
            }
            "norm" | "frobenius_norm" => {
                let (_, len) = agg_elem_and_len(&self.local_ty_of(&args[0], scopes));
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let x0 = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 0);
                let mut sum_sq = self.emit_mul(&x0, &x0, true);
                for i in 1..len {
                    let xi = self.agg_load_elem(&a_ptr, "double", &Ty::F64, i);
                    let sq = self.emit_mul(&xi, &xi, true);
                    sum_sq = self.emit_add(&sum_sq, &sq, true);
                }
                Ok(self.emit_call1("@llvm.sqrt.f64", &sum_sq))
            }
            "norm1" => {
                let (_, len) = agg_elem_and_len(&self.local_ty_of(&args[0], scopes));
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let x0 = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 0);
                let mut acc = self.emit_call1("@llvm.fabs.f64", &x0);
                for i in 1..len {
                    let xi = self.agg_load_elem(&a_ptr, "double", &Ty::F64, i);
                    let abs_xi = self.emit_call1("@llvm.fabs.f64", &xi);
                    acc = self.emit_add(&acc, &abs_xi, true);
                }
                Ok(acc)
            }
            "norm_inf" => {
                let (_, len) = agg_elem_and_len(&self.local_ty_of(&args[0], scopes));
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let x0 = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 0);
                let mut acc = self.emit_call1("@llvm.fabs.f64", &x0);
                for i in 1..len {
                    let xi = self.agg_load_elem(&a_ptr, "double", &Ty::F64, i);
                    let abs_xi = self.emit_call1("@llvm.fabs.f64", &xi);
                    acc = self.emit_call2("@llvm.maxnum.f64", &acc, &abs_xi);
                }
                Ok(acc)
            }
            "trace" => {
                let Ty::Matrix(elem, n, _) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a square Matrix")
                };
                let elem_llty = llvm_ty(&elem)?;
                let is_float = *elem == Ty::F64;
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let mut acc = self.agg_load_elem(&a_ptr, &elem_llty, &elem, 0);
                for i in 1..n {
                    let v = self.agg_load_elem(&a_ptr, &elem_llty, &elem, i * n + i);
                    acc = self.emit_add(&acc, &v, is_float);
                }
                Ok(acc)
            }
            "distance" => {
                let (_, len) = agg_elem_and_len(&self.local_ty_of(&args[0], scopes));
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let b_ptr = self.expr_ptr(&args[1], scopes)?;
                let a0 = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 0);
                let b0 = self.agg_load_elem(&b_ptr, "double", &Ty::F64, 0);
                let d0 = self.emit_sub(&a0, &b0);
                let mut sum_sq = self.emit_mul(&d0, &d0, true);
                for i in 1..len {
                    let ai = self.agg_load_elem(&a_ptr, "double", &Ty::F64, i);
                    let bi = self.agg_load_elem(&b_ptr, "double", &Ty::F64, i);
                    let di = self.emit_sub(&ai, &bi);
                    let sq = self.emit_mul(&di, &di, true);
                    sum_sq = self.emit_add(&sum_sq, &sq, true);
                }
                Ok(self.emit_call1("@llvm.sqrt.f64", &sum_sq))
            }
            "bearing" => {
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let b_ptr = self.expr_ptr(&args[1], scopes)?;
                let (lat1, lon1, lat2, lon2, deg) = self.bearing_deg(&a_ptr, &b_ptr);
                let _ = (lat1, lon1, lat2, lon2);
                Ok(deg)
            }
            "is_symmetric" | "is_diag" => {
                let Ty::Matrix(_, n, _) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a square Matrix(f64,_,_)")
                };
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let mut acc: Option<String> = None;
                for i in 0..n {
                    for j in 0..n {
                        // `is_diag`'s `i == j` cells are trivially true
                        // (`interpreter.rs`'s own `i == j ||` short-
                        // circuit) — a compile-time-known fact per
                        // unrolled iteration here, so skip emitting any
                        // comparison for them at all rather than
                        // computing `elems[i*n+i] == elems[i*n+i]`.
                        if name == "is_diag" && i == j {
                            continue;
                        }
                        let lhs = self.agg_load_elem(&a_ptr, "double", &Ty::F64, i * n + j);
                        let rhs = if name == "is_symmetric" {
                            self.agg_load_elem(&a_ptr, "double", &Ty::F64, j * n + i)
                        } else {
                            Self::float_const(0.0)
                        };
                        let eq = self.fcmp("oeq", &lhs, &rhs)?;
                        acc = Some(match acc {
                            None => eq,
                            Some(prev) => {
                                let out = self.fresh_reg("builtin_and");
                                writeln!(self.out, "  {out} = and i1 {prev}, {eq}").unwrap();
                                out
                            }
                        });
                    }
                }
                // A 1x1 Matrix is both symmetric and diagonal trivially --
                // `is_diag`'s loop skips its only (i==j) cell entirely, so
                // `acc` would otherwise stay `None`; `is_symmetric`'s only
                // cell compares `elems[0]` to itself, always `Some`. Both
                // are correctly `true` either way.
                Ok(acc.unwrap_or_else(|| "true".to_string()))
            }
            "is_square" => {
                let Ty::Matrix(_, r, c) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a Matrix")
                };
                // Genuinely O(1): a `Matrix`'s shape is baked into its
                // `Ty`, so this is decidable at codegen time -- no load,
                // no comparison instruction, unlike every other builtin
                // here.
                Ok(if r == c { "true".to_string() } else { "false".to_string() })
            }
            // Phase 5: genuine data-dependent control flow (partial-pivot
            // row selection) — a linked native `call` into
            // `runtime_kernels.rs`'s staticlib instead of unrolled IR
            // (module doc's `PHASE5_BUILTINS` note, and `build()`'s
            // embedded-lib linking). Neither is fallible: `det` returns
            // `0.0` for a singular matrix (a real, legitimate answer,
            // matching `interpreter.rs::matrix_det`'s own contract) and
            // `rank`'s row-echelon reduction never fails outright.
            "det" => {
                let Ty::Matrix(_, n, _) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a square Matrix(f64,_,_)")
                };
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let out = self.fresh_reg("det_result");
                writeln!(self.out, "  {out} = call double @nir_det(ptr {a_ptr}, i64 {n})").unwrap();
                Ok(out)
            }
            "rank" => {
                let Ty::Matrix(_, rows, cols) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a Matrix(f64,_,_)")
                };
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let out = self.fresh_reg("rank_result");
                writeln!(self.out, "  {out} = call i64 @nir_rank(ptr {a_ptr}, i64 {rows}, i64 {cols})").unwrap();
                Ok(out)
            }
            _ => unreachable!("PHASE4_BUILTINS'/PHASE5_BUILTINS' aggregate-returning names go through call_builtin_agg instead"),
        }
    }

    /// `bearing`'s formula, factored out so `call_builtin_scalar`'s match
    /// arm stays short — returns `(lat1_rad, lon1_rad, lat2_rad, lon2_rad,
    /// result_deg)`; only the last is actually used by `bearing` itself,
    /// but returning the intermediates avoids a second, pointless
    /// abstraction boundary for a function with exactly one real caller.
    /// Mirrors `interpreter.rs::bearing_deg` exactly, including its
    /// `(deg + 360.0) % 360.0` final wrap (`frem`, IEEE remainder --
    /// matching Rust's `f64::rem` semantics `%` already uses there).
    fn bearing_deg(&mut self, from_ptr: &str, to_ptr: &str) -> (String, String, String, String, String) {
        let deg_to_rad = Self::float_const(std::f64::consts::PI / 180.0);
        let rad_to_deg = Self::float_const(180.0 / std::f64::consts::PI);

        let lat1_deg = self.agg_load_elem(from_ptr, "double", &Ty::F64, 0);
        let lon1_deg = self.agg_load_elem(from_ptr, "double", &Ty::F64, 1);
        let lat2_deg = self.agg_load_elem(to_ptr, "double", &Ty::F64, 0);
        let lon2_deg = self.agg_load_elem(to_ptr, "double", &Ty::F64, 1);
        let lat1 = self.emit_mul(&lat1_deg, &deg_to_rad, true);
        let lon1 = self.emit_mul(&lon1_deg, &deg_to_rad, true);
        let lat2 = self.emit_mul(&lat2_deg, &deg_to_rad, true);
        let lon2 = self.emit_mul(&lon2_deg, &deg_to_rad, true);
        let dlon = self.emit_sub(&lon2, &lon1);

        let sin_dlon = self.emit_call1("@llvm.sin.f64", &dlon);
        let cos_lat2 = self.emit_call1("@llvm.cos.f64", &lat2);
        let y = self.emit_mul(&sin_dlon, &cos_lat2, true);

        let cos_lat1 = self.emit_call1("@llvm.cos.f64", &lat1);
        let sin_lat2 = self.emit_call1("@llvm.sin.f64", &lat2);
        let term1 = self.emit_mul(&cos_lat1, &sin_lat2, true);
        let sin_lat1 = self.emit_call1("@llvm.sin.f64", &lat1);
        let cos_dlon = self.emit_call1("@llvm.cos.f64", &dlon);
        let t2a = self.emit_mul(&sin_lat1, &cos_lat2, true);
        let term2 = self.emit_mul(&t2a, &cos_dlon, true);
        let x = self.emit_sub(&term1, &term2);

        let ang = self.emit_call2("@atan2", &y, &x);
        let deg = self.emit_mul(&ang, &rad_to_deg, true);
        let plus_360 = self.emit_add(&deg, &Self::float_const(360.0), true);
        let wrapped = self.fresh_reg("bearing_wrap");
        writeln!(self.out, "  {wrapped} = frem double {plus_360}, {}", Self::float_const(360.0)).unwrap();
        (lat1, lon1, lat2, lon2, wrapped)
    }

    /// Every Phase-4 builtin that yields a `Vector`/`Matrix` result —
    /// reached from `call_ptr()`'s `expr_ptr()` path. `builtin_result_ty`
    /// picks the destination's shape up front (the same function
    /// `local_ty_of` uses), so every arm below just fills it in.
    fn call_builtin_agg(&mut self, name: &str, args: &[Expr], scopes: &mut Scopes) -> Result<String, CodegenError> {
        let result_ty = self.builtin_result_ty(name, args, scopes);
        let agg_llty = llvm_ty(&result_ty)?;
        let dest = self.fresh_reg("builtin_result.addr");
        self.emit_alloca(&dest, &agg_llty);

        match name {
            "transpose" => {
                let Ty::Matrix(elem, rows, cols) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a Matrix")
                };
                let elem_llty = llvm_ty(&elem)?;
                let m_ptr = self.expr_ptr(&args[0], scopes)?;
                // Matches interpreter.rs: `for j in 0..cols { for i in
                // 0..rows { out.push(elems[i*cols+j]) } }` -- output is
                // row-major over the (cols, rows) transposed shape, so
                // out[j*rows+i] = elems[i*cols+j].
                for j in 0..cols {
                    for i in 0..rows {
                        let v = self.agg_load_elem(&m_ptr, &elem_llty, &elem, i * cols + j);
                        self.agg_store_elem(&dest, &elem_llty, &elem, j * rows + i, &v)?;
                    }
                }
            }
            "cross" => {
                let a_ty = self.local_ty_of(&args[0], scopes);
                let (elem, _) = agg_elem_and_len(&a_ty);
                let elem = elem.clone();
                let elem_llty = llvm_ty(&elem)?;
                let is_float = elem == Ty::F64;
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let b_ptr = self.expr_ptr(&args[1], scopes)?;
                // Matches interpreter.rs's three `term(i,j,k,l) = a[i]*b[j]
                // - a[k]*b[l]` calls exactly.
                for (out_i, (i, j, k, l)) in [(1usize, 2usize, 2usize, 1usize), (2, 0, 0, 2), (0, 1, 1, 0)].into_iter().enumerate() {
                    let ai = self.agg_load_elem(&a_ptr, &elem_llty, &elem, i);
                    let bj = self.agg_load_elem(&b_ptr, &elem_llty, &elem, j);
                    let p1 = self.emit_mul(&ai, &bj, is_float);
                    let ak = self.agg_load_elem(&a_ptr, &elem_llty, &elem, k);
                    let bl = self.agg_load_elem(&b_ptr, &elem_llty, &elem, l);
                    let p2 = self.emit_mul(&ak, &bl, is_float);
                    let c = if is_float {
                        self.emit_sub(&p1, &p2)
                    } else {
                        let out = self.fresh_reg("agg_sub");
                        writeln!(self.out, "  {out} = sub i64 {p1}, {p2}").unwrap();
                        out
                    };
                    self.agg_store_elem(&dest, &elem_llty, &elem, out_i, &c)?;
                }
            }
            "zeros" | "ones" => {
                let fill = Self::float_const(if name == "zeros" { 0.0 } else { 1.0 });
                let (_, len) = agg_elem_and_len(&result_ty);
                for i in 0..len {
                    self.agg_store_elem(&dest, "double", &Ty::F64, i, &fill)?;
                }
            }
            "identity" => {
                let Ty::Matrix(_, n, _) = result_ty else { unreachable!("builtin_result_ty always returns Matrix for identity") };
                let zero = Self::float_const(0.0);
                let one = Self::float_const(1.0);
                for i in 0..n {
                    for j in 0..n {
                        let v = if i == j { &one } else { &zero };
                        self.agg_store_elem(&dest, "double", &Ty::F64, i * n + j, v)?;
                    }
                }
            }
            "lla_to_ecef" => {
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let lat_deg = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 0);
                let lon_deg = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 1);
                let alt = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 2);
                let (x, y, z) = self.lla_to_ecef_vals(&lat_deg, &lon_deg, &alt);
                self.agg_store_elem(&dest, "double", &Ty::F64, 0, &x)?;
                self.agg_store_elem(&dest, "double", &Ty::F64, 1, &y)?;
                self.agg_store_elem(&dest, "double", &Ty::F64, 2, &z)?;
            }
            "ecef_to_lla" => {
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let x = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 0);
                let y = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 1);
                let z = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 2);
                let (lat, lon, alt) = self.ecef_to_lla_vals(&x, &y, &z);
                self.agg_store_elem(&dest, "double", &Ty::F64, 0, &lat)?;
                self.agg_store_elem(&dest, "double", &Ty::F64, 1, &lon)?;
                self.agg_store_elem(&dest, "double", &Ty::F64, 2, &alt)?;
            }
            "ecef_to_enu" | "enu_to_ecef" => {
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let ref_ptr = self.expr_ptr(&args[1], scopes)?;
                let a0 = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 0);
                let a1 = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 1);
                let a2 = self.agg_load_elem(&a_ptr, "double", &Ty::F64, 2);
                let ref_lat_deg = self.agg_load_elem(&ref_ptr, "double", &Ty::F64, 0);
                let ref_lon_deg = self.agg_load_elem(&ref_ptr, "double", &Ty::F64, 1);
                let ref_alt = self.agg_load_elem(&ref_ptr, "double", &Ty::F64, 2);
                let (ref_x, ref_y, ref_z) = self.lla_to_ecef_vals(&ref_lat_deg, &ref_lon_deg, &ref_alt);
                let deg_to_rad = Self::float_const(std::f64::consts::PI / 180.0);
                let ref_lat = self.emit_mul(&ref_lat_deg, &deg_to_rad, true);
                let ref_lon = self.emit_mul(&ref_lon_deg, &deg_to_rad, true);
                let r = self.enu_rotation_vals(&ref_lat, &ref_lon);

                let (out0, out1, out2) = if name == "ecef_to_enu" {
                    let d0 = self.emit_sub(&a0, &ref_x);
                    let d1 = self.emit_sub(&a1, &ref_y);
                    let d2 = self.emit_sub(&a2, &ref_z);
                    let d = [d0, d1, d2];
                    let mut outs: Vec<String> = Vec::with_capacity(3);
                    for k in 0..3 {
                        let t0 = self.emit_mul(&r[k * 3], &d[0], true);
                        let t1 = self.emit_mul(&r[k * 3 + 1], &d[1], true);
                        let t2 = self.emit_mul(&r[k * 3 + 2], &d[2], true);
                        let s01 = self.emit_add(&t0, &t1, true);
                        outs.push(self.emit_add(&s01, &t2, true));
                    }
                    (outs[0].clone(), outs[1].clone(), outs[2].clone())
                } else {
                    let enu = [a0, a1, a2];
                    let mut d: Vec<String> = Vec::with_capacity(3);
                    for j in 0..3 {
                        let t0 = self.emit_mul(&r[j], &enu[0], true);
                        let t1 = self.emit_mul(&r[3 + j], &enu[1], true);
                        let t2 = self.emit_mul(&r[6 + j], &enu[2], true);
                        let s01 = self.emit_add(&t0, &t1, true);
                        d.push(self.emit_add(&s01, &t2, true));
                    }
                    (self.emit_add(&ref_x, &d[0], true), self.emit_add(&ref_y, &d[1], true), self.emit_add(&ref_z, &d[2], true))
                };
                self.agg_store_elem(&dest, "double", &Ty::F64, 0, &out0)?;
                self.agg_store_elem(&dest, "double", &Ty::F64, 1, &out1)?;
                self.agg_store_elem(&dest, "double", &Ty::F64, 2, &out2)?;
            }
            "kf_predict_state" | "kf_predict_cov" => {
                // All four args evaluated left to right regardless of
                // which this specific half actually needs, matching the
                // interpreter's own call-argument evaluation (every
                // `Expr::Call` argument is evaluated before
                // `eval_builtin` dispatches on `name`, independent of
                // which arm ends up using which value).
                let x_ptr = self.expr_ptr(&args[0], scopes)?;
                let p_ptr = self.expr_ptr(&args[1], scopes)?;
                let f_ptr = self.expr_ptr(&args[2], scopes)?;
                let q_ptr = self.expr_ptr(&args[3], scopes)?;
                let Ty::Vector(_, n) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved x is Vector(f64,n)")
                };
                if name == "kf_predict_state" {
                    // `x' = F x` -- `P`/`Q` unused here, matching
                    // `interpreter.rs::kf_predict`'s own `x_new`
                    // computation exactly.
                    let x_new = self.mat_vec_mul_ptr_vals(&f_ptr, n, n, &x_ptr);
                    self.store_all_f64_vals(&dest, &x_new)?;
                } else {
                    // `P' = F P F^T + Q` -- `x` unused here.
                    let fp = self.mat_mul_ptr_vals(&f_ptr, n, n, &p_ptr, n);
                    let fpft = self.mat_mul_a_bt_vals(&fp, &f_ptr, n);
                    let q_vals = self.load_all_f64_vals(&q_ptr, n * n);
                    let p_new = self.vec_add_vals(&fpft, &q_vals);
                    self.store_all_f64_vals(&dest, &p_new)?;
                }
            }
            // Phase 5: `dest` is already allocated (shared setup above);
            // each arm below just hands it to the linked runtime call as
            // the out-pointer and traps via `guard_call_ok` on failure —
            // no unrolled IR, no separate `agg_*` computation, matching
            // `PHASE5_BUILTINS`' whole point (module doc).
            "inv" => {
                let Ty::Matrix(_, n, _) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a square Matrix(f64,_,_)")
                };
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let ok = self.fresh_reg("inv_ok");
                writeln!(self.out, "  {ok} = call i32 @nir_inv(ptr {a_ptr}, i64 {n}, ptr {dest})").unwrap();
                self.guard_call_ok(&ok);
            }
            "solve" => {
                let Ty::Matrix(_, n, _) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a square Matrix(f64,_,_)")
                };
                let a_ptr = self.expr_ptr(&args[0], scopes)?;
                let b_ptr = self.expr_ptr(&args[1], scopes)?;
                let ok = self.fresh_reg("solve_ok");
                writeln!(self.out, "  {ok} = call i32 @nir_solve(ptr {a_ptr}, i64 {n}, ptr {b_ptr}, ptr {dest})").unwrap();
                self.guard_call_ok(&ok);
            }
            "kf_update_state" | "kf_update_cov" => {
                let Ty::Vector(_, n) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved x is Vector(f64,n)")
                };
                let Ty::Vector(_, m) = self.local_ty_of(&args[2], scopes) else {
                    unreachable!("typeck.rs already proved z is Vector(f64,m)")
                };
                let x_ptr = self.expr_ptr(&args[0], scopes)?;
                let p_ptr = self.expr_ptr(&args[1], scopes)?;
                let z_ptr = self.expr_ptr(&args[2], scopes)?;
                let h_ptr = self.expr_ptr(&args[3], scopes)?;
                let r_ptr = self.expr_ptr(&args[4], scopes)?;
                let func = if name == "kf_update_state" { "@nir_kf_update_state" } else { "@nir_kf_update_cov" };
                let ok = self.fresh_reg("kf_update_ok");
                writeln!(
                    self.out,
                    "  {ok} = call i32 {func}(ptr {x_ptr}, ptr {p_ptr}, ptr {z_ptr}, ptr {h_ptr}, ptr {r_ptr}, i64 {n}, i64 {m}, ptr {dest})"
                )
                .unwrap();
                self.guard_call_ok(&ok);
            }
            _ => unreachable!("PHASE4_BUILTINS'/PHASE5_BUILTINS' scalar-returning names go through call_builtin_scalar instead"),
        }
        Ok(dest)
    }

    /// `lla_to_ecef`'s formula on already-loaded `lat_deg`/`lon_deg`/`alt`
    /// SSA values — factored out so `call_builtin_agg`'s own `lla_to_ecef`
    /// arm and `ecef_to_enu`/`enu_to_ecef`'s internal reference-point
    /// conversion (`interpreter.rs` calls the same Rust fn for both) share
    /// one implementation rather than two independently-written copies
    /// that could drift apart. Mirrors `interpreter.rs::lla_to_ecef`
    /// exactly. Returns `(x, y, z)`.
    fn lla_to_ecef_vals(&mut self, lat_deg: &str, lon_deg: &str, alt: &str) -> (String, String, String) {
        let deg_to_rad = Self::float_const(std::f64::consts::PI / 180.0);
        let one = Self::float_const(1.0);
        let e2 = Self::float_const(WGS84_E2);
        let lat = self.emit_mul(lat_deg, &deg_to_rad, true);
        let lon = self.emit_mul(lon_deg, &deg_to_rad, true);
        let sin_lat = self.emit_call1("@llvm.sin.f64", &lat);
        let sin_lat_sq = self.emit_mul(&sin_lat, &sin_lat, true);
        let e2_sinsq = self.emit_mul(&e2, &sin_lat_sq, true);
        let one_minus = self.emit_sub(&one, &e2_sinsq);
        let sqrt_term = self.emit_call1("@llvm.sqrt.f64", &one_minus);
        let n = self.fresh_reg("wgs84_n");
        writeln!(self.out, "  {n} = fdiv double {}, {sqrt_term}", Self::float_const(WGS84_A)).unwrap();
        let cos_lat = self.emit_call1("@llvm.cos.f64", &lat);
        let cos_lon = self.emit_call1("@llvm.cos.f64", &lon);
        let sin_lon = self.emit_call1("@llvm.sin.f64", &lon);
        let n_plus_alt = self.emit_add(&n, alt, true);
        let np_cos_lat = self.emit_mul(&n_plus_alt, &cos_lat, true);
        let x = self.emit_mul(&np_cos_lat, &cos_lon, true);
        let y = self.emit_mul(&np_cos_lat, &sin_lon, true);
        let one_minus_e2 = self.emit_sub(&one, &e2);
        let n_1me2 = self.emit_mul(&n, &one_minus_e2, true);
        let n_1me2_plus_alt = self.emit_add(&n_1me2, alt, true);
        let z = self.emit_mul(&n_1me2_plus_alt, &sin_lat, true);
        (x, y, z)
    }

    /// `ecef_to_lla`'s formula — five fixed Newton-refinement iterations,
    /// unrolled (not data-dependent: always exactly 5, matching
    /// `interpreter.rs::ecef_to_lla`'s `for _ in 0..5`). Returns
    /// `(lat_deg, lon_deg, alt)`.
    fn ecef_to_lla_vals(&mut self, x: &str, y: &str, z: &str) -> (String, String, String) {
        let e2 = Self::float_const(WGS84_E2);
        let one = Self::float_const(1.0);
        let lon = self.emit_call2("@atan2", y, x);
        let x2 = self.emit_mul(x, x, true);
        let y2 = self.emit_mul(y, y, true);
        let x2y2 = self.emit_add(&x2, &y2, true);
        let p = self.emit_call1("@llvm.sqrt.f64", &x2y2);
        let one_minus_e2 = self.emit_sub(&one, &e2);
        let p_1me2 = self.emit_mul(&p, &one_minus_e2, true);
        let mut lat = self.emit_call2("@atan2", z, &p_1me2);
        let mut alt = Self::float_const(0.0);
        for _ in 0..5 {
            let sin_lat = self.emit_call1("@llvm.sin.f64", &lat);
            let sin_lat_sq = self.emit_mul(&sin_lat, &sin_lat, true);
            let e2_sinsq = self.emit_mul(&e2, &sin_lat_sq, true);
            let one_minus = self.emit_sub(&one, &e2_sinsq);
            let sqrt_term = self.emit_call1("@llvm.sqrt.f64", &one_minus);
            let n = self.fresh_reg("wgs84_n");
            writeln!(self.out, "  {n} = fdiv double {}, {sqrt_term}", Self::float_const(WGS84_A)).unwrap();
            let cos_lat = self.emit_call1("@llvm.cos.f64", &lat);
            let p_over_cos = self.fresh_reg("p_over_cos");
            writeln!(self.out, "  {p_over_cos} = fdiv double {p}, {cos_lat}").unwrap();
            alt = self.emit_sub(&p_over_cos, &n);
            let n_plus_alt = self.emit_add(&n, &alt, true);
            let e2n = self.emit_mul(&e2, &n, true);
            let e2n_over = self.fresh_reg("e2n_over");
            writeln!(self.out, "  {e2n_over} = fdiv double {e2n}, {n_plus_alt}").unwrap();
            let inner = self.emit_sub(&one, &e2n_over);
            let p_inner = self.emit_mul(&p, &inner, true);
            lat = self.emit_call2("@atan2", z, &p_inner);
        }
        let rad_to_deg = Self::float_const(180.0 / std::f64::consts::PI);
        let lat_deg = self.emit_mul(&lat, &rad_to_deg, true);
        let lon_deg = self.emit_mul(&lon, &rad_to_deg, true);
        (lat_deg, lon_deg, alt)
    }

    /// The 3x3 ENU rotation matrix's 9 entries at `ref_lat`/`ref_lon`
    /// (already-loaded radians SSA values), row-major flattened
    /// (`r[i][j]` at index `i*3+j`) — mirrors `interpreter.rs::
    /// enu_rotation` exactly.
    fn enu_rotation_vals(&mut self, ref_lat: &str, ref_lon: &str) -> [String; 9] {
        let sin_lat = self.emit_call1("@llvm.sin.f64", ref_lat);
        let cos_lat = self.emit_call1("@llvm.cos.f64", ref_lat);
        let sin_lon = self.emit_call1("@llvm.sin.f64", ref_lon);
        let cos_lon = self.emit_call1("@llvm.cos.f64", ref_lon);
        let neg_sin_lon = self.fresh_reg("neg");
        writeln!(self.out, "  {neg_sin_lon} = fneg double {sin_lon}").unwrap();
        let neg_sin_lat = self.fresh_reg("neg");
        writeln!(self.out, "  {neg_sin_lat} = fneg double {sin_lat}").unwrap();
        let r00 = neg_sin_lon;
        let r01 = cos_lon.clone();
        let r02 = Self::float_const(0.0);
        let r10 = self.emit_mul(&neg_sin_lat, &cos_lon, true);
        let r11 = self.emit_mul(&neg_sin_lat, &sin_lon, true);
        let r12 = cos_lat.clone();
        let r20 = self.emit_mul(&cos_lat, &cos_lon, true);
        let r21 = self.emit_mul(&cos_lat, &sin_lon, true);
        let r22 = sin_lat;
        [r00, r01, r02, r10, r11, r12, r20, r21, r22]
    }

    /// `A(ar x ac) * B(ac x bc)` on already-materialized pointers,
    /// flattened row-major as `ar*bc` SSA values (not yet stored anywhere)
    /// — the pointer-level analog of `agg_mul`'s Matrix*Matrix arm,
    /// needed because `kf_predict_cov` starts from raw argument pointers,
    /// not `Expr` nodes to feed `expr_ptr`/`agg_mul`. Same accumulation
    /// order as `interpreter.rs::mat_mul_f64` — bit-identical per the
    /// `0.0 + x == x` identity `call_builtin_scalar`'s doc comment
    /// already relies on.
    fn mat_mul_ptr_vals(&mut self, a_ptr: &str, ar: usize, ac: usize, b_ptr: &str, bc: usize) -> Vec<String> {
        let mut out = Vec::with_capacity(ar * bc);
        for i in 0..ar {
            for j in 0..bc {
                let a0 = self.agg_load_elem(a_ptr, "double", &Ty::F64, i * ac);
                let b0 = self.agg_load_elem(b_ptr, "double", &Ty::F64, j);
                let mut sum = self.emit_mul(&a0, &b0, true);
                for k in 1..ac {
                    let ak = self.agg_load_elem(a_ptr, "double", &Ty::F64, i * ac + k);
                    let bk = self.agg_load_elem(b_ptr, "double", &Ty::F64, k * bc + j);
                    let prod = self.emit_mul(&ak, &bk, true);
                    sum = self.emit_add(&sum, &prod, true);
                }
                out.push(sum);
            }
        }
        out
    }

    /// `A(n x n) * B^T` where `A` is already-computed values (`a_vals`,
    /// row-major) and `B` is a raw pointer read transposed in place
    /// (`B^T[k,j] = B[j,k]`) rather than materialized — `kf_predict_cov`'s
    /// `F P F^T` needs exactly this shape (`A = F P`, `B = F`). Bit-for-
    /// bit identical to `interpreter.rs::mat_mul_f64(fp, n, n,
    /// &mat_transpose_f64(f,n,n), n)`: `mat_transpose_f64` only
    /// rearranges data (no arithmetic), so reading `B` transposed in
    /// place here is the same accumulation, term for term.
    fn mat_mul_a_bt_vals(&mut self, a_vals: &[String], b_ptr: &str, n: usize) -> Vec<String> {
        let mut out = Vec::with_capacity(n * n);
        for i in 0..n {
            for j in 0..n {
                let a0 = a_vals[i * n].clone();
                let b0 = self.agg_load_elem(b_ptr, "double", &Ty::F64, j * n);
                let mut sum = self.emit_mul(&a0, &b0, true);
                for k in 1..n {
                    let ak = a_vals[i * n + k].clone();
                    let bk = self.agg_load_elem(b_ptr, "double", &Ty::F64, j * n + k);
                    let prod = self.emit_mul(&ak, &bk, true);
                    sum = self.emit_add(&sum, &prod, true);
                }
                out.push(sum);
            }
        }
        out
    }

    /// `A(ar x ac) * v` on a raw pointer — the pointer-level analog of
    /// `agg_mul`'s Matrix*Vector arm, for `kf_predict_state`'s `F x`.
    fn mat_vec_mul_ptr_vals(&mut self, a_ptr: &str, ar: usize, ac: usize, v_ptr: &str) -> Vec<String> {
        let mut out = Vec::with_capacity(ar);
        for i in 0..ar {
            let a0 = self.agg_load_elem(a_ptr, "double", &Ty::F64, i * ac);
            let v0 = self.agg_load_elem(v_ptr, "double", &Ty::F64, 0);
            let mut sum = self.emit_mul(&a0, &v0, true);
            for k in 1..ac {
                let ak = self.agg_load_elem(a_ptr, "double", &Ty::F64, i * ac + k);
                let vk = self.agg_load_elem(v_ptr, "double", &Ty::F64, k);
                let prod = self.emit_mul(&ak, &vk, true);
                sum = self.emit_add(&sum, &prod, true);
            }
            out.push(sum);
        }
        out
    }

    /// Elementwise `a + b` on two already-loaded value lists — the
    /// pointer-level analog of `agg_elementwise`'s `Add` arm, for
    /// `kf_predict_cov`'s final `+ Q`.
    fn vec_add_vals(&mut self, a: &[String], b: &[String]) -> Vec<String> {
        let mut out = Vec::with_capacity(a.len());
        for (x, y) in a.iter().zip(b.iter()) {
            out.push(self.emit_add(x, y, true));
        }
        out
    }

    /// Loads all `len` `f64` elements of a flat aggregate buffer into a
    /// `Vec<String>` of SSA values, in flat order — `kf_predict_cov`'s `Q`
    /// read.
    fn load_all_f64_vals(&mut self, ptr: &str, len: usize) -> Vec<String> {
        let mut out = Vec::with_capacity(len);
        for i in 0..len {
            out.push(self.agg_load_elem(ptr, "double", &Ty::F64, i));
        }
        out
    }

    /// Stores a `Vec<String>` of already-computed `f64` values into a
    /// destination buffer at consecutive flat offsets — the write side of
    /// `load_all_f64_vals`/`mat_mul_ptr_vals`/`mat_vec_mul_ptr_vals`'s
    /// results.
    fn store_all_f64_vals(&mut self, dest: &str, vals: &[String]) -> Result<(), CodegenError> {
        for (i, v) in vals.iter().enumerate() {
            self.agg_store_elem(dest, "double", &Ty::F64, i, v)?;
        }
        Ok(())
    }

    /// The aggregate-value twin of `expr()` — returns a pointer to a
    /// stack-allocated `Vector`/`Matrix` value instead of a bare SSA
    /// register, since an aggregate can't live in one (module doc /
    /// design decision 1 of the Vector/Matrix codegen plan). Only ever
    /// called where the caller already knows (typically via
    /// `local_ty_of(e, scopes).is_aggregate()`) that `e`'s type is
    /// `Vector`/`Matrix`. Deliberately narrow this phase — only the
    /// forms actually needed to bind, reassign, and pass/return an
    /// aggregate value exist yet; anything else (e.g. an aggregate-typed
    /// `if` expression) fails as a clean `CodegenError`, a real scope
    /// boundary for a later phase, not a gap found by accident.
    fn expr_ptr(&mut self, e: &Expr, scopes: &mut Scopes) -> Result<String, CodegenError> {
        match e {
            Expr::Ident(name, _) => {
                // No copy here — this is *the* variable's own storage.
                // A caller that's about to bind a new name or overwrite
                // an existing one (`Stmt::Let`, this fn's own
                // `Expr::Assign` arm) is responsible for copying out of
                // this pointer itself; a caller that's just forwarding
                // the value onward (a call argument, matching the
                // callee's own copy-in prologue) is not.
                let (_, ptr) = scopes.get(name).expect("typeck.rs already proved this resolves");
                Ok(ptr)
            }
            Expr::ArrayLit(elements, _) => self.array_lit(elements, scopes),
            Expr::Call(name, args, _) => self.call_ptr(name, args, scopes),
            Expr::Assign(name, rhs, _) => {
                let src = self.expr_ptr(rhs, scopes)?;
                let (ty, dst) = scopes.get(name).expect("typeck.rs already proved this resolves");
                let (_, _, bytes) = agg_layout(&ty);
                writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {dst}, ptr {src}, i64 {bytes}, i1 false)").unwrap();
                Ok(dst)
            }
            // Every Vector/Matrix-*producing* binary operator (elementwise
            // `+`/`-`/`.*`/`./`, and `*` in its three legal shapes) --
            // `==`/`!=` on aggregate operands still yield a scalar `bool`
            // and stay on the `expr()`/`binary()` path (`agg_eq`), not
            // here.
            Expr::Binary(op, l, r, span) => self.agg_binary(*op, l, r, *span, scopes),
            // `*e` where the unwrapped payload is itself an aggregate
            // (`box Vector(f64,3)`, `&Matrix(f64,2,2)`, ...) — `e`'s own
            // type (`Box`/`Ref`) is never `is_aggregate()`, so `e` itself
            // is fetched via the ordinary `expr()` path (a bare pointer
            // value), then handed straight back as this dereference's
            // own pointer-returning result. No allocation, no copy —
            // dereferencing exposes the same storage the box/ref already
            // points at, it doesn't clone it (matches the interpreter's
            // `Value::Boxed(inner) | Value::Ref(inner) => *inner`, and
            // `expr()`'s own `Expr::Deref` arm for the non-aggregate
            // case — same value, different result shape only).
            Expr::Deref(inner, _) => self.expr(inner, scopes),
            _ => unsupported(
                "codegen doesn't support this Vector/Matrix expression form yet — only \
                 identifiers, literals, assignment, binary operators, dereferencing a boxed/\
                 borrowed aggregate, and function calls/returns are supported so far \
                 (indexing reads a scalar element via `expr()`, and builtins land in a later \
                 phase)",
            ),
        }
    }

    /// `expr_ptr`'s `Expr::ArrayLit` case: allocate a fresh destination
    /// sized to the literal's own inferred shape, then fill it — element
    /// by element for a `Vector` (each a plain scalar `expr()`), row by
    /// row for a `Matrix` (each row's own pointer via `expr_ptr`,
    /// `memcpy`'d into the right flat offset — row-major, matching
    /// `interpreter.rs`'s `Value::Matrix` exactly).
    fn array_lit(&mut self, elements: &[Expr], scopes: &mut Scopes) -> Result<String, CodegenError> {
        let ty = self.array_lit_ty(elements, scopes);
        let agg_llty = llvm_ty(&ty)?;
        let dest = self.fresh_reg("arraylit.addr");
        self.emit_alloca(&dest, &agg_llty);

        match &ty {
            Ty::Vector(elem, _) => {
                let elem_llty = llvm_ty(elem)?;
                for (i, e) in elements.iter().enumerate() {
                    let v = self.expr(e, scopes)?;
                    let v = if elem.is_integer() { self.narrow_from_i64(&v, elem)? } else { v };
                    let gep = self.fresh_reg("arraylit.elem");
                    writeln!(self.out, "  {gep} = getelementptr {agg_llty}, ptr {dest}, i64 0, i64 {i}").unwrap();
                    writeln!(self.out, "  store {elem_llty} {v}, ptr {gep}").unwrap();
                }
            }
            Ty::Matrix(elem, _rows, cols) => {
                let row_bytes = *cols as u64 * elem_byte_size(elem);
                for (i, row_expr) in elements.iter().enumerate() {
                    let row_ptr = self.expr_ptr(row_expr, scopes)?;
                    let row_dest = self.fresh_reg("arraylit.row");
                    writeln!(self.out, "  {row_dest} = getelementptr {agg_llty}, ptr {dest}, i64 0, i64 {}", i * cols).unwrap();
                    writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {row_dest}, ptr {row_ptr}, i64 {row_bytes}, i1 false)").unwrap();
                }
            }
            _ => unreachable!("array_lit_ty always returns Vector or Matrix"),
        }
        Ok(dest)
    }

    /// `expr_ptr`'s `Expr::Call` case — the sret side of the by-pointer
    /// return convention `function()` sets up: allocate the destination
    /// here (the caller's responsibility, matching a normal `let`'s own
    /// alloca), pass its address as the implicit first argument, and
    /// hand that same pointer back as this call expression's own
    /// "value" — exactly `expr_ptr`'s contract.
    fn call_ptr(&mut self, name: &str, args: &[Expr], scopes: &mut Scopes) -> Result<String, CodegenError> {
        if PHASE4_BUILTINS.contains(&name)
            || matches!(name, "inv" | "solve" | "kf_update_state" | "kf_update_cov")
        {
            return self.call_builtin_agg(name, args, scopes);
        }
        let sig_params = self.sigs.get(name).expect("typeck.rs already resolved this call").params.clone();
        let sig_ret = self.sigs.get(name).expect("typeck.rs already resolved this call").ret.clone();
        let arg_vals = self.call_args(args, &sig_params, scopes)?;

        let agg_llty = llvm_ty(&sig_ret)?;
        let dest = self.fresh_reg("call_result.addr");
        self.emit_alloca(&dest, &agg_llty);
        let mut all_args = vec![format!("ptr {dest}")];
        all_args.extend(arg_vals);
        writeln!(self.out, "  call void @{name}({})", all_args.join(", ")).unwrap();
        Ok(dest)
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span, scopes: &mut Scopes) -> Result<String, CodegenError> {
        if op == BinOp::And || op == BinOp::Or {
            return self.short_circuit(op, lhs, rhs, scopes);
        }

        // `==`/`!=` are the one pair of operators typeck.rs allows on
        // `Vector`/`Matrix` operands -- structural equality, not the
        // scalar `fcmp`/`icmp` this function does for everything else.
        // Caught here, before the `is_float` dispatch below (which
        // assumes a bare scalar SSA value on both sides), rather than in
        // `expr()`'s own `Expr::Binary` arm, so every other call site of
        // `binary()` stays untouched.
        if matches!(op, BinOp::Eq | BinOp::NotEq) {
            let lhs_ty = self.local_ty_of(lhs, scopes);
            if lhs_ty.is_aggregate() {
                return self.agg_eq(op, lhs, rhs, &lhs_ty, scopes);
            }
            // `str` is the other non-`is_float`/non-plain-integer operand
            // shape `==`/`!=` has to special-case before the `is_float`
            // dispatch below (which assumes a bare scalar `fcmp`/`icmp`-
            // ready SSA value on both sides, not a `{ptr, i64}` struct).
            if lhs_ty == Ty::Str {
                return self.str_eq(op, lhs, rhs, scopes);
            }
        }

        // Arithmetic and ordering comparisons only ever apply to numeric
        // operands (typeck.rs's `unify_operands` rejects `bool` for all
        // of these except `==`/`!=`) — every arm below except Eq/NotEq
        // dispatches on whether the operands are `f64` or plain integer;
        // `l`/`r` are `i64` (or `i1`, for the Eq/NotEq-on-bool case) for
        // the integer case, or a genuine `double` for the float case —
        // see the module doc and `Ty::F64`'s codegen note in `llvm_ty`.
        // typeck.rs's `unify_operands`/`infer_hadamard` already proved
        // both operands share exactly one type, so checking `lhs` alone
        // is enough to know which.
        let is_float = self.local_ty_of(lhs, scopes) == Ty::F64;
        let l = self.expr(lhs, scopes)?;
        let r = self.expr(rhs, scopes)?;
        match op {
            BinOp::Add => {
                let out = self.fresh_reg("add");
                if is_float {
                    writeln!(self.out, "  {out} = fadd double {l}, {r}").unwrap();
                } else {
                    writeln!(self.out, "  {out} = add i64 {l}, {r}").unwrap();
                }
                Ok(out)
            }
            BinOp::Sub => {
                let out = self.fresh_reg("sub");
                if is_float {
                    writeln!(self.out, "  {out} = fsub double {l}, {r}").unwrap();
                } else {
                    writeln!(self.out, "  {out} = sub i64 {l}, {r}").unwrap();
                }
                Ok(out)
            }
            // `.*` on plain scalars is legal too (`infer_hadamard`'s doc
            // comment: two matching scalars are trivially "the same
            // shape") and means exactly the same thing as `*` there --
            // `scalar_binop`'s own `Int`/`Float` arms already treat
            // `Mul`/`ElemMul` identically, so this does too.
            BinOp::Mul | BinOp::ElemMul => {
                let out = self.fresh_reg("mul");
                if is_float {
                    writeln!(self.out, "  {out} = fmul double {l}, {r}").unwrap();
                } else {
                    writeln!(self.out, "  {out} = mul i64 {l}, {r}").unwrap();
                }
                Ok(out)
            }
            // Same story as `ElemMul` above -- `scalar_binop` treats
            // `Div`/`ElemDiv` identically, so `2 ./ 3` on plain scalars
            // takes this arm too.
            BinOp::Div | BinOp::ElemDiv => {
                if is_float {
                    // IEEE 754 division by zero saturates to inf/-inf/NaN
                    // rather than trapping (`Ty::F64`'s doc comment,
                    // matching the interpreter exactly) -- no guard to
                    // emit here at all, audited or not.
                    let out = self.fresh_reg("fdiv");
                    writeln!(self.out, "  {out} = fdiv double {l}, {r}").unwrap();
                    return Ok(out);
                }
                self.guard_nonzero_divisor(&r, span);
                let out = self.fresh_reg("sdiv");
                writeln!(self.out, "  {out} = sdiv i64 {l}, {r}").unwrap();
                Ok(out)
            }
            // `==`/`!=` are the one pair typeck.rs allows on `bool`
            // operands too — pick i1/i64/double based on the *operand's*
            // declared type.
            BinOp::Eq | BinOp::NotEq => {
                if is_float {
                    let cond = if op == BinOp::Eq { "oeq" } else { "one" };
                    return self.fcmp(cond, &l, &r);
                }
                let cmp_ty = if self.local_ty_of(lhs, scopes) == Ty::Bool { "i1" } else { "i64" };
                let cond = if op == BinOp::Eq { "eq" } else { "ne" };
                self.icmp(cond, cmp_ty, &l, &r)
            }
            BinOp::Lt if is_float => self.fcmp("olt", &l, &r),
            BinOp::Gt if is_float => self.fcmp("ogt", &l, &r),
            BinOp::LtEq if is_float => self.fcmp("ole", &l, &r),
            BinOp::GtEq if is_float => self.fcmp("oge", &l, &r),
            BinOp::Lt => self.icmp("slt", "i64", &l, &r),
            BinOp::Gt => self.icmp("sgt", "i64", &l, &r),
            BinOp::LtEq => self.icmp("sle", "i64", &l, &r),
            BinOp::GtEq => self.icmp("sge", "i64", &l, &r),
            BinOp::And | BinOp::Or => unreachable!("handled above"),
        }
    }

    /// Shared by scalar `/`/`./`'s int-path guard and `agg_elementwise`'s
    /// per-element int `./` guard -- `if !audited && not already proven
    /// nonzero: icmp eq 0 -> br -> trap(abort+unreachable) / ok`, the
    /// same shape every Tier-1/2 guard in this file follows.
    fn guard_nonzero_divisor(&mut self, divisor: &str, span: Span) {
        if self.audited || self.smt_report.proven_nonzero_divisor.contains(&span) {
            return;
        }
        let is_zero = self.fresh_reg("div_zero");
        writeln!(self.out, "  {is_zero} = icmp eq i64 {divisor}, 0").unwrap();
        let trap = self.fresh_label("div_trap");
        let ok = self.fresh_label("div_ok");
        writeln!(self.out, "  br i1 {is_zero}, label %{trap}, label %{ok}").unwrap();
        writeln!(self.out, "{trap}:").unwrap();
        writeln!(self.out, "  call void @abort()").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        writeln!(self.out, "{ok}:").unwrap();
    }

    /// Phase 5's fallible-runtime-call analog of `guard_nonzero_divisor`:
    /// traps if `ok_i32` (a `nir_inv`/`nir_solve`/`nir_kf_update_*` call
    /// result) is `0` — the singular-matrix case, which `interpreter.rs`
    /// surfaces as `ErrorKind::SingularMatrix` (an ordinary runtime
    /// `Err`, not a panic there either). No `proven_*` elision set
    /// applies here — matrix singularity is a genuine runtime data fact,
    /// not something either bounds-prover can decide statically — so
    /// this always emits the check unless `audited`, same convention
    /// every other Tier-2 guard in this file already follows.
    fn guard_call_ok(&mut self, ok_i32: &str) {
        if self.audited {
            return;
        }
        let is_fail = self.fresh_reg("call_failed");
        writeln!(self.out, "  {is_fail} = icmp eq i32 {ok_i32}, 0").unwrap();
        let trap = self.fresh_label("singular_trap");
        let ok = self.fresh_label("singular_ok");
        writeln!(self.out, "  br i1 {is_fail}, label %{trap}, label %{ok}").unwrap();
        writeln!(self.out, "{trap}:").unwrap();
        writeln!(self.out, "  call void @abort()").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        writeln!(self.out, "{ok}:").unwrap();
    }

    /// A `tcp` runtime kernel's `i64` result is "bytes/fd on success, `-1`
    /// on failure" — negative traps, matching the interpreter's
    /// `ChannelIoError` being fatal (there's no `try`/`catch` anywhere in
    /// the language to recover from it), same `abort()` trap idiom as
    /// every other guard in this file.
    fn guard_io_ok(&mut self, result_i64: &str) {
        if self.audited {
            return;
        }
        let is_fail = self.fresh_reg("io_failed");
        writeln!(self.out, "  {is_fail} = icmp slt i64 {result_i64}, 0").unwrap();
        let trap = self.fresh_label("io_trap");
        let ok = self.fresh_label("io_ok");
        writeln!(self.out, "  br i1 {is_fail}, label %{trap}, label %{ok}").unwrap();
        writeln!(self.out, "{trap}:").unwrap();
        writeln!(self.out, "  call void @abort()").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        writeln!(self.out, "{ok}:").unwrap();
    }

    /// `recv`'s `0` (peer closed) is *also* an error — `interpreter.rs`'s
    /// `read_tcp` treats `n == 0` as `ChannelIoError`, not a valid empty
    /// read (module doc's "one chunk, not a message boundary" note) — so
    /// this traps on `<= 0`, not just `< 0` like `guard_io_ok`.
    fn guard_recv_ok(&mut self, result_i64: &str) {
        if self.audited {
            return;
        }
        let is_fail = self.fresh_reg("recv_failed");
        writeln!(self.out, "  {is_fail} = icmp sle i64 {result_i64}, 0").unwrap();
        let trap = self.fresh_label("recv_trap");
        let ok = self.fresh_label("recv_ok");
        writeln!(self.out, "  br i1 {is_fail}, label %{trap}, label %{ok}").unwrap();
        writeln!(self.out, "{trap}:").unwrap();
        writeln!(self.out, "  call void @abort()").unwrap();
        writeln!(self.out, "  unreachable").unwrap();
        writeln!(self.out, "{ok}:").unwrap();
    }

    /// Evaluates a `str`-typed expression and extracts its `(ptr, i64
    /// len)` fields — the marshaling every `tcp` kernel call needs for a
    /// `str` argument, factored out since `connect`'s host and `send`'s
    /// payload both need exactly this.
    fn str_parts(&mut self, e: &Expr, scopes: &mut Scopes) -> Result<(String, String), CodegenError> {
        let v = self.expr(e, scopes)?;
        let ptr = self.fresh_reg("str_ptr");
        writeln!(self.out, "  {ptr} = extractvalue {{ptr, i64}} {v}, 0").unwrap();
        let len = self.fresh_reg("str_len");
        writeln!(self.out, "  {len} = extractvalue {{ptr, i64}} {v}, 1").unwrap();
        Ok((ptr, len))
    }

    fn icmp(&mut self, cond: &str, llty: &str, l: &str, r: &str) -> Result<String, CodegenError> {
        let out = self.fresh_reg("cmp");
        writeln!(self.out, "  {out} = icmp {cond} {llty} {l}, {r}").unwrap();
        Ok(out)
    }

    /// `o{cond}` (ordered) comparisons throughout — `==`/`!=`/`<`/`>`/
    /// `<=`/`>=` on `f64` all mean "and neither operand is NaN," LLVM's
    /// "ordered" family, not "unordered" (`u{cond}`, true if *either*
    /// side is NaN) — the same comparison semantics Rust's own `f64`
    /// `PartialOrd`/`PartialEq` already use, which `interpreter.rs`'s
    /// `eval_binary` inherits for free via native Rust `<`/`==` on `f64`
    /// (no separate NaN-handling decision was made there; this just has
    /// to agree with it).
    fn fcmp(&mut self, cond: &str, l: &str, r: &str) -> Result<String, CodegenError> {
        let out = self.fresh_reg("fcmp");
        writeln!(self.out, "  {out} = fcmp {cond} double {l}, {r}").unwrap();
        Ok(out)
    }

    /// `getelementptr` to one element of a flat aggregate buffer at a
    /// *compile-time-constant* flat index -- the workhorse every unrolled
    /// elementwise/product loop below uses (unlike `Expr::Index`'s own
    /// GEP, whose offset is a runtime SSA value; this one's offset is a
    /// literal, since every loop here unrolls over a compile-time-known
    /// shape).
    fn agg_elem_ptr(&mut self, base_ptr: &str, elem_llty: &str, flat_idx: usize) -> String {
        let gep = self.fresh_reg("agg_elem.addr");
        writeln!(self.out, "  {gep} = getelementptr {elem_llty}, ptr {base_ptr}, i64 {flat_idx}").unwrap();
        gep
    }

    /// Load one element and widen it to this backend's internal i64/
    /// double convention (`widen_to_i64`) -- so the result composes
    /// directly with a plain scalar `expr()` value (e.g. the scalar
    /// operand of `scalar * Matrix`) and with `emit_mul`/`emit_add`.
    fn agg_load_elem(&mut self, base_ptr: &str, elem_llty: &str, elem_ty: &Ty, flat_idx: usize) -> String {
        let gep = self.agg_elem_ptr(base_ptr, elem_llty, flat_idx);
        let loaded = self.fresh_reg("agg_elem.val");
        writeln!(self.out, "  {loaded} = load {elem_llty}, ptr {gep}").unwrap();
        self.widen_to_i64(&loaded, elem_ty)
    }

    /// Store one element, narrowing back down from the internal i64
    /// convention first if the element type is a narrower integer --
    /// the store side of `agg_load_elem`'s widen, mirroring how
    /// `array_lit` already narrows before storing a literal's elements.
    fn agg_store_elem(&mut self, base_ptr: &str, elem_llty: &str, elem_ty: &Ty, flat_idx: usize, val: &str) -> Result<(), CodegenError> {
        let store_val = if elem_ty.is_integer() { self.narrow_from_i64(val, elem_ty)? } else { val.to_string() };
        let gep = self.agg_elem_ptr(base_ptr, elem_llty, flat_idx);
        writeln!(self.out, "  store {elem_llty} {store_val}, ptr {gep}").unwrap();
        Ok(())
    }

    /// `l * r` at the internal i64/double width -- shared by `agg_mul`'s
    /// three shapes (scale, mat-vec, mat-mat), all of which need the
    /// same scalar multiply repeated many times.
    fn emit_mul(&mut self, l: &str, r: &str, is_float: bool) -> String {
        let out = self.fresh_reg("agg_mul_elem");
        if is_float {
            writeln!(self.out, "  {out} = fmul double {l}, {r}").unwrap();
        } else {
            writeln!(self.out, "  {out} = mul i64 {l}, {r}").unwrap();
        }
        out
    }

    /// `l + r` at the internal i64/double width -- the accumulation half
    /// of `agg_mul`'s mat-vec/mat-mat dot-product chains.
    fn emit_add(&mut self, l: &str, r: &str, is_float: bool) -> String {
        let out = self.fresh_reg("agg_add_elem");
        if is_float {
            writeln!(self.out, "  {out} = fadd double {l}, {r}").unwrap();
        } else {
            writeln!(self.out, "  {out} = add i64 {l}, {r}").unwrap();
        }
        out
    }

    /// `l - r` at `double` width — the geometry/norm builtins' analog of
    /// `emit_add`/`emit_mul` (Phase 4 only ever subtracts `f64`s: no
    /// integer-typed builtin in this phase's scope needs it).
    fn emit_sub(&mut self, l: &str, r: &str) -> String {
        let out = self.fresh_reg("agg_sub_elem");
        writeln!(self.out, "  {out} = fsub double {l}, {r}").unwrap();
        out
    }

    /// LLVM's own hex bit-pattern float literal — the exact format
    /// `Expr::Float`'s own codegen already uses (module doc there), reused
    /// here for every closed-form constant Phase 4's geometry builtins
    /// need (`pi`, WGS84's `a`/`e2`, `360.0`, ...) that has no
    /// corresponding `Expr::Float` AST node to read it from.
    fn float_const(f: f64) -> String {
        format!("0x{:016X}", f.to_bits())
    }

    /// A one-`double`-argument call against a `declare`d LLVM intrinsic or
    /// libm function (`func` already includes its own `@` sigil, e.g.
    /// `"@llvm.sqrt.f64"`) — the geometry/norm builtins' shared workhorse
    /// for everything transcendental this backend doesn't have a plain
    /// instruction for.
    fn emit_call1(&mut self, func: &str, arg: &str) -> String {
        let r = self.fresh_reg("libm");
        writeln!(self.out, "  {r} = call double {func}(double {arg})").unwrap();
        r
    }

    /// The two-argument analog of `emit_call1` — `atan2`/`llvm.maxnum.f64`.
    fn emit_call2(&mut self, func: &str, a: &str, b: &str) -> String {
        let r = self.fresh_reg("libm");
        writeln!(self.out, "  {r} = call double {func}(double {a}, double {b})").unwrap();
        r
    }

    /// Structural `==`/`!=` on `Vector`/`Matrix` — unrolled element-by-
    /// element comparison chained with `and` into one final `i1`,
    /// matching `interpreter.rs::eval_binary`'s `Value`-level structural
    /// equality (`Vector`: elementwise `PartialEq` on the whole slice;
    /// `Matrix`: shape-then-elements — shape is already guaranteed equal
    /// here since typeck makes differently-shaped Vectors/Matrices
    /// different `Ty`s, so there's no shape check left to emit).
    /// Produces a scalar `bool`, so this is reached from `binary()`
    /// (`expr()`'s path), not `expr_ptr()`.
    fn agg_eq(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, ty: &Ty, scopes: &mut Scopes) -> Result<String, CodegenError> {
        let l_ptr = self.expr_ptr(lhs, scopes)?;
        let r_ptr = self.expr_ptr(rhs, scopes)?;
        let (elem, len) = agg_elem_and_len(ty);
        let elem_llty = llvm_ty(elem)?;
        let is_float = *elem == Ty::F64;

        let mut acc: Option<String> = None;
        for i in 0..len {
            let l = self.agg_load_elem(&l_ptr, &elem_llty, elem, i);
            let r = self.agg_load_elem(&r_ptr, &elem_llty, elem, i);
            let eq = if is_float { self.fcmp("oeq", &l, &r)? } else { self.icmp("eq", "i64", &l, &r)? };
            acc = Some(match acc {
                None => eq,
                Some(prev) => {
                    let out = self.fresh_reg("agg_eq_and");
                    writeln!(self.out, "  {out} = and i1 {prev}, {eq}").unwrap();
                    out
                }
            });
        }
        // Only reachable for a non-empty Vector/Matrix -- `Vector(_, 0)`
        // has no literal/builtin construction path in this language, so
        // `acc` is always `Some` by the time the loop above finishes.
        let all_eq = acc.expect("Vector/Matrix length is always > 0 for any constructible value");
        if op == BinOp::Eq {
            Ok(all_eq)
        } else {
            let out = self.fresh_reg("agg_neq");
            writeln!(self.out, "  {out} = xor i1 {all_eq}, true").unwrap();
            Ok(out)
        }
    }

    /// `str`'s `==`/`!=` — a linked call into `nir_str_eq` (length check +
    /// byte compare, `runtime_kernels.rs`), the same "reuse proven Rust
    /// code via a call" choice as `det`/`inv`/etc., not hand-emitted IR.
    /// `binary()`'s `Eq`/`NotEq` intercept routes here before the
    /// `is_float` dispatch that assumes a scalar operand.
    fn str_eq(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, scopes: &mut Scopes) -> Result<String, CodegenError> {
        let l = self.expr(lhs, scopes)?;
        let r = self.expr(rhs, scopes)?;
        let l_ptr = self.fresh_reg("str_eq_lptr");
        writeln!(self.out, "  {l_ptr} = extractvalue {{ptr, i64}} {l}, 0").unwrap();
        let l_len = self.fresh_reg("str_eq_llen");
        writeln!(self.out, "  {l_len} = extractvalue {{ptr, i64}} {l}, 1").unwrap();
        let r_ptr = self.fresh_reg("str_eq_rptr");
        writeln!(self.out, "  {r_ptr} = extractvalue {{ptr, i64}} {r}, 0").unwrap();
        let r_len = self.fresh_reg("str_eq_rlen");
        writeln!(self.out, "  {r_len} = extractvalue {{ptr, i64}} {r}, 1").unwrap();
        let raw = self.fresh_reg("str_eq_raw");
        writeln!(self.out, "  {raw} = call i32 @nir_str_eq(ptr {l_ptr}, i64 {l_len}, ptr {r_ptr}, i64 {r_len})").unwrap();
        // `nir_str_eq` returns 1 (equal) / 0 (not equal) — `Eq` wants
        // "raw != 0", `NotEq` wants "raw == 0".
        let cond = if op == BinOp::Eq { "ne" } else { "eq" };
        self.icmp(cond, "i32", &raw, "0")
    }

    /// `expr_ptr`'s `Expr::Binary` case — every Vector/Matrix-*producing*
    /// binary operator (elementwise `+`/`-`/`.*`/`./`, and `*` in its
    /// three legal shapes), fully unrolled at codegen time since every
    /// shape involved is a compile-time literal (typeck's
    /// `literal_dimension` rule) — no runtime loop, no data-dependent
    /// control flow anywhere in this phase.
    fn agg_binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span, scopes: &mut Scopes) -> Result<String, CodegenError> {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::ElemMul | BinOp::ElemDiv => self.agg_elementwise(op, lhs, rhs, span, scopes),
            BinOp::Mul => self.agg_mul(lhs, rhs, scopes),
            _ => unreachable!("typeck.rs never types another binary op as Vector/Matrix-producing"),
        }
    }

    /// Elementwise `+`/`-`/`.*`/`./` — same shape on both sides
    /// (typeck-guaranteed, not re-checked here), one scalar instruction
    /// per element. Integer `./` traps on a zero divisor per-element,
    /// same as scalar `/`/`./` (`guard_nonzero_divisor`); float `./`
    /// saturates, never traps, matching `scalar_binop`'s `Float` arm.
    fn agg_elementwise(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span, scopes: &mut Scopes) -> Result<String, CodegenError> {
        let ty = self.local_ty_of(lhs, scopes);
        let agg_llty = llvm_ty(&ty)?;
        let dest = self.fresh_reg("agg_elemwise.addr");
        self.emit_alloca(&dest, &agg_llty);

        let l_ptr = self.expr_ptr(lhs, scopes)?;
        let r_ptr = self.expr_ptr(rhs, scopes)?;
        let (elem, len) = agg_elem_and_len(&ty);
        let elem = elem.clone();
        let elem_llty = llvm_ty(&elem)?;
        let is_float = elem == Ty::F64;

        for i in 0..len {
            let l = self.agg_load_elem(&l_ptr, &elem_llty, &elem, i);
            let r = self.agg_load_elem(&r_ptr, &elem_llty, &elem, i);
            let out = match op {
                BinOp::Add => self.emit_add(&l, &r, is_float),
                BinOp::Sub => {
                    let out = self.fresh_reg("agg_sub");
                    if is_float {
                        writeln!(self.out, "  {out} = fsub double {l}, {r}").unwrap();
                    } else {
                        writeln!(self.out, "  {out} = sub i64 {l}, {r}").unwrap();
                    }
                    out
                }
                BinOp::ElemMul => self.emit_mul(&l, &r, is_float),
                BinOp::ElemDiv => {
                    if is_float {
                        let out = self.fresh_reg("agg_fdiv");
                        writeln!(self.out, "  {out} = fdiv double {l}, {r}").unwrap();
                        out
                    } else {
                        self.guard_nonzero_divisor(&r, span);
                        let out = self.fresh_reg("agg_sdiv");
                        writeln!(self.out, "  {out} = sdiv i64 {l}, {r}").unwrap();
                        out
                    }
                }
                _ => unreachable!("agg_binary only dispatches elementwise ops here"),
            };
            self.agg_store_elem(&dest, &elem_llty, &elem, i, &out)?;
        }
        Ok(dest)
    }

    /// `*` in its three legal aggregate-producing shapes. Loop nesting
    /// and accumulation order match `interpreter.rs::eval_binary`'s
    /// `Matrix`/`Vector` `Mul` arms exactly (module doc, design decision
    /// 3) — first term computed directly, then each remaining term
    /// folded in left-to-right — so floating-point summation order is
    /// bit-identical to the interpreter's output, not just mathematically
    /// equivalent.
    fn agg_mul(&mut self, lhs: &Expr, rhs: &Expr, scopes: &mut Scopes) -> Result<String, CodegenError> {
        let lt = self.local_ty_of(lhs, scopes);
        let rt = self.local_ty_of(rhs, scopes);
        let result_ty = self.mul_result_ty(lhs, rhs, scopes);
        let agg_llty = llvm_ty(&result_ty)?;
        let dest = self.fresh_reg("agg_mul.addr");
        self.emit_alloca(&dest, &agg_llty);

        match (&lt, &rt) {
            // scalar * Matrix, either order -- elementwise scale.
            (s, mt @ Ty::Matrix(..)) if !s.is_aggregate() => {
                let scalar = self.expr(lhs, scopes)?;
                let m_ptr = self.expr_ptr(rhs, scopes)?;
                self.agg_scale(&scalar, &m_ptr, mt, &dest)?;
            }
            (mt @ Ty::Matrix(..), s) if !s.is_aggregate() => {
                let m_ptr = self.expr_ptr(lhs, scopes)?;
                let scalar = self.expr(rhs, scopes)?;
                self.agg_scale(&scalar, &m_ptr, mt, &dest)?;
            }
            // Matrix * Vector -- unrolled dot-product-per-row. Matches
            // interpreter.rs: `sum = m[i,0]*v[0]; for k in 1..cols: sum
            // += m[i,k]*v[k]`.
            (Ty::Matrix(m_elem, rows, cols), Ty::Vector(..)) => {
                let m_ptr = self.expr_ptr(lhs, scopes)?;
                let v_ptr = self.expr_ptr(rhs, scopes)?;
                let elem_llty = llvm_ty(m_elem)?;
                let is_float = **m_elem == Ty::F64;
                let cols = *cols;
                for i in 0..*rows {
                    let m0 = self.agg_load_elem(&m_ptr, &elem_llty, m_elem, i * cols);
                    let v0 = self.agg_load_elem(&v_ptr, &elem_llty, m_elem, 0);
                    let mut sum = self.emit_mul(&m0, &v0, is_float);
                    for k in 1..cols {
                        let mk = self.agg_load_elem(&m_ptr, &elem_llty, m_elem, i * cols + k);
                        let vk = self.agg_load_elem(&v_ptr, &elem_llty, m_elem, k);
                        let prod = self.emit_mul(&mk, &vk, is_float);
                        sum = self.emit_add(&sum, &prod, is_float);
                    }
                    self.agg_store_elem(&dest, &elem_llty, m_elem, i, &sum)?;
                }
            }
            // Matrix * Matrix -- unrolled triple-nested accumulation.
            // Matches interpreter.rs: `sum = a[i,0]*b[0,j]; for k in
            // 1..ac: sum += a[i,k]*b[k,j]`.
            (Ty::Matrix(l_elem, r1, c1), Ty::Matrix(_, _r2, c2)) => {
                let a_ptr = self.expr_ptr(lhs, scopes)?;
                let b_ptr = self.expr_ptr(rhs, scopes)?;
                let elem_llty = llvm_ty(l_elem)?;
                let is_float = **l_elem == Ty::F64;
                let (ac, bc) = (*c1, *c2);
                for i in 0..*r1 {
                    for j in 0..bc {
                        let a0 = self.agg_load_elem(&a_ptr, &elem_llty, l_elem, i * ac);
                        let b0 = self.agg_load_elem(&b_ptr, &elem_llty, l_elem, j);
                        let mut sum = self.emit_mul(&a0, &b0, is_float);
                        for k in 1..ac {
                            let ak = self.agg_load_elem(&a_ptr, &elem_llty, l_elem, i * ac + k);
                            let bk = self.agg_load_elem(&b_ptr, &elem_llty, l_elem, k * bc + j);
                            let prod = self.emit_mul(&ak, &bk, is_float);
                            sum = self.emit_add(&sum, &prod, is_float);
                        }
                        self.agg_store_elem(&dest, &elem_llty, l_elem, i * bc + j, &sum)?;
                    }
                }
            }
            _ => unreachable!("typeck::infer_mul already restricted the legal shapes"),
        }
        Ok(dest)
    }

    /// `agg_mul`'s scalar × `Matrix` case (either operand order already
    /// normalized by the caller) — elementwise scale, unrolled.
    fn agg_scale(&mut self, scalar: &str, m_ptr: &str, mat_ty: &Ty, dest: &str) -> Result<(), CodegenError> {
        let (elem, len) = agg_elem_and_len(mat_ty);
        let elem = elem.clone();
        let elem_llty = llvm_ty(&elem)?;
        let is_float = elem == Ty::F64;
        for i in 0..len {
            let m = self.agg_load_elem(m_ptr, &elem_llty, &elem, i);
            let out = self.emit_mul(&m, scalar, is_float);
            self.agg_store_elem(dest, &elem_llty, &elem, i, &out)?;
        }
        Ok(())
    }

    /// `&&`/`||` as real branches, not eager `and`/`or` — see module doc.
    fn short_circuit(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, scopes: &mut Scopes) -> Result<String, CodegenError> {
        let result_ptr = self.fresh_reg("logic_result.addr");
        self.emit_alloca(&result_ptr, "i1");

        let l = self.expr(lhs, scopes)?;
        let rhs_label = self.fresh_label("logic_rhs");
        let short_label = self.fresh_label("logic_short");
        let merge_label = self.fresh_label("logic_merge");

        if op == BinOp::And {
            writeln!(self.out, "  br i1 {l}, label %{rhs_label}, label %{short_label}").unwrap();
        } else {
            writeln!(self.out, "  br i1 {l}, label %{short_label}, label %{rhs_label}").unwrap();
        }

        writeln!(self.out, "{short_label}:").unwrap();
        writeln!(self.out, "  store i1 {l}, ptr {result_ptr}").unwrap();
        writeln!(self.out, "  br label %{merge_label}").unwrap();

        writeln!(self.out, "{rhs_label}:").unwrap();
        let r = self.expr(rhs, scopes)?;
        writeln!(self.out, "  store i1 {r}, ptr {result_ptr}").unwrap();
        writeln!(self.out, "  br label %{merge_label}").unwrap();

        writeln!(self.out, "{merge_label}:").unwrap();
        let out = self.fresh_reg("logic_val");
        writeln!(self.out, "  {out} = load i1, ptr {result_ptr}").unwrap();
        Ok(out)
    }

    /// The type an if-expression's value slot needs to be, so it can
    /// correctly hold a `bool` (`i1`) result and not just an integer one
    /// — the fix for the gap this function used to have (a hardcoded
    /// `i64` result slot, wrong for a genuinely `bool`-valued `if` whose
    /// branches both fall through). `typeck::check_if` already proved
    /// both branches agree in type at any real value-position use, so
    /// inspecting only the `then` branch's trailing type is sound: if
    /// the program passed type checking, the `else` branch's trailing
    /// type is guaranteed to match.
    fn block_trailing_ty(&self, block: &Block, scopes: &Scopes) -> Ty {
        match block.stmts.last() {
            Some(Stmt::Expr(e)) => self.local_ty_of(e, scopes),
            _ => Ty::Unit,
        }
    }

    fn if_expr(
        &mut self,
        cond: &Expr,
        then_block: &Block,
        else_block: Option<&ElseBranch>,
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<String, CodegenError> {
        let c = self.expr(cond, scopes)?;
        let then_label = self.fresh_label("if_then");
        let else_label = self.fresh_label("if_else");
        let merge_label = self.fresh_label("if_merge");

        let result_ty = self.block_trailing_ty(then_block, scopes);
        // `unit` has no LLVM value to hold at all (`alloca void` isn't
        // legal IR) — a `unit`-valued if is only ever run for its
        // branches' side effects, so there's no slot to allocate, only
        // both branches to execute.
        let slot = if result_ty == Ty::Unit {
            None
        } else {
            let llty = llvm_ty(&result_ty)?;
            let ptr = self.fresh_reg("if_result.addr");
            self.emit_alloca(&ptr, &llty);
            Some((ptr, llty))
        };

        writeln!(self.out, "  br i1 {c}, label %{then_label}, label %{else_label}").unwrap();

        writeln!(self.out, "{then_label}:").unwrap();
        self.terminated = false;
        scopes.push();
        let then_val = self.block_value(then_block, scopes)?;
        // Each branch is its own scope with its own independent box
        // ownership — only the branch that actually runs at runtime frees
        // what it itself still owns there; the other branch's own
        // (different) still-owned set, if any, is a separate `FreeMap`
        // entry keyed by the same `if`'s span plus the other bool.
        if !self.terminated
            && let Some(names) = self.free_map.at_if_branch_end.get(&(span, true)).cloned()
        {
            self.emit_frees_for_names(&names, scopes);
        }
        scopes.pop();
        if !self.terminated {
            if let Some((ptr, llty)) = &slot {
                writeln!(self.out, "  store {llty} {then_val}, ptr {ptr}").unwrap();
            }
            writeln!(self.out, "  br label %{merge_label}").unwrap();
        }
        let then_terminated = self.terminated;

        writeln!(self.out, "{else_label}:").unwrap();
        self.terminated = false;
        let else_val = match else_block {
            Some(ElseBranch::Block(b)) => {
                scopes.push();
                let v = self.block_value(b, scopes)?;
                if !self.terminated
                    && let Some(names) = self.free_map.at_if_branch_end.get(&(span, false)).cloned()
                {
                    self.emit_frees_for_names(&names, scopes);
                }
                scopes.pop();
                v
            }
            Some(ElseBranch::If(e2)) => self.expr(e2, scopes)?,
            None => "0".to_string(),
        };
        if !self.terminated {
            if let Some((ptr, llty)) = &slot {
                writeln!(self.out, "  store {llty} {else_val}, ptr {ptr}").unwrap();
            }
            writeln!(self.out, "  br label %{merge_label}").unwrap();
        }
        let else_terminated = self.terminated;

        // The merge block is only reachable if at least one branch falls
        // through to it — if both branches unconditionally `return`,
        // there's nothing to merge, and the merge block would be dead
        // (valid but pointless) IR. Emit it regardless for simplicity;
        // `terminated` correctly reflects "both branches returned" so
        // the caller (a `let`/`return` around this `if`) won't try to
        // use a value that was never actually produced on any live path.
        writeln!(self.out, "{merge_label}:").unwrap();
        self.terminated = then_terminated && else_terminated;
        if self.terminated {
            writeln!(self.out, "  unreachable").unwrap();
            return Ok("0".to_string());
        }
        match slot {
            Some((ptr, llty)) => {
                let out = self.fresh_reg("if_val");
                writeln!(self.out, "  {out} = load {llty}, ptr {ptr}").unwrap();
                Ok(out)
            }
            None => Ok("0".to_string()), // unit; never meaningfully read
        }
    }

    fn block_value(&mut self, block: &Block, scopes: &mut Scopes) -> Result<String, CodegenError> {
        match block.stmts.split_last() {
            None => Ok("0".to_string()),
            Some((last, rest)) => {
                self.stmts(rest, scopes)?;
                if self.terminated {
                    return Ok("0".to_string()); // unreachable; value never used
                }
                match last {
                    Stmt::Expr(e) => self.expr(e, scopes),
                    other => {
                        self.stmt(other, scopes)?;
                        Ok("0".to_string())
                    }
                }
            }
        }
    }

    /// The real OS-level entry point — Nirdosha's own `main` was renamed
    /// to `@nir_main` (module doc) to avoid the clash. Exit code
    /// convention: `unit`-returning `main` exits 0; an integer-returning
    /// one truncates/extends its result to `i32`, the same "the returned
    /// value is the program's result" convention `main.rs`'s CLI already
    /// uses for the interpreter.
    fn emit_c_main(&mut self, program: &Program) -> Result<(), CodegenError> {
        let main_fn = program.fns.iter().find(|f| f.name == "main").expect("typeck.rs already required a main");
        if main_fn.ret.is_aggregate() {
            // There's no sensible "exit code" for a raw Vector/Matrix
            // the way there is for an integer/f64 result (every other
            // branch below truncates/converts to `i32`) — and nothing
            // in this phase can print one either (`call()`'s `print`
            // arm rejects an aggregate argument). Fails cleanly here
            // rather than emitting a `call {ret_ty} @nir_main()` against
            // a callee whose real LLVM signature is actually `void`
            // (sret convention) — a genuine invalid-IR mismatch this
            // guard exists specifically to prevent.
            return unsupported(
                "codegen doesn't support `main` returning a Vector/Matrix directly yet — print \
                 its elements instead of returning the aggregate itself",
            );
        }
        writeln!(self.out, "define i32 @main() {{").unwrap();
        writeln!(self.out, "entry:").unwrap();
        if main_fn.ret == Ty::Unit {
            writeln!(self.out, "  call void @nir_main()").unwrap();
            writeln!(self.out, "  ret i32 0").unwrap();
        } else if main_fn.ret == Ty::Str {
            // Same "no sensible exit code" reasoning as the aggregate
            // case above — `str` is a `{ptr, i64}` struct value, not
            // something `sext`/`trunc`/`fptosi` (the only conversions
            // this fallback knows) can turn into an `i32`. Found by
            // actually compiling a `main() -> str` program and watching
            // the fallback's `sext` arm emit `sext {ptr, i64} ... to
            // i32` — invalid IR clang rejects, exactly the failure mode
            // this guard exists to turn into a clean `CodegenError`
            // instead (same discipline as the aggregate-return guard).
            return unsupported(
                "codegen doesn't support `main` returning `str` directly yet — print it instead \
                 of returning it",
            );
        } else {
            let llty = llvm_ty(&main_fn.ret)?;
            let r = self.fresh_reg("main_result");
            writeln!(self.out, "  {r} = call {llty} @nir_main()").unwrap();
            let r32 = match llty.as_str() {
                "i64" => {
                    let t = self.fresh_reg("exit_code");
                    writeln!(self.out, "  {t} = trunc i64 {r} to i32").unwrap();
                    t
                }
                "i32" => r,
                // `sext`/`trunc` are integer-only instructions -- `f64`
                // needs its own conversion (`fptosi`, truncating toward
                // zero, the same as Rust's `as i32` would). A real,
                // previously-latent bug (this arm used to be the
                // catch-all `_ => sext`, which is simply invalid LLVM IR
                // for a `double`) -- found and fixed while adding `f64`
                // support, the same way this file's other bugs were:
                // by actually compiling a program that hit it.
                "double" => {
                    let t = self.fresh_reg("exit_code");
                    writeln!(self.out, "  {t} = fptosi double {r} to i32").unwrap();
                    t
                }
                _ => {
                    let t = self.fresh_reg("exit_code");
                    writeln!(self.out, "  {t} = sext {llty} {r} to i32").unwrap();
                    t
                }
            };
            writeln!(self.out, "  ret i32 {r32}").unwrap();
        }
        writeln!(self.out, "}}").unwrap();
        Ok(())
    }
}

/// Full pipeline from a well-typed, ownership-checked `Program` to a real
/// native executable at `output_path`: emit LLVM IR, write it to a temp
/// `.ll` file, invoke the system `clang` to assemble and link it. Returns
/// `clang`'s stderr on failure — a `CodegenError`-shaped failure (an
/// unsupported construct) is reported before `clang` ever runs, so the
/// two failure modes stay distinguishable to a caller.
/// The IR itself is unoptimized either way (module doc: "correctness over
/// cleverness," alloca everywhere) — `OptLevel` controls only whether
/// `clang` is asked to optimize *after* that, the same as it would for C
/// source. `O2` is the default `build()` uses because goal.md row 5 is
/// about hardware speed, not about this backend's own IR being clever;
/// `O0` stays available for debugging a miscompile without an optimizer
/// in the way, and — not incidentally — running the exact same IR
/// through both levels is a real stress test: LLVM treats `unreachable`
/// (this backend emits it for provably-dead code, e.g. a definitely-
/// returning function's fallthrough) as a hard guarantee and optimizes
/// aggressively around it, so a subtly wrong `unreachable` that `-O0`
/// happens not to disturb is exactly the kind of bug `-O2` would expose.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OptLevel {
    O0,
    O2,
}

impl OptLevel {
    fn clang_flag(self) -> &'static str {
        match self {
            OptLevel::O0 => "-O0",
            OptLevel::O2 => "-O2",
        }
    }
}

/// The `det`/`inv`/`solve`/`rank`/`kf_update_state`/`kf_update_cov`
/// kernels, compiled once at `nirdosha`'s own build time by `build.rs`
/// from `src/runtime_kernels.rs` and embedded here — `build()` writes
/// this out alongside the generated `.ll` file and links it in, so every
/// native binary `nirdosha build` produces carries its own copy, with no
/// runtime dependency on this compiler's installation.
static RUNTIME_KERNELS_LIB: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/libnirdosha_runtime.a"));

pub fn build(
    program: &Program,
    smt_report: &SmtReport,
    output_path: &std::path::Path,
    opt: OptLevel,
) -> Result<(), String> {
    let ir = emit_llvm_ir(program, smt_report).map_err(|e| e.to_string())?;

    // `process::id()` alone is **not** unique enough: it's identical
    // across every thread inside one process, so two concurrent `build`
    // calls in the same process (e.g. two tests running in parallel,
    // which is `cargo test`'s default) would race on the same temp file
    // — one call's IR silently overwriting or getting deleted out from
    // under the other. Found exactly this way: `cargo test`'s default
    // parallelism turned three independently-correct compiles into three
    // empty-stdout failures, not a hypothetical worry. A process-wide
    // atomic counter, combined with the pid, makes each call's filename
    // genuinely unique regardless of how many `build`s run concurrently.
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let mut ll_path = std::env::temp_dir();
    ll_path.push(format!("nirdosha_{}_{n}.ll", std::process::id()));
    std::fs::write(&ll_path, &ir).map_err(|e| format!("writing {}: {e}", ll_path.display()))?;

    let mut runtime_lib_path = std::env::temp_dir();
    runtime_lib_path.push(format!("nirdosha_runtime_{}_{n}.a", std::process::id()));
    std::fs::write(&runtime_lib_path, RUNTIME_KERNELS_LIB)
        .map_err(|e| format!("writing {}: {e}", runtime_lib_path.display()))?;

    let result = std::process::Command::new("clang")
        .arg(&ll_path)
        .arg(&runtime_lib_path)
        .arg(opt.clang_flag())
        // Phase 4's `declare double @atan2(double, double)` (geometry
        // builtins) has no LLVM intrinsic form, unlike `sqrt`/`sin`/`cos`
        // — it's the plain libm function, so it needs to actually be
        // linked. Harmless to pass unconditionally even for a program
        // that never calls it.
        .arg("-lm")
        .arg("-o")
        .arg(output_path)
        .output();
    let _ = std::fs::remove_file(&ll_path); // best-effort cleanup either way
    let _ = std::fs::remove_file(&runtime_lib_path);

    match result {
        Ok(output) if output.status.success() => Ok(()),
        Ok(output) => Err(format!(
            "clang failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        )),
        Err(e) => Err(format!("could not run `clang`: {e} (is it installed and on PATH?)")),
    }
}
