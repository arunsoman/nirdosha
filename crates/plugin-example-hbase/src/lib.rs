//! Reference Kind-A plugin #5 of 5 (Track F of the plugin-ecosystem
//! plan): HBase, via the Thrift gateway (`hbase-thrift` crate) — built
//! last, deliberately, and disclosed honestly as the roughest of this
//! gallery's five (see README.md's "why this is the hardest one"). This
//! was the *original* motivating example in the conversation that led
//! to this whole plugin-ecosystem plan ("what if a developer needs
//! HBase or Cassandra?") — it's fitting that it's also the one proving
//! the Kind-A mechanism still works even where the Rust crate ecosystem
//! for a target system is thin.
//!
//! Four builtins: `hbase_connect(host: str, port: i64) -> i64`,
//! `hbase_put(handle: i64, table: str, row: str, family: str,
//! qualifier: str, value: str) -> i64`, `hbase_get(handle: i64, table:
//! str, row: str, family: str, qualifier: str) -> str` (empty string if
//! the cell doesn't exist), `hbase_close(handle: i64) -> i64`.
//!
//! Sync, not async — the Thrift RPC framing here is a plain blocking
//! `TTcpChannel`, so unlike the Cassandra/Neo4j plugins in this
//! gallery, there's no `nirdosha_plugin_support::block_on` bridge
//! needed at all; this one is closer in shape to the MySQL plugin.

use hbase_thrift::hbase::{ColumnDescriptor, HbaseSyncClient, THbaseSyncClient};
use hbase_thrift::MutationBuilder;
use nirdosha::ast::{Effect, Ty};
use nirdosha::interpreter::{RuntimeError, Value};
use nirdosha::plugin::{NirdoshaPlugin, PluginBuiltin};
use nirdosha::token::Span;
use nirdosha_plugin_support::{int_arg, plugin_error, str_arg, HandleRegistry};
use std::collections::BTreeMap;
use std::sync::{Arc, OnceLock};
use hbase_thrift::thrift::protocol::{TBinaryInputProtocol, TBinaryOutputProtocol};
use hbase_thrift::thrift::transport::{
    ReadHalf, TBufferedReadTransport, TBufferedWriteTransport, TIoChannel, TTcpChannel, WriteHalf,
};

const PLUGIN: &str = "hbase";

type InProt = TBinaryInputProtocol<TBufferedReadTransport<ReadHalf<TTcpChannel>>>;
type OutProt = TBinaryOutputProtocol<TBufferedWriteTransport<WriteHalf<TTcpChannel>>>;
type Client = HbaseSyncClient<InProt, OutProt>;

fn clients() -> &'static HandleRegistry<Client> {
    static REG: OnceLock<HandleRegistry<Client>> = OnceLock::new();
    REG.get_or_init(HandleRegistry::new)
}

pub struct HbasePlugin;

impl NirdoshaPlugin for HbasePlugin {
    fn builtins(&self) -> Vec<PluginBuiltin> {
        vec![
            PluginBuiltin {
                name: "hbase_connect".to_string(),
                params: vec![Ty::Str, Ty::I64],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(hbase_connect_call),
            },
            PluginBuiltin {
                name: "hbase_create_table".to_string(),
                params: vec![Ty::I64, Ty::Str, Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(hbase_create_table_call),
            },
            PluginBuiltin {
                name: "hbase_put".to_string(),
                params: vec![Ty::I64, Ty::Str, Ty::Str, Ty::Str, Ty::Str, Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(hbase_put_call),
            },
            PluginBuiltin {
                name: "hbase_get".to_string(),
                params: vec![Ty::I64, Ty::Str, Ty::Str, Ty::Str, Ty::Str],
                ret: Ty::Str,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(hbase_get_call),
            },
            PluginBuiltin {
                name: "hbase_close".to_string(),
                params: vec![Ty::I64],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(hbase_close_call),
            },
        ]
    }
}

fn hbase_connect_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let host = str_arg(args, 0, PLUGIN, span)?;
    let port = int_arg(args, 1, PLUGIN, span)?;
    let addr = format!("{host}:{port}");
    let mut channel = TTcpChannel::new();
    channel.open(&addr).map_err(|e| plugin_error(PLUGIN, span, format!("TCP connect to {addr} failed: {e}")))?;
    let (i_chan, o_chan) =
        channel.split().map_err(|e| plugin_error(PLUGIN, span, format!("failed to split channel: {e}")))?;
    let i_prot = TBinaryInputProtocol::new(TBufferedReadTransport::new(i_chan), true);
    let o_prot = TBinaryOutputProtocol::new(TBufferedWriteTransport::new(o_chan), true);
    let client = HbaseSyncClient::new(i_prot, o_prot);
    Ok(Value::Int(clients().insert(client)))
}

/// Unlike SQL/CQL's inline `CREATE TABLE IF NOT EXISTS`, HBase's Thrift
/// `createTable` errors (`AlreadyExists`) if the table is already
/// there -- so this is genuinely idempotent-*ish*, not idempotent: a
/// second call against an existing table is a clean `RuntimeError`, not
/// a silent no-op or a panic. A real plugin author polishing this
/// further would check `hbase_thrift`'s error variants and swallow
/// `AlreadyExists` specifically; left as a real (spanned, catchable)
/// error here since this crate is proving the mechanism, not chasing
/// full idempotency parity with the SQL plugins in this gallery.
fn hbase_create_table_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let table = str_arg(args, 1, PLUGIN, span)?.to_string();
    let family = str_arg(args, 2, PLUGIN, span)?.to_string();
    let result = clients().with(handle, |client| -> hbase_thrift::thrift::Result<()> {
        let col = ColumnDescriptor { name: Some(format!("{family}:").into_bytes()), ..Default::default() };
        client.create_table(table.clone().into(), vec![col])
    });
    match result {
        Some(Ok(())) => Ok(Value::Int(1)),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("create_table failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open hbase connection for handle {handle}"))),
    }
}

