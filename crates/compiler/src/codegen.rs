//! LLVM codegen — docs/goal.md row 5 ("native, hardware-speed codegen"),
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
//! `check_supported`'s own `unsupported(...)` call sites are the
//! authoritative list of what's rejected — treat that function itself as
//! the source of truth, not this comment, which has drifted out of sync
//! with what actually compiles more than once already (see
//! `docs/LANGUAGE.md` §10's "Updated 22 Aug 2026" note for a concrete
//! example: this file's own earlier doc comment claimed `box`/`&`/`*`
//! had no codegen, when `nir_alloc`/`nir_free` (driven by
//! `ownership.rs`'s `FreeMap`) already compiled real heap alloc/free by
//! the time that claim was read again). As of this writing: every
//! scalar integer type, signed (`i8`/`i16`/`i32`/`i64`) and unsigned
//! (`u8`/`u16`/`u32`/`u64`/`usize` — `widen_to_i64`'s doc comment on why
//! unsigned needed only one small, contained change, not a parallel
//! signed/unsigned codegen path), plus `bool`, `unit`, `f64`, `str`,
//! `box`/`&`/`*`, `tcp`/`tcp_listener`, `sha256_hex`/
//! `constant_time_str_eq` (`STR_CRYPTO_BUILTINS`), and `Vector`/`Matrix`
//! (plus most of the dense-linalg/geometry/Kalman builtin surface) all
//! compile — including `print` on every one of those scalar shapes
//! (`Codegen::call`'s `Ty::Bool`/`Ty::Unit` arms handle the two that
//! used to be rejected) — and, as of Phase 4a, `struct`/`enum`/`match`
//! over **non-affine** payloads (construction, `expr.field` access, and
//! `match`'s enum-variant + literal-pattern arms). Rejected: an
//! **affine-containing** `struct`/`enum`/`match` (a `box`/`&`/`tcp`/`file`/
//! `db`/`mq` field/payload, transitively — Phase 4b; needs
//! `ownership.rs`'s `FreeMap` generalized beyond `Ty::Box`-only
//! `still_owned_boxes` plus a new `at_match_arm_end` entry), `thread`/
//! `chan`/`sandbox`/`file`/`json`/`db`/`mq`/`transact`/every Row 12
//! identity builtin, `fn(..)->..`/`acquire`/`requires(...)`, and `print`
//! on a whole `Vector`/`Matrix` argument.
//!
//! **Tier 1 vs Tier 2 finally means something.** `refine.rs` and
//! `smt.rs` both proved things and both said, explicitly, "not wired to
//! elide the runtime check — there's no backend to spend the payoff on
//! yet." There is now. A `let`/assignment whose span is in the passed-in
//! `SmtReport::proven_in_range` gets no runtime bounds check emitted at
//! all (Tier 1, silent, exactly as docs/goal.md §4 describes); one that
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

use std::collections::{HashMap, HashSet};
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

/// `sha256_hex`/`constant_time_str_eq` — also linked native calls into
/// `runtime_kernels.rs` (a from-scratch SHA-256, since that file has no
/// access to the `sha2` crate `interpreter.rs` uses — its own module doc
/// explains why), but kept as their own list rather than folded into
/// `PHASE5_BUILTINS`: that list's whole documented reason is "genuine
/// data-dependent control flow in dense linear algebra," which doesn't
/// describe these two at all (bit-manipulation over a runtime-length
/// byte buffer, not a matrix). Handled directly in `Codegen::call`
/// (like `print`), not through `call_builtin_scalar`/`call_builtin_agg`
/// — neither fits: `sha256_hex` returns `str` (not `Ty::is_aggregate()`,
/// so not `call_builtin_agg`'s convention; not a plain numeric scalar
/// either, so not `call_builtin_scalar`'s).
const STR_CRYPTO_BUILTINS: &[&str] = &["sha256_hex", "constant_time_str_eq"];

/// `rand_seed`/`rand_f64`/`rand_gaussian` — a process-wide SplitMix64/
/// Box-Muller stream in `runtime_kernels.rs` (its own module doc on why
/// this needed real RNG *state* in generated code, the one thing that
/// was actually missing before — the algorithm itself is a small, pure
/// function, same class as `sha256_hex`). Its own list, not folded into
/// `STR_CRYPTO_BUILTINS`, for the same "describes something different"
/// reason that one isn't folded into `PHASE5_BUILTINS`.
const RAND_BUILTINS: &[&str] = &["rand_seed", "rand_f64", "rand_gaussian"];

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
/// `interpreter.rs`'s `Value::Matrix` storage exactly); a non-affine
/// `struct`/`enum` instantiation (`Ty::Named`) maps to a real named LLVM
/// type (`declare_named_type`'s doc — this function only ever returns
/// its *name*, `%Point`/`%Result$i64$str`; the actual `%Name = type
/// {...}` declaration is a separate, `&mut self`-requiring step, see
/// `Codegen::llvm_ty`/`Codegen::declare_named_type`); everything else is
/// rejected — see module doc for exactly what and why. Returns an owned
/// `String`, not `&'static str`, because an aggregate's type string
/// depends on its compile-time-known but not statically-fixed length.
///
/// True iff every affine value reachable inside `ty` can be torn down
/// by the current codegen runtime. Only `box` (via `nir_free`) and
/// `tcp`/`tcp_listener` (via `nir_tcp_stop`) are supported today; any
/// other affine leaf (`thread`, `sandbox`, `file`, `db`, `mq`) or a
/// struct/enum containing one keeps the whole type rejected.
fn affine_codegen_supported(registry: &TypeRegistry, ty: &Ty) -> bool {
    let mut visiting = Vec::new();
    affine_codegen_supported_visiting(registry, ty, &mut visiting)
}

