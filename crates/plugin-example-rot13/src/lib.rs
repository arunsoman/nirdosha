//! The one real reference plugin `docs/ROADMAP.md` Track G, G1 /
//! `docs/ECOSYSTEM.md` §G1's Stage 1 plan calls for: a genuine, if
//! small, third-party-shaped Rust crate that adds one new Nirdosha
//! builtin — `rot13(s: str) -> str` — proving the whole round trip
//! (declare a signature, get real static type-checking against it,
//! get real runtime dispatch) actually works, not just that the trait
//! compiles in isolation.
//!
//! `tests/end_to_end.rs` is the actual proof: it runs a real `.nir`
//! program, through the real lex/parse/typecheck/ownership/interpret
//! pipeline (`nirdosha::run_with_plugins`), that calls `rot13` and
//! checks the returned value — not a unit test of this crate's Rust
//! function in isolation, which would prove nothing about the plugin
//! mechanism itself.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{ErrorKind, RuntimeError, Value};
use nirdosha::plugin::{NirdoshaPlugin, PluginBuiltin};
use nirdosha::token::Span;
use std::sync::Arc;

pub struct Rot13Plugin;

impl NirdoshaPlugin for Rot13Plugin {
    fn builtins(&self) -> Vec<PluginBuiltin> {
        vec![PluginBuiltin {
            name: "rot13".to_string(),
            params: vec![Ty::Str],
            ret: Ty::Str,
            // A pure string transform -- no I/O, no RNG, nothing
            // concurrent, no network. The empty set (rfcs/0003-plugin-abi-v2.md).
            effects: Default::default(),
            call: Arc::new(rot13_call),
        }]
    }
}

/// `typeck.rs::infer_builtin_call`'s plugin arm has already proved
/// `args` is exactly one `str` — this never has to defensively check
/// arity/type, the same trust every hand-written builtin arm in
/// `interpreter.rs`'s own `eval_builtin` already places in `typeck.rs`
/// having run first. Still returns `Result`, not a bare `Value`: a
/// plugin builtin can always fail for a reason typechecking can't see
/// (this one never does, but e.g. a real HTTP-calling plugin builtin
/// would) — `ErrorKind::PluginError` is that real, spanned failure
/// path, not a Rust-level panic.
fn rot13_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let Value::Str(s) = &args[0] else {
        return Err(RuntimeError {
            kind: ErrorKind::PluginError {
                plugin: "rot13".to_string(),
                message: format!("expected a str argument, got {:?}", args[0]),
            },
            span,
        });
    };
    let rotated: String = s
        .chars()
        .map(|c| match c {
            'a'..='z' => (((c as u8 - b'a' + 13) % 26) + b'a') as char,
            'A'..='Z' => (((c as u8 - b'A' + 13) % 26) + b'A') as char,
            other => other,
        })
        .collect();
    Ok(Value::Str(Arc::from(rotated.as_str())))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A plain Rust-level sanity check on the cipher itself (self-inverse,
    /// non-letters untouched) — deliberately *not* the crate's real proof
    /// of the plugin mechanism; see `tests/end_to_end.rs` for that.
    #[test]
    fn rot13_is_self_inverse() {
        let span = Span { line: 0, col: 0 };
        let once = rot13_call(&[Value::Str(Arc::from("Hello, Nirdosha! 123"))], span).unwrap();
        let Value::Str(once) = once else { panic!("expected Str") };
        assert_eq!(&*once, "Uryyb, Aveqbfun! 123");
        let twice = rot13_call(&[Value::Str(once)], span).unwrap();
        let Value::Str(twice) = twice else { panic!("expected Str") };
        assert_eq!(&*twice, "Hello, Nirdosha! 123");
    }
}
