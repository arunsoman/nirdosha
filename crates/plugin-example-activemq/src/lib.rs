//! Reference Kind-A plugin #2 of 5 (Track F of the plugin-ecosystem
//! plan): ActiveMQ, over STOMP. Parallels the existing `Ty::Mq`/Redis
//! builtin shape in `interpreter.rs` (connect/send/receive/close), so
//! it's a natural comparison point against a first-class message-queue
//! type instead of a plugin-defined one.
//!
//! Four builtins: `activemq_connect(url: str) -> i64`,
//! `activemq_send(handle: i64, queue: str, body: str) -> i64`,
//! `activemq_receive(handle: i64, queue: str, timeout_ms: i64) -> str`
//! (empty string on timeout -- see `stomp::StompConn::receive_one`),
//! `activemq_close(handle: i64) -> i64`.

mod stomp;

use nirdosha::ast::{Effect, Ty};
use nirdosha::interpreter::{RuntimeError, Value};
use nirdosha::plugin::{NirdoshaPlugin, PluginBuiltin};
use nirdosha::token::Span;
use nirdosha_plugin_support::{int_arg, plugin_error, str_arg, HandleRegistry};
use std::sync::{Arc, OnceLock};
use std::time::Duration;
use stomp::StompConn;

const PLUGIN: &str = "activemq";

fn connections() -> &'static HandleRegistry<StompConn> {
    static REG: OnceLock<HandleRegistry<StompConn>> = OnceLock::new();
    REG.get_or_init(HandleRegistry::new)
}

pub struct ActiveMqPlugin;

impl NirdoshaPlugin for ActiveMqPlugin {
    fn builtins(&self) -> Vec<PluginBuiltin> {
        vec![
            PluginBuiltin {
                name: "activemq_connect".to_string(),
                params: vec![Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(activemq_connect_call),
            },
            PluginBuiltin {
                name: "activemq_send".to_string(),
                params: vec![Ty::I64, Ty::Str, Ty::Str],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(activemq_send_call),
            },
            PluginBuiltin {
                name: "activemq_receive".to_string(),
                params: vec![Ty::I64, Ty::Str, Ty::I64],
                ret: Ty::Str,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(activemq_receive_call),
            },
            PluginBuiltin {
                name: "activemq_close".to_string(),
                params: vec![Ty::I64],
                ret: Ty::I64,
                effects: [Effect::Network].into_iter().collect(),
                call: Arc::new(activemq_close_call),
            },
        ]
    }
}

/// `url`: `stomp://[user[:pass]@]host:port` (`stomp://` prefix optional).
fn activemq_connect_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let url = str_arg(args, 0, PLUGIN, span)?;
    let (login, passcode, host, port) = stomp::parse_url(url).map_err(|e| plugin_error(PLUGIN, span, e))?;
    let conn = StompConn::connect(&host, port, login.as_deref(), passcode.as_deref())
        .map_err(|e| plugin_error(PLUGIN, span, format!("connect failed: {e}")))?;
    Ok(Value::Int(connections().insert(conn)))
}

fn activemq_send_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let queue = str_arg(args, 1, PLUGIN, span)?.to_string();
    let body = str_arg(args, 2, PLUGIN, span)?.to_string();
    let destination = as_destination(&queue);
    let result = connections().with(handle, |conn| conn.send(&destination, &body));
    match result {
        Some(Ok(())) => Ok(Value::Int(1)),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("send failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open activemq connection for handle {handle}"))),
    }
}

fn activemq_receive_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    let queue = str_arg(args, 1, PLUGIN, span)?.to_string();
    let timeout_ms = int_arg(args, 2, PLUGIN, span)?;
    let destination = as_destination(&queue);
    let timeout = Duration::from_millis(timeout_ms.max(0) as u64);
    let result = connections().with(handle, |conn| conn.receive_one(&destination, timeout));
    match result {
        Some(Ok(Some(body))) => Ok(Value::Str(Arc::from(body.as_str()))),
        Some(Ok(None)) => Ok(Value::Str(Arc::from(""))), // timeout: no message, not an error
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("receive failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open activemq connection for handle {handle}"))),
    }
}

fn activemq_close_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    match connections().remove(handle) {
        Some(mut conn) => {
            conn.disconnect();
            Ok(Value::Int(1))
        }
        None => Err(plugin_error(
            PLUGIN,
            span,
            format!("activemq_close: handle {handle} was already closed or never existed"),
        )),
    }
}

/// A bare queue name (`"orders"`) is shorthand for STOMP's
/// `/queue/orders` destination convention; a caller who already passed a
/// full `/queue/...`/`/topic/...` destination is used as-is.
fn as_destination(queue: &str) -> String {
    if queue.starts_with('/') {
        queue.to_string()
    } else {
        format!("/queue/{queue}")
    }
}
