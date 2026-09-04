//! The real proof `docs/ROADMAP.md` Track G, G1 / `docs/ECOSYSTEM.md`
//! §G1's Stage 1 asked for: a `.nir` *source program* that calls a
//! plugin-contributed builtin, run through the actual compiler pipeline
//! `nirdosha::run_with_plugins` drives (lex, parse, typecheck against
//! the plugin's declared signature, ownership-check, interpret) — not a
//! unit test of the plugin's own Rust function, and not a hand-rolled
//! shortcut that skips typechecking.

use nirdosha::interpreter::Value;
use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_rot13::Rot13Plugin;

#[test]
fn rot13_builtin_is_callable_from_nir_source() {
    let src = r#"
        fn main() {
            let scrambled: str = rot13("Nirdosha")
            print(scrambled)
        }
    "#;
    let plugins = Rot13Plugin.builtins();
    let result = nirdosha::run_with_plugins(src, &plugins);
    assert!(result.is_ok(), "expected the program to run cleanly, got {result:?}");
    assert_eq!(result.unwrap(), Value::Unit, "fn main() returns nothing -- rot13's actual output is checked via print below");
}

/// Same program, but returning the value directly instead of `print`ing
/// it — confirms `rot13`'s *result* is correct, not just that the call
/// didn't error. Wrapped in a carrier struct rather than `fn main() ->
/// str` directly: `str` can't cross a function boundary bare
/// (`docs/LANGUAGE.md` §6b's enum-favoring str ban) — this crate's own
/// plugin builtin is subject to that same rule (it declares `ret:
/// Ty::Str`, legal for a *builtin call expression's* type, same as
/// `dec_to_str`/`json_get_str` already do; only a `fn`'s own declared
/// signature is restricted), so proving the *value* round-trips means
/// using the documented workaround, exactly as a real `.nir` author
/// would have to.
#[test]
fn rot13_returns_the_correctly_rotated_string() {
    let src = r#"
        struct Text { value: str }
        fn main() -> Text {
            return Text(rot13("Nirdosha"))
        }
    "#;
    let plugins = Rot13Plugin.builtins();
    let result = nirdosha::run_with_plugins(src, &plugins).expect("program should run cleanly");
    assert_eq!(
        result,
        Value::Struct(std::sync::Arc::from("Text"), std::sync::Arc::from([Value::Str(std::sync::Arc::from("Aveqbfun"))]))
    );
}

/// A wrong-arity call is a real, caught *type* error (`typeck.rs`'s
/// plugin arm), the same way calling any real builtin with the wrong
/// number of arguments is — never a runtime panic, never silently
/// accepted.
#[test]
fn wrong_arity_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(rot13("too", "many", "args"))
        }
    "#;
    let plugins = Rot13Plugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong arity must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

/// A wrong-*type* call (an `i64` where `rot13` declares `str`) is
/// likewise a real static type error, proving `infer_builtin_call`'s
/// plugin arm actually checks each argument against `params`, not just
/// counts them.
#[test]
fn wrong_type_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(rot13(42))
        }
    "#;
    let plugins = Rot13Plugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong argument type must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

/// Without `Rot13Plugin` registered at all, `rot13` is just an unknown
/// function — the same error any misspelled/undeclared call gets. Confirms
/// the plugin mechanism is real registration, not some always-on hook.
#[test]
fn without_the_plugin_registered_rot13_is_unknown() {
    let src = r#"
        fn main() {
            print(rot13("Nirdosha"))
        }
    "#;
    let err = nirdosha::run_with_plugins(src, &[]).expect_err("rot13 must be unresolvable with no plugins");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}