fn hbase_put_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let table = str_arg(args, 1, PLUGIN, span)?.to_string();
    let row = str_arg(args, 2, PLUGIN, span)?.to_string();
    let family = str_arg(args, 3, PLUGIN, span)?.to_string();
    let qualifier = str_arg(args, 4, PLUGIN, span)?.to_string();
    let value = str_arg(args, 5, PLUGIN, span)?.to_string();
    let result = clients().with(handle, |client| -> hbase_thrift::thrift::Result<()> {
        let mut mutation = MutationBuilder::default();
        mutation.column(family.clone(), qualifier.clone());
        mutation.value(value.as_str());
        // Raw `THbaseSyncClient::mutate_row` directly, not the
        // `THbaseSyncClientExt::put` convenience wrapper: the ext
        // trait's blanket impl doesn't resolve against this crate's
        // published 0.3.0 build (confirmed by trying it -- "method not
        // found" despite the trait being in scope). `mutate_row` is the
        // trait `HbaseSyncClient` itself implements directly, so it's
        // both simpler and more reliable here.
        client.mutate_row(table.clone().into(), row.clone().into(), vec![mutation.build()], BTreeMap::new())
    });
    match result {
        Some(Ok(())) => Ok(Value::Int(1)),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("put failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open hbase connection for handle {handle}"))),
    }
}

/// Empty string if the cell doesn't exist -- `get`'s own Thrift contract
/// (`Hbase.thrift`'s doc comment: "Returns an empty list if no such
/// value exists") is already "absence, not an error," so this plugin
/// preserves that instead of manufacturing a `RuntimeError` for a
/// perfectly ordinary "not found."
fn hbase_get_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let table = str_arg(args, 1, PLUGIN, span)?.to_string();
    let row = str_arg(args, 2, PLUGIN, span)?.to_string();
    let family = str_arg(args, 3, PLUGIN, span)?.to_string();
    let qualifier = str_arg(args, 4, PLUGIN, span)?.to_string();
    let column = format!("{family}:{qualifier}");
    let result = clients().with(handle, |client| -> hbase_thrift::thrift::Result<String> {
        let cells = client.get(table.clone().into(), row.clone().into(), column.clone().into(), BTreeMap::new())?;
        let text = cells
            .into_iter()
            .next()
            .and_then(|cell| cell.value)
            .map(|bytes| String::from_utf8_lossy(&bytes).into_owned())
            .unwrap_or_default();
        Ok(text)
    });
    match result {
        Some(Ok(text)) => Ok(Value::Str(Arc::from(text.as_str()))),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("get failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open hbase connection for handle {handle}"))),
    }
}

fn hbase_close_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    match clients().remove(handle) {
        Some(_client) => Ok(Value::Int(1)),
        None => Err(plugin_error(PLUGIN, span, format!("hbase_close: handle {handle} was already closed or never existed"))),
    }
}
