use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_neo4j::Neo4jPlugin;

fn uri() -> String {
    std::env::var("NEO4J_URI").unwrap_or_else(|_| "127.0.0.1:7687".to_string())
}
fn user() -> String {
    std::env::var("NEO4J_USER").unwrap_or_else(|_| "neo4j".to_string())
}
fn pass() -> String {
    std::env::var("NEO4J_PASS").unwrap_or_else(|_| "nirdosha123".to_string())
}

#[test]
fn wrong_arity_call_is_a_type_error_not_a_panic() {
    let src = r#"
        fn main() {
            print(neo4j_connect("x"))
        }
    "#;
    let plugins = Neo4jPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("wrong arity must be rejected");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

#[test]
fn without_the_plugin_registered_neo4j_connect_is_unknown() {
    let src = r#"
        fn main() {
            print(neo4j_connect("x", "y", "z"))
        }
    "#;
    let err = nirdosha::run_with_plugins(src, &[]).expect_err("neo4j_connect must be unresolvable with no plugins");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}

/// `neo4rs::Graph::new` builds a *lazy* connection pool (`deadpool`
/// underneath) -- it succeeds even against an unreachable address
/// without proving a connection actually works, the same documented
/// "get_or_create alone doesn't validate" behavior `crates/compiler/src
/// /pool.rs` calls out for its own r2d2-backed pools. The real, honest
/// failure surfaces on first actual use, not at connect time -- proven
/// here the same way `pool.rs`'s own tests prove it: connect, then run.
#[test]
fn an_unreachable_server_fails_cleanly_on_first_use_not_at_connect_time() {
    let src = r#"
        fn main() {
            let h: i64 = neo4j_connect("127.0.0.1:1", "neo4j", "wrong")
            print(neo4j_run(h, "RETURN 1"))
        }
    "#;
    let plugins = Neo4jPlugin.builtins();
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("an unreachable server must error cleanly on first use");
    assert!(err.contains("plugin"), "expected the plugin error channel, got: {err}");
}

/// The real end-to-end proof, against a live Neo4j -- `docker compose -f
/// crates/plugin-examples/docker-compose.yml up -d neo4j` (give it a
/// few seconds to accept Bolt connections), then `cargo test -p
/// nirdosha-plugin-neo4j -- --ignored`.
#[test]
#[ignore = "needs a live Neo4j server; see crates/plugin-examples/docker-compose.yml"]
fn create_and_match_round_trip_against_a_real_server() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = neo4j_connect("{uri}", "{user}", "{pass}")
            neo4j_run(h, "CREATE (w:NirdoshaSmoke {{name: 'from-nirdosha'}})")
            let rows: str = neo4j_run(h, "MATCH (w:NirdoshaSmoke) RETURN w.name AS name")
            print(rows)
            neo4j_close(h)
        }}
    "#,
        uri = uri(),
        user = user(),
        pass = pass()
    );
    let plugins = Neo4jPlugin.builtins();
    let result = nirdosha::run_with_plugins(&src, &plugins);
    assert!(result.is_ok(), "expected the program to run cleanly against a live server, got {result:?}");
}

#[test]
#[ignore = "needs a live Neo4j server; see crates/plugin-examples/docker-compose.yml"]
fn double_close_is_a_clean_runtime_error_not_a_panic() {
    let src = format!(
        r#"
        fn main() {{
            let h: i64 = neo4j_connect("{uri}", "{user}", "{pass}")
            neo4j_close(h)
            neo4j_close(h)
        }}
    "#,
        uri = uri(),
        user = user(),
        pass = pass()
    );
    let plugins = Neo4jPlugin.builtins();
    let err = nirdosha::run_with_plugins(&src, &plugins).expect_err("a double-close must be rejected");
    assert!(err.contains("already closed"), "expected a clear double-close message, got: {err}");
}
