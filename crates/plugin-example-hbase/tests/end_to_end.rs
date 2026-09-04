use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_hbase::HbasePlugin;

fn host() -> String {
    std::env::var("HBASE_HOST").unwrap_or_else(|_| "127.0.0.1".to_string())
}
fn port() -> String {
    std::env::var("HBASE_THRIFT_PORT").unwrap_or_else(|_| "9090".to_string())
}

#[test]
fn wrong_arity_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(hbase_connect())
        }
    "#;
    let plugins = HbasePlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong arity must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn without_the_plugin_registered_hbase_connect_is_unknown() {
    let src = r#"
        fn main() {
            print(hbase_connect("127.0.0.1", 9090))
        }
    "#;
    let err = nirdosha::run_with_plugins(src, &[]).expect_err("hbase_connect must be unresolvable with no plugins");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn an_unreachable_gateway_is_a_clean_runtime_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(hbase_connect("127.0.0.1", 1))
        }
    "#;
    let plugins = HbasePlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("an unreachable gateway must error cleanly");
    assert!(err.contains("plugin"), "expected the plugin error channel, got: {err}");
}

/// The real end-to-end proof, against a live HBase Thrift gateway --
/// `docker compose -f crates/plugin-examples/docker-compose.yml up -d hbase`
/// (HBase is slow to become ready, give it a minute+), then `cargo test
/// -p nirdosha-plugin-hbase -- --ignored`.
#[test]
#[ignore = "needs a live HBase Thrift gateway; see crates/plugin-examples/docker-compose.yml"]
fn create_table_put_get_and_close_round_trip_against_a_real_gateway() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = hbase_connect("{host}", {port})
            hbase_create_table(h, "nirdosha_smoke", "data")
            hbase_put(h, "nirdosha_smoke", "row1", "data", "label", "from-nirdosha")
            let label: str = hbase_get(h, "nirdosha_smoke", "row1", "data", "label")
            print(label)
            hbase_close(h)
        }}
    "#,
        host = host(),
        port = port()
    );
    let plugins = HbasePlugin.builtins();
    let result = nirdosha::run_with_plugins(&src, &plugins);
    assert!(result.is_ok(), "expected the program to run cleanly against a live gateway, got {result:?}");
}

/// A `get` for a row/column that was never written is an empty string,
/// not an error -- matches the Thrift `get` contract itself ("returns
/// an empty list if no such value exists").
#[test]
#[ignore = "needs a live HBase Thrift gateway; see crates/plugin-examples/docker-compose.yml"]
fn get_on_a_missing_cell_is_a_clean_empty_string_not_an_error() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = hbase_connect("{host}", {port})
            hbase_create_table(h, "nirdosha_missing_test", "data")
            let missing: str = hbase_get(h, "nirdosha_missing_test", "no-such-row", "data", "no-such-col")
            print(missing)
            hbase_close(h)
        }}
    "#,
        host = host(),
        port = port()
    );
    let plugins = HbasePlugin.builtins();
    let result = nirdosha::run_with_plugins(&src, &plugins);
    assert!(result.is_ok(), "a missing cell must not be a runtime error, got {result:?}");
}

#[test]
#[ignore = "needs a live HBase Thrift gateway; see crates/plugin-examples/docker-compose.yml"]
fn double_close_is_a_clean_runtime_error_not_a_panic() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = hbase_connect("{host}", {port})
            hbase_close(h)
            hbase_close(h)
        }}
    "#,
        host = host(),
        port = port()
    );
    let plugins = HbasePlugin.builtins();
    let err = nirdosha::run_with_plugins(&src, &plugins).expect_err("a double-close must be rejected");
    assert!(err.contains("already closed"), "expected a clear double-close message, got: {err}");
}
