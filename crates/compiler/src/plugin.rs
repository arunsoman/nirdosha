//! `docs/ROADMAP.md` Track G, G1 / `docs/ECOSYSTEM.md` §G1's Stage 1: a
//! real, additive extension point that lets a third-party Rust crate
//! register new builtin functions into `typeck.rs`'s static checking and
//! `interpreter.rs`'s evaluation, *without* editing either file — the
//! "native/builtin extension package (Kind A)" half of that design.
//!
//! Deliberately **not** dynamic loading (no `dlopen`, no stable Rust
//! ABI to rely on for that): a plugin crate is an ordinary Rust
//! dependency, compiled and statically linked into whatever binary
//! calls [`nirdosha::run_with_plugins`](crate::run_with_plugins) —
//! exactly the same "`cargo add` a crate, `cargo build` links it in"
//! story any other native Rust dependency already has, which is the
//! whole point of reusing Cargo instead of inventing a bespoke
//! registry (`docs/ECOSYSTEM.md` §G1's "why this is more promising"
//! section).
//!
//! `crates/plugin-example-rot13/` is the one real reference
//! implementation this Stage 1 pass built to prove the shape actually
//! works end to end (see that crate's own `README.md`) — not a second,
//! parallel toy example invented separately from what a real
//! third-party plugin author would write.

use crate::ast::{Effect, Ty};
use crate::interpreter::{RuntimeError, Value};
use crate::token::Span;
use std::collections::BTreeSet;
use std::sync::Arc;

/// The signature + implementation of one new builtin a plugin crate
/// contributes. `name` must not collide with any name in
/// `ast::BUILTIN_NAMES`, a user `fn`, a `struct`/enum-variant
/// constructor, or another plugin's own `name` — `typecheck_with_plugins`
/// rejects that the same way it already rejects a user `fn` shadowing a
/// real builtin (`TypeErrorKind::FnNameShadowsBuiltin`/
/// `DuplicateConstructor`).
///
/// Deliberately flat and positional (`params`/`ret` as plain `Ty`s, no
/// generics, no overloading by arity) — the same "narrow, concrete,
/// cheap to extend later" discipline every builtin in `ast::BUILTIN_NAMES`
/// already follows, not a new capability class of its own.
#[derive(Clone)]
pub struct PluginBuiltin {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// Which of `ast::Effect`'s four tags (`Rng`/`Io`/`Concurrent`/
    /// `Network`) this builtin's `call` produces — the empty set for a
    /// pure function like `rot13`. **rfcs/0003-plugin-abi-v2.md**: this
    /// field exists to close a real, previously-live unsoundness —
    /// before it, `effects.rs`'s `Expr::Call` arm attributed *zero*
    /// effect tags to any plugin builtin (it matched neither
    /// `is_builtin` nor a known user function), so a plugin doing real
    /// network I/O could be called from an `effect(pure)`-declared
    /// function and typecheck clean. Never invent a new tag here — a
    /// plugin effect is always a new *producer* of one of the four
    /// existing kinds, never a fifth kind (see `effects.rs`'s own
    /// module doc on why the set is deliberately closed).
    pub effects: BTreeSet<Effect>,
    pub call: PluginFn,
}

/// A plugin builtin's runtime implementation. Takes the already-evaluated
/// argument `Value`s (typecheck has already proved there are exactly
/// `params.len()` of them, each the declared type) and the call site's
/// `Span`, for building a real, spanned `RuntimeError` on failure — the
/// same error type `eval_builtin` itself returns, not a bespoke plugin
/// error channel. `Arc`, not a plain `fn` pointer or `Box`: needs to be
/// cheaply `Clone`d into `Interpreter::plugins` and propagated to every
/// spawned child `Interpreter` (`Expr::Spawn`'s handler) the same way
/// `sandbox_exe`/`tracer` already are.
pub type PluginFn = Arc<dyn Fn(&[Value], Span) -> Result<Value, RuntimeError> + Send + Sync>;

/// Implemented by a plugin crate's own top-level type (see
/// `crates/plugin-example-rot13/src/lib.rs` for the reference shape).
/// One crate can contribute more than one builtin from a single
/// `builtins()` call — there's no requirement that a plugin crate map
/// 1:1 to one builtin.
pub trait NirdoshaPlugin {
    fn builtins(&self) -> Vec<PluginBuiltin>;
}

/// Convenience: build the `HashMap` shape both `typeck.rs` (signatures
/// only) and `interpreter.rs` (implementations only) each need from one
/// flat `&[PluginBuiltin]` list — the shape a caller naturally has after
/// calling one or more plugins' `builtins()`. Kept here, not duplicated
/// at each of `run_with_plugins`'s two call sites into typeck/interpreter.
pub(crate) fn signatures(plugins: &[PluginBuiltin]) -> std::collections::HashMap<String, (Vec<Ty>, Ty)> {
    plugins.iter().map(|p| (p.name.clone(), (p.params.clone(), p.ret.clone()))).collect()
}

