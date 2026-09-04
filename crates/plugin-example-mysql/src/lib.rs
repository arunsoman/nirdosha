//! Reference Kind-A plugin #1 of 5 (Track F of the plugin-ecosystem
//! plan): MySQL, via the sync `mysql` crate — deliberately built first
//! because it's the closest in shape to what `crates/compiler/src/
//! dbconn.rs` already does for SQLite/Postgres (a plain, synchronous
//! client, no async bridge needed), making it the lowest-risk proof that
//! `nirdosha-plugin-support::HandleRegistry` actually works end to end
//! before the async-backed plugins (Cassandra, Neo4j) build on it too.
//!
//! Four builtins: `mysql_connect(dsn: str) -> i64` (a handle — see
//! `nirdosha-plugin-support`'s own doc comment for exactly what
//! affine-safety guarantees this handle does and doesn't have),
//! `mysql_query(handle: i64, sql: str) -> str` (a JSON array of row
//! objects), `mysql_execute(handle: i64, sql: str) -> i64` (affected row
//! count, for INSERT/UPDATE/DELETE/DDL), `mysql_close(handle: i64) ->
//! i64`.

use mysql::prelude::*;
use mysql::{Conn, Opts};
use nirdosha::ast::{Effect, Ty};
use nirdosha::interpreter::{RuntimeError, Value};
use nirdosha::plugin::{NirdoshaPlugin, PluginBuiltin};
use nirdosha::token::Span;
use nirdosha_plugin_support::{int_arg, plugin_error, str_arg, HandleRegistry};
use std::sync::{Arc, OnceLock};

const PLUGIN: &str = "mysql";

/// One process-wide table of live `Conn`s, keyed by the opaque `i64`
/// handed back to `.nir` source. Lazily created on first use, same as
/// every other plugin in this gallery — see `nirdosha-plugin-support`'s
/// own doc comment for why this is a `HandleRegistry`, not a first-class
/// `Ty`.
fn connections() -> &'static HandleRegistry<Conn> {
    static REG: OnceLock<HandleRegistry<Conn>> = OnceLock::new();
    REG.get_or_init(HandleRegistry::new)
}

pub struct MysqlPlugin;

impl NirdoshaPlugin for MysqlPlugin {
    fn builtins(&self) -> Vec<PluginBuiltin> {
        vec![
            PluginBuiltin {
                name: "mysql_connect".to_string(),
                params: vec![Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(mysql_connect_call),
            },
            PluginBuiltin {
                name: "mysql_query".to_string(),
                params: vec![Ty::I64, Ty::Str],
                ret: Ty::Str,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(mysql_query_call),
            },
            PluginBuiltin {
                name: "mysql_execute".to_string(),
                params: vec![Ty::I64, Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(mysql_execute_call),
            },
            PluginBuiltin {
                name: "mysql_close".to_string(),
                params: vec![Ty::I64],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(mysql_close_call),
            },
        ]
    }
}

/// `dsn` is a standard MySQL URL: `mysql://user:pass@host:port/dbname`.
fn mysql_connect_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let dsn = str_arg(args, 0, PLUGIN, span)?;
    let opts = Opts::from_url(dsn)
        .map_err(|e| plugin_error(PLUGIN, span, format!("invalid MySQL connection string: {e}")))?;
    let conn = Conn::new(opts).map_err(|e| plugin_error(PLUGIN, span, format!("connect failed: {e}")))?;
    Ok(Value::Int(connections().insert(conn)))
}

fn mysql_query_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let sql = str_arg(args, 1, PLUGIN, span)?.to_string();
    let result = connections().with(handle, |conn| -> Result<String, mysql::Error> {
        let rows: Vec<mysql::Row> = conn.query(&sql)?;
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let columns = row.columns();
            let values = row.unwrap();
            let mut obj = serde_json::Map::new();
            for (col, val) in columns.iter().zip(values) {
                obj.insert(col.name_str().to_string(), mysql_value_to_json(val));
            }
            out.push(serde_json::Value::Object(obj));
        }
        Ok(serde_json::to_string(&out).unwrap())
    });
    match result {
        Some(Ok(json)) => Ok(Value::Str(Arc::from(json.as_str()))),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("query failed: {e}"))),
        None => Err(plugin_error(
            PLUGIN,
            span,
            format!("no open mysql connection for handle {handle} (already closed, or never connected)"),
        )),
    }
}

fn mysql_execute_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let sql = str_arg(args, 1, PLUGIN, span)?.to_string();
    let result = connections().with(handle, |conn| -> Result<u64, mysql::Error> {
        conn.query_drop(&sql)?;
        Ok(conn.affected_rows())
    });
    match result {
        Some(Ok(n)) => Ok(Value::Int(n as i64)),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("execute failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open mysql connection for handle {handle}"))),
    }
}

fn mysql_close_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    match connections().remove(handle) {
        Some(_conn) => Ok(Value::Int(1)),
        None => {
            Err(plugin_error(PLUGIN, span, format!("mysql_close: handle {handle} was already closed or never existed")))
        }
    }
}

fn mysql_value_to_json(v: mysql::Value) -> serde_json::Value {
    use mysql::Value as MV;
    match v {
        MV::NULL => serde_json::Value::Null,
        MV::Bytes(b) => serde_json::Value::String(String::from_utf8_lossy(&b).to_string()),
        MV::Int(i) => serde_json::json!(i),
        MV::UInt(u) => serde_json::json!(u),
        MV::Float(f) => serde_json::json!(f),
        MV::Double(d) => serde_json::json!(d),
        other => serde_json::Value::String(format!("{other:?}")),
    }
}
