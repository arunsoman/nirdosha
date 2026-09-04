//! The WebSocket relay itself: accepts a browser connection, verifies its
//! identity token, registers/deregisters presence, and forwards
//! `notify()`'s Redis-published events to the right live connection.
//! `docs/WORKFLOW.md`'s presence bridge names this shape directly: "a small
//! standalone service ... that terminates the real client connections,
//! calls `_presence_connect`/`_presence_disconnect` as they open/close
//! ..., and subscribes to each `nirdosha:push:<subject>` channel to relay
//! to the right live connection."

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::stream::SplitSink;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use tokio::io::AsyncWriteExt;
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::watch;
use tokio::task::JoinSet;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::WebSocketStream;

use crate::jwt::{self, KeySet};
use crate::presence::PresenceClient;
use crate::registry::PresenceRegistry;

pub struct Config {
    pub host: String,
    pub port: u16,
    pub keys: KeySet,
    pub issuer: String,
    pub audience: String,
    pub presence: PresenceClient,
    pub redis_url: String,
    pub auth_timeout: Duration,
    pub heartbeat_interval: Duration,
    /// Bound on how long shutdown waits for active connections to drain
    /// (send their own close frame and finish cleanup) before giving up
    /// and exiting anyway — a stuck client should never wedge the whole
    /// process open forever during a rolling restart.
    pub drain_timeout: Duration,
}

/// Runs the gateway until `shutdown` is signalled `true` (`main.rs`'s
/// SIGTERM handler), then stops accepting new connections and waits (up
/// to `Config::drain_timeout`) for every already-open connection to close
/// itself and run its own presence cleanup.
pub async fn run(config: Config, mut shutdown: watch::Receiver<bool>) -> std::io::Result<()> {
    let addr = format!("{}:{}", config.host, config.port);
    let listener = TcpListener::bind(&addr).await?;
    eprintln!("nirdosha-presence-gateway listening on ws://{addr}/ws (health: /healthz, /readyz)");
    let config = Arc::new(config);
    let registry = Arc::new(PresenceRegistry::new());
    let mut tasks = JoinSet::new();

    loop {
        tokio::select! {
            accepted = listener.accept() => {
                let (stream, peer) = match accepted {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("accept error: {e}");
                        continue;
                    }
                };
                let config = Arc::clone(&config);
                let registry = Arc::clone(&registry);
                let conn_shutdown = shutdown.clone();
                tasks.spawn(async move {
                    if let Err(e) = handle_connection(stream, peer, config, registry, conn_shutdown).await {
                        eprintln!("connection {peer} ended with error: {e}");
                    }
                });
            }
            // Reap already-finished connections as they happen, not just
            // at shutdown -- `JoinSet::len()` counts every task that
            // hasn't been `join_next()`-ed yet, *including* ones that
            // already finished (a health probe closes in milliseconds,
            // but its task handle would otherwise sit in the set
            // unconsumed for the rest of this process's uptime). The
            // `Some(res) = ...` pattern in a `select!` branch only fires
            // when the future actually resolves to `Some` -- an empty
            // `JoinSet` makes `join_next()` resolve to `None` immediately
            // and the branch is skipped that poll, so this never busy-loops
            // on an idle set.
            Some(res) = tasks.join_next() => {
                if let Err(e) = res {
                    eprintln!("a connection task panicked: {e}");
                }
            }
            changed = shutdown.changed() => {
                if changed.is_ok() && *shutdown.borrow() {
                    break;
                }
            }
        }
    }

    eprintln!("shutting down: draining {} active connection(s)", tasks.len());
    let drained = tokio::time::timeout(config.drain_timeout, async {
        while tasks.join_next().await.is_some() {}
    })
    .await;
    if drained.is_err() {
        eprintln!(
            "graceful drain timed out after {:?} with {} connection(s) still open — exiting anyway \
             (a controlled SIGTERM path only; an ungraceful SIGKILL/OOM has no way to run this at all, \
             the same disclosed at-least-once-cleanup limit docs/WORKFLOW.md's own non-goals section already \
             keeps for notify() itself)",
            config.drain_timeout,
            tasks.len()
        );
    }
    Ok(())
}