pub(crate) fn implementations(plugins: &[PluginBuiltin]) -> std::collections::HashMap<String, PluginFn> {
    plugins.iter().map(|p| (p.name.clone(), p.call.clone())).collect()
}

/// Same shape/purpose as `signatures`/`implementations` above, for
/// `effects.rs::infer_effects_with_plugins` (rfcs/0003-plugin-abi-v2.md):
/// the one flat map it needs to attribute a plugin call's real effects
/// instead of silently attributing none.
pub(crate) fn effect_map(plugins: &[PluginBuiltin]) -> std::collections::HashMap<String, BTreeSet<Effect>> {
    plugins.iter().map(|p| (p.name.clone(), p.effects.clone())).collect()
}

/// rfcs/0005-plugin-boundary-safety-and-performance.md §3: the
/// compiled-path answer `docs/ECOSYSTEM.md` names as a real, deliberate
/// gap ("no stable calling convention from generated LLVM IR into an
/// opaque `Arc<dyn Fn>` exists"). `PluginFn`'s own shape is exactly
/// right for the interpreter and exactly wrong for `codegen.rs` — a
/// Rust trait object has no meaning to LLVM IR generated by a
/// *different* compilation. This is the other half: a plugin that
/// *also* exports a plain, `#[no_mangle] extern "C"` symbol with a
/// scalar-only signature can be called directly from compiled `.nir`
/// code, exactly the way `codegen.rs`'s own `nir_det`/`nir_rank`/
/// `nir_str_eq` (Phase 5's "linked native call into a staticlib"
/// pattern, `emit_llvm_ir`'s preamble) already work — this generalizes
/// that mechanism to a third-party-provided symbol/library instead of
/// only `runtime-kernels/src/lib.rs`'s own.
///
/// Deliberately **not** a field on `PluginBuiltin` (which stays exactly
/// as it was): a native-callable builtin is a *stricter* subset (no
/// `str`/aggregate/`Db`/`Handle` — anything `codegen.rs::llvm_ty`
/// doesn't already emit a scalar LLVM type for, checked by
/// [`NativePluginBuiltin::validate`]), and keeping it a separate,
/// explicit opt-in avoids ever silently expecting a `str`-taking plugin
/// builtin to somehow compile.
pub struct NativePluginBuiltin {
    /// Must equal the corresponding `PluginBuiltin.name` this native
    /// form backs, and must also be the exact `#[no_mangle] extern "C"`
    /// symbol name in `static_lib` — `codegen.rs` emits one `declare`
    /// and one `call`, both spelled with this one name, the same way a
    /// user `fn`'s own name already doubles as its LLVM symbol.
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    /// A precompiled `staticlib` (`.a`) containing `name`'s
    /// `#[no_mangle] extern "C"` definition — a plugin crate's own
    /// `include_bytes!(concat!(env!("OUT_DIR"), "/lib....a"))`, the
    /// exact pattern `codegen.rs`'s own `RUNTIME_KERNELS_LIB` already
    /// uses for its hand-written kernels. Linked into the final `clang`
    /// invocation alongside the generated `.ll` — see
    /// `codegen::build_with_native_plugins`.
    pub static_lib: &'static [u8],
}

impl NativePluginBuiltin {
    /// Every param and the return type must be a type `codegen.rs`
    /// already emits as a plain LLVM scalar (or `void`) — `str`,
    /// `Vector`/`Matrix`, `Json`, `Db`/`Mq`/`Handle`, any `Ty::Named`
    /// struct/enum, are all real ABI questions (multi-word values,
    /// pointers with real ownership) this narrow mechanism doesn't
    /// attempt to answer; see rfcs/0005 §3's own "harder, still-open
    /// question" for why that's a separate, bigger design, not an
    /// oversight here.
    pub fn validate(&self) -> Result<(), String> {
        fn is_native_scalar(ty: &Ty) -> bool {
            matches!(
                ty,
                Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize | Ty::F64 | Ty::Bool | Ty::Unit
            )
        }
        for p in &self.params {
            if !is_native_scalar(p) {
                return Err(format!(
                    "native plugin builtin `{}`: parameter type `{}` isn't a supported native-ABI \
                     scalar (only i8..usize/f64/bool are) -- str/aggregate/Db/Mq/Handle types can't \
                     cross this boundary yet (rfcs/0005 §3)",
                    self.name,
                    p.name()
                ));
            }
        }
        if !is_native_scalar(&self.ret) {
            return Err(format!(
                "native plugin builtin `{}`: return type `{}` isn't a supported native-ABI scalar",
                self.name,
                self.ret.name()
            ));
        }
        Ok(())
    }
}
