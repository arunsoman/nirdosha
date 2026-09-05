//! Reference Kind-A plugin #3 of 5 (Track F of the plugin-ecosystem
//! plan): Cassandra, via the `scylla` driver (async, CQL-compatible
//! with real Apache Cassandra, not just ScyllaDB). The first plugin in
//! this gallery that's actually async-backed — every call bridges into
//! `nirdosha_plugin_support::block_on` from inside a plain synchronous
//! `PluginFn` closure, the sanctioned pattern
//! `nirdosha-plugin-support`'s own doc comment describes (Nirdosha's
//! interpreter has no `.await` point anywhere; this is the honest,
//! documented alternative to each plugin hand-rolling its own runtime).
//!
//! Four builtins: `cassandra_connect(nodes: str) -> i64`,
//! `cassandra_query(handle: i64, cql: str) -> str` (JSON array of row
//! objects), `cassandra_execute(handle: i64, cql: str) -> i64` (`1` on
//! success — CQL doesn't report affected-row counts the way SQL does),
//! `cassandra_close(handle: i64) -> i64`.

use nirdosha::ast::{Effect, Ty};
use nirdosha::interpreter::{RuntimeError, Value};
use nirdosha::plugin::{NirdoshaPlugin, PluginBuiltin};
use nirdosha::token::Span;
use nirdosha_plugin_support::{block_on, int_arg, plugin_error, str_arg, HandleRegistry};
use scylla::client::session::Session;
use scylla::client::session_builder::SessionBuilder;
use scylla::value::{CqlValue, Row};
use std::sync::{Arc, OnceLock};

const PLUGIN: &str = "cassandra";

fn sessions() -> &'static HandleRegistry<Session> {
    static REG: OnceLock<HandleRegistry<Session>> = OnceLock::new();
    REG.get_or_init(HandleRegistry::new)
}

pub struct CassandraPlugin;

impl NirdoshaPlugin for CassandraPlugin {
    fn builtins(&self) -> Vec<PluginBuiltin> {
        vec![
            PluginBuiltin {
                name: "cassandra_connect".to_string(),
                params: vec![Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(cassandra_connect_call),
            },
            PluginBuiltin {
                name: "cassandra_query".to_string(),
                params: vec![Ty::I64, Ty::Str],
                ret: Ty::Str,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(cassandra_query_call),
            },
            PluginBuiltin {
                name: "cassandra_execute".to_string(),
                params: vec![Ty::I64, Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(cassandra_execute_call),
            },
            PluginBuiltin {
                name: "cassandra_close".to_string(),
                params: vec![Ty::I64],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(cassandra_close_call),
            },
        ]
    }
}

/// `nodes`: a comma-separated list of `host:port` contact points, e.g.
/// `"127.0.0.1:9042"`.
fn cassandra_connect_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let nodes = str_arg(args, 0, PLUGIN, span)?;
    let result: Result<Session, String> = block_on(async {
        let mut builder = SessionBuilder::new();
        for node in nodes.split(',').map(str::trim).filter(|s| !s.is_empty()) {
            builder = builder.known_node(node);
        }
        builder.build().await.map_err(|e| e.to_string())
    });
    let session = result.map_err(|e| plugin_error(PLUGIN, span, format!("connect failed: {e}")))?;
    Ok(Value::Int(sessions().insert(session)))
}

fn cassandra_query_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let cql = str_arg(args, 1, PLUGIN, span)?.to_string();
    let result = sessions().with(handle, |session| {
        block_on(async {
            let rows_result = session.query_unpaged(cql.as_str(), &[]).await?.into_rows_result()?;
            let names: Vec<String> = rows_result.column_specs().iter().map(|c| c.name().to_string()).collect();
            let mut out = Vec::new();
            for row in rows_result.rows::<Row>()? {
                let row = row?;
                let mut obj = serde_json::Map::new();
                for (name, value) in names.iter().zip(row.columns.into_iter()) {
                    obj.insert(name.clone(), cql_value_to_json(value));
                }
                out.push(serde_json::Value::Object(obj));
            }
            Ok::<String, Box<dyn std::error::Error + Send + Sync>>(serde_json::to_string(&out).unwrap())
        })
    });
    match result {
        Some(Ok(json)) => Ok(Value::Str(Arc::from(json.as_str()))),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("query failed: {e}"))),
        None => Err(plugin_error(
            PLUGIN,
            span,
            format!("no open cassandra session for handle {handle} (already closed, or never connected)"),
        )),
    }
}

fn cassandra_execute_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let cql = str_arg(args, 1, PLUGIN, span)?.to_string();
    let result = sessions().with(handle, |session| {
        block_on(async { session.query_unpaged(cql.as_str(), &[]).await.map(|_| ()) })
    });
    match result {
        Some(Ok(())) => Ok(Value::Int(1)),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("execute failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open cassandra session for handle {handle}"))),
    }
}

fn cassandra_close_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    match sessions().remove(handle) {
        Some(_session) => Ok(Value::Int(1)),
        None => Err(plugin_error(
            PLUGIN,
            span,
            format!("cassandra_close: handle {handle} was already closed or never existed"),
        )),
    }
}

fn cql_value_to_json(v: Option<CqlValue>) -> serde_json::Value {
    match v {
        None => serde_json::Value::Null,
        Some(CqlValue::Text(s)) | Some(CqlValue::Ascii(s)) => serde_json::Value::String(s),
        Some(CqlValue::Int(i)) => serde_json::json!(i),
        Some(CqlValue::SmallInt(i)) => serde_json::json!(i),
        Some(CqlValue::TinyInt(i)) => serde_json::json!(i),
        Some(CqlValue::BigInt(i)) => serde_json::json!(i),
        Some(CqlValue::Boolean(b)) => serde_json::json!(b),
        Some(CqlValue::Float(f)) => serde_json::json!(f),
        Some(CqlValue::Double(d)) => serde_json::json!(d),
        Some(CqlValue::Uuid(u)) => serde_json::Value::String(u.to_string()),
        Some(other) => serde_json::Value::String(format!("{other:?}")),
    }
}
