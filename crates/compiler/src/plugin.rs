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
