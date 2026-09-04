//! Reference Kind-A plugin #4 of 5 (Track F of the plugin-ecosystem
//! plan): Neo4j, via the async `neo4rs` driver (Bolt protocol) — same
//! `block_on`-bridging pattern `nirdosha-plugin-cassandra` uses (see
//! that crate's README for why), applied to a graph-query domain
//! instead of tabular SQL/CQL, proving the connect/query/close shape
//! generalizes past "it's basically a database."
//!
//! Three builtins: `neo4j_connect(uri: str, user: str, pass: str) -> i64`,
//! `neo4j_run(handle: i64, cypher: str) -> str` (JSON array of row
//! objects), `neo4j_close(handle: i64) -> i64`.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{RuntimeError, Value};
use nirdosha::plugin::{NirdoshaPlugin, PluginBuiltin};
use nirdosha::token::Span;
use nirdosha_plugin_support::{block_on, int_arg, plugin_error, str_arg, HandleRegistry};
use neo4rs::{query, BoltType, Graph};
use std::collections::HashMap;
use std::sync::{Arc, OnceLock};

const PLUGIN: &str = "neo4j";

fn graphs() -> &'static HandleRegistry<Graph> {
    static REG: OnceLock<HandleRegistry<Graph>> = OnceLock::new();
    REG.get_or_init(HandleRegistry::new)
}

pub struct Neo4jPlugin;

impl NirdoshaPlugin for Neo4jPlugin {
    fn builtins(&self) -> Vec<PluginBuiltin> {
        vec![
            PluginBuiltin {
                name: "neo4j_connect".to_string(),
                params: vec![Ty::Str, Ty::Str, Ty::Str],
                ret: Ty::I64,
                call: Arc::new(neo4j_connect_call),
            },
            PluginBuiltin {
                name: "neo4j_run".to_string(),
                params: vec![Ty::I64, Ty::Str],
                ret: Ty::Str,
                call: Arc::new(neo4j_run_call),
            },
            PluginBuiltin {
                name: "neo4j_close".to_string(),
                params: vec![Ty::I64],
                ret: Ty::I64,
                call: Arc::new(neo4j_close_call),
            },
        ]
    }
}

/// `uri`: e.g. `"127.0.0.1:7687"` or `"bolt://127.0.0.1:7687"`.
fn neo4j_connect_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let uri = str_arg(args, 0, PLUGIN, span)?.to_string();
    let user = str_arg(args, 1, PLUGIN, span)?.to_string();
    let pass = str_arg(args, 2, PLUGIN, span)?.to_string();
    let graph = block_on(async { Graph::new(&uri, &user, &pass).await })
        .map_err(|e| plugin_error(PLUGIN, span, format!("connect failed: {e}")))?;
    Ok(Value::Int(graphs().insert(graph)))
}

fn neo4j_run_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let cypher = str_arg(args, 1, PLUGIN, span)?.to_string();
    let result = graphs().with(handle, |graph| {
        block_on(async {
            let mut stream = graph.execute(query(&cypher)).await.map_err(|e| e.to_string())?;
            let mut out = Vec::new();
            while let Some(row) = stream.next().await.map_err(|e| e.to_string())? {
                let fields: HashMap<String, BoltType> = row.to().map_err(|e| e.to_string())?;
                let mut obj = serde_json::Map::new();
                for (name, value) in fields {
                    obj.insert(name, bolt_value_to_json(value));
                }
                out.push(serde_json::Value::Object(obj));
            }
            Ok::<String, String>(serde_json::to_string(&out).unwrap())
        })
    });
    match result {
        Some(Ok(json)) => Ok(Value::Str(Arc::from(json.as_str()))),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("query failed: {e}"))),
        None => Err(plugin_error(
            PLUGIN,
            span,
            format!("no open neo4j connection for handle {handle} (already closed, or never connected)"),
        )),
    }
}

fn neo4j_close_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    match graphs().remove(handle) {
        Some(_graph) => Ok(Value::Int(1)),
        None => {
            Err(plugin_error(PLUGIN, span, format!("neo4j_close: handle {handle} was already closed or never existed")))
        }
    }
}

fn bolt_value_to_json(v: BoltType) -> serde_json::Value {
    match v {
        BoltType::String(s) => serde_json::Value::String(s.value),
        BoltType::Boolean(b) => serde_json::json!(b.value),
        BoltType::Integer(i) => serde_json::json!(i.value),
        BoltType::Float(f) => serde_json::json!(f.value),
        BoltType::Null(_) => serde_json::Value::Null,
        other => serde_json::Value::String(format!("{other:?}")),
    }
}