/// `affine_codegen_supported`'s real recursion, guarded the same
/// "track names on the current path, a repeat is the cycle" way
/// `TypeRegistry::is_affine_visiting` is (`ast.rs`). Needed once
/// `check_supported`'s `has_cyclic_layout` guard started letting a
/// genuinely-finite `box`-indirected self-reference through (a
/// `struct Node { next: box Node }` cons-list shape, same as any real
/// language's) — that recursion crosses back into `Ty::Named` through
/// the `Ty::Box` arm below with no size limit of its own, so without a
/// `visiting` set it walks `Node -> box Node -> Node -> ...` forever,
/// a real stack overflow confirmed empirically the same way
/// `is_affine`'s own cycle bug was. A repeat on the path is sound to
/// treat as "supported" here (not "unsupported", unlike
/// `is_affine_visiting`'s `false`): the cycle is only reachable at all
/// because every step across it was a `box` — the one affine leaf this
/// function already tears down via `nir_free` — so a second pass over
/// the same name can only re-confirm what the first pass already
/// found, never turn up a new unsupported leaf.
fn affine_codegen_supported_visiting(registry: &TypeRegistry, ty: &Ty, visiting: &mut Vec<String>) -> bool {
    if !registry.is_affine(ty) {
        return true;
    }
    match ty {
        Ty::Box(inner) => affine_codegen_supported_visiting(registry, inner, visiting),
        Ty::Tcp | Ty::TcpListener => true,
        Ty::Named(name, args) => {
            if visiting.iter().any(|v| v == name.as_str()) {
                return true;
            }
            if let Some(fields) = registry.struct_fields(name) {
                let type_params = registry.struct_type_params(name).unwrap_or(&[]);
                let subst = zip_type_params(type_params, args);
                visiting.push(name.clone());
                let result = fields
                    .iter()
                    .all(|f| affine_codegen_supported_visiting(registry, &substitute_ty(&f.ty, &subst), visiting));
                visiting.pop();
                result
            } else if let Some(variants) = registry.enum_variants(name) {
                let type_params = registry.enum_type_params(name).unwrap_or(&[]);
                let subst = zip_type_params(type_params, args);
                visiting.push(name.clone());
                let result = variants.iter().all(|v| {
                    v.payload
                        .iter()
                        .all(|t| affine_codegen_supported_visiting(registry, &substitute_ty(t, &subst), visiting))
                });
                visiting.pop();
                result
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Pure and stateless on purpose — no declaration emission, no mutable
/// state — so it's cheaply callable both from `check_supported`'s
/// pre-pass (which runs before any `Codegen` exists) and from every real
/// codegen call site (via `Codegen::llvm_ty`, the `&mut self` wrapper
/// that also ensures the real declaration gets emitted for a
/// `Ty::Named`).
fn llvm_ty(ty: &Ty, registry: &TypeRegistry) -> Result<String, CodegenError> {
    match ty {
        Ty::I8 => Ok("i8".to_string()),
        Ty::I16 => Ok("i16".to_string()),
        Ty::I32 => Ok("i32".to_string()),
        Ty::I64 => Ok("i64".to_string()),
        Ty::Bool => Ok("i1".to_string()),
        Ty::Unit => Ok("void".to_string()),
        // Same bit widths as their signed counterparts -- LLVM has no
        // separate unsigned integer *type*, only separate *instructions*
        // for the operations that actually care (`icmp`/`div`/`rem`, and
        // widening a narrower value up to i64). See
        // `Codegen::widen_to_i64`'s doc comment for why that's the one
        // and only place this backend needs the signed-vs-unsigned
        // choice: every intermediate value is computed at `i64` width
        // (module doc), and `Ty::bounds()` already caps every unsigned
        // type's legal range at `[0, i64::MAX]` — so once a value is
        // correctly widened (`zext`, not `sext`), plain/signed `+`/`-`/
        // `*`/`icmp`/`div` at `i64` width give byte-identical results to
        // their unsigned counterparts (both interpretations agree on any
        // bit pattern whose sign bit is clear, which a validly-widened
        // unsigned value's always is) -- confirmed by actually compiling
        // and running comparison/division/boundary-value programs for
        // every one of `u8`/`u16`/`u32`/`u64`/`usize`, not just reasoned
        // about.
        Ty::U8 => Ok("i8".to_string()),
        Ty::U16 => Ok("i16".to_string()),
        Ty::U32 => Ok("i32".to_string()),
        Ty::U64 | Ty::Usize => Ok("i64".to_string()),
        // A single heap (`Box`) or borrowed (`Ref`) pointer — one word,
        // passed by value exactly like `Ty::Str`'s `{ptr, i64}` above,
        // just narrower. `Ref` needs no allocation of its own (it's
        // always just an existing binding's own storage address —
        // `Codegen::expr`'s `Expr::Ref` arm). `Box` allocates on the heap
        // at construction (`nir_alloc`, `Expr::Box`'s own codegen) and is
        // freed for real: `ownership.rs`'s `FreeMap` records each
        // binding's last use, and `emit_frees_for_names`/`emit_affine_free`
        // emit `nir_free` for it at every scope-closing point that data
        // lists — confirmed in generated IR (`nirdosha emit-llvm`) for a
        // simple `let b: box i64 = ...` case: the `nir_free` call is
        // really there, right after `b`'s last use.
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
        Ty::Dec128 => unsupported(
            "codegen doesn't support `dec128` yet — decimal arithmetic is interpreter-only for now (docs/LANGUAGE.md §6c/§6d)",
        ),
        Ty::Json => unsupported("codegen doesn't support `json` yet — JSON is interpreter-only for now"),
        Ty::Db => unsupported("codegen doesn't support `db` yet — DB connectivity is interpreter-only for now"),
        Ty::Handle(kind) => unsupported(&format!(
            "codegen doesn't support plugin handle types (`{kind}`) — plugins are interpreter-only for now"
        )),
        Ty::Mq => unsupported("codegen doesn't support `mq` yet — message-queue connectivity is interpreter-only for now"),
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
            let elem_llty = llvm_ty(elem, registry)?;
            Ok(format!("[{len} x {elem_llty}]"))
        }
        // Phase B1 (str/tcp codegen plan): a `tcp`/`tcp_listener` handle
        // is just a raw OS file descriptor — the kernel already tracks
        // everything a "handle" needs, so no separate handle table is
        // needed the way `Value::Tcp`'s `Arc<Mutex<Option<..>>>` wrapper
        // gives the interpreter. Both lower to the same `i64` regardless
        // of which one they are (`nir_tcp_stop` closes either uniformly).
        Ty::Tcp | Ty::TcpListener => Ok("i64".to_string()),
        // Row 11: a `struct`/`enum` instantiation lowers to a real named
        // LLVM type — `%Point`, or `%Result$i64$str` for a generic
        // instantiation (`mangle_ty` gives every distinct concrete
        // instantiation its own distinct name, since their layouts
        // genuinely differ). An affine-containing instantiation is allowed
        // as long as every affine leaf inside it is one this backend can
        // actually tear down today (`box` via `nir_free`, `tcp`/
        // `tcp_listener` via `nir_tcp_stop`); if it contains an unsupported
        // affine leaf (`thread`, `sandbox`, `file`, `db`, `mq`, etc.), the
        // whole type is still rejected cleanly rather than mis-compiled.
        Ty::Named(name, _) => {
            if registry.is_affine(ty) && !affine_codegen_supported(registry, ty) {
                return unsupported(format!(
                    "codegen doesn't support `{name}` yet — it (transitively) contains an \
                     affine field of a type that has no native codegen yet (`thread`/`sandbox`/\
                     `file`/`db`/`mq`/etc.); a struct/enum whose only affine fields are `box` \
                     or `tcp` compiles now"
                ));
            }
            Ok(format!("%{}", mangle_ty(ty)))
        }
        Ty::Fn(_, _) => unsupported(
            "codegen doesn't support `fn(..)->..` yet — first-class/privileged functions \
             (requires/acquire) are interpreter-only for now",
        ),
        Ty::Error => unreachable!("a program with a type error is never handed to codegen"),
    }
}

/// A short, identifier-safe token for `ty`, used to name a generic
/// struct/enum instantiation's own LLVM named type distinctly per
/// concrete instantiation — `Result(i64, str)` and `Result(str, str)`
/// need different LLVM types, since their layouts differ, even though
/// they share one declaration. Only ever needs to handle the *non-affine*
/// `Ty` subset: `llvm_ty`'s `Ty::Named` arm already rejects an
/// affine-containing instantiation before this can run on one, and no
/// affine type (`Box`/`Tcp`/etc.) can otherwise legally appear as a
/// generic type argument here. Returns the name *without* a leading `%`
/// sigil (so a nested `Ty::Named` argument mangles into the middle of
/// the string without an illegal embedded sigil) — callers that need the
/// real LLVM identifier prepend `%` themselves (`llvm_ty`'s `Ty::Named`
/// arm, `declare_named_type`).
fn mangle_ty(ty: &Ty) -> String {
    match ty {
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
        Ty::F64 => "f64".to_string(),
        Ty::Str => "str".to_string(),
        Ty::Vector(elem, n) => format!("vec{n}_{}", mangle_ty(elem)),
        Ty::Matrix(elem, r, c) => format!("mat{r}x{c}_{}", mangle_ty(elem)),
        Ty::Named(name, args) if args.is_empty() => name.clone(),
        Ty::Named(name, args) => {
            format!("{name}${}", args.iter().map(mangle_ty).collect::<Vec<_>>().join("$"))
        }
        // Every other `Ty` (`Box`/`Ref`/`Thread`/`Channel`/`Sandbox`/
        // `Tcp`/`TcpListener`/`File`/`Json`/`Db`/`Mq`/`Fn`/`Error`) is
        // either affine (already rejected before this can run on one) or
        // otherwise never legally a struct/enum generic type argument —
        // this arm is a defensive fallback, not expected to actually run
        // for any program that reaches this point.
        _ => ty.name().replace(['(', ')', ',', ' '], "_"),
    }
}

/// A conservative, always-safe-to-over-allocate word count (8 bytes
/// each) for `ty` — used only to size an enum's raw `[N x i64]` payload
/// buffer (`declare_named_type`'s enum branch). Rounds every field up to
/// a whole 8-byte word rather than hand-replicating LLVM's exact
/// alignment rules, so the buffer is never undersized regardless of what
/// LLVM's real struct layout would have chosen for a synthesized
/// variant-payload type — a few wasted bytes in the rare case a variant
/// packs several sub-8-byte fields, in exchange for *never* under-sizing
/// the payload buffer, consistent with this codebase's repeatedly-stated
/// "correctness over cleverness" bias. Every field type this language
/// has needs at most 8-byte alignment, so this always produces a
/// correctly-aligned offset for every field (`declare_named_type`'s own
/// doc has the full reasoning).
fn conservative_word_count(ty: &Ty, registry: &TypeRegistry) -> u64 {
    match ty {
        Ty::Str => 2,
        Ty::Vector(_, _) | Ty::Matrix(_, _, _) => {
            let (_, _, bytes) = agg_layout(ty);
            bytes.div_ceil(8)
        }
        Ty::Named(name, args) => {
            if let Some(fields) = registry.struct_fields(name) {
                let subst = zip_type_params(registry.struct_type_params(name).unwrap_or(&[]), args);
                fields.iter().map(|f| conservative_word_count(&substitute_ty(&f.ty, &subst), registry)).sum()
            } else if let Some(variants) = registry.enum_variants(name) {
                let subst = zip_type_params(registry.enum_type_params(name).unwrap_or(&[]), args);
                1 + variants
                    .iter()
                    .map(|v| v.payload.iter().map(|t| conservative_word_count(&substitute_ty(t, &subst), registry)).sum::<u64>())
                    .max()
                    .unwrap_or(0)
            } else {
                unreachable!("typeck.rs already proved this Ty::Named resolves to a struct or enum")
            }
        }
        // Every other `Ty` this fn can legally be reached with under
        // Phase 4a's non-affine scope (`llvm_ty`'s affine check already
        // rejected anything containing `Box`/`Tcp`/etc.) is one scalar
        // word or less (`Unit` needs zero, but rounding it up to one
        // costs nothing and keeps this fn's contract simple: "at most
        // this many words").
        _ => 1,
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

/// The heap-allocation size (in bytes, as an LLVM `i64` operand — see
/// below) `box e` needs for a value of type `ty` — unlike
/// `elem_byte_size` (scalars only, for aggregate elements) or
/// `agg_layout` (`Vector`/`Matrix` only), this covers every `Ty` `box`
/// can legally wrap (ast.rs: "any type, recursively" — `box i64`, `box
/// box i64`, `box Vector(f64,3)`, `box Point{..}`, etc.), since `box`
/// itself has no such restriction. Matches `llvm_ty`'s own type-to-size
/// story exactly: `Vector`/`Matrix` use their flat byte layout, a
/// non-affine `Ty::Named` (Row 11) uses `agg_byte_size_operand`'s
/// sizeof-via-GEP constant expression, `Ty::Str` is the two-word `{ptr,
/// i64}` value (16 bytes), every pointer-shaped handle (`Box`, `Ref`,
/// and — once later phases compile them — `Thread`/`Channel`/`Sandbox`/
/// `Tcp`/`TcpListener`) is one pointer-word (8 bytes on every platform
/// this backend targets), and everything else is a plain scalar via
/// `elem_byte_size`.
///
/// Returns a `String` operand, not a plain `u64`, for the same reason
/// `agg_byte_size_operand` does: a `Ty::Named`'s real size isn't a
/// number this side can compute without risking disagreement with
/// LLVM's own struct-layout/alignment rules, so it's expressed as an
/// LLVM constant expression instead and left for LLVM itself to
/// evaluate. Every other arm still produces a plain integer literal
/// (unchanged from before), which is exactly as valid an `i64` operand
/// as the constant expression is.
fn ty_byte_size(ty: &Ty, registry: &TypeRegistry) -> String {
    match ty {
        Ty::Vector(_, _) | Ty::Matrix(_, _, _) | Ty::Named(_, _) => agg_byte_size_operand(ty, registry),
        Ty::Str => "16".to_string(),
        Ty::Box(_) | Ty::Ref(_) | Ty::Thread(_) | Ty::Channel(_) | Ty::Sandbox | Ty::Tcp | Ty::TcpListener | Ty::File => {
            "8".to_string()
        }
        Ty::Unit => "0".to_string(),
        _ => elem_byte_size(ty).to_string(),
    }
}

/// `(element Ty, flat element count, total byte size)` for a `Vector`/
/// `Matrix` type — everything `llvm.memcpy`-based aggregate codegen
/// (function prologue copy-in, `let`/assignment copies, literal row
/// copies) needs in one call. `Ty::Named` (Row 11) deliberately doesn't
/// go through this — see `agg_byte_size_operand`.
fn agg_layout(ty: &Ty) -> (&Ty, usize, u64) {
    let (elem, len) = agg_elem_and_len(ty);
    let bytes = len as u64 * elem_byte_size(elem);
    (elem, len, bytes)
}

/// The byte-size *operand text* for `ty` (`Ty::is_aggregate()` only) —
/// used everywhere an aggregate's whole-value byte count is needed as an
/// `llvm.memcpy`/`nir_alloc` operand. `Vector`/`Matrix` keep using
/// `agg_layout`'s plain integer literal exactly as before this function
/// existed. `Ty::Named` has no such literal available on the Rust side
/// without hand-replicating LLVM's own struct-layout/alignment rules (a
/// real under-sizing risk if that math ever drifted from LLVM's actual
/// choice) — instead this emits the standard LLVM "sizeof via
/// GEP-of-null-plus-one" constant expression, which LLVM itself computes
/// correctly from the real declared type: `ptrtoint (ptr getelementptr
/// (%Name, ptr null, i32 1) to i64)`. Constant-foldable, so it costs
/// nothing extra after `-O2` — the same "trust `clang -O2` to fold
/// constant-shaped IR" precedent already used to justify a plain
/// `llvm.memcpy` over hand-unrolled loads/stores (module doc).
fn agg_byte_size_operand(ty: &Ty, registry: &TypeRegistry) -> String {
    match ty {
        Ty::Vector(_, _) | Ty::Matrix(_, _, _) => {
            let (_, _, bytes) = agg_layout(ty);
            bytes.to_string()
        }
        Ty::Named(_, _) => {
            let llty = llvm_ty(ty, registry).expect("check_supported already validated this type");
            format!("ptrtoint (ptr getelementptr ({llty}, ptr null, i32 1) to i64)")
        }
        _ => unreachable!("only called on Ty::is_aggregate() types"),
    }
}

/// Structural pre-check: walks the whole program and rejects, with a
/// specific reason, anything `llvm_ty` or the `print`-argument rule
/// would reject — run once, up front, so codegen itself can assume every
/// type/expression it encounters is in the supported subset.
///
/// Row 11 (`struct`/`enum`/`match`) is handled the same way every other
/// type-level restriction here is: `llvm_ty`, threaded a `TypeRegistry`
/// built fresh for this pre-pass (this runs before any real `Codegen`
/// exists, so it can't reuse one), rejects any *affine-containing*
/// `Ty::Named` reaching a param/`let`/return type — `program.enums` is
/// never actually empty (`Option`/`Result` are injected into every
/// program at parse time, `ast::prelude_enums`'s doc comment), but a
/// non-affine struct/enum is now genuinely supported, so there's nothing
/// left here that needs a bespoke ctor-name check the way there used to
/// be (a struct/variant construction is syntactically just `Expr::Call`,
/// walked exactly like any other call below).
/// True iff `ty`, expanded through direct (non-pointer) struct/enum field
/// containment, transitively contains itself — the shape LLVM itself
/// refuses at codegen time ("identified structure type 'X' is recursive":
/// an infinite-size aggregate). `Box`/`Ref`/`Thread`/`Channel`/`Sandbox`/
/// `Tcp`/`TcpListener`/`File`/`Fn` fields break the cycle on purpose —
/// they're a fixed-size handle regardless of what's behind them, the same
/// "pointer, not inline storage" reasoning `llvm_ty`'s `Ty::Box`/`Ty::Ref`
/// arm documents — so only a field inlined directly into the aggregate's
/// own byte layout can make it infinitely sized. `Vector`/`Matrix`
/// inline their element `n`/`r*c` times (`llvm_ty`'s own `[len x elem]`
/// lowering), so an element cycle is exactly as fatal as a struct-field
/// one and is walked the same way. Mirrors
/// `TypeRegistry::is_affine_visiting`'s "track names on the current path,
/// a repeat is the cycle" shape (`ast.rs`, same 2026-08-27 pass this
/// guards the codegen-only half of).
fn has_cyclic_layout(ty: &Ty, registry: &TypeRegistry, visiting: &mut Vec<String>) -> bool {
    match ty {
        Ty::Named(name, args) => {
            if visiting.iter().any(|v| v == name.as_str()) {
                return true;
            }
            if let Some(fields) = registry.struct_fields(name) {
                let subst = zip_type_params(registry.struct_type_params(name).unwrap_or(&[]), args);
                visiting.push(name.clone());
                let result = fields.iter().any(|f| has_cyclic_layout(&substitute_ty(&f.ty, &subst), registry, visiting));
                visiting.pop();
                result
            } else if let Some(variants) = registry.enum_variants(name) {
                let subst = zip_type_params(registry.enum_type_params(name).unwrap_or(&[]), args);
                visiting.push(name.clone());
                let result = variants
                    .iter()
                    .any(|v| v.payload.iter().any(|t| has_cyclic_layout(&substitute_ty(t, &subst), registry, visiting)));
                visiting.pop();
                result
            } else {
                // Unknown name -- typeck.rs reports this separately; no
                // cycle to report from here.
                false
            }
        }
        Ty::Vector(elem, _) | Ty::Matrix(elem, _, _) => has_cyclic_layout(elem, registry, visiting),
        // Every other `Ty` is either a plain scalar (nothing to recurse
        // into) or a pointer-ish handle whose own size never depends on
        // what it points to, so it can't be part of an infinite-size
        // cycle regardless of what's behind it.
        _ => false,
    }
}

pub fn check_supported(program: &Program) -> Result<(), CodegenError> {
    check_supported_with_plugins(program, &std::collections::HashSet::new())
}

/// Same as [`check_supported`], plus a set of plugin builtin names
/// (`docs/ROADMAP.md` Track G, G1 / rfcs/0003-plugin-abi-v2.md's plugin
/// gallery) to reject explicitly rather than silently falling through.
/// Before this existed, a plugin builtin's name matched neither
/// `is_builtin` (deliberately excluded from `ast::BUILTIN_NAMES`) nor
/// any rejection arm here, so `check_expr`'s `Expr::Call` case walked
/// straight past it into the argument loop and returned `Ok(())` for
/// the call itself — meaning the moment `typecheck_with_plugins` made
/// a plugin call pass typechecking, it would reach real codegen
/// (`Codegen::call`) with no matching entry in either the builtin or
/// user-fn tables, an untested "unknown function" path rather than a
/// clean, named rejection. `check_supported` itself keeps its exact
/// existing signature (an empty `plugin_names` set) — its one real
/// caller (`emit_llvm_ir`) is unaffected until a future plugin-aware
/// `build`/`emit-llvm` path (not yet wired up — plugins are
/// interpreter-only for the CLI today) calls this sibling instead.
pub fn check_supported_with_plugins(
    program: &Program,
    plugin_names: &std::collections::HashSet<String>,
) -> Result<(), CodegenError> {
    // Real namespacing/`pub`/`use` (`docs/ROADMAP.md` Track F, F2;
    // `docs/NEXT_GEN.md` §F2) isn't ported to the compiled path yet — same
    // incremental-porting pattern Track B already uses for `transact`/
    // `db`/`json`/`mq`/etc. A namespaced (`module Ident { ... }`)
    // declaration's own `name` is deliberately left unmangled (`ast::
    // StructDecl::ns`'s doc comment: every pre-F2 consumer, including
    // this one, reads `.name` directly), so two such declarations in
    // different modules sharing a bare name would silently collide as
    // the exact same unmangled LLVM symbol if this ever reached real
    // codegen — rejected up front, honestly, rather than risking a
    // miscompile. A program with no real (`ns: Some(_)`) declaration is
    // completely unaffected — every existing `.nir` program, and every
    // legacy string-named `module "Display Name" { ... }` block.
    if let Some(f) = program.fns.iter().find(|f| f.ns.is_some()) {
        return unsupported(format!(
            "`{}` is declared inside a real `module {} {{ ... }}` namespace -- modules/`pub`/`use` \
             aren't supported by the compiled path (`nirdosha build`/`emit-llvm`) yet, only the \
             interpreter (`nirdosha <file>`/`serve`)",
            f.name,
            f.ns.as_deref().unwrap_or("")
        ));
    }
    if let Some(s) = program.structs.iter().find(|s| s.ns.is_some()) {
        return unsupported(format!(
            "`{}` is declared inside a real `module {} {{ ... }}` namespace -- modules/`pub`/`use` \
             aren't supported by the compiled path yet, only the interpreter",
            s.name,
            s.ns.as_deref().unwrap_or("")
        ));
    }
    if let Some(e) = program.enums.iter().find(|e| e.ns.is_some()) {
        return unsupported(format!(
            "`{}` is declared inside a real `module {} {{ ... }}` namespace -- modules/`pub`/`use` \
             aren't supported by the compiled path yet, only the interpreter",
            e.name,
            e.ns.as_deref().unwrap_or("")
        ));
    }
    let registry = TypeRegistry::build(program);
    // Reject a cyclic struct/enum *declaration* itself, before any
    // function signature or body is even walked — the same "reject,
    // don't leak the backend's own error text" standard every other
    // `check_supported` rejection already holds itself to, rather than
    // letting `struct A { b: B } struct B { a: A }` reach real LLVM
    // codegen and surface a raw `clang`/LLVM "identified structure type
    // is recursive" error. `box`/`&` back-references remain fine (they
    // don't recurse past a pointer field — see `has_cyclic_layout`'s doc
    // comment), so this only fires on a genuinely infinite-size shape.
    for s in &program.structs {
        let mut visiting = vec![s.name.clone()];
        if s.fields.iter().any(|f| has_cyclic_layout(&f.ty, &registry, &mut visiting)) {
            return unsupported(format!(
                "`{}` is a cyclic struct type -- one of its fields eventually contains `{}` \
                 again with no `box`/`&` indirection in between, which has no finite size; wrap \
                 the back-reference in `box {}` (or `&{}`) to break the cycle",
                s.name, s.name, s.name, s.name
            ));
        }
    }
    for e in &program.enums {
        let mut visiting = vec![e.name.clone()];
        if e.variants.iter().any(|v| v.payload.iter().any(|t| has_cyclic_layout(t, &registry, &mut visiting))) {
            return unsupported(format!(
                "`{}` is a cyclic enum type -- one of its variants eventually contains `{}` \
                 again with no `box`/`&` indirection in between, which has no finite size; wrap \
                 the back-reference in `box {}` (or `&{}`) to break the cycle",
                e.name, e.name, e.name, e.name
            ));
        }
    }
    for f in &program.fns {
        for p in &f.params {
            llvm_ty(&p.ty, &registry)?;
        }
        llvm_ty(&f.ret, &registry)?;
        check_stmts(&f.body.stmts, plugin_names, &registry)?;
    }
    Ok(())
}

fn check_stmts(stmts: &[Stmt], plugin_names: &std::collections::HashSet<String>, registry: &TypeRegistry) -> Result<(), CodegenError> {
    for s in stmts {
        check_stmt(s, plugin_names, registry)?;
    }
    Ok(())
}

fn check_stmt(s: &Stmt, plugin_names: &std::collections::HashSet<String>, registry: &TypeRegistry) -> Result<(), CodegenError> {
    match s {
        Stmt::Let { ty, value, .. } => {
            llvm_ty(ty, registry)?;
            check_expr(value, plugin_names, registry)
        }
        Stmt::Return { value: Some(e), .. } => check_expr(e, plugin_names, registry),
        Stmt::Return { value: None, .. } => Ok(()),
        Stmt::While { cond, body, .. } => {
            check_expr(cond, plugin_names, registry)?;
            check_stmts(&body.stmts, plugin_names, registry)
        }
        Stmt::Expr(e) => check_expr(e, plugin_names, registry),
        // `audited` only suppresses guard *emission* (`Codegen::audited`,
        // checked inside `guard_in_range`/the division trap) -- every
        // statement inside still has to be otherwise codegen-supported,
        // so this walks in exactly like `While`'s body.
        Stmt::Audited { body, .. } => check_stmts(body, plugin_names, registry),
    }
}

fn check_expr(e: &Expr, plugin_names: &std::collections::HashSet<String>, registry: &TypeRegistry) -> Result<(), CodegenError> {
    match e {
        Expr::Int(_, _) | Expr::Bool(_, _) | Expr::Ident(_, _) | Expr::Float(_, _) | Expr::Str(_, _) => Ok(()),
        Expr::Unary(_, inner, _) => check_expr(inner, plugin_names, registry),
        Expr::Binary(_, l, r, _) => {
            check_expr(l, plugin_names, registry)?;
            check_expr(r, plugin_names, registry)
        }
        Expr::Call(name, args, _) => {
            // A struct/variant constructor call is syntactically just
            // `Expr::Call` (no dedicated AST node) — nothing special to
            // check here beyond its arguments, same as any other call;
            // `Codegen::construct`/`expr_ptr`'s real handling is where
            // the actual construction codegen lives.
            if name == "print" {
                // No syntactic rejection needed here any more: `print`
                // now handles every scalar shape (`Codegen::call`'s
                // `Ty::Bool`/`Ty::Unit` arms) — the one real remaining
                // rejection, a `Vector`/`Matrix` argument, needs real
                // type info this purely-syntactic pre-pass doesn't have,
                // so it's caught later in `Codegen::call` itself (the
                // existing `arg_ty.is_aggregate()` check there), same as
                // before. Each argument still gets walked for its own
                // recursive validity by the shared loop below.
            } else if is_builtin(name)
                && !PHASE4_BUILTINS.contains(&name.as_str())
                && !PHASE5_BUILTINS.contains(&name.as_str())
                && !STR_CRYPTO_BUILTINS.contains(&name.as_str())
                && !RAND_BUILTINS.contains(&name.as_str())
            {
                // Every builtin not in `PHASE4_BUILTINS` (unrolled IR),
                // `PHASE5_BUILTINS`/`STR_CRYPTO_BUILTINS` (linked runtime
                // call), or `RAND_BUILTINS` (linked call into a
                // process-wide RNG stream) is rejected here with a
                // specific reason rather than falling through to
                // `check_expr`'s per-argument walk, which would report a
                // less specific one.
                return unsupported(format!(
                    "codegen doesn't support `{name}` yet — this builtin is interpreter-only \
                     for now (numeric codegen lands in a later phase)"
                ));
            } else if plugin_names.contains(name) {
                // rfcs/0003-plugin-abi-v2.md: a plugin builtin's `call`
                // is an opaque `Arc<dyn Fn>` with no stable calling
                // convention into generated LLVM IR — interpreter-only,
                // permanently, not "not yet" the way a real numeric
                // builtin above might be. Named and rejected explicitly
                // here rather than falling through to `Codegen::call`'s
                // user-fn lookup, which has no entry for a plugin name
                // (plugins are never part of `program.fns`) and would
                // hit an untested "unknown function" path instead of a
                // clean, actionable error.
                return unsupported(format!(
                    "codegen doesn't support plugin builtin `{name}` yet — plugin calls are \
                     interpreter-only; `nirdosha build`/`emit-llvm` can't link an opaque Rust \
                     closure into generated native code without a stable C-ABI plugin-calling \
                     convention, which doesn't exist yet"
                ));
            }
            for a in args {
                check_expr(a, plugin_names, registry)?;
            }
            Ok(())
        }
        Expr::If { cond, then_block, else_block, .. } => {
            check_expr(cond, plugin_names, registry)?;
            check_stmts(&then_block.stmts, plugin_names, registry)?;
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => check_stmts(&b.stmts, plugin_names, registry),
                Some(ElseBranch::If(e2)) => check_expr(e2, plugin_names, registry),
                None => Ok(()),
            }
        }
        Expr::Assign(_, rhs, _) => check_expr(rhs, plugin_names, registry),
        // Row 11: both now recurse structurally, same "walk, don't
        // reject" treatment every other now-supported construct gets —
        // real type-directed validation (the affine check) happens where
        // `Codegen`'s own methods can see a base/scrutinee's actual
        // resolved type (`Codegen::field_access`/`Codegen::match_expr`),
        // not in this purely-syntactic pre-pass.
        Expr::FieldAccess(base, _, _) => check_expr(base, plugin_names, registry),
        Expr::Match { scrutinee, arms, .. } => {
            check_expr(scrutinee, plugin_names, registry)?;
            for arm in arms {
                check_expr(&arm.body, plugin_names, registry)?;
            }
            Ok(())
        }
        // `box`/`*`/`&` land as of this phase — see `llvm_ty`'s
        // `Ty::Box`/`Ty::Ref` arm and `Codegen::expr`'s real `Expr::Box`/
        // `Expr::Deref`/`Expr::Ref` arms for the actual codegen. This
        // structural pre-pass has no type info (that's `local_ty_of`'s
        // job, at real IR-gen time), so it just recurses into whatever's
        // inside — same "walk, don't reject" treatment every other
        // already-supported unary-ish construct gets.
        Expr::Box(inner, _) | Expr::Deref(inner, _) | Expr::Ref(inner, _) => check_expr(inner, plugin_names, registry),
        Expr::Spawn(_, _, _) | Expr::Join(_, _) => {
            unsupported("codegen doesn't support `spawn`/`join` yet — interpreter-only for now")
        }
        Expr::Acquire(_, _, _) => unsupported(
            "codegen doesn't support `acquire` yet — first-class/privileged functions \
             are interpreter-only for now",
        ),
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
            check_expr(chan, plugin_names, registry)?;
            check_expr(value, plugin_names, registry)
        }
        Expr::Recv(chan, _) => check_expr(chan, plugin_names, registry),
        // Same reasoning as `send`/`recv` above: `stop` is one AST node
        // (`Expr::StopSandbox`) shared by `sandbox`/`tcp`/`tcp_listener`,
        // dispatched on the operand's type — `sandbox` itself stays
        // rejected (unsupported below), `stop` recurses structurally so
        // `Codegen::expr` can accept it for a `Ty::Tcp`/`Ty::TcpListener`
        // operand and reject it for `Ty::Sandbox` with real type info.
        Expr::SpawnSandbox(_, _, _) => {
            unsupported("codegen doesn't support `sandbox` yet — interpreter-only for now")
        }
        Expr::StopSandbox(inner, _) => check_expr(inner, plugin_names, registry),
        // `open`/file I/O is interpreter-only for now, same treatment
        // `Expr::Chan`/`Expr::SpawnSandbox` above get — no type info at
        // this structural pre-pass, so straight rejection rather than
        // recursing into a program that's unsupported regardless.
        Expr::Open(_, _, _) => {
            unsupported("codegen doesn't support `open`/file I/O yet — interpreter-only for now")
        }
        Expr::Connect(host, port, _) => {
            check_expr(host, plugin_names, registry)?;
            check_expr(port, plugin_names, registry)
        }
        Expr::Listen(port, _) => check_expr(port, plugin_names, registry),
        Expr::Accept(listener, _) => check_expr(listener, plugin_names, registry),
        // Vector/Matrix indexing lands as of this phase — the base and
        // every index expression are walked the same way `ArrayLit`'s
        // elements are, so a still-unsupported construct nested inside
        // either (e.g. a `.*` index expression) keeps its own specific
        // rejection reason instead of being silently accepted because
        // `Expr::Index` itself is now fine.
        Expr::Index(base, indices, _) => {
            check_expr(base, plugin_names, registry)?;
            for idx in indices {
                check_expr(idx, plugin_names, registry)?;
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
                check_expr(e, plugin_names, registry)?;
            }
            Ok(())
        }
        // `docs/TRANSACT.md`'s own decision: "Compiled backend (codegen.rs) is
        // out of scope until the interpreter version is proven" — same
        // "reject, don't mis-compile" treatment every other unimplemented
        // construct gets (`thread`, `chan`, `sandbox`, `struct`/`enum`/
        // `match`, `db`/`mq`/`json` — see `docs/LANGUAGE.md` §10 for the
        // current list; `box`/`tcp` compile now, so they've dropped off
        // it). `transact` joins the still-unsupported list, not an
        // exception to it.
        Expr::Transact { .. } => {
            unsupported("codegen doesn't support `transact` yet — interpreter-only for now")
        }
    }
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
/// doc / docs/PHASE0.md's write-up of this milestone).
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
    /// their guard entirely when it's set (docs/goal.md §4's Tier-3 escape
    /// hatch). A plain `bool`, not a depth counter: nested `audited`
    /// blocks don't need their own count, only "is at least one
    /// enclosing scope audited" — restored to its prior value (not
    /// unconditionally cleared) on exit so a nested `audited` inside a
    /// non-audited function correctly re-enables guards afterward, and
    /// one written (redundantly) inside an already-audited block doesn't
    /// prematurely turn guards back on when *it* exits.
    audited: bool,
    /// The program's struct/enum declaration table — built once at the
    /// start of `emit_llvm_ir` and consulted by every Row 11 codegen path
    /// (`llvm_ty`'s struct/enum arms, `construct`, `match_expr`, field
    /// access) to resolve a `Ty::Named`'s fields/payloads/variants and to
    /// re-check affinity (the free `llvm_ty`/`check_supported` pre-pass
    /// builds its own throwaway copy, since it runs before this `Codegen`
    /// exists). Borrows from `program` for the same lifetime `smt_report`
    /// does — see `emit_llvm_ir`'s `'a` unification.
    registry: TypeRegistry<'a>,
    /// Every distinct concrete `struct`/`enum` instantiation already
    /// emitted as a real LLVM named-type declaration (`%Point = type
    /// {...}`), keyed by mangled name — so `declare_named_type` declares
    /// each one exactly once even when many call sites construct the
    /// same instantiation. Built up over the whole `emit_llvm_ir` run,
    /// then the collected declarations are prepended to the module top
    /// (before any `define` that references one) at the very end.
    declared_named_types: HashSet<String>,
    /// The `%Name = type { ... }` text itself, accumulated in dependency
    /// order (`declare_named_type` recurses into a struct's named-typed
    /// fields first so a `%Outer = type { %Point }` always follows its
    /// `%Point = type { ... }`), then spliced into `self.out` at position
    /// 0 once `emit_llvm_ir` finishes. Module-scoped, never reset — same
    /// structural shape as `string_globals` (appended once at the end),
    /// just prepended instead because a named type must textually precede
    /// any `define` that mentions it.
    named_type_decls: String,
}

pub fn emit_llvm_ir<'a>(program: &'a Program, smt_report: &'a SmtReport) -> Result<String, CodegenError> {
    check_supported(program)?;
    let registry = TypeRegistry::build(program);
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
            registry,
            declared_named_types: HashSet::new(),
            named_type_decls: String::new(),
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
    // `sha256_hex`/`constant_time_str_eq` — `STR_CRYPTO_BUILTINS`'
    // doc comment.
    writeln!(cg.out, "declare void @nir_sha256_hex(ptr, i64, ptr, i64, ptr)").unwrap();
    writeln!(cg.out, "declare i32 @nir_constant_time_str_eq(ptr, i64, ptr, i64)").unwrap();
    // `rand_seed`/`rand_f64`/`rand_gaussian` — `RAND_BUILTINS`' doc comment.
    writeln!(cg.out, "declare void @nir_rand_seed(i64)").unwrap();
    writeln!(cg.out, "declare double @nir_rand_f64()").unwrap();
    writeln!(cg.out, "declare double @nir_rand_gaussian(double, double)").unwrap();
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
    // "()\n\0" — 4 bytes. No format specifier at all: `unit` carries no
    // runtime data to interpolate, so this is printed as a fixed literal
    // — matching `interpreter.rs`'s `render()`, which prints `"()"` for
    // `Value::Unit`, so interpreted/compiled output agree.
    writeln!(cg.out, "@.unit_fmt = private unnamed_addr constant [4 x i8] c\"()\\0A\\00\"").unwrap();
    writeln!(cg.out).unwrap();

    for f in &program.fns {
        cg.function(f)?;
    }

    cg.emit_c_main(program)?;
    // See `string_globals`'s own doc — every `str` literal's backing
    // global constant, collected during function codegen since it can't
    // be written mid-function-body, appended here once at the end.
    cg.out.push_str(&cg.string_globals);
    // See `named_type_decls`'s own doc — every `%Name = type {...}`
    // declaration, collected during function codegen (a named type is
    // only discovered when a concrete instantiation is actually used)
    // and prepended here at the *top* of the module, so each one
    // textually precedes the `define` blocks that reference it (LLVM
    // textual IR requires a named struct type to be declared before any
    // use in a function body). `declare_named_type` already emitted
    // them in dependency order (a struct's named-typed fields before the
    // struct itself), so prepending the whole buffer preserves that
    // order in the final text.
    cg.out.insert_str(0, &cg.named_type_decls);
    Ok(cg.out)
}

/// `typeck::bind_type_params`'s exact substitution logic, duplicated here
/// (that function is private to `typeck.rs`) so `Codegen::ctor_ty`/
/// `infer_type_args` can resolve a generic constructor's type parameters
/// from its arguments' own types the same way `typeck.rs`'s own
/// `resolve_type_args` fall-back path does — no inference theory of
/// codegen's own, just the same structural bind `typeck.rs` already
/// proved correct for every generic program.
fn bind_type_params_owned(decl_ty: &Ty, concrete_ty: &Ty, type_params: &[String], subst: &mut HashMap<String, Ty>) {
    match (decl_ty, concrete_ty) {
        (Ty::Named(name, args), _) if args.is_empty() && type_params.iter().any(|p| p == name) => {
            subst.entry(name.clone()).or_insert_with(|| concrete_ty.clone());
        }
        (Ty::Box(a), Ty::Box(b))
        | (Ty::Ref(a), Ty::Ref(b))
        | (Ty::Thread(a), Ty::Thread(b))
        | (Ty::Channel(a), Ty::Channel(b)) => bind_type_params_owned(a, b, type_params, subst),
        (Ty::Vector(a, _), Ty::Vector(b, _)) | (Ty::Matrix(a, _, _), Ty::Matrix(b, _, _)) => {
            bind_type_params_owned(a, b, type_params, subst)
        }
        (Ty::Named(dn, dargs), Ty::Named(cn, cargs)) if dn == cn && dargs.len() == cargs.len() => {
            for (da, ca) in dargs.iter().zip(cargs.iter()) {
                bind_type_params_owned(da, ca, type_params, subst);
            }
        }
        _ => {}
    }
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

    /// The `&mut self` wrapper every real-codegen call site uses
    /// instead of the free `llvm_ty` function: resolves `ty` to its
    /// LLVM type string exactly as the free function does, *and* — for
    /// a `Ty::Named` — ensures the real `%Name = type {...}`
    /// declaration has been emitted into `named_type_decls` (memoized
    /// by mangled name) before handing back the name a `define`/GEP/
    /// `alloca` is about to reference. The free function can't do this
    /// half (it has no `&mut self`); the `check_supported` pre-pass
    /// doesn't need this half (it never emits a `define`, only
    /// validates — so it calls the free function directly with its own
    /// throwaway registry). Pure delegation otherwise — no type-
    /// resolution logic of its own.
    fn llvm_ty(&mut self, ty: &Ty) -> Result<String, CodegenError> {
        if let Ty::Named(_, _) = ty {
            self.declare_named_type(ty)?;
        }
        llvm_ty(ty, &self.registry)
    }

    /// Emit the real `%Name = type { ... }` declaration for one concrete
    /// `struct`/`enum` instantiation, exactly once per distinct mangled
    /// name (memoized via `declared_named_types`). Called from `llvm_ty`
    /// above — so any codegen path that resolves a `Ty::Named` to its
    /// LLVM name automatically declares it too, no separate "declare
    /// every struct" pass needed.
    ///
    /// **Struct layout**: `{ f0_llty, f1_llty, ... }` in field order, each
    /// field's LLVM type resolved via the free `llvm_ty` against the
    /// instantiation's own type-argument substitution. A named-typed
    /// field (a nested struct/enum) is declared *first*, recursively, so
    /// the outer `%Outer = type { %Point }` textually follows `%Point` —
    /// LLVM requires a named struct type to be declared before any use,
    /// including as another struct's field type. LLVM's own struct
    /// layout then computes the real per-field alignment/padding; this
    /// codebase never reads field offsets by hand for a struct (always
    /// via `getelementptr %Name, ..., i32 0, i32 <idx>`), so the exact
    /// padding is LLVM's concern, not ours.
    ///
    /// **Enum layout**: `{ i64 tag, [N x i64] payload }` — a hand-rolled
    /// tagged union (LLVM has no native enum/sum-type equivalent).
    /// `tag` is the variant's declaration-order index
    /// (`registry.enum_variants` order, matching `typeck.rs`'s own
    /// exhaustiveness-checking order). `N` is
    /// `1 + max_over_variants(sum_of(conservative_word_count(payload_field)))`,
    /// a real compile-time integer, so the same buffer fits every
    /// variant's payload without a per-variant type. Payload fields are
    /// stored into this raw `[N x i64]` buffer at 8-byte-aligned word
    /// offsets by `construct`/`match_expr` — see those for the GEP
    /// arithmetic; the buffer's element type is always `i64` regardless
    /// of a field's real type, so a `getelementptr i64, ptr %payload,
    /// i64 <word_off>` gives an always-8-byte-aligned address for any
    /// field store/load (every type this language has needs at most
    /// 8-byte alignment).
    fn declare_named_type(&mut self, ty: &Ty) -> Result<(), CodegenError> {
        let Ty::Named(decl_name, args) = ty else {
            return Ok(());
        };
        let mangled = mangle_ty(ty);
        if !self.declared_named_types.insert(mangled.clone()) {
            return Ok(()); // already declared — memoized
        }
        let decl_name = decl_name.as_str();
        if let Some(fields) = self.registry.struct_fields(decl_name) {
            let type_params = self.registry.struct_type_params(decl_name).unwrap_or(&[]);
            let subst = zip_type_params(type_params, args);
            let mut parts: Vec<String> = Vec::with_capacity(fields.len());
            for f in fields {
                let field_ty = substitute_ty(&f.ty, &subst);
                // A named-typed field is a dependency of this struct's
                // own declaration — declare it first so its `%Name =
                // type {...}` textually precedes this one. Recurses
                // through `llvm_ty`'s `Ty::Named` branch (which calls
                // this method), so arbitrarily-deep nesting is ordered
                // correctly in one pass.
                if matches!(field_ty, Ty::Named(_, _)) {
                    self.declare_named_type(&field_ty)?;
                }
                parts.push(llvm_ty(&field_ty, &self.registry)?);
            }
            writeln!(self.named_type_decls, "%{mangled} = type {{ {} }}", parts.join(", ")).unwrap();
            Ok(())
        } else if let Some(variants) = self.registry.enum_variants(decl_name) {
            let type_params = self.registry.enum_type_params(decl_name).unwrap_or(&[]);
            let subst = zip_type_params(type_params, args);
            let max_payload_words: u64 = variants
                .iter()
                .map(|v| {
                    v.payload
                        .iter()
                        .map(|t| conservative_word_count(&substitute_ty(t, &subst), &self.registry))
                        .sum::<u64>()
                })
                .max()
                .unwrap_or(0);
            let n = max_payload_words;
            writeln!(self.named_type_decls, "%{mangled} = type {{ i64, [{n} x i64] }}").unwrap();
            Ok(())
        } else {
            unreachable!("typeck.rs already proved every Ty::Named resolves to a struct or enum")
        }
    }

    /// Resolve the concrete `Ty::Named(name, type_args)` a constructor call
    /// `name(args)` produces, *without* an expected-type context — the
    /// structural-inference fallback `expr_ptr`'s own `Expr::Call` ctor
    /// branch and `local_ty_of`'s `Expr::Call` ctor arm need (the four
    /// real construction sites — `Stmt::Let`/`Stmt::Return`/`call_args`/
    /// nested-`construct` — already have a concrete `expected` in hand and
    /// never call this; they call `construct` directly). Mirrors
    /// `typeck.rs::resolve_type_args`'s fall-back path: for a generic
    /// declaration, infer each type parameter from the corresponding
    /// argument's own type via `bind_type_params`, then collect. Returns
    /// `None` only for the genuinely-ambiguous case a zero-payload variant
    /// (`None`) reached with no enclosing type context at all — exactly
    /// the case the struct/enum codegen plan names as the one disclosed
    /// `CodegenError` to fail rather than guess on.
    fn ctor_ty(&self, name: &str, args: &[Expr], scopes: &Scopes) -> Option<Ty> {
        if let Some(decl) = self.registry.struct_decl(name) {
            let type_params = decl.type_params.clone();
            let decl_tys: Vec<Ty> = decl.fields.iter().map(|f| f.ty.clone()).collect();
            let type_args = self.infer_type_args(&type_params, &decl_tys, args, scopes)?;
            Some(Ty::Named(name.to_string(), type_args))
        } else if let Some((enum_name, variant)) = self.registry.find_variant(name) {
            let type_params = self.registry.enum_type_params(&enum_name)?.to_vec();
            let decl_tys = variant.payload.clone();
            let type_args = self.infer_type_args(&type_params, &decl_tys, args, scopes)?;
            Some(Ty::Named(enum_name, type_args))
        } else {
            None
        }
    }

    /// The shared inference core of `ctor_ty` — `typeck.rs::
    /// resolve_type_args` minus its expected-type shortcut and its
    /// diagnostic path (codegen only ever sees an already-well-typed
    /// program, so a genuinely-ambiguous constructor is a real, disclosed
    /// `CodegenError` this returns `None` for, not a recoverable type
    /// error to report). Returns the fully-resolved `type_args` if every
    /// parameter was bound, `None` otherwise.
    fn infer_type_args(&self, type_params: &[String], decl_tys: &[Ty], args: &[Expr], scopes: &Scopes) -> Option<Vec<Ty>> {
        if type_params.is_empty() {
            return Some(Vec::new());
        }
        let mut subst: HashMap<String, Ty> = HashMap::new();
        for (decl_ty, arg) in decl_tys.iter().zip(args.iter()) {
            let arg_ty = self.local_ty_of(arg, scopes);
            if arg_ty != Ty::Error {
                bind_type_params_owned(decl_ty, &arg_ty, type_params, &mut subst);
            }
        }
        type_params.iter().map(|p| subst.get(p).cloned()).collect()
    }

    /// `(field_index, substituted_field_type)` for `field` of the struct
    /// type `base_ty` — the one piece of information both `expr()`'s and
    /// `expr_ptr()`'s `Expr::FieldAccess` arms need, factored out so they
    /// share one resolution path. `base_ty` is always a concrete struct
    /// instantiation by the time this runs (`typeck.rs` already proved the
    /// base is a struct and the field exists); the `None` return is a
    /// defense-in-depth fallback, not an expected path.
    fn field_index_and_ty(&self, base_ty: &Ty, field: &str) -> Option<(usize, Ty)> {
        if let Ty::Named(name, type_args) = base_ty
            && let Some(fields) = self.registry.struct_fields(name)
        {
            let type_params = self.registry.struct_type_params(name).unwrap_or(&[]);
            let subst = zip_type_params(type_params, type_args);
            for (i, f) in fields.iter().enumerate() {
                if f.name == field {
                    return Some((i, substitute_ty(&f.ty, &subst)));
                }
            }
        }
        None
    }

    /// `expr_ptr`'s `Expr::Call` ctor branch, and the shared backend the
    /// three expected-type-bearing construction sites (`Stmt::Let`/
    /// `Stmt::Return`/`call_args`, via `expr_ptr_expected`) route
    /// through. Allocates a fresh destination sized to `expected`'s real
    /// LLVM type, fills its fields (struct) or its tag + payload words
    /// (enum variant) from `args`, and returns the destination pointer —
    /// the `expr_ptr`-shaped result every aggregate value produces.
    ///
    /// `expected` is always a *concrete* `Ty::Named(decl_name, type_args)`
    /// at every call site: `Stmt::Let`/`Stmt::Return`/`call_args` hand
    /// over their own already-resolved declared/return/parameter type, and
    /// a nested constructor argument recurses with its field's own
    /// substituted type. So this is pure lookup + substitution, never
    /// inference — the inference the plan's design-decision 4 names is
    /// entirely `ctor_ty`'s job (the `expr_ptr`-reached fallback), not
    /// this method's.
    #[allow(clippy::too_many_arguments)]
    fn construct(
        &mut self,
        name: &str,
        args: &[Expr],
        expected: &Ty,
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<String, CodegenError> {
        let dest_llty = self.llvm_ty(expected)?;
        let dest = self.fresh_reg("ctor.addr");
        self.emit_alloca(&dest, &dest_llty);

        if self.registry.is_struct(name) {
            self.construct_struct(name, args, expected, &dest, span, scopes)?;
        } else if let Some((enum_name, variant)) = self.registry.find_variant(name) {
            self.construct_variant(&enum_name, variant, args, expected, &dest, span, scopes)?;
        } else {
            unreachable!("typeck.rs already proved `{name}` is a struct or variant constructor")
        }
        Ok(dest)
    }

    /// The struct half of `construct` — stores each argument into its
    /// field slot via `getelementptr %Name, ptr dest, i32 0, i32 i`,
    /// recursing through `construct` for a nested constructor argument
    /// (so `Outer(Inner(1))` constructs the `Inner` into its own temp,
    /// then memcpys it into `Outer`'s field — a small extra copy, kept
    /// for simplicity over a "construct directly into the field slot"
    /// optimization that would need a different `construct` shape).
    fn construct_struct(
        &mut self,
        name: &str,
        args: &[Expr],
        expected: &Ty,
        dest: &str,
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<(), CodegenError> {
        let decl_name = if let Ty::Named(n, _) = expected { n.as_str() } else { name };
        let fields = self.registry.struct_fields(decl_name).expect("just proved this is a struct");
        let type_params = self.registry.struct_type_params(decl_name).unwrap_or(&[]);
        let type_args = if let Ty::Named(_, a) = expected { a.clone() } else { Vec::new() };
        let subst = zip_type_params(type_params, &type_args);
        let base_llty = self.llvm_ty(expected)?;
        for (i, (arg, f)) in args.iter().zip(fields.iter()).enumerate() {
            let field_ty = substitute_ty(&f.ty, &subst);
            let field_ptr = self.fresh_reg("field.addr");
            writeln!(self.out, "  {field_ptr} = getelementptr inbounds {base_llty}, ptr {dest}, i32 0, i32 {i}").unwrap();
            self.store_value_into(arg, &field_ty, &field_ptr, span, scopes)?;
        }
        Ok(())
    }

    /// The enum-variant half of `construct` — stores the variant's
    /// declaration-order index at the tag word (GEP field 0), then stores
    /// each payload argument at its 8-byte-aligned word offset inside the
    /// `[N x i64]` payload buffer (GEP field 1, then a word-granularity
    /// `getelementptr i64` into it). Word offsets are the cumulative
    /// `conservative_word_count` of the preceding payload fields in this
    /// variant — exactly the over-allocating, never-under-sizing scheme
    /// `declare_named_type`'s enum-layout doc explains.
    #[allow(clippy::too_many_arguments)]
    fn construct_variant(
        &mut self,
        enum_name: &str,
        variant: &Variant,
        args: &[Expr],
        expected: &Ty,
        dest: &str,
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<(), CodegenError> {
        let variants = self.registry.enum_variants(enum_name).expect("just proved this is an enum");
        let vidx = variants.iter().position(|v| v.name == variant.name).expect("typeck.rs proved this variant exists");
        let type_params = self.registry.enum_type_params(enum_name).unwrap_or(&[]);
        let type_args = if let Ty::Named(_, a) = expected { a.clone() } else { Vec::new() };
        let subst = zip_type_params(type_params, &type_args);
        let base_llty = self.llvm_ty(expected)?;

        // Tag word at GEP field 0.
        let tag_ptr = self.fresh_reg("tag.addr");
        writeln!(self.out, "  {tag_ptr} = getelementptr inbounds {base_llty}, ptr {dest}, i32 0, i32 0").unwrap();
        writeln!(self.out, "  store i64 {vidx}, ptr {tag_ptr}").unwrap();

        // Payload buffer at GEP field 1 — `[N x i64]`, element-addressable
        // by a plain `getelementptr i64, ptr %payload, i64 <word_off>`.
        let payload = self.fresh_reg("payload.addr");
        writeln!(self.out, "  {payload} = getelementptr inbounds {base_llty}, ptr {dest}, i32 0, i32 1").unwrap();

        let mut word_off: u64 = 0;
        for (arg, decl_ty) in args.iter().zip(variant.payload.iter()) {
            let field_ty = substitute_ty(decl_ty, &subst);
            let field_ptr = self.fresh_reg("payfield.addr");
            writeln!(self.out, "  {field_ptr} = getelementptr inbounds i64, ptr {payload}, i64 {word_off}").unwrap();
            self.store_value_into(arg, &field_ty, &field_ptr, span, scopes)?;
            word_off += conservative_word_count(&field_ty, &self.registry);
        }
        Ok(())
    }

    /// Store `arg`'s value (a scalar) or whole value (an aggregate) into
    /// the slot at `field_ptr`, with the slot's own declared `field_ty`
    /// driving the scalar-vs-aggregate choice exactly the way `call_args`
    /// does for a call argument. A nested constructor argument recurses
    /// through `construct` (with `field_ty` as its expected type) instead
    /// of going through `expr_ptr`'s no-expected fallback — so a nested
    /// `Outer(Inner(1))` never hits `ctor_ty`'s ambiguous-case `None`.
    fn store_value_into(
        &mut self,
        arg: &Expr,
        field_ty: &Ty,
        field_ptr: &str,
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<(), CodegenError> {
        if field_ty.is_aggregate() {
            if let Expr::Call(name, cargs, _) = arg
                && (self.registry.is_struct(name) || self.registry.find_variant(name).is_some())
            {
                let src = self.construct(name, cargs, field_ty, span, scopes)?;
                let bytes = agg_byte_size_operand(field_ty, &self.registry);
                writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {field_ptr}, ptr {src}, i64 {bytes}, i1 false)").unwrap();
                return Ok(());
            }
            let src = self.expr_ptr(arg, scopes)?;
            let bytes = agg_byte_size_operand(field_ty, &self.registry);
            writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {field_ptr}, ptr {src}, i64 {bytes}, i1 false)").unwrap();
        } else {
            let v = self.expr(arg, scopes)?;
            let v = if field_ty.is_integer() { self.narrow_from_i64(&v, field_ty)? } else { v };
            let field_llty = self.llvm_ty(field_ty)?;
            writeln!(self.out, "  store {field_llty} {v}, ptr {field_ptr}").unwrap();
        }
        Ok(())
    }

    /// The expected-type-bearing entry point the three aggregate-value
    /// call sites that already have a concrete type in hand (`Stmt::Let`'s
    /// own declared type, `Stmt::Return`'s `current_fn_ret`, `call_args`'
    /// per-argument `sig_params[i]`) route through instead of `expr_ptr` —
    /// so a constructor reached in one of those positions is built with
    /// its real expected type (never `ctor_ty`'s inference fallback), and
    /// every other aggregate expression is forwarded to `expr_ptr`
    /// unchanged. A constructor's `expected` is always a concrete
    /// `Ty::Named` here (a `let`/return/parameter type is fully resolved
    /// by `typeck.rs` before codegen runs), so `construct`'s "no
    /// inference" contract holds at every entry.
    fn expr_ptr_expected(
        &mut self,
        e: &Expr,
        expected: &Ty,
        scopes: &mut Scopes,
    ) -> Result<String, CodegenError> {
        if let Expr::Call(name, args, span) = e
            && (self.registry.is_struct(name) || self.registry.find_variant(name).is_some())
        {
            return self.construct(name, args, expected, *span, scopes);
        }
        self.expr_ptr(e, scopes)
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
        let ret_llty: String = if is_agg_ret { "void".to_string() } else { self.llvm_ty(&f.ret)? };
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
                params.push(format!("{} %arg.{}", self.llvm_ty(&p.ty)?, p.name));
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
                let agg_llty = self.llvm_ty(&p.ty)?;
                let local_ptr = format!("%{}.addr", p.name);
                self.emit_alloca(&local_ptr, &agg_llty);
                let bytes = agg_byte_size_operand(&p.ty, &self.registry);
                writeln!(
                    self.out,
                    "  call void @llvm.memcpy.p0.p0.i64(ptr {local_ptr}, ptr %arg.{}, i64 {bytes}, i1 false)",
                    p.name
                )
                .unwrap();
                scopes.define(&p.name, p.ty.clone(), local_ptr);
            } else {
                let ty = self.llvm_ty(&p.ty)?;
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
                    // existing pointer with no copy of its own). A
                    // constructor expression here is built with `ty` as its
                    // expected type (`expr_ptr_expected`), so a generic
                    // zero-payload variant like `None` resolves from this
                    // `let`'s own annotation rather than the no-context
                    // `ctor_ty` fallback.
                    let src = self.expr_ptr_expected(value, ty, scopes)?;
                    let agg_llty = self.llvm_ty(ty)?;
                    let dest = self.fresh_reg(&format!("{name}.addr"));
                    self.emit_alloca(&dest, &agg_llty);
                    let bytes = agg_byte_size_operand(ty, &self.registry);
                    writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {dest}, ptr {src}, i64 {bytes}, i1 false)").unwrap();
                    scopes.define(name, ty.clone(), dest);
                    return Ok(());
                }
                let val = self.expr(value, scopes)?; // i64 (or i1 for bool)
                let val = self.guard_in_range(&val, ty, *span)?; // checked at i64 width, before narrowing
                let val = if ty.is_integer() { self.narrow_from_i64(&val, ty)? } else { val };
                let llty = self.llvm_ty(ty)?;
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
                            let src = self.expr_ptr_expected(e, &ret_ty, scopes)?;
                            let sret = self
                                .current_fn_sret
                                .clone()
                                .expect("an aggregate-returning function always sets current_fn_sret");
                            let bytes = agg_byte_size_operand(&ret_ty, &self.registry);
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
                            let ret_llty = self.llvm_ty(&ret_ty)?;
                            writeln!(self.out, "  ret {} {val}", ret_llty).unwrap();
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
            // Not in `self.sigs` (that table is user-defined functions
            // only) -- without this arm, the fallback below would
            // wrongly report `Ty::I64` for these two builtins.
            Expr::Call(name, _, _) if name == "sha256_hex" => Ty::Str,
            Expr::Call(name, _, _) if name == "constant_time_str_eq" => Ty::Bool,
            Expr::Call(name, _, _) if name == "rand_f64" || name == "rand_gaussian" => Ty::F64,
            Expr::Call(name, _, _) if name == "rand_seed" => Ty::Unit,
            // Row 11: a struct/variant constructor call produces a
            // `Ty::Named` value (the struct's own type, or the owning
            // enum's), not the `Ty::I64` the `self.sigs.get` fallback
            // below would wrongly hand back for a name that isn't a user
            // fn. `ctor_ty`'s `None` (a zero-payload generic variant
            // reached with no expected type, e.g. a bare `None`
            // expression statement) falls back to a placeholder `Named`
            // so `is_aggregate()` routing still picks the right
            // `expr_ptr`/`expr` fork — the real construction path
            // (`expr_ptr_expected`) always has the true expected type and
            // never reaches this `None` branch for a real program.
            Expr::Call(name, args, _) if self.registry.is_struct(name) || self.registry.find_variant(name).is_some() => {
                self.ctor_ty(name, args, scopes).unwrap_or_else(|| Ty::Named(name.to_string(), Vec::new()))
            }
            Expr::Call(name, _, _) => self.sigs.get(name).map(|s| s.ret.clone()).unwrap_or(Ty::I64),
            // Row 11: `base.field`'s type is `base`'s struct type's
            // substituted field type — factored through
            // `field_index_and_ty` so `expr()`/`expr_ptr()`'s own
            // `Expr::FieldAccess` arms share one resolution path with
            // this `local_ty_of` arm. The `Ty::I64` fallback is
            // defense-in-depth (typeck already proved the base is a struct
            // and the field exists), not an expected path.
            Expr::FieldAccess(base, field, _) => {
                self.field_index_and_ty(&self.local_ty_of(base, scopes), field)
                    .map(|(_, ty)| ty)
                    .unwrap_or(Ty::I64)
            }
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
            // Aggregate-result `if`/`match`: typeck already proved every
            // branch/arm body has the same type, so the first one is
            // representative. This is only needed so that nested
            // aggregate control flow (e.g. an `if` inside an `if` branch)
            // can resolve its own result type.
            Expr::If { then_block, .. } => self.block_trailing_ty(then_block, scopes),
            Expr::Match { arms, .. } => self.local_ty_of(&arms[0].body, scopes),
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
    /// read out of a narrower-than-`i64` stack slot gets widened
    /// immediately, before it can participate in anything else. `zext`
    /// for an unsigned type (`Ty::is_unsigned`), `sext` for a signed
    /// one — the *only* place this backend needs that distinction at
    /// all (see `llvm_ty`'s `Ty::U8`/etc. arm for why nothing
    /// downstream does). A no-op for `bool` (stays `i1` throughout — it
    /// never enters the i64 scheme) or a value whose LLVM width is
    /// already 64 bits (`Ty::I64`, and also `Ty::U64`/`Ty::Usize`, which
    /// map to the same `i64` width — checked by comparing the actual
    /// LLVM type string, not the `Ty` variant, since `Ty::U64 !=
    /// Ty::I64` even though they compile to the identical width).
    fn widen_to_i64(&mut self, val: &str, ty: &Ty) -> String {
        if !ty.is_integer() {
            return val.to_string();
        }
        let llty = self.llvm_ty(ty).expect("check_supported already validated this type");
        if llty == "i64" {
            return val.to_string();
        }
        let r = self.fresh_reg("widen");
        let op = if ty.is_unsigned() { "zext" } else { "sext" };
        writeln!(self.out, "  {r} = {op} {llty} {val} to i64").unwrap();
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
        let llty = self.llvm_ty(ty)?;
        if llty == "i64" {
            return Ok(val.to_string());
        }
        let r = self.fresh_reg("narrow");
        writeln!(self.out, "  {r} = trunc i64 {val} to {llty}").unwrap();
        Ok(r)
    }

    /// Tier 1 vs Tier 2, for real: if `span` is in the SMT report's
    /// proven-safe set, `val` is used exactly as computed — no runtime
    /// check, no cost, matching docs/goal.md §4's Tier 1. Otherwise, emits an
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

    /// Tear down one affine-typed value whose storage lives at `value_ptr`.
    /// Recurses into `box` contents, `struct` affine fields, and the live
    /// variant's affine payload fields of `enum`s. `value_ptr` is always a
    /// pointer to where the value is stored (a stack slot, a field address,
    /// or the heap address for the contents of a box), not the scalar value
    /// itself.
    fn emit_affine_free(&mut self, value_ptr: &str, ty: &Ty) {
        match ty {
            Ty::Box(inner) => {
                let heap_ptr = self.fresh_reg("box.heap_ptr");
                writeln!(self.out, "  {heap_ptr} = load ptr, ptr {value_ptr}").unwrap();
                if self.registry.is_affine(inner) {
                    self.emit_affine_free(&heap_ptr, inner);
                }
                writeln!(self.out, "  call void @nir_free(ptr {heap_ptr})").unwrap();
            }
            Ty::Tcp | Ty::TcpListener => {
                let fd = self.fresh_reg("tcp.fd");
                writeln!(self.out, "  {fd} = load i64, ptr {value_ptr}").unwrap();
                writeln!(self.out, "  call i32 @nir_tcp_stop(i64 {fd})").unwrap();
            }
            Ty::Named(name, args) => {
                if let Some(fields) = self.registry.struct_fields(name) {
                    let type_params = self.registry.struct_type_params(name).unwrap_or(&[]);
                    let subst = zip_type_params(type_params, args);
                    let struct_llty = self.llvm_ty(ty).expect("check_supported already accepted this type");
                    for (i, f) in fields.iter().enumerate() {
                        let field_ty = substitute_ty(&f.ty, &subst);
                        if self.registry.is_affine(&field_ty) {
                            let field_ptr = self.fresh_reg("struct_field.addr");
                            writeln!(
                                self.out,
                                "  {field_ptr} = getelementptr inbounds {struct_llty}, ptr {value_ptr}, i32 0, i32 {i}"
                            )
                            .unwrap();
                            self.emit_affine_free(&field_ptr, &field_ty);
                        }
                    }
                } else if let Some(variants) = self.registry.enum_variants(name) {
                    let type_params = self.registry.enum_type_params(name).unwrap_or(&[]);
                    let subst = zip_type_params(type_params, args);
                    let enum_llty = self.llvm_ty(ty).expect("check_supported already accepted this type");
                    let tag_ptr = self.fresh_reg("enum_tag.addr");
                    writeln!(
                        self.out,
                        "  {tag_ptr} = getelementptr inbounds {enum_llty}, ptr {value_ptr}, i32 0, i32 0"
                    )
                    .unwrap();
                    let tag = self.fresh_reg("enum_tag");
                    writeln!(self.out, "  {tag} = load i64, ptr {tag_ptr}").unwrap();
                    let payload = self.fresh_reg("enum_payload.addr");
                    writeln!(
                        self.out,
                        "  {payload} = getelementptr inbounds {enum_llty}, ptr {value_ptr}, i32 0, i32 1"
                    )
                    .unwrap();

                    let mut case_labels: Vec<String> = Vec::new();
                    for _ in variants.iter() {
                        case_labels.push(self.fresh_label("enum_free_arm"));
                    }
                    let default_label = self.fresh_label("enum_free_default");
                    let merge_label = self.fresh_label("enum_free_merge");

                    writeln!(self.out, "  switch i64 {tag}, label %{default_label} [").unwrap();
                    for (vidx, label) in case_labels.iter().enumerate() {
                        writeln!(self.out, "    i64 {vidx}, label %{label}").unwrap();
                    }
                    writeln!(self.out, "  ]").unwrap();

                    writeln!(self.out, "{default_label}:").unwrap();
                    writeln!(self.out, "  unreachable").unwrap();

                    for (variant, label) in variants.iter().zip(case_labels.iter()) {
                        writeln!(self.out, "{label}:").unwrap();
                        let mut word_off: u64 = 0;
                        for decl_ty in variant.payload.iter() {
                            let field_ty = substitute_ty(decl_ty, &subst);
                            if self.registry.is_affine(&field_ty) {
                                let field_ptr = self.fresh_reg("enum_payload_field.addr");
                                writeln!(
                                    self.out,
                                    "  {field_ptr} = getelementptr inbounds i64, ptr {payload}, i64 {word_off}"
                                )
                                .unwrap();
                                self.emit_affine_free(&field_ptr, &field_ty);
                            }
                            word_off += conservative_word_count(&field_ty, &self.registry);
                        }
                        writeln!(self.out, "  br label %{merge_label}").unwrap();
                    }

                    writeln!(self.out, "{merge_label}:").unwrap();
                }
            }
            _ => {}
        }
    }

    /// Emits teardown for every named binding in a `FreeMap` entry,
    /// looking each one up in the scope it's still live in. The binding's
    /// stack slot pointer is passed straight to `emit_affine_free`, which
    /// decides whether the slot contains a heap pointer (`box`), an fd
    /// (`tcp`), or a struct/enum value with affine fields. Callers are
    /// every scope-closing point `FreeMap`'s doc comment lists — always
    /// guarded by `!self.terminated` except `Stmt::Return`'s own call,
    /// which fires unconditionally since it's what's *about* to set
    /// `terminated`, not something already past it.
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
            self.emit_affine_free(&stack_ptr, &ty);
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
                let llty = self.llvm_ty(&ty)?;
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
                let llty = self.llvm_ty(&ty)?;
                writeln!(self.out, "  store {llty} {store_val}, ptr {ptr}").unwrap();
                // `val` (still i64/i1), not `store_val` (narrow) — every
                // other `expr()` result is i64 for an integer type, and
                // an assignment-expression's own value has to match that
                // convention so a caller combining it further doesn't
                // need to know it came from an assignment specifically.
                Ok(val)
            }
            Expr::Spawn(_, _, _)
            | Expr::Acquire(_, _, _)
            | Expr::Join(_, _)
            | Expr::Chan(_)
            | Expr::SpawnSandbox(_, _, _)
            | Expr::Open(_, _, _)
            | Expr::Transact { .. } => {
                unreachable!("check_supported already rejected this program")
            }
            // Row 11: `base.field` where the field is a plain scalar —
            // GEP to the field's index within `base`'s real named struct
            // type, then `load` the field's own LLVM type, exactly the
            // shape every other scalar `expr()` arm has. An
            // aggregate-typed field (a nested struct/Vector/Matrix) never
            // reaches `expr()` — `local_ty_of` reports it as
            // `is_aggregate()`, so `Stmt::Let`/`Stmt::Expr`/`call_args`
            // all route it to `expr_ptr()`'s own `Expr::FieldAccess` arm
            // instead; the guard below keeps this arm a clean
            // `CodegenError` rather than a wrong-shaped `load` if some
            // future construct defies that routing.
            Expr::FieldAccess(base, field, span) => {
                let base_ty = self.local_ty_of(base, scopes);
                let (idx, field_ty) = self
                    .field_index_and_ty(&base_ty, field)
                    .expect("typeck.rs already proved this base is a struct with this field");
                if field_ty.is_aggregate() {
                    return unsupported(format!(
                        "codegen doesn't support an aggregate-typed field access `.{field}` at {span:?} in this scalar expression position yet — bind it via `let` instead (same pre-existing gap `if`/`match` themselves have for an aggregate result)"
                    ));
                }
                let base_ptr = self.expr_ptr(base, scopes)?;
                let base_llty = self.llvm_ty(&base_ty)?;
                let gep = self.fresh_reg("fieldptr");
                writeln!(self.out, "  {gep} = getelementptr inbounds {base_llty}, ptr {base_ptr}, i32 0, i32 {idx}").unwrap();
                let field_llty = self.llvm_ty(&field_ty)?;
                let loaded = self.fresh_reg("fieldval");
                writeln!(self.out, "  {loaded} = load {field_llty}, ptr {gep}").unwrap();
                Ok(self.widen_to_i64(&loaded, &field_ty))
            }
            // Row 11: `match scrutinee { ... }` with a scalar result —
            // the overwhelmingly common real case (`structs_enums.nir`'s
            // own `area()`). An aggregate-result `match` is deliberately
            // out of scope here, the same pre-existing gap `expr_ptr`'s
            // own `_ => unsupported(...)` already covers for `if` — it
            // fails cleanly via `expr_ptr_expected`'s `expr_ptr` fallback
            // rather than being silently absent.
            Expr::Match { scrutinee, arms, span } => self.match_expr(scrutinee, arms, *span, scopes),
            // `box e` — heap-allocate `e`'s type's own byte size
            // (`ty_byte_size`, covers every `Ty` a `box` can wrap, not
            // just aggregates), copy `e`'s value in, return the heap
            // pointer as this expression's own value. `Ty::Box` is a
            // single pointer word (`llvm_ty`), so — like `Ty::Str` — it's
            // an ordinary `expr()` result, never routed through
            // `expr_ptr()`'s sret convention.
            //
            // **Allocation only — the `free` half lives elsewhere, not
            // because it's missing.** This arm just calls `nir_alloc`;
            // the matching `nir_free` is emitted later, at whichever
            // scope-closing point actually owns this box's last use
            // (`ownership.rs`'s `FreeMap`, consumed by
            // `emit_frees_for_names`/`emit_affine_free`) — a real, working
            // free, not a placeholder (confirmed in generated IR for a
            // simple `let`-bound box). Keeping allocation and free as two
            // separate emission sites mirrors how the rest of this file
            // separates "construct a value" from "clean it up when its
            // scope ends."
            Expr::Box(inner, _) => {
                let inner_ty = self.local_ty_of(inner, scopes);
                let size = ty_byte_size(&inner_ty, &self.registry);
                let heap_ptr = self.fresh_reg("box_heap");
                writeln!(self.out, "  {heap_ptr} = call ptr @nir_alloc(i64 {size})").unwrap();
                if inner_ty.is_aggregate() {
                    let src = self.expr_ptr(inner, scopes)?;
                    writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {heap_ptr}, ptr {src}, i64 {size}, i1 false)").unwrap();
                } else {
                    let v = self.expr(inner, scopes)?;
                    let llty = self.llvm_ty(&inner_ty)?;
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
                let llty = self.llvm_ty(&result_ty)?;
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
                let elem_llty = self.llvm_ty(&elem)?;
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
        // Row 11: a struct/variant constructor is always aggregate-valued
        // (`is_aggregate()` now covers `Ty::Named`), so a scalar `expr()`
        // result is the wrong shape for it — every well-typed caller
        // already routed it to `expr_ptr`/`expr_ptr_expected` via
        // `local_ty_of`'s `is_aggregate()` fork. This guard keeps the
        // scalar path a clean `CodegenError` (not the `sigs.get(name)
        // .expect(...)` panic a ctor name would otherwise hit below) if
        // some future construct defies that routing — the same
        // defense-in-depth shape `Expr::Ident`/`Expr::Assign`'s own
        // aggregate guards already use.
        if self.registry.is_struct(name) || self.registry.find_variant(name).is_some() {
            return unsupported(format!(
                "codegen doesn't support constructing `{name}` in this scalar expression position yet — a struct/enum value is aggregate; bind it via `let`, or pass/return it through a function call"
            ));
        }
        // `check_supported` (`check_expr`'s `Expr::Call` arm, above)
        // already rejected every builtin except `print`,
        // `STR_CRYPTO_BUILTINS`, `RAND_BUILTINS`, and `PHASE4_BUILTINS`/
        // `PHASE5_BUILTINS`' names before this ever runs -- explicit
        // `== "print"`/`== "sha256_hex"`/etc. checks here, not
        // `is_builtin`, state that invariant directly rather than
        // leaning on it silently.
        if name == "print" {
            for a in args {
                // `local_ty_of` picks the right `printf` format
                // string/vararg type: `double` for `f64`, the `{ptr,
                // i64}` two-word convention for `str`, `i1`-widened for
                // `bool` (a bare bool variable, a bool literal, and a
                // comparison result `x > y` all resolve to `Ty::Bool`
                // here identically -- `local_ty_of`'s `Expr::Binary` arm
                // already maps every comparison/`&&`/`||` operator to
                // `Ty::Bool`, so there's exactly one bool-shaped case to
                // handle, not several), a fixed `"()"` string for `unit`
                // (there's no `unit` *literal* syntax -- the only way to
                // produce a unit-typed argument is a call to a `-> unit`
                // function; its side effect still has to run, so `expr()`
                // below is still called unconditionally, its nominal
                // result just isn't a meaningful value to print), and
                // plain `i64` for everything else.
                let arg_ty = self.local_ty_of(a, scopes);
                if arg_ty.is_aggregate() {
                    // Printing a whole Vector/Matrix isn't built yet —
                    // this purely-syntactic pre-pass (`check_expr`) has
                    // no type info to catch it earlier, so it's caught
                    // here instead, with a specific reason rather than
                    // falling through to a scalar `expr()` call that
                    // would fail confusingly.
                    return unsupported(
                        "codegen doesn't support `print` on a Vector/Matrix argument yet — \
                         only integer/f64/str/bool/unit-typed arguments are supported so far",
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
                    // Prints `1`/`0`, not `interpreter.rs`'s `render()`
                    // `"true"`/`"false"` — a real, honest cosmetic
                    // difference between the two execution paths (same
                    // class as `@.float_fmt`'s `%f`-vs-Rust-formatting
                    // note above), not a semantic one: both agree on
                    // which of the two boolean values it is.
                    let widened = self.fresh_reg("bool_as_i64");
                    writeln!(self.out, "  {widened} = zext i1 {v} to i64").unwrap();
                    writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.int_fmt, i64 {widened})").unwrap();
                } else if arg_ty == Ty::Unit {
                    // `v` (the call's nominal "result") carries no real
                    // data for a `void`-returning callee — deliberately
                    // ignored. `interpreter.rs`'s `render()` prints
                    // `"()"` for `Value::Unit`; match that exactly so
                    // interpreted/compiled output agree.
                    let _ = v;
                    writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.unit_fmt)").unwrap();
                } else {
                    writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.int_fmt, i64 {v})").unwrap();
                }
            }
            return Ok("0".to_string()); // print's own "value" is unit; never read
        }
        // `sha256_hex`/`constant_time_str_eq` — linked calls into
        // `runtime_kernels.rs`'s from-scratch SHA-256 (`STR_CRYPTO_BUILTINS`'
        // doc comment on why these two don't go through
        // `call_builtin_scalar`/`call_builtin_agg` like `PHASE4`/
        // `PHASE5_BUILTINS` do).
        if name == "sha256_hex" {
            let a = self.expr(&args[0], scopes)?;
            let a_ptr = self.fresh_reg("sha256_a_ptr");
            writeln!(self.out, "  {a_ptr} = extractvalue {{ptr, i64}} {a}, 0").unwrap();
            let a_len = self.fresh_reg("sha256_a_len");
            writeln!(self.out, "  {a_len} = extractvalue {{ptr, i64}} {a}, 1").unwrap();
            // The 1-arg form passes a null `b_ptr`/`0` `b_len` -- the
            // kernel never dereferences `b_ptr` when `b_len` is 0 (its
            // own doc comment), so an absent second argument needs no
            // real buffer, just these two placeholder values.
            let (b_ptr, b_len) = if args.len() == 2 {
                let b = self.expr(&args[1], scopes)?;
                let b_ptr = self.fresh_reg("sha256_b_ptr");
                writeln!(self.out, "  {b_ptr} = extractvalue {{ptr, i64}} {b}, 0").unwrap();
                let b_len = self.fresh_reg("sha256_b_len");
                writeln!(self.out, "  {b_len} = extractvalue {{ptr, i64}} {b}, 1").unwrap();
                (b_ptr, b_len)
            } else {
                ("null".to_string(), "0".to_string())
            };
            // 64 bytes, always -- a hex-encoded SHA-256 digest is a
            // fixed size, never data-dependent. Heap-allocated and never
            // freed (`nir_sha256_hex`'s own doc comment on why: `Ty::Str`
            // isn't affine, so there's no scope-closing point to hook a
            // matching `nir_free` onto).
            let out_ptr = self.fresh_reg("sha256_out");
            writeln!(self.out, "  {out_ptr} = call ptr @nir_alloc(i64 64)").unwrap();
            writeln!(
                self.out,
                "  call void @nir_sha256_hex(ptr {a_ptr}, i64 {a_len}, ptr {b_ptr}, i64 {b_len}, ptr {out_ptr})"
            )
            .unwrap();
            let partial = self.fresh_reg("sha256_str_partial");
            writeln!(self.out, "  {partial} = insertvalue {{ptr, i64}} undef, ptr {out_ptr}, 0").unwrap();
            let result = self.fresh_reg("sha256_str");
            writeln!(self.out, "  {result} = insertvalue {{ptr, i64}} {partial}, i64 64, 1").unwrap();
            return Ok(result);
        }
        if name == "constant_time_str_eq" {
            let a = self.expr(&args[0], scopes)?;
            let b = self.expr(&args[1], scopes)?;
            let a_ptr = self.fresh_reg("cteq_a_ptr");
            writeln!(self.out, "  {a_ptr} = extractvalue {{ptr, i64}} {a}, 0").unwrap();
            let a_len = self.fresh_reg("cteq_a_len");
            writeln!(self.out, "  {a_len} = extractvalue {{ptr, i64}} {a}, 1").unwrap();
            let b_ptr = self.fresh_reg("cteq_b_ptr");
            writeln!(self.out, "  {b_ptr} = extractvalue {{ptr, i64}} {b}, 0").unwrap();
            let b_len = self.fresh_reg("cteq_b_len");
            writeln!(self.out, "  {b_len} = extractvalue {{ptr, i64}} {b}, 1").unwrap();
            let raw = self.fresh_reg("cteq_raw");
            writeln!(
                self.out,
                "  {raw} = call i32 @nir_constant_time_str_eq(ptr {a_ptr}, i64 {a_len}, ptr {b_ptr}, i64 {b_len})"
            )
            .unwrap();
            return self.icmp("ne", "i32", &raw, "0");
        }
        if name == "rand_seed" {
            // Every integer-typed `expr()` result is already `i64`
            // (module doc) regardless of `rand_seed`'s argument's own
            // declared width (`typeck.rs` accepts any integer type) --
            // no extra widening needed here.
            let seed = self.expr(&args[0], scopes)?;
            writeln!(self.out, "  call void @nir_rand_seed(i64 {seed})").unwrap();
            return Ok("0".to_string()); // rand_seed's own "value" is unit; never read
        }
        if name == "rand_f64" {
            let r = self.fresh_reg("rand_f64");
            writeln!(self.out, "  {r} = call double @nir_rand_f64()").unwrap();
            return Ok(r);
        }
        if name == "rand_gaussian" {
            let mean = self.expr(&args[0], scopes)?;
            let stddev = self.expr(&args[1], scopes)?;
            let r = self.fresh_reg("rand_gaussian");
            writeln!(self.out, "  {r} = call double @nir_rand_gaussian(double {mean}, double {stddev})").unwrap();
            return Ok(r);
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

        let ret_llty = self.llvm_ty(&sig_ret)?;
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
                // storage, or a fresh literal's). A constructor argument
                // is built with `want` (this parameter's declared type)
                // as its expected type via `expr_ptr_expected`, so an
                // `Option(i64)` parameter receives a `None`/`Some(5)`
                // argument constructed against exactly that instantiation.
                let ptr = self.expr_ptr_expected(a, want, scopes)?;
                arg_vals.push(format!("ptr {ptr}"));
                continue;
            }
            let llty = self.llvm_ty(want)?;
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
                let elem_llty = self.llvm_ty(&elem)?;
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
                let elem_llty = self.llvm_ty(&elem)?;
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
                let elem_llty = self.llvm_ty(&elem)?;
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
        let agg_llty = self.llvm_ty(&result_ty)?;
        let dest = self.fresh_reg("builtin_result.addr");
        self.emit_alloca(&dest, &agg_llty);

        match name {
            "transpose" => {
                let Ty::Matrix(elem, rows, cols) = self.local_ty_of(&args[0], scopes) else {
                    unreachable!("typeck.rs already proved this is a Matrix")
                };
                let elem_llty = self.llvm_ty(&elem)?;
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
                let elem_llty = self.llvm_ty(&elem)?;
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
            // Row 11: a constructor call reached *without* an expected
            // type in hand (a bare constructor expression statement, or
            // any other `expr_ptr`-reached position `expr_ptr_expected`
            // didn't intercept). Resolve the expected type by
            // structural inference from the arguments' own types
            // (`ctor_ty`, the same fall-back `typeck.rs::resolve_type_args`
            // uses when no context is available); the one genuinely-
            // ambiguous case — a zero-payload variant like `None` with no
            // enclosing type context — is the disclosed `CodegenError`
            // the struct/enum codegen plan names, not a guess.
            Expr::Call(name, args, span) => {
                if self.registry.is_struct(name) || self.registry.find_variant(name).is_some() {
                    let expected = self.ctor_ty(name, args, scopes).ok_or_else(|| CodegenError {
                        message: format!(
                            "codegen can't infer the concrete type of `{name}(...)` here without an enclosing type context — give it one (a `let` annotation, a `return` in a typed function, or a typed call argument); this is the one genuinely-ambiguous construction case (a zero-payload variant reached with no expected type)"
                        ),
                    })?;
                    self.construct(name, args, &expected, *span, scopes)
                } else {
                    self.call_ptr(name, args, scopes)
                }
            }
            // Row 11: `base.field` where the field is itself an aggregate
            // (a nested struct, a `Vector`/`Matrix` field) — GEP to the
            // field's index within `base`'s real named struct type and
            // return that pointer directly, no load, no copy (the same
            // "expose the storage, don't clone it" shape `expr_ptr`'s
            // `Expr::Ident` and `Expr::Deref` arms already use). A
            // scalar field reached here is returned as a pointer to the
            // scalar slot — valid for any `expr_ptr` consumer that just
            // forwards the pointer onward, and never the path a scalar
            // `let`/argument takes (those route through `expr()`'s own
            // `Expr::FieldAccess` arm via `local_ty_of`'s scalar result).
            Expr::FieldAccess(base, field, _) => {
                let base_ty = self.local_ty_of(base, scopes);
                let (idx, _field_ty) = self
                    .field_index_and_ty(&base_ty, field)
                    .expect("typeck.rs already proved this base is a struct with this field");
                let base_ptr = self.expr_ptr(base, scopes)?;
                let base_llty = self.llvm_ty(&base_ty)?;
                let gep = self.fresh_reg("fieldptr");
                writeln!(self.out, "  {gep} = getelementptr inbounds {base_llty}, ptr {base_ptr}, i32 0, i32 {idx}").unwrap();
                Ok(gep)
            }
            Expr::Assign(name, rhs, _) => {
                let src = self.expr_ptr(rhs, scopes)?;
                let (ty, dst) = scopes.get(name).expect("typeck.rs already proved this resolves");
                let bytes = agg_byte_size_operand(&ty, &self.registry);
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
            // Aggregate-result `if`/`match` — `expr_ptr` is reached only
            // when the caller already knows this expression has an aggregate
            // type, so `if_expr`/`match_expr` will allocate a slot and
            // return its pointer.
            Expr::If { cond, then_block, else_block, span } => {
                self.if_expr(cond, then_block, else_block.as_deref(), *span, scopes)
            }
            Expr::Match { scrutinee, arms, span } => self.match_expr(scrutinee, arms, *span, scopes),
            _ => unsupported(
                "codegen doesn't support this aggregate expression form yet — only \
                 identifiers, literals, assignment, binary operators, dereferencing a boxed/\
                 borrowed aggregate, function calls/returns, `if`, and `match` are supported so far \
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
        let agg_llty = self.llvm_ty(&ty)?;
        let dest = self.fresh_reg("arraylit.addr");
        self.emit_alloca(&dest, &agg_llty);

        match &ty {
            Ty::Vector(elem, _) => {
                let elem_llty = self.llvm_ty(elem)?;
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

        let agg_llty = self.llvm_ty(&sig_ret)?;
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

    /// Recursively emit a structural equality comparison for two values
    /// of the same type `ty`, pointed to by `l_ptr` and `r_ptr`. Returns an
    /// `i1` SSA register that is true iff the values are equal. Mirrors
    /// `Value::PartialEq` in the interpreter: content equality for `box`/
    /// `&`, field-by-field for structs, tag-then-payload for enums, and
    /// elementwise for `Vector`/`Matrix`.
    fn emit_deep_eq(&mut self, l_ptr: &str, r_ptr: &str, ty: &Ty, span: Span) -> Result<String, CodegenError> {
        match ty {
            Ty::Unit => {
                let one = self.fresh_reg("unit_eq");
                writeln!(self.out, "  {one} = add i1 0, 1").unwrap();
                Ok(one)
            }
            Ty::Bool | Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => {
                let llty = self.llvm_ty(ty)?;
                let l = self.fresh_reg("l");
                let r = self.fresh_reg("r");
                writeln!(self.out, "  {l} = load {llty}, ptr {l_ptr}").unwrap();
                writeln!(self.out, "  {r} = load {llty}, ptr {r_ptr}").unwrap();
                self.icmp("eq", &llty, &l, &r)
            }
            Ty::F64 => {
                let l = self.fresh_reg("l");
                let r = self.fresh_reg("r");
                writeln!(self.out, "  {l} = load double, ptr {l_ptr}").unwrap();
                writeln!(self.out, "  {r} = load double, ptr {r_ptr}").unwrap();
                self.fcmp("oeq", &l, &r)
            }
            Ty::Str => {
                let l_ptr_reg = self.fresh_reg("str_l_ptr");
                let l_len = self.fresh_reg("str_l_len");
                let r_ptr_reg = self.fresh_reg("str_r_ptr");
                let r_len = self.fresh_reg("str_r_len");
                writeln!(self.out, "  {l_ptr_reg} = load ptr, ptr {l_ptr}").unwrap();
                writeln!(self.out, "  {l_len} = load i64, ptr getelementptr ({{ptr, i64}}, ptr {l_ptr}, i32 0, i32 1)").unwrap();
                writeln!(self.out, "  {r_ptr_reg} = load ptr, ptr {r_ptr}").unwrap();
                writeln!(self.out, "  {r_len} = load i64, ptr getelementptr ({{ptr, i64}}, ptr {r_ptr}, i32 0, i32 1)").unwrap();
                let raw = self.fresh_reg("str_eq_raw");
                writeln!(
                    self.out,
                    "  {raw} = call i32 @nir_str_eq(ptr {l_ptr_reg}, i64 {l_len}, ptr {r_ptr_reg}, i64 {r_len})"
                )
                .unwrap();
                self.icmp("ne", "i32", &raw, "0")
            }
            Ty::Box(inner) | Ty::Ref(inner) => {
                let l = self.fresh_reg("box_l");
                let r = self.fresh_reg("box_r");
                writeln!(self.out, "  {l} = load ptr, ptr {l_ptr}").unwrap();
                writeln!(self.out, "  {r} = load ptr, ptr {r_ptr}").unwrap();
                self.emit_deep_eq(&l, &r, inner, span)
            }
            Ty::Vector(elem, n) | Ty::Matrix(elem, _, n) => {
                let len = if matches!(ty, Ty::Vector(..)) { *n } else { n * n };
                let elem_llty = self.llvm_ty(elem)?;
                let mut acc: Option<String> = None;
                for i in 0..len {
                    let l_elem = self.agg_elem_ptr(l_ptr, &elem_llty, i);
                    let r_elem = self.agg_elem_ptr(r_ptr, &elem_llty, i);
                    let eq = self.emit_deep_eq(&l_elem, &r_elem, elem, span)?;
                    acc = Some(match acc {
                        None => eq,
                        Some(prev) => {
                            let out = self.fresh_reg("agg_eq_and");
                            writeln!(self.out, "  {out} = and i1 {prev}, {eq}").unwrap();
                            out
                        }
                    });
                }
                let all_eq = acc.unwrap_or_else(|| {
                    let one = self.fresh_reg("empty_agg_eq");
                    writeln!(self.out, "  {one} = add i1 0, 1").unwrap();
                    one
                });
                Ok(all_eq)
            }
            Ty::Named(name, args) => {
                if let Some(fields) = self.registry.struct_fields(name) {
                    let type_params = self.registry.struct_type_params(name).unwrap_or(&[]);
                    let subst = zip_type_params(type_params, args);
                    let struct_ty = Ty::Named(name.to_string(), args.to_vec());
                    let struct_llty = self.llvm_ty(&struct_ty)?;
                    let mut acc: Option<String> = None;
                    for (i, f) in fields.iter().enumerate() {
                        let field_ty = substitute_ty(&f.ty, &subst);
                        let l_field = self.fresh_reg("l_field");
                        let r_field = self.fresh_reg("r_field");
                        writeln!(self.out, "  {l_field} = getelementptr inbounds {struct_llty}, ptr {l_ptr}, i32 0, i32 {i}").unwrap();
                        writeln!(self.out, "  {r_field} = getelementptr inbounds {struct_llty}, ptr {r_ptr}, i32 0, i32 {i}").unwrap();
                        let eq = self.emit_deep_eq(&l_field, &r_field, &field_ty, span)?;
                        acc = Some(match acc {
                            None => eq,
                            Some(prev) => {
                                let out = self.fresh_reg("struct_eq_and");
                                writeln!(self.out, "  {out} = and i1 {prev}, {eq}").unwrap();
                                out
                            }
                        });
                    }
                    let all_eq = acc.unwrap_or_else(|| {
                        let one = self.fresh_reg("empty_struct_eq");
                        writeln!(self.out, "  {one} = add i1 0, 1").unwrap();
                        one
                    });
                    Ok(all_eq)
                } else if let Some(variants) = self.registry.enum_variants(name) {
                    let type_params = self.registry.enum_type_params(name).unwrap_or(&[]);
                    let subst = zip_type_params(type_params, args);
                    let enum_ty = Ty::Named(name.to_string(), args.to_vec());
                    let enum_llty = self.llvm_ty(&enum_ty)?;

                    let l_tag_ptr = self.fresh_reg("l_tag_addr");
                    let r_tag_ptr = self.fresh_reg("r_tag_addr");
                    writeln!(self.out, "  {l_tag_ptr} = getelementptr inbounds {enum_llty}, ptr {l_ptr}, i32 0, i32 0").unwrap();
                    writeln!(self.out, "  {r_tag_ptr} = getelementptr inbounds {enum_llty}, ptr {r_ptr}, i32 0, i32 0").unwrap();
                    let l_tag = self.fresh_reg("l_tag");
                    let r_tag = self.fresh_reg("r_tag");
                    writeln!(self.out, "  {l_tag} = load i64, ptr {l_tag_ptr}").unwrap();
                    writeln!(self.out, "  {r_tag} = load i64, ptr {r_tag_ptr}").unwrap();
                    let tag_eq = self.icmp("eq", "i64", &l_tag, &r_tag)?;

                    let neq_label = self.fresh_label("enum_eq_neq");
                    let cmp_label = self.fresh_label("enum_eq_cmp");
                    let merge_label = self.fresh_label("enum_eq_merge");
                    writeln!(self.out, "  br i1 {tag_eq}, label %{cmp_label}, label %{neq_label}").unwrap();
                    self.terminated = true;

                    writeln!(self.out, "{neq_label}:").unwrap();
                    self.terminated = false;
                    writeln!(self.out, "  br label %{merge_label}").unwrap();
                    self.terminated = true;

                    writeln!(self.out, "{cmp_label}:").unwrap();
                    self.terminated = false;
                    let l_payload = self.fresh_reg("l_payload");
                    let r_payload = self.fresh_reg("r_payload");
                    writeln!(self.out, "  {l_payload} = getelementptr inbounds {enum_llty}, ptr {l_ptr}, i32 0, i32 1").unwrap();
                    writeln!(self.out, "  {r_payload} = getelementptr inbounds {enum_llty}, ptr {r_ptr}, i32 0, i32 1").unwrap();

                    let default_label = self.fresh_label("enum_eq_default");
                    let mut case_labels: Vec<String> = Vec::new();
                    let mut phi_entries: Vec<(String, String)> = Vec::new();
                    for (vidx, _v) in variants.iter().enumerate() {
                        let label = self.fresh_label(&format!("enum_eq_v{vidx}"));
                        case_labels.push(label.clone());
                    }
                    writeln!(self.out, "  switch i64 {l_tag}, label %{default_label} [").unwrap();
                    // case labels are emitted below, inside the switch brackets
                    for (vidx, _v) in variants.iter().enumerate() {
                        writeln!(self.out, "    i64 {vidx}, label %{}", case_labels[vidx]).unwrap();
                    }
                    writeln!(self.out, "  ]").unwrap();
                    self.terminated = true;

                    writeln!(self.out, "{default_label}:").unwrap();
                    self.terminated = false;
                    writeln!(self.out, "  unreachable").unwrap();
                    self.terminated = true;

                    for (vidx, v) in variants.iter().enumerate() {
                        let label = &case_labels[vidx];
                        writeln!(self.out, "{label}:").unwrap();
                        self.terminated = false;
                        let mut acc: Option<String> = None;
                        let mut word_off: u64 = 0;
                        for decl_ty in &v.payload {
                            let field_ty = substitute_ty(decl_ty, &subst);
                            let l_field = self.fresh_reg("l_field");
                            let r_field = self.fresh_reg("r_field");
                            writeln!(self.out, "  {l_field} = getelementptr inbounds i64, ptr {l_payload}, i64 {word_off}").unwrap();
                            writeln!(self.out, "  {r_field} = getelementptr inbounds i64, ptr {r_payload}, i64 {word_off}").unwrap();
                            let eq = self.emit_deep_eq(&l_field, &r_field, &field_ty, span)?;
                            acc = Some(match acc {
                                None => eq,
                                Some(prev) => {
                                    let out = self.fresh_reg("enum_field_and");
                                    writeln!(self.out, "  {out} = and i1 {prev}, {eq}").unwrap();
                                    out
                                }
                            });
                            word_off += conservative_word_count(&field_ty, &self.registry);
                        }
                        let variant_eq = acc.unwrap_or_else(|| {
                            let one = self.fresh_reg("empty_variant_eq");
                            writeln!(self.out, "  {one} = add i1 0, 1").unwrap();
                            one
                        });
                        writeln!(self.out, "  br label %{merge_label}").unwrap();
                        self.terminated = true;
                        phi_entries.push((variant_eq, label.clone()));
                    }

                    writeln!(self.out, "{merge_label}:").unwrap();
                    self.terminated = false;
                    let result = self.fresh_reg("enum_eq");
                    let mut phi = format!("  {result} = phi i1 [ 0, %{neq_label} ]");
                    for (val, label) in &phi_entries {
                        phi.push_str(&format!(", [ {val}, %{label} ]"));
                    }
                    writeln!(self.out, "{phi}").unwrap();
                    Ok(result)
                } else {
                    Err(CodegenError {
                        message: format!("equality not supported for named type `{name}` in compiled code"),
                    })
                }
            }
            _ => Err(CodegenError {
                message: format!("equality not supported for `{ty:?}` in compiled code"),
            }),
        }
    }

    /// Structural `==`/`!=` on aggregate values (`Vector`/`Matrix`/
    /// `struct`/`enum`) — delegates to `emit_deep_eq` for the recursive,
    /// type-driven comparison. Produces a scalar `bool`, so this is reached
    /// from `binary()` (`expr()`'s path), not `expr_ptr()`.
    fn agg_eq(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, ty: &Ty, scopes: &mut Scopes) -> Result<String, CodegenError> {
        let l_ptr = self.expr_ptr(lhs, scopes)?;
        let r_ptr = self.expr_ptr(rhs, scopes)?;
        let eq = self.emit_deep_eq(&l_ptr, &r_ptr, ty, lhs.span())?;
        if op == BinOp::Eq {
            Ok(eq)
        } else {
            let out = self.fresh_reg("agg_neq");
            writeln!(self.out, "  {out} = xor i1 {eq}, true").unwrap();
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
        let agg_llty = self.llvm_ty(&ty)?;
        let dest = self.fresh_reg("agg_elemwise.addr");
        self.emit_alloca(&dest, &agg_llty);

        let l_ptr = self.expr_ptr(lhs, scopes)?;
        let r_ptr = self.expr_ptr(rhs, scopes)?;
        let (elem, len) = agg_elem_and_len(&ty);
        let elem = elem.clone();
        let elem_llty = self.llvm_ty(&elem)?;
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
        let agg_llty = self.llvm_ty(&result_ty)?;
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
                let elem_llty = self.llvm_ty(m_elem)?;
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
                let elem_llty = self.llvm_ty(l_elem)?;
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
        let elem_llty = self.llvm_ty(&elem)?;
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
            let llty = self.llvm_ty(&result_ty)?;
            let ptr = self.fresh_reg("if_result.addr");
            self.emit_alloca(&ptr, &llty);
            Some((ptr, llty))
        };
        let is_aggregate = result_ty.is_aggregate();

        writeln!(self.out, "  br i1 {c}, label %{then_label}, label %{else_label}").unwrap();

        writeln!(self.out, "{then_label}:").unwrap();
        self.terminated = false;
        scopes.push();
        if let Some((ptr, _)) = &slot {
            self.block_value_to_slot(then_block, &ptr, &result_ty, scopes)?;
        } else {
            self.block_side_effects(then_block, scopes)?;
        }
        // Each branch is its own scope with its own independent affine
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
        let then_terminated = self.terminated;
        if !self.terminated {
            writeln!(self.out, "  br label %{merge_label}").unwrap();
        }

        writeln!(self.out, "{else_label}:").unwrap();
        self.terminated = false;
        match else_block {
            Some(ElseBranch::Block(b)) => {
                scopes.push();
                if let Some((ptr, _)) = &slot {
                    self.block_value_to_slot(b, &ptr, &result_ty, scopes)?;
                } else {
                    self.block_side_effects(b, scopes)?;
                }
                if !self.terminated
                    && let Some(names) = self.free_map.at_if_branch_end.get(&(span, false)).cloned()
                {
                    self.emit_frees_for_names(&names, scopes);
                }
                scopes.pop();
            }
            Some(ElseBranch::If(e2)) => {
                // An `else if` produces a single value; route through the
                // same `if_expr` so nested scalar/aggregate handling is
                // uniform. The slot is shared with the outer `if`.
                if let Some((ptr, _)) = &slot {
                    if result_ty.is_aggregate() {
                        let src = self.expr_ptr(e2, scopes)?;
                        if !self.terminated {
                            let bytes = agg_byte_size_operand(&result_ty, &self.registry);
                            writeln!(
                                self.out,
                                "  call void @llvm.memcpy.p0.p0.i64(ptr {ptr}, ptr {src}, i64 {bytes}, i1 false)"
                            )
                            .unwrap();
                        }
                    } else {
                        let v = self.expr(e2, scopes)?;
                        if !self.terminated {
                            let llty = self.llvm_ty(&result_ty)?;
                            writeln!(self.out, "  store {llty} {v}, ptr {ptr}").unwrap();
                        }
                    }
                } else {
                    // unit-valued else-if: evaluate for side effects only.
                    self.expr(e2, scopes)?;
                }
            }
            None => {}
        };
        let else_terminated = self.terminated;
        if !self.terminated {
            writeln!(self.out, "  br label %{merge_label}").unwrap();
        }

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
                if is_aggregate {
                    // The caller asked for a pointer (we're on the
                    // `expr_ptr` path); the slot itself is the value.
                    Ok(ptr)
                } else {
                    let out = self.fresh_reg("if_val");
                    writeln!(self.out, "  {out} = load {llty}, ptr {ptr}").unwrap();
                    Ok(out)
                }
            }
            None => Ok("0".to_string()), // unit; never meaningfully read
        }
    }

    /// `match scrutinee { ... }` — Row 11's destructuring expression,
    /// shaped exactly like `if_expr`'s slot-allocate/branch/merge
    /// structure, generalized from 2 branches to N. Two scrutinee shapes,
    /// exactly matching `typeck.rs`'s own `check_match`/
    /// `check_literal_match` split and `interpreter.rs`'s own `Expr::Match`
    /// eval:
    ///
    /// **Enum-variant arms** — the scrutinee is a `Ty::Named` enum value
    /// (`is_aggregate()` now), fetched via `expr_ptr`; the variant tag is
    /// loaded from GEP field 0 and dispatched with a real LLVM `switch`
    /// over the declaration-order variant indices (`enum_variants` order,
    /// the same order `declare_named_type`/`construct_variant` use).
    /// `%default` is a single `unreachable` block — `typeck.rs`'s
    /// exhaustiveness check already guarantees every variant is covered,
    /// the same "typeck already proved this" trust the interpreter's own
    /// `.expect(...)` at this exact point already relies on. Each arm
    /// binds its payload fields into fresh stack slots (mirroring the
    /// function-prologue param-binding pattern), evaluates its body into
    /// the shared result slot, and `br`s to the merge block.
    ///
    /// **Literal-pattern arms** — a `str`/`i64`/`bool` scrutinee (never
    /// an enum). `i64`/`bool` use a real LLVM `switch` (native integer
    /// dispatch, `bool` widened to `i64` for a uniform switch type);
    /// `str` has no native switch, so codegen emits a sequential
    /// `nir_str_eq`-then-`br` chain per arm (first match wins, falling
    /// through to the next), ending at the mandatory trailing `_` arm's
    /// block unconditionally — `typeck.rs::check_literal_match` already
    /// guarantees exactly one trailing wildcard and no duplicate literal
    /// arms.
    ///
    /// An aggregate-*result* `match` (an arm body producing a struct/
    /// enum/`Vector`/`Matrix`) is deliberately out of scope, the same
    /// pre-existing gap `expr_ptr`'s own `_ => unsupported(...)` already
    /// covers for `if`: it fails cleanly via `expr_ptr_expected`'s
    /// `expr_ptr` fallback rather than being silently absent. The
    /// overwhelmingly common real case — a scalar result (`area()`'s
    /// `f64`, an `Option`-unwrap's `i64`, a `str`-dispatch's `str`) — goes
    /// through `if_expr`'s already-proven slot/merge mechanism unchanged.
    fn match_expr(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<String, CodegenError> {
        let scrutinee_ty = self.local_ty_of(scrutinee, scopes);
        // All arm bodies share one result type (typeck already proved
        // they agree); `local_ty_of` of the first arm's body is the
        // match's own result type, the same way `if_expr` uses
        // `block_trailing_ty` of the `then` block.
        let result_ty = self.local_ty_of(&arms[0].body, scopes);
        let merge_label = self.fresh_label("match_merge");
        let slot = if result_ty == Ty::Unit {
            None
        } else {
            let llty = self.llvm_ty(&result_ty)?;
            let ptr = self.fresh_reg("match_result.addr");
            self.emit_alloca(&ptr, &llty);
            Some((ptr, llty))
        };
        let is_aggregate = result_ty.is_aggregate();

        match &scrutinee_ty {
            Ty::Named(enum_name, type_args) if self.registry.is_enum(enum_name) => {
                self.match_enum(scrutinee, enum_name, type_args, arms, &result_ty, slot.as_ref(), &merge_label, span, scopes)?
            }
            Ty::Str | Ty::I64 | Ty::Bool => {
                self.match_literal(scrutinee, &scrutinee_ty, arms, &result_ty, slot.as_ref(), &merge_label, span, scopes)?
            }
            _ => unreachable!("typeck.rs already restricted a match scrutinee to an enum or str/i64/bool"),
        }

        writeln!(self.out, "{merge_label}:").unwrap();
        // The merge block is only reachable if at least one arm falls
        // through to it; if every arm unconditionally `return`s, the
        // match itself is a definite-return and `terminated` reflects
        // that so a caller doesn't read a never-produced value.
        if self.terminated {
            writeln!(self.out, "  unreachable").unwrap();
            return Ok("0".to_string());
        }
        match slot {
            Some((ptr, llty)) => {
                if is_aggregate {
                    Ok(ptr)
                } else {
                    let out = self.fresh_reg("match_val");
                    writeln!(self.out, "  {out} = load {llty}, ptr {ptr}").unwrap();
                    Ok(out)
                }
            }
            None => Ok("0".to_string()),
        }
    }

    /// The enum-variant-arms half of `match_expr` — loads the tag word
    /// and `switch`es on it, one case per arm in declaration order. See
    /// `match_expr`'s doc for the overall shape.
    #[allow(clippy::too_many_arguments)]
    fn match_enum(
        &mut self,
        scrutinee: &Expr,
        enum_name: &str,
        type_args: &[Ty],
        arms: &[MatchArm],
        result_ty: &Ty,
        slot: Option<&(String, String)>,
        merge_label: &str,
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<(), CodegenError> {
        let scrutinee_ptr = self.expr_ptr(scrutinee, scopes)?;
        let enum_ty = Ty::Named(enum_name.to_string(), type_args.to_vec());
        let enum_llty = self.llvm_ty(&enum_ty)?;
        let tag_ptr = self.fresh_reg("tag.addr");
        writeln!(self.out, "  {tag_ptr} = getelementptr inbounds {enum_llty}, ptr {scrutinee_ptr}, i32 0, i32 0").unwrap();
        let tag = self.fresh_reg("tag");
        writeln!(self.out, "  {tag} = load i64, ptr {tag_ptr}").unwrap();
        let payload = self.fresh_reg("payload.addr");
        writeln!(self.out, "  {payload} = getelementptr inbounds {enum_llty}, ptr {scrutinee_ptr}, i32 0, i32 1").unwrap();

        let variants = self.registry.enum_variants(enum_name).expect("typeck.rs proved this is an enum");
        let type_params = self.registry.enum_type_params(enum_name).unwrap_or(&[]);
        let subst = zip_type_params(type_params, type_args);

        let default_label = self.fresh_label("match_default");
        // Build the `switch` cases: one per arm, keyed by the variant's
        // declaration-order index. `typeck.rs`'s exhaustiveness check
        // guarantees every variant is covered exactly once, so `default`
        // is genuinely unreachable.
        let mut cases: Vec<String> = Vec::new();
        let mut arm_labels: Vec<String> = Vec::new();
        for arm in arms {
            let vidx = variants.iter().position(|v| v.name == arm.variant).expect("typeck.rs proved every match arm names a declared variant");
            let label = self.fresh_label("match_arm");
            cases.push(format!("i64 {vidx}, label %{label}"));
            arm_labels.push(label);
        }
        writeln!(self.out, "  switch i64 {tag}, label %{default_label} [").unwrap();
        for c in &cases {
            writeln!(self.out, "    {c}").unwrap();
        }
        writeln!(self.out, "  ]").unwrap();
        // The unreachable default — exhaustiveness is typeck's
        // guarantee, the same trust the interpreter's `.expect` relies on.
        writeln!(self.out, "{default_label}:").unwrap();
        writeln!(self.out, "  unreachable").unwrap();

        let mut any_fell_through = false;
        for (arm_idx, (arm, arm_label)) in arms.iter().zip(arm_labels.iter()).enumerate() {
            writeln!(self.out, "{arm_label}:").unwrap();
            self.terminated = false;
            scopes.push();
            let variant = variants.iter().find(|v| v.name == arm.variant).expect("just proved this variant exists");
            let mut word_off: u64 = 0;
            for (name, decl_ty) in arm.bindings.iter().zip(variant.payload.iter()) {
                let field_ty = substitute_ty(decl_ty, &subst);
                let field_ptr = self.fresh_reg("armfield.addr");
                writeln!(self.out, "  {field_ptr} = getelementptr inbounds i64, ptr {payload}, i64 {word_off}").unwrap();
                // Every binding gets its own stack slot — the same
                // "even a scalar gets an alloca" convention `function`'s
                // param-binding uses — so a later reassignment inside the
                // arm body has real storage to store into.
                let slot_llty = self.llvm_ty(&field_ty)?;
                let slot_ptr = self.fresh_reg(&format!("{name}.addr"));
                self.emit_alloca(&slot_ptr, &slot_llty);
                if field_ty.is_aggregate() {
                    let bytes = agg_byte_size_operand(&field_ty, &self.registry);
                    writeln!(self.out, "  call void @llvm.memcpy.p0.p0.i64(ptr {slot_ptr}, ptr {field_ptr}, i64 {bytes}, i1 false)").unwrap();
                } else {
                    let loaded = self.fresh_reg(&format!("{name}.val"));
                    writeln!(self.out, "  {loaded} = load {slot_llty}, ptr {field_ptr}").unwrap();
                    let loaded = self.widen_to_i64(&loaded, &field_ty);
                    let stored = if field_ty.is_integer() { self.narrow_from_i64(&loaded, &field_ty)? } else { loaded };
                    writeln!(self.out, "  store {slot_llty} {stored}, ptr {slot_ptr}").unwrap();
                }
                scopes.define(name, field_ty.clone(), slot_ptr);
                word_off += conservative_word_count(&field_ty, &self.registry);
            }
            let fell_through = if result_ty.is_aggregate() {
                let src = self.expr_ptr(&arm.body, scopes)?;
                if let Some((ptr, _)) = slot {
                    let bytes = agg_byte_size_operand(result_ty, &self.registry);
                    writeln!(
                        self.out,
                        "  call void @llvm.memcpy.p0.p0.i64(ptr {ptr}, ptr {src}, i64 {bytes}, i1 false)"
                    )
                    .unwrap();
                }
                !self.terminated
            } else {
                let body_val = self.expr(&arm.body, scopes)?;
                if !self.terminated {
                    if let Some((ptr, llty)) = slot {
                        let stored = if result_ty.is_integer() { self.narrow_from_i64(&body_val, result_ty)? } else { body_val.clone() };
                        writeln!(self.out, "  store {llty} {stored}, ptr {ptr}").unwrap();
                    }
                }
                !self.terminated
            };
            // Affine payload bindings declared inside this arm are freed
            // here, before we leave the arm's scope; the match-arm-end
            // FreeMap field is populated in Part 2.
            if !self.terminated {
                if let Some(names) = self.free_map.at_match_arm_end.get(&(span, arm_idx)).cloned() {
                    self.emit_frees_for_names(&names, scopes);
                }
            }
            scopes.pop();
            if fell_through {
                writeln!(self.out, "  br label %{merge_label}").unwrap();
                any_fell_through = true;
            }
        }
        self.terminated = !any_fell_through;
        Ok(())
    }

    /// The literal-pattern-arms half of `match_expr` — a `str`/`i64`/
    /// `bool` scrutinee matched against literal-value arms plus a
    /// mandatory trailing `_`. `i64`/`bool` use a real LLVM `switch`;
    /// `str` uses a sequential `nir_str_eq`-then-`br` chain (no native
    /// string switch). See `match_expr`'s doc for the overall shape.
    #[allow(clippy::too_many_arguments)]
    fn match_literal(
        &mut self,
        scrutinee: &Expr,
        scrutinee_ty: &Ty,
        arms: &[MatchArm],
        result_ty: &Ty,
        slot: Option<&(String, String)>,
        merge_label: &str,
        span: Span,
        scopes: &mut Scopes,
    ) -> Result<(), CodegenError> {
        // `typeck.rs::check_literal_match` guarantees the last arm is the
        // `_` wildcard and no earlier arm duplicates a literal.
        let (literal_arms, wildcard) = arms.split_at(arms.len() - 1);
        let wildcard = &wildcard[0];

        // Emit one arm's body into the shared result slot, then `br` to
        // the merge block (unless the body itself terminated). Shared by
        // every arm across both the int/bool `switch` and the `str` chain.
        // `scopes` is left untouched here (no payload bindings exist for
        // a literal arm — `check_literal_match` keeps `bindings` empty);
        // the caller wraps its own `scopes.push()`/`pop()` where needed.
        let emit_arm_body = |cg: &mut Codegen<'_>,
                             arm_idx: usize,
                             body: &Expr,
                             scopes: &mut Scopes|
         -> Result<bool, CodegenError> {
            if result_ty.is_aggregate() {
                let src = cg.expr_ptr(body, scopes)?;
                if !cg.terminated {
                    if let Some((ptr, _)) = slot {
                        let bytes = agg_byte_size_operand(result_ty, &cg.registry);
                        writeln!(
                            cg.out,
                            "  call void @llvm.memcpy.p0.p0.i64(ptr {ptr}, ptr {src}, i64 {bytes}, i1 false)"
                        )
                        .unwrap();
                    }
                }
            } else {
                let body_val = cg.expr(body, scopes)?;
                if !cg.terminated {
                    if let Some((ptr, llty)) = slot {
                        let stored = if result_ty.is_integer() { cg.narrow_from_i64(&body_val, result_ty)? } else { body_val.clone() };
                        writeln!(cg.out, "  store {llty} {stored}, ptr {ptr}").unwrap();
                    }
                }
            }
            if !cg.terminated {
                if let Some(names) = cg.free_map.at_match_arm_end.get(&(span, arm_idx)).cloned() {
                    cg.emit_frees_for_names(&names, scopes);
                }
                writeln!(cg.out, "  br label %{merge_label}").unwrap();
            }
            Ok(!cg.terminated)
        };

        let mut any_fell_through = false;

        match scrutinee_ty {
            Ty::I64 | Ty::Bool => {
                let raw = self.expr(scrutinee, scopes)?;
                let tag = if *scrutinee_ty == Ty::Bool {
                    let t = self.fresh_reg("match_bool");
                    writeln!(self.out, "  {t} = zext i1 {raw} to i64").unwrap();
                    t
                } else {
                    raw
                };
                let arm_labels: Vec<String> = literal_arms.iter().map(|_| self.fresh_label("match_arm")).collect();
                let default_label = self.fresh_label("match_default");
                let mut cases: Vec<String> = Vec::new();
                for (a, label) in literal_arms.iter().zip(arm_labels.iter()) {
                    let lit = match a.pattern.as_ref().expect("literal arm always has a pattern") {
                        LiteralPattern::Int(n) => format!("i64 {n}"),
                        LiteralPattern::Bool(b) => format!("i64 {}", if *b { 1 } else { 0 }),
                        LiteralPattern::Wildcard | LiteralPattern::Str(_) => {
                            unreachable!("check_literal_match keeps int/bool arms int/bool-typed")
                        }
                    };
                    cases.push(format!("{lit}, label %{label}"));
                }
                writeln!(self.out, "  switch i64 {tag}, label %{default_label} [").unwrap();
                for c in &cases {
                    writeln!(self.out, "    {c}").unwrap();
                }
                writeln!(self.out, "  ]").unwrap();
                // The default is the wildcard arm — every non-listed
                // value falls through to it, exactly the `_` semantics.
                for (arm_idx, (a, label)) in literal_arms.iter().zip(arm_labels.iter()).enumerate() {
                    writeln!(self.out, "{label}:").unwrap();
                    self.terminated = false;
                    let fell = emit_arm_body(self, arm_idx, &a.body, scopes)?;
                    any_fell_through |= fell;
                }
                writeln!(self.out, "{default_label}:").unwrap();
                self.terminated = false;
                let wildcard_idx = literal_arms.len();
                let fell = emit_arm_body(self, wildcard_idx, &wildcard.body, scopes)?;
                any_fell_through |= fell;
            }
            Ty::Str => {
                // No native string switch — a sequential `nir_str_eq`-
                // then-`br` chain, first match wins, falling through to
                // the next comparison; after the last literal arm, the
                // fall-through goes straight to the wildcard arm.
                let s = self.expr(scrutinee, scopes)?;
                let s_ptr = self.fresh_reg("match_str_ptr");
                writeln!(self.out, "  {s_ptr} = extractvalue {{ptr, i64}} {s}, 0").unwrap();
                let s_len = self.fresh_reg("match_str_len");
                writeln!(self.out, "  {s_len} = extractvalue {{ptr, i64}} {s}, 1").unwrap();

                let arm_labels: Vec<String> = literal_arms.iter().map(|_| self.fresh_label("match_arm")).collect();
                let wildcard_label = self.fresh_label("match_arm");
                // `cmp_label[0]` is the first comparison's block; the
                // initial `br` targets it. Each comparison falls through
                // to the next comparison's block (or the wildcard's, after
                // the last literal arm).
                let cmp_labels: Vec<String> = literal_arms.iter().map(|_| self.fresh_label("match_str_cmp")).collect();
                let fallthrough_targets: Vec<String> = cmp_labels
                    .iter()
                    .skip(1)
                    .cloned()
                    .chain(std::iter::once(wildcard_label.clone()))
                    .collect();

                writeln!(self.out, "  br label %{cmp0}", cmp0 = cmp_labels[0]).unwrap();
                for (i, (a, arm_label)) in literal_arms.iter().zip(arm_labels.iter()).enumerate() {
                    let LiteralPattern::Str(lit) = a.pattern.as_ref().expect("str arm") else { unreachable!() };
                    let cmp_label = &cmp_labels[i];
                    let next_label = &fallthrough_targets[i];
                    writeln!(self.out, "{cmp_label}:").unwrap();
                    self.terminated = false;
                    // The literal's backing global — same emission shape
                    // `Expr::Str` uses, but the literal lives in `a.pattern`,
                    // not an `Expr::Str`, so emit it inline here.
                    let global = self.fresh_global("str");
                    let bytes = lit.as_bytes();
                    let escaped = llvm_escape_bytes(bytes);
                    writeln!(self.string_globals, "{global} = private unnamed_addr constant [{} x i8] c\"{escaped}\\00\"", bytes.len() + 1).unwrap();
                    let lit_ptr = self.fresh_reg("match_lit_ptr");
                    writeln!(self.out, "  {lit_ptr} = getelementptr [{} x i8], ptr {global}, i64 0, i64 0", bytes.len() + 1).unwrap();
                    let lit_len = bytes.len() as i64;
                    let raw = self.fresh_reg("match_streq");
                    writeln!(self.out, "  {raw} = call i32 @nir_str_eq(ptr {s_ptr}, i64 {s_len}, ptr {lit_ptr}, i64 {lit_len})").unwrap();
                    let is_eq = self.icmp("ne", "i32", &raw, "0")?;
                    writeln!(self.out, "  br i1 {is_eq}, label %{arm_label}, label %{next_label}").unwrap();
                    // The arm body block.
                    writeln!(self.out, "{arm_label}:").unwrap();
                    self.terminated = false;
                    let fell = emit_arm_body(self, i, &a.body, scopes)?;
                    any_fell_through |= fell;
                }
                // Wildcard arm — the unconditional catch-all.
                writeln!(self.out, "{wildcard_label}:").unwrap();
                self.terminated = false;
                let wildcard_idx = literal_arms.len();
                let fell = emit_arm_body(self, wildcard_idx, &wildcard.body, scopes)?;
                any_fell_through |= fell;
            }
            _ => unreachable!("caller already restricted scrutinee_ty to str/i64/bool"),
        }
        self.terminated = !any_fell_through;
        Ok(())
    }

    /// Run `block` for side effects only and return whether it falls
    /// through (so the caller knows whether to emit a branch to the merge
    /// label). Used by unit-valued `if`/`match` arms.
    fn block_side_effects(&mut self, block: &Block, scopes: &mut Scopes) -> Result<(), CodegenError> {
        match block.stmts.split_last() {
            None => {}
            Some((last, rest)) => {
                self.stmts(rest, scopes)?;
                if !self.terminated {
                    match last {
                        Stmt::Expr(e) => {
                            self.expr(e, scopes)?;
                        }
                        other => {
                            self.stmt(other, scopes)?;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    /// Evaluate `block` and store its trailing value into `slot_ptr`.
    /// Used by `if_expr`/`match_expr` for both scalar and aggregate result
    /// slots.
    fn block_value_to_slot(
        &mut self,
        block: &Block,
        slot_ptr: &str,
        result_ty: &Ty,
        scopes: &mut Scopes,
    ) -> Result<(), CodegenError> {
        match block.stmts.split_last() {
            None => Ok(()),
            Some((last, rest)) => {
                self.stmts(rest, scopes)?;
                if self.terminated {
                    return Ok(());
                }
                match last {
                    Stmt::Expr(e) => {
                        if result_ty.is_aggregate() {
                            let src = self.expr_ptr(e, scopes)?;
                            let bytes = agg_byte_size_operand(result_ty, &self.registry);
                            writeln!(
                                self.out,
                                "  call void @llvm.memcpy.p0.p0.i64(ptr {slot_ptr}, ptr {src}, i64 {bytes}, i1 false)"
                            )
                            .unwrap();
                        } else {
                            let v = self.expr(e, scopes)?;
                            let llty = self.llvm_ty(result_ty)?;
                            writeln!(self.out, "  store {llty} {v}, ptr {slot_ptr}").unwrap();
                        }
                    }
                    other => {
                        self.stmt(other, scopes)?;
                    }
                }
                Ok(())
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
                "codegen doesn't support `main` returning a Vector/Matrix/struct/enum directly yet — print \
                 its elements/fields instead of returning the aggregate itself",
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
            // the generic fallback below knows) can turn into an `i32`.
            // Resolved the same way `Ty::Unit` above is: there's no exit
            // code to compute either way, so print the value (exactly
            // `Codegen::call`'s own `Ty::Str` print-arm sequence — same
            // `%.*s` format, same explicit length rather than a NUL scan,
            // `Ty::Str`'s own note in `llvm_ty` on why) and exit 0. This
            // is precisely the workaround the old rejection message told
            // callers to do by hand (`print(...)` then return `unit`);
            // automating it here removes the need for that workaround.
            let r = self.fresh_reg("main_str_result");
            writeln!(self.out, "  {r} = call {{ptr, i64}} @nir_main()").unwrap();
            let ptr_reg = self.fresh_reg("main_str_ptr");
            writeln!(self.out, "  {ptr_reg} = extractvalue {{ptr, i64}} {r}, 0").unwrap();
            let len_reg = self.fresh_reg("main_str_len");
            writeln!(self.out, "  {len_reg} = extractvalue {{ptr, i64}} {r}, 1").unwrap();
            let len_i32 = self.fresh_reg("main_str_len_i32");
            writeln!(self.out, "  {len_i32} = trunc i64 {len_reg} to i32").unwrap();
            writeln!(self.out, "  call i32 (ptr, ...) @printf(ptr @.str_fmt, i32 {len_i32}, ptr {ptr_reg})").unwrap();
            writeln!(self.out, "  ret i32 0").unwrap();
        } else {
            let llty = self.llvm_ty(&main_fn.ret)?;
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
                // `i8`/`i16`-width results (`i8`/`i16`, and their
                // unsigned counterparts `u8`/`u16`, which map to the
                // exact same LLVM widths — `llty` alone can't tell them
                // apart, hence checking `main_fn.ret.is_unsigned()`
                // directly) need widening up to `i32`; `zext` for
                // unsigned, `sext` for signed, same distinction
                // `widen_to_i64` makes and for the same reason.
                _ => {
                    let t = self.fresh_reg("exit_code");
                    let op = if main_fn.ret.is_unsigned() { "zext" } else { "sext" };
                    writeln!(self.out, "  {t} = {op} {llty} {r} to i32").unwrap();
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
/// source. `O2` is the default `build()` uses because docs/goal.md row 5 is
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

/// The OS-level system libraries `RUNTIME_KERNELS_LIB`'s own code (now
/// including the `nir_tcp_*` kernels' `std::net` calls) needs at final
/// link time — captured by `build.rs` via `rustc --print=native-static-
/// libs` at the same moment it builds that staticlib, since `rustc`
/// itself doesn't drive this crate's final link (see `build.rs`'s doc
/// comment for the real Windows failure this fixes: `ws2_32.lib` wasn't
/// being linked, so `nir_tcp_connect`/etc. were unresolved externals).
/// Whitespace-separated, already in whatever form the platform's own
/// linker expects (`-lfoo` on Unix, `foo.lib` on Windows-MSVC) — passed
/// through to `clang` as separate arguments unchanged, not parsed
/// further.
#[allow(dead_code)] // only read under `#[cfg(windows)]` below; Unix has its own `-lm` arm
static NATIVE_STATIC_LIBS: &str = include_str!(concat!(env!("OUT_DIR"), "/native_static_libs.txt"));

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

    let mut clang_cmd = std::process::Command::new("clang");
    clang_cmd.arg(&ll_path).arg(&runtime_lib_path).arg(opt.clang_flag());
    // Phase 4's `declare double @atan2(double, double)` (geometry
    // builtins) has no LLVM intrinsic form, unlike `sqrt`/`sin`/`cos` —
    // it's the plain libm function, so it needs to actually be linked.
    // Harmless to pass unconditionally on Unix even for a program that
    // never calls it. Windows has no separate `libm` to link against —
    // math functions live in the C runtime clang already links by
    // default — and passing `-lm` there makes the MSVC linker fail
    // outright looking for a nonexistent `m.lib`, so this flag is
    // Unix-only.
    #[cfg(unix)]
    clang_cmd.arg("-lm");
    // Windows has no equivalent hand-picked single flag — `std::net`
    // (the `nir_tcp_*` kernels) needs `ws2_32.lib`, and other stdlib
    // pieces need their own system libs beside it, so the captured,
    // rustc-verified list is used instead of guessing which ones.
    // `NATIVE_STATIC_LIBS`'s doc comment on its declaration above has the
    // real failure this fixes.
    //
    // Can't pass rustc's tokens straight through as positional args:
    // found on real Windows CI, a real second failure past the first —
    // `clang: error: no such file or directory: 'kernel32.lib'`. Clang
    // preflight-checks any *positional* (non-flag) argument as a literal
    // path relative to the current directory, even though a plain
    // `foo.lib` token is exactly what MSVC's own linker resolves via its
    // library search path, never by looking in the cwd. `-lfoo` (Clang's
    // ordinary, cross-target library flag) skips that preflight check
    // entirely and does reach the linker's search path — so each
    // `foo.lib` token here is stripped to `foo` and passed as `-lfoo`
    // instead. rustc's list also has at least one token that isn't
    // `.lib`-suffixed at all (`/defaultlib:msvcrt`, already a raw linker
    // flag) — that one is forwarded verbatim via `-Xlinker`, which routes
    // it straight to the linker unexamined, the same reason `-l` works
    // for the others.
    #[cfg(windows)]
    for token in NATIVE_STATIC_LIBS.split_whitespace() {
        match token.strip_suffix(".lib") {
            Some(name) => {
                clang_cmd.arg(format!("-l{name}"));
            }
            None => {
                clang_cmd.arg("-Xlinker").arg(token);
            }
        }
    }
    let result = clang_cmd.arg("-o").arg(output_path).output();
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
