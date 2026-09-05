//! Proves `Ty::Handle` closes the real, named gap
//! `nirdosha-plugin-support::HandleRegistry`'s own doc comment
//! disclosed: "a handle... is just a `Value::Int`... `ownership.rs`
//! gives it none of the affine... guarantees a real `Ty::Db`/`Ty::Mq`/
//! `Ty::Sandbox` handle gets today." A mock plugin below (`connect`/
//! `close`/`query`, the same shape every real stateful plugin in
//! `crates/plugin-example-{mysql,cassandra,neo4j,...}` already has)
//! declares its handle-typed builtins with `Ty::Handle("Widget".into())`
//! instead of the `Ty::I64` those plugins use today. No change to
//! `plugin.rs`, `interpreter.rs`, or `HandleRegistry` itself was needed
//! for this to work — see `Ty::Handle`'s own doc comment in `ast.rs` for
//! why the ownership fix is entirely a type-checker-level change.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{ErrorKind, RuntimeError, Value};
use nirdosha::plugin::PluginBuiltin;
use nirdosha::token::Span;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::Arc;

/// A minimal stand-in for `HandleRegistry<T>` -- just enough to prove
/// the *type-checker* side works; this test isn't about the runtime
/// registry, which is unchanged.
fn mock_widget_plugin() -> Vec<PluginBuiltin> {
    static NEXT_ID: AtomicI64 = AtomicI64::new(1);
    static OPEN: std::sync::Mutex<Option<std::collections::HashSet<i64>>> = std::sync::Mutex::new(None);

    fn open_set() -> std::sync::MutexGuard<'static, Option<std::collections::HashSet<i64>>> {
        let mut g = OPEN.lock().unwrap();
        if g.is_none() {
            *g = Some(std::collections::HashSet::new());
        }
        g
    }

    vec![
        PluginBuiltin {
            name: "widget_connect".to_string(),
            params: vec![],
            ret: Ty::Handle("Widget".to_string()),
            effects: Default::default(),
            call: Arc::new(|_args: &[Value], _span: Span| {
                let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
                open_set().as_mut().unwrap().insert(id);
                Ok(Value::Int(id))
            }),
        },
        PluginBuiltin {
            name: "widget_close".to_string(),
            params: vec![Ty::Handle("Widget".to_string())],
            ret: Ty::I64,
            effects: Default::default(),
            call: Arc::new(|args: &[Value], span: Span| {
                let Value::Int(id) = &args[0] else { unreachable!() };
                let removed = open_set().as_mut().unwrap().remove(id);
                if removed {
                    Ok(Value::Int(1))
                } else {
                    Err(RuntimeError {
                        kind: ErrorKind::PluginError {
                            plugin: "widget".to_string(),
                            message: format!("handle {id} was already closed"),
                        },
                        span,
                    })
                }
            }),
        },
        PluginBuiltin {
            name: "widget_query".to_string(),
            // `&handle(Widget)`, not a bare `handle(Widget)`: a "read"
            // operation must borrow, not consume, the same way `Ty::Ref`
            // already lets *any* affine content be freely, repeatedly
            // read (`Ty::is_affine`'s own doc comment) -- see this file's
            // module doc for why this is the generalizable answer,
            // unlike `db_query`/`db_execute`'s hardcoded per-builtin-name
            // exemption in `ownership.rs`, which only the compiler itself
            // can extend, never a third-party plugin author.
            params: vec![Ty::Ref(Box::new(Ty::Handle("Widget".to_string())))],
            ret: Ty::I64,
            effects: Default::default(),
            call: Arc::new(|args: &[Value], _span: Span| {
                // `Ty::Ref(Ty::Handle(_))` arrives as a real `Value::Ref`
                // wrapper (`interpreter.rs`'s own runtime shape for any
                // `&T`) -- a plugin author writing a borrowing builtin
                // unwraps it exactly once, the same as any other
                // `&`-typed plugin parameter would.
                let inner = match &args[0] {
                    Value::Ref(inner) => inner.as_ref(),
                    other => other,
                };
                let Value::Int(id) = inner else { unreachable!() };
                Ok(Value::Int(*id))
            }),
        },
    ]
}

/// The actual proof: using a handle, closing it, then using it *again*
/// (a real double-close/use-after-close bug pattern) is now a real
/// *compile-time* `UseAfterMove` ownership error -- exactly the same
/// class of error `examples/killer_demo`-style `db`-handle misuse
/// already gets, and exactly what a plain `Ty::I64` handle could never
/// catch before this fix (it would instead be a *runtime* `PluginError`
/// from `widget_close`'s own bookkeeping, one call too late).
#[test]
fn double_close_on_a_plugin_handle_is_a_compile_time_ownership_error() {
    let src = r#"
        fn main() {
            let h: handle(Widget) = widget_connect()
            widget_close(h)
            widget_close(h)
        }
    "#;
    let plugins = mock_widget_plugin();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("a double-close must be rejected before the program ever runs");
    assert!(
        err.contains("ownership") || err.contains("moved") || err.contains("UseAfterMove"),
        "expected an ownership/use-after-move error, got: {err}"
    );
}

/// The straight-line, single-use case -- connect, use, close exactly
/// once -- still works and actually runs (not just typechecks), proving
/// this is a real additive fix, not a regression that breaks ordinary
/// handle usage.
#[test]
fn single_use_then_close_still_works_and_runs() {
    let src = r#"
        fn main() -> i64 {
            let h: handle(Widget) = widget_connect()
            let v: i64 = widget_query(&h)
            widget_close(h)
            return v
        }
    "#;
    let plugins = mock_widget_plugin();
    let result = nirdosha::run_with_plugins(src, &plugins).expect("single-use-then-close must run cleanly");
    assert_eq!(result, Value::Int(1), "widget_connect's first-ever id is 1");
}

/// Storing a handle in a struct field and dropping the struct without
/// closing it is a *leak*, not a double-close -- `Ty::is_affine`'s
/// existing struct-recursion (unchanged by this fix, ast.rs's
/// `TypeRegistry::is_affine_visiting`) means the *containing* struct is
/// affine too, so a `.nir` program can't silently copy a struct holding
/// a handle either. This test documents that boundary, not just the
/// double-close case above.
#[test]
fn a_struct_holding_a_handle_is_affine_too() {
    let src = r#"
        struct WidgetBox {
            h: handle(Widget),
        }
        fn main() {
            let b: WidgetBox = WidgetBox(widget_connect())
            let c: WidgetBox = b
            let d: WidgetBox = b
        }
    "#;
    let plugins = mock_widget_plugin();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("copying an affine-holding struct twice must be rejected");
    assert!(
        err.contains("ownership") || err.contains("moved") || err.contains("UseAfterMove"),
        "expected an ownership/use-after-move error, got: {err}"
    );
}