/// Routes one freshly-accepted connection to either the WebSocket
/// handshake (`/ws`) or a plain HTTP response (`/healthz`/`/readyz`,
/// anything else) — the same "one listener, a couple of routes" shape
/// `nirdosha serve` itself already takes for its own `/healthz`/
/// `/readyz` (`serve.rs`), rather than standing up a second listener/
/// dependency just for k8s probes.
///
/// Deliberately **not** `tokio_tungstenite::accept_hdr_async` with a
/// routing callback — tried first, and confirmed empirically not to
/// work: `tungstenite`'s server handshake parser requires a
/// well-formed `Upgrade: websocket` request *before* it will even invoke
/// a callback, so a plain `GET /healthz` with no `Upgrade`/`Connection`
/// headers (exactly what a real kubelet probe sends) fails handshake
/// parsing itself, never reaching the callback at all. `TcpStream::peek`
/// reads the request line without consuming it from the socket, so the
/// original, untouched stream can still be handed to `accept_async` for
/// the one path that's a real upgrade.
async fn route_connection(stream: TcpStream, peer: SocketAddr) -> Result<Option<WebSocketStream<TcpStream>>, String> {
    let mut buf = [0u8; 512];
    let n = stream.peek(&mut buf).await.map_err(|e| format!("peek failed for {peer}: {e}"))?;
    let head = String::from_utf8_lossy(&buf[..n]);
    let path = head.lines().next().and_then(|line| line.split_whitespace().nth(1)).unwrap_or("");
    match path {
        "/ws" => match tokio_tungstenite::accept_async(stream).await {
            Ok(ws) => Ok(Some(ws)),
            Err(e) => Err(format!("WS handshake with {peer} failed: {e}")),
        },
        "/healthz" | "/readyz" => {
            respond_plain_http(stream, 200, "OK", "ok").await;
            Ok(None)
        }
        _ => {
            respond_plain_http(stream, 404, "Not Found", "not found").await;
            Ok(None)
        }
    }
}

