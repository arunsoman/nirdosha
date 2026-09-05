//! The cross-cutting proof Track F of the plugin-ecosystem plan asked
//! for: all five reference plugins registered in a SINGLE
//! `run_with_plugins` call, proving they compose (no name collisions,
//! no interference in typecheck/registration) and not just that each
//! one works in isolation. This is the documented multi-plugin pattern
//! `crates/plugin-example-rot13/README.md` already describes —
//! concatenate each plugin's own `builtins()` list.
//!
//! Deliberately static-only (no live services): the point here is
//! registration/typecheck composition across all five plugin crates at
//! once, which needs no network I/O at all. Each plugin's own
//! `tests/end_to_end.rs` (in its own crate) is where the real,
//! live-service proof lives.

use nirdosha::plugin::{NirdoshaPlugin, PluginBuiltin};

fn all_plugins() -> Vec<PluginBuiltin> {
    let mut plugins = nirdosha_plugin_mysql::MysqlPlugin.builtins();
    plugins.extend(nirdosha_plugin_activemq::ActiveMqPlugin.builtins());
    plugins.extend(nirdosha_plugin_cassandra::CassandraPlugin.builtins());
    plugins.extend(nirdosha_plugin_neo4j::Neo4jPlugin.builtins());
    plugins.extend(nirdosha_plugin_hbase::HbasePlugin.builtins());
    plugins
}

#[test]
fn all_five_plugins_register_with_no_name_collisions() {
    let plugins = all_plugins();
    let mut names: Vec<&str> = plugins.iter().map(|p| p.name.as_str()).collect();
    let total = names.len();
    names.sort_unstable();
    names.dedup();
    assert_eq!(names.len(), total, "two plugins in this gallery declared the same builtin name");
    // 8 (mysql: 4 bespoke mysql_* + 4 db_provider_mysql_*, docs/adr/0004)
    // + 8 (activemq: 4 bespoke activemq_* + 4 mq_provider_stomp_*, docs/adr/0004)
    // + 4 (cassandra) + 3 (neo4j) + 5 (hbase, incl. hbase_create_table)
    assert_eq!(total, 28, "expected builtin count to match the gallery's five plugins' declared signatures");
}

/// A single `.nir` program referencing a builtin from every one of the
/// five plugins typechecks cleanly against the concatenated registry --
/// proving composition at the typecheck/registration level (the only
/// level this test can exercise without live services).
#[test]
fn a_program_referencing_all_five_plugins_typechecks_cleanly() {
    let src = r#"
        fn main() {
            let use_mysql: bool = false
            let use_activemq: bool = false
            let use_cassandra: bool = false
            let use_neo4j: bool = false
            let use_hbase: bool = false
            if use_mysql {
                let h: i64 = mysql_connect("mysql://x")
                mysql_close(h)
            }
            if use_activemq {
                let h: i64 = activemq_connect("stomp://x")
                activemq_close(h)
            }
            if use_cassandra {
                let h: i64 = cassandra_connect("x")
                cassandra_close(h)
            }
            if use_neo4j {
                let h: i64 = neo4j_connect("x", "y", "z")
                neo4j_close(h)
            }
            if use_hbase {
                let h: i64 = hbase_connect("x", 9090)
                hbase_close(h)
            }
        }
    "#;
    let plugins = all_plugins();
    let result = nirdosha::run_with_plugins(src, &plugins);
    assert!(result.is_ok(), "a program referencing all five plugins' builtins should typecheck and run, got {result:?}");
}

/// Without any one plugin registered, its builtins are unresolvable --
/// same "real registration, not an always-on hook" proof
/// `plugin-example-rot13`'s own tests make for a single plugin, checked
/// here for the case where FOUR of the five ARE registered and the
/// fifth (mysql) deliberately isn't.
#[test]
fn a_plugin_left_out_of_the_registry_is_still_unknown_even_with_the_other_four_present() {
    let src = r#"
        fn main() {
            print(mysql_connect("mysql://x"))
        }
    "#;
    let mut plugins = nirdosha_plugin_activemq::ActiveMqPlugin.builtins();
    plugins.extend(nirdosha_plugin_cassandra::CassandraPlugin.builtins());
    plugins.extend(nirdosha_plugin_neo4j::Neo4jPlugin.builtins());
    plugins.extend(nirdosha_plugin_hbase::HbasePlugin.builtins());
    let err = nirdosha::run_with_plugins(src, &plugins).expect_err("mysql_connect must stay unresolvable");
    assert!(err.contains("type error"), "expected a type error, got: {err}");
}
