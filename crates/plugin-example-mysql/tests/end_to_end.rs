//! Same discipline as `crates/plugin-example-rot13/tests/end_to_end.rs`:
//! real `.nir` source through the real `nirdosha::run_with_plugins`
//! pipeline, not a unit test of this crate's Rust functions in
//! isolation. The static-only tests below need no MySQL at all (they
//! prove typecheck/registration, the same things rot13's tests prove);
//! the `#[ignore]`d ones need a real server — see this crate's README
//! for how to start one via `crates/plugin-examples/docker-compose.yml`.

use nirdosha::interpreter::Value;
use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_mysql::MysqlPlugin;

fn dsn() -> String {
    std::env::var("MYSQL_DSN").unwrap_or_else(|_| "mysql://root:nirdosha@127.0.0.1:3306/nirdosha_test".to_string())
}

#[test]
fn wrong_arity_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(mysql_connect())
        }
    "#;
    let plugins = MysqlPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong arity must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn wrong_type_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(mysql_connect(42))
        }
    "#;
    let plugins = MysqlPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong argument type must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn without_the_plugin_registered_mysql_connect_is_unknown() {
    let src = r#"
        fn main() {
            print(mysql_connect("mysql://x"))
        }
    "#;
    let err = nirdosha::run_with_plugins(src, &[]).expect_err("mysql_connect must be unresolvable with no plugins");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

/// A bad DSN is a real, spanned `RuntimeError` (`ErrorKind::PluginError`)
/// surfaced through `run_with_plugins`'s error string, not a panic --
/// needs no live server, since parsing/connecting to garbage fails
/// before any network round-trip that would matter.
#[test]
fn a_malformed_dsn_is_a_clean_runtime_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(mysql_connect("not-a-valid-mysql-url"))
        }
    "#;
    let plugins = MysqlPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("a malformed DSN must be rejected");
    assert!(err.contains("plugin"), "expected the plugin error channel, got: {err}");
}

/// The real end-to-end proof, against a live MySQL server -- run
/// `docker compose -f crates/plugin-examples/docker-compose.yml up -d`
/// first, then `cargo test -p nirdosha-plugin-mysql -- --ignored`.
#[test]
#[ignore = "needs a live MySQL server; see crates/plugin-examples/docker-compose.yml"]
fn connect_create_insert_query_and_close_round_trip_against_a_real_server() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = mysql_connect("{dsn}")
            mysql_execute(h, "CREATE TABLE IF NOT EXISTS nirdosha_smoke (id INT PRIMARY KEY, label VARCHAR(64))")
            mysql_execute(h, "REPLACE INTO nirdosha_smoke (id, label) VALUES (1, 'from-nirdosha')")
            let rows: str = mysql_query(h, "SELECT id, label FROM nirdosha_smoke WHERE id = 1")
            print(rows)
            mysql_close(h)
        }}
    "#,
        dsn = dsn()
    );
    let plugins = MysqlPlugin.builtins();
    let result = nirdosha::run_with_plugins(&src, &plugins);
    assert!(result.is_ok(), "expected the program to run cleanly against a live MySQL, got {result:?}");
    assert_eq!(result.unwrap(), Value::Unit);
}

/// A double-close is a clean runtime error, not a panic -- the real,
/// disclosed cost of an opaque `i64` handle (`nirdosha-plugin-support`'s
/// own doc comment): nothing in the type system stops this, only the
/// plugin's own `HandleRegistry::remove` returning `None` the second
/// time.
#[test]
#[ignore = "needs a live MySQL server; see crates/plugin-examples/docker-compose.yml"]
fn double_close_is_a_clean_runtime_error_not_a_panic() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = mysql_connect("{dsn}")
            mysql_close(h)
            mysql_close(h)
        }}
    "#,
        dsn = dsn()
    );
    let plugins = MysqlPlugin.builtins();
    let err = nirdosha::run_with_plugins(&src, &plugins).expect_err("a double-close must be rejected");
    assert!(err.contains("already closed"), "expected a clear double-close message, got: {err}");
}
