use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_cassandra::CassandraPlugin;

fn nodes() -> String {
    std::env::var("CASSANDRA_NODES").unwrap_or_else(|_| "127.0.0.1:9042".to_string())
}

#[test]
fn wrong_arity_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(cassandra_connect())
        }
    "#;
    let plugins = CassandraPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong arity must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn without_the_plugin_registered_cassandra_connect_is_unknown() {
    let src = r#"
        fn main() {
            print(cassandra_connect("127.0.0.1:9042"))
        }
    "#;
    let err = nirdosha::run_with_plugins(src, &[]).expect_err("cassandra_connect must be unresolvable with no plugins");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn an_unreachable_cluster_is_a_clean_runtime_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(cassandra_connect("127.0.0.1:1"))
        }
    "#;
    let plugins = CassandraPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("an unreachable cluster must error cleanly");
    assert!(err.contains("plugin"), "expected the plugin error channel, got: {err}");
}

/// The real end-to-end proof, against a live Cassandra cluster --
/// `docker compose -f crates/plugin-examples/docker-compose.yml up -d cassandra`
/// (Cassandra is slow to become ready, give it a minute+), then
/// `cargo test -p nirdosha-plugin-cassandra -- --ignored`.
#[test]
#[ignore = "needs a live Cassandra cluster; see crates/plugin-examples/docker-compose.yml"]
fn keyspace_table_insert_query_and_close_round_trip_against_a_real_cluster() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = cassandra_connect("{nodes}")
            cassandra_execute(h, "CREATE KEYSPACE IF NOT EXISTS nirdosha_smoke WITH replication = {{'class': 'SimpleStrategy', 'replication_factor': 1}}")
            cassandra_execute(h, "CREATE TABLE IF NOT EXISTS nirdosha_smoke.t (id int PRIMARY KEY, label text)")
            cassandra_execute(h, "INSERT INTO nirdosha_smoke.t (id, label) VALUES (1, 'from-nirdosha')")
            let rows: str = cassandra_query(h, "SELECT id, label FROM nirdosha_smoke.t WHERE id = 1")
            print(rows)
            cassandra_close(h)
        }}
    "#,
        nodes = nodes()
    );
    let plugins = CassandraPlugin.builtins();
    let result = nirdosha::run_with_plugins(&src, &plugins);
    assert!(result.is_ok(), "expected the program to run cleanly against a live cluster, got {result:?}");
}

#[test]
#[ignore = "needs a live Cassandra cluster; see crates/plugin-examples/docker-compose.yml"]
fn double_close_is_a_clean_runtime_error_not_a_panic() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = cassandra_connect("{nodes}")
            cassandra_close(h)
            cassandra_close(h)
        }}
    "#,
        nodes = nodes()
    );
    let plugins = CassandraPlugin.builtins();
    let err = nirdosha::run_with_plugins(&src, &plugins).expect_err("a double-close must be rejected");
    assert!(err.contains("already closed"), "expected a clear double-close message, got: {err}");
}