async fn respond_plain_http(mut stream: TcpStream, status: u16, reason: &str, body: &str) {
    let resp = format!("HTTP/1.1 {status} {reason}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
    // Best-effort -- a probe client that hung up mid-write, or any other
    // transient socket error here, isn't worth failing the connection
    // task over; the worst case is one missed probe response, which
    // kubelet will just retry on its own schedule.
    let _ = stream.write_all(resp.as_bytes()).await;
    let _ = stream.shutdown().await;
}

#[derive(Deserialize)]
struct AuthMessage {
    token: String,
}

type WsWriter = SplitSink<WebSocketStream<TcpStream>, Message>;

async fn send_json(write: &mut WsWriter, value: &serde_json::Value) -> Result<(), ()> {
    write.send(Message::Text(value.to_string().into())).await.map_err(|_| ())
}

async fn send_error(write: &mut WsWriter, message: &str) {
    let _ = send_json(write, &serde_json::json!({ "type": "error", "message": message })).await;
}

async fn handle_connection(
    stream: TcpStream,
    peer: SocketAddr,
    config: Arc<Config>,
    registry: Arc<PresenceRegistry>,
    mut shutdown: watch::Receiver<bool>,
) -> Result<(), String> {
    let ws_stream = match route_connection(stream, peer).await? {
        Some(s) => s,
        // A plain-HTTP probe (`/healthz`, `/readyz`, or any other path) --
        // `route_connection` already wrote the real response over the
        // wire; nothing left to relay.
        None => return Ok(()),
    };
    let (mut write, mut read) = ws_stream.split();

    // First frame must be `{"token": "<jwt>"}`, within a bounded window --
    // an unauthenticated socket left open indefinitely would be a trivial
    // resource-exhaustion vector.
    let token = match tokio::time::timeout(config.auth_timeout, read.next()).await {
        Ok(Some(Ok(Message::Text(text)))) => match serde_json::from_str::<AuthMessage>(&text) {
            Ok(msg) => msg.token,
            Err(e) => {
                send_error(&mut write, &format!("first message must be `{{\"token\":...}}`: {e}")).await;
                return Ok(());
            }
        },
        Ok(Some(Ok(_))) => {
            send_error(&mut write, "first message must be a text frame carrying `{\"token\":...}`").await;
            return Ok(());
        }
        Ok(Some(Err(e))) => return Err(format!("WS read error from {peer} before auth: {e}")),
        Ok(None) => return Ok(()), // closed before authenticating
        Err(_) => {
            send_error(&mut write, "no token received within the auth timeout").await;
            return Ok(());
        }
    };

    let subject = match jwt::verify(&token, &config.keys, &config.issuer, &config.audience) {
        Ok(claims) => claims.sub,
        Err(e) => {
            send_error(&mut write, &e.to_string()).await;
            return Ok(());
        }
    };

    // Subscribe to this subject's Redis channel *before* registering
    // presence or acking the client -- `notify()`'s Redis `PUBLISH` is
    // fire-and-forget (no persistence for a subscriber that arrives
    // late), so any caller able to observe this subject as "online"
    // (which requires the presence-connect call below to have already
    // completed) must never be able to publish before this subscribe has
    // actually landed. Getting this ordering right is the one thing that
    // actually matters here -- confirmed the hard way, not just reasoned
    // about: an earlier version of this function acked "connected" (and
    // so let a caller reasonably assume it was now safe to notify)
    // *before* subscribing at all, and this crate's own integration test
    // that calls a real `notify()` immediately after "connected" caught
    // it losing the message outright.
    let redis_client = redis::Client::open(config.redis_url.as_str()).map_err(|e| format!("redis::Client::open failed: {e}"))?;
    let mut pubsub = match redis_client.get_async_pubsub().await {
        Ok(p) => p,
        Err(e) => {
            send_error(&mut write, "gateway temporarily unable to reach its message broker").await;
            return Err(format!("redis pubsub connection failed: {e}"));
        }
    };
    let channel = format!("nirdosha:push:{subject}");
    if let Err(e) = pubsub.subscribe(&channel).await {
        send_error(&mut write, "gateway temporarily unable to subscribe").await;
        return Err(format!("redis SUBSCRIBE {channel} failed: {e}"));
    }

    // Presence registration -- only on this subject's 0 -> 1 transition
    // (`registry.rs`'s own doc comment: a second open tab must not
    // re-announce, and must not be the one that later marks it offline).
    let is_first_connection = registry.increment(&subject);
    if is_first_connection {
        if let Err(e) = config.presence.connect(&subject).await {
            registry.decrement(&subject);
            send_error(&mut write, &format!("failed to register presence: {e}")).await;
            return Err(format!("presence connect failed for `{subject}`: {e}"));
        }
    }

    if send_json(&mut write, &serde_json::json!({ "type": "connected", "subject": subject })).await.is_err() {
        cleanup_presence(&registry, &config.presence, &subject).await;
        return Ok(());
    }

    // Everything from here down is the "registered" region: exactly one
    // exit point (falling out of `relay_loop` below), with cleanup run
    // unconditionally right after it -- deliberately not a `Drop` guard
    // spawning a detached cleanup task, which could race the process
    // exiting before its `disconnect` HTTP call actually lands. Awaiting
    // cleanup inline here means `run`'s own drain loop (which awaits this
    // whole task via `JoinSet`) only observes a connection as finished
    // once its presence bookkeeping is *actually* done.
    let result = relay_loop(&mut write, &mut read, &subject, &channel, &mut pubsub, &config, &mut shutdown).await;
    cleanup_presence(&registry, &config.presence, &subject).await;
    result
}

async fn cleanup_presence(registry: &PresenceRegistry, presence: &PresenceClient, subject: &str) {
    if registry.decrement(subject) {
        if let Err(e) = presence.disconnect(subject).await {
            eprintln!("failed to mark `{subject}` offline: {e}");
        }
    }
}

async fn relay_loop(
    write: &mut WsWriter,
    read: &mut futures_util::stream::SplitStream<WebSocketStream<TcpStream>>,
    subject: &str,
    channel: &str,
    pubsub: &mut redis::aio::PubSub,
    config: &Config,
    shutdown: &mut watch::Receiver<bool>,
) -> Result<(), String> {
    let mut redis_messages = pubsub.on_message();

    let mut heartbeat = tokio::time::interval(config.heartbeat_interval);
    heartbeat.tick().await; // first tick fires immediately; consume it so the real cadence starts from "now"
    let mut last_pong = Instant::now();

    loop {
        tokio::select! {
            msg = read.next() => {
                match msg {
                    Some(Ok(Message::Pong(_))) => { last_pong = Instant::now(); }
                    Some(Ok(Message::Ping(payload))) => {
                        if write.send(Message::Pong(payload)).await.is_err() { break; }
                    }
                    Some(Ok(Message::Close(_))) | None => break,
                    // This is a server -> client relay, not a two-way
                    // protocol -- any other client message (a stray text
                    // frame, a binary frame) is deliberately ignored, not
                    // an error.
                    Some(Ok(_)) => {}
                    Some(Err(e)) => {
                        eprintln!("WS read error for `{subject}`: {e}");
                        break;
                    }
                }
            }
            redis_msg = redis_messages.next() => {
                let Some(redis_msg) = redis_msg else { break }; // pubsub connection dropped
                let payload: String = match redis_msg.get_payload() {
                    Ok(p) => p,
                    Err(e) => {
                        eprintln!("redis message payload decode failed for `{subject}`: {e}");
                        continue;
                    }
                };
                // `interpreter.rs::publish_push_event` always publishes a
                // JSON object (`{"template","vars","sent_at"}`) -- parsing
                // and re-wrapping it (rather than forwarding the raw
                // string) means a malformed payload from anything else
                // that happened to publish on this channel gets dropped
                // with a log line instead of handed to the browser as
                // opaque, unparseable text.
                let parsed: serde_json::Value = match serde_json::from_str(&payload) {
                    Ok(v) => v,
                    Err(e) => {
                        eprintln!("dropping malformed notify payload on `{channel}`: {e}");
                        continue;
                    }
                };
                if send_json(write, &serde_json::json!({ "type": "notify", "payload": parsed })).await.is_err() {
                    break;
                }
            }
            _ = heartbeat.tick() => {
                if last_pong.elapsed() > config.heartbeat_interval * 3 {
                    eprintln!("closing `{subject}`'s connection: no pong received in {:?}", last_pong.elapsed());
                    break;
                }
                if write.send(Message::Ping(Vec::new().into())).await.is_err() { break; }
            }
            changed = shutdown.changed() => {
                if changed.is_ok() && *shutdown.borrow() {
                    let _ = write.send(Message::Close(None)).await;
                    break;
                }
            }
        }
    }
    Ok(())
}
