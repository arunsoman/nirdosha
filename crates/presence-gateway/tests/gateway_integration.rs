//! Real, full-stack integration tests for `nirdosha-presence-gateway` —
//! same "verify against something real, not a hand-rolled stand-in"
//! discipline `crates/compiler/tests/mq.rs`'s own doc comment states: a real
//! `nirdosha serve` (in-process, `nirdosha::serve::run`, the same
//! technique `crates/compiler/tests/serve.rs::start_server` uses), a real Redis
//! (`127.0.0.1:6379` — the same instance `crates/compiler/tests/mq.rs` already
//! expects running), and a real `notify()` call through the actual
//! interpreter — not a mocked HTTP server standing in for `nirdosha
//! serve`, and not a hand-crafted Redis `PUBLISH` standing in for a real
//! workflow's own `notify()` invocation. `nirdosha` is a
//! `[dev-dependencies]`-only dependency of this crate (`Cargo.toml`'s own
//! doc comment) — used here, nowhere in the shipped binary.

use std::sync::Arc;
use std::time::Duration;

use futures_util::{SinkExt, StreamExt};
use nirdosha::ast::Program;
use nirdosha::durability::LogTarget;
use nirdosha::interpreter::Value;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::serve::AuthConfig;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;
use tokio_tungstenite::tungstenite::Message;

use nirdosha_presence_gateway::gateway::{self, Config};
use nirdosha_presence_gateway::jwt::KeySet;
use nirdosha_presence_gateway::presence::PresenceClient;

// Same symmetric test JWKS `crates/compiler/tests/serve.rs`'s own `mint_token`
// helper uses -- `mock_issue_token` only ever mints `HS256` tokens
// (`interpreter.rs`'s own doc comment), so the gateway's JWKS here has to
// carry a matching `oct` key for this to verify at all.
const JWKS: &str = r#"{"keys":[{"kid":"key1","kty":"oct","k":"bXktc2VjcmV0LWtleQ"}]}"#;
const ISSUER: &str = "https://mock-idp.local";
const AUDIENCE: &str = "presence-gateway-tests";
const PRESENCE_TOKEN: &str = "test-presence-token";

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

fn build_program(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
    program
}

fn now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs() as i64
}

fn unique_subject(tag: &str) -> String {
    format!("presence-gateway-test-{tag}-{}", now())
}

/// Starts a real `nirdosha serve` in-process (a background OS thread —
/// same technique `crates/compiler/tests/serve.rs::start_server` uses) with the
/// presence bridge enabled, sharing `workflow_log` with whatever later
/// calls `notify()` against the same file.
fn start_nirdosha_serve(workflow_log: std::path::PathBuf) -> u16 {
    let port = free_port();
    let program = Arc::new(build_program("fn main() {}"));
    let transact_log = std::env::temp_dir().join(format!("nirdosha-pg-test-transact-{port}.db"));
    let auth = AuthConfig { jwks_json: JWKS.to_string(), issuer: ISSUER.to_string(), audience: AUDIENCE.to_string() };
    std::thread::spawn(move || {
        nirdosha::serve::run(
            program,
            "127.0.0.1",
            port,
            Some(auth),
            None,
            transact_log,
            workflow_log,
            Some(PRESENCE_TOKEN.to_string()),
            None,
            None,
            None,
            None,
            None,
            false,
            None,
        )
        .expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if std::net::TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("nirdosha serve on port {port} never came up");
}

/// A one-field carrier struct wrapping free text — `str` can't cross a
/// function boundary (`docs/LANGUAGE.md` §6b, `docs/ROADMAP.md`'s enum-favoring
/// `str` ban), so every test helper here that needs `main()` to hand a
/// string back out uses this, the same `Text { value: str }` convention
/// `crates/compiler/tests/structs_enums.rs` already establishes.
fn text_wrapped(src: &str) -> String {
    let wrapped = format!("struct Text {{ value: str }}\n{src}");
    match nirdosha::run(&wrapped) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => s.to_string(),
            other => panic!("Text.value: unexpected field {other:?}"),
        },
        other => panic!("text_wrapped: unexpected result {other:?}"),
    }
}

/// Mints a real, signed test token via the interpreter's own
/// `mock_issue_token` builtin — the same deterministic test-only token
/// minter `crates/compiler/tests/serve.rs::mint_token` drives (`docs/LANGUAGE.md`
/// §9).
fn mint_token(subject: &str, issued_at: i64, ttl_secs: i64) -> String {
    text_wrapped(&format!(
        r#"
        fn main() -> Text {{
            return match mock_issue_token("{subject}", "{ISSUER}", "{AUDIENCE}", {issued_at}, {ttl_secs}, "{{}}", "{jwks}") {{
                Ok(t) => Text(t),
                Err(e) => Text(e),
            }}
        }}
        "#,
        jwks = JWKS.replace('"', "\\\"")
    ))
}

/// Calls the real `notify(BySubject(subject), ...)` through the real
/// interpreter, pointed at the same `workflow_log` file `nirdosha
/// serve`'s own `_presence_connect`/`_disconnect` routes share — exactly
/// the "some other request handler calls notify() after presence was
/// registered" shape a real deployment has. Returns what `notify()`
/// itself reported: `true` only on `Ok(true)` (an actual online-push or
/// offline-email send), matching `dispatch_notify`'s own `Ok(Value::Bool)`
/// contract.
fn call_notify(workflow_log: &std::path::Path, subject: &str, template: &str) -> bool {
    let src = format!(
        r#"
        fn main() -> bool {{
            return match db_connect(":memory:") {{
                Ok(conn) => match mq_connect("127.0.0.1", 6379) {{
                    Ok(mq_conn) => match json_parse("{{}}") {{
                        Ok(vars) => match notify(conn, mq_conn, BySubject("{subject}"), "{template}", vars) {{
                            Ok(sent) => sent,
                            Err(e) => false,
                        }},
                        Err(e) => false,
                    }},
                    Err(e) => false,
                }},
                Err(e) => false,
            }}
        }}
        "#
    );
    match nirdosha::run_with_tracer_transact_and_workflow_log(&src, None, None, Some(LogTarget::Sqlite(workflow_log.to_path_buf()))) {
        Ok(Value::Bool(b)) => b,
        other => panic!("call_notify: unexpected result {other:?}"),
    }
}

/// Starts the gateway itself against a real `nirdosha serve` + real
/// Redis. Returns its WS port and a shutdown sender so each test can stop
/// it cleanly.
fn start_gateway(nirdosha_port: u16) -> (u16, tokio::sync::watch::Sender<bool>) {
    let port = free_port();
    let keys = KeySet::from_json(JWKS).expect("test JWKS should parse");
    let presence = PresenceClient::new(format!("http://127.0.0.1:{nirdosha_port}"), PRESENCE_TOKEN.to_string());
    let config = Config {
        host: "127.0.0.1".to_string(),
        port,
        keys,
        issuer: ISSUER.to_string(),
        audience: AUDIENCE.to_string(),
        presence,
        redis_url: "redis://127.0.0.1:6379/".to_string(),
        auth_timeout: Duration::from_secs(5),
        heartbeat_interval: Duration::from_secs(30),
        drain_timeout: Duration::from_secs(5),
    };
    let (tx, rx) = tokio::sync::watch::channel(false);
    tokio::spawn(async move {
        if let Err(e) = gateway::run(config, rx).await {
            eprintln!("gateway::run exited with error: {e}");
        }
    });
    (port, tx)
}

async fn wait_for_port(port: u16) {
    for _ in 0..100 {
        if tokio::net::TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("gateway on port {port} never came up");
}

async fn recv_json(ws: &mut (impl StreamExt<Item = Result<Message, tokio_tungstenite::tungstenite::Error>> + Unpin)) -> serde_json::Value {
    let msg = tokio::time::timeout(Duration::from_secs(5), ws.next())
        .await
        .expect("should receive a message before timeout")
        .expect("stream should not end")
        .expect("should be a valid WS message");
    let Message::Text(text) = msg else { panic!("expected a text frame, got {msg:?}") };
    serde_json::from_str(&text).expect("relayed frame should be valid JSON")
}

#[tokio::test]
async fn a_valid_token_authenticates_and_receives_a_real_notify_push() {
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-pg-test-workflow-{}.db", free_port()));
    let nirdosha_port = start_nirdosha_serve(workflow_log.clone());
    let (gw_port, shutdown) = start_gateway(nirdosha_port);
    wait_for_port(gw_port).await;

    let subject = unique_subject("roundtrip");
    let token = mint_token(&subject, now() - 5, 3600);

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{gw_port}/ws")).await.expect("ws connect should succeed");
    ws.send(Message::Text(serde_json::json!({ "token": token }).to_string().into())).await.expect("send auth frame");

    let connected = recv_json(&mut ws).await;
    assert_eq!(connected["type"], "connected");
    assert_eq!(connected["subject"], subject);

    // `handle_connection` only sends the "connected" ack *after*
    // `PresenceClient::connect` has already succeeded (`gateway.rs`) --
    // by the time the assertion above passes, `identity_presence` really
    // does have this subject marked online, so this isn't a race against
    // `call_notify` below.
    let sent_live = call_notify(&workflow_log, &subject, "a_real_template");
    assert!(sent_live, "notify() should report success");

    let relayed = recv_json(&mut ws).await;
    assert_eq!(relayed["type"], "notify");
    assert_eq!(relayed["payload"]["template"], "a_real_template");

    let _ = shutdown.send(true);
}

#[tokio::test]
async fn two_connections_for_the_same_subject_keep_it_online_until_both_close() {
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-pg-test-workflow-{}.db", free_port()));
    let nirdosha_port = start_nirdosha_serve(workflow_log.clone());
    let (gw_port, shutdown) = start_gateway(nirdosha_port);
    wait_for_port(gw_port).await;

    let subject = unique_subject("multi-tab");
    let token_a = mint_token(&subject, now() - 5, 3600);
    let token_b = mint_token(&subject, now() - 5, 3600);

    let (mut ws_a, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{gw_port}/ws")).await.expect("ws connect (a) should succeed");
    ws_a.send(Message::Text(serde_json::json!({ "token": token_a }).to_string().into())).await.expect("send auth frame (a)");
    assert_eq!(recv_json(&mut ws_a).await["type"], "connected");

    let (mut ws_b, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{gw_port}/ws")).await.expect("ws connect (b) should succeed");
    ws_b.send(Message::Text(serde_json::json!({ "token": token_b }).to_string().into())).await.expect("send auth frame (b)");
    assert_eq!(recv_json(&mut ws_b).await["type"], "connected");

    // Close the first tab. The second is still open, so the subject must
    // stay online -- a naive one-connect-one-disconnect gateway would
    // wrongly mark this subject offline right here (`registry.rs`'s own
    // doc comment names exactly this bug).
    ws_a.close(None).await.expect("closing ws_a should succeed");
    // Give the close frame time to actually propagate through the
    // gateway's own connection-close handling before asserting on its
    // effect.
    tokio::time::sleep(Duration::from_millis(300)).await;

    let sent_live = call_notify(&workflow_log, &subject, "still_online_template");
    assert!(sent_live, "notify() should report success");
    let relayed = recv_json(&mut ws_b).await;
    assert_eq!(relayed["type"], "notify", "tab B should still receive a live push, proving the subject stayed online after tab A closed");

    let _ = shutdown.send(true);
}

#[tokio::test]
async fn an_expired_token_is_rejected_not_silently_accepted() {
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-pg-test-workflow-{}.db", free_port()));
    let nirdosha_port = start_nirdosha_serve(workflow_log);
    let (gw_port, shutdown) = start_gateway(nirdosha_port);
    wait_for_port(gw_port).await;

    let subject = unique_subject("expired");
    let token = mint_token(&subject, now() - 7200, 3600); // issued 2h ago, 1h TTL -- long expired

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{gw_port}/ws")).await.expect("ws connect should succeed");
    ws.send(Message::Text(serde_json::json!({ "token": token }).to_string().into())).await.expect("send auth frame");

    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["type"], "error");

    let _ = shutdown.send(true);
}

#[tokio::test]
async fn a_token_for_the_wrong_audience_is_rejected() {
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-pg-test-workflow-{}.db", free_port()));
    let nirdosha_port = start_nirdosha_serve(workflow_log);
    let (gw_port, shutdown) = start_gateway(nirdosha_port);
    wait_for_port(gw_port).await;

    let subject = unique_subject("wrong-aud");
    let token = text_wrapped(&format!(
        r#"
        fn main() -> Text {{
            return match mock_issue_token("{subject}", "{ISSUER}", "some-other-audience", {issued_at}, 3600, "{{}}", "{jwks}") {{
                Ok(t) => Text(t),
                Err(e) => Text(e),
            }}
        }}
        "#,
        issued_at = now() - 5,
        jwks = JWKS.replace('"', "\\\"")
    ));

    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://127.0.0.1:{gw_port}/ws")).await.expect("ws connect should succeed");
    ws.send(Message::Text(serde_json::json!({ "token": token }).to_string().into())).await.expect("send auth frame");

    let reply = recv_json(&mut ws).await;
    assert_eq!(reply["type"], "error");

    let _ = shutdown.send(true);
}

#[tokio::test]
async fn a_plain_http_get_to_healthz_gets_a_real_200_not_a_websocket_upgrade() {
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-pg-test-workflow-{}.db", free_port()));
    let nirdosha_port = start_nirdosha_serve(workflow_log);
    let (gw_port, shutdown) = start_gateway(nirdosha_port);
    wait_for_port(gw_port).await;

    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let mut stream = tokio::net::TcpStream::connect(("127.0.0.1", gw_port)).await.unwrap();
    stream.write_all(b"GET /healthz HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").await.unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).await.unwrap();
    assert!(resp.starts_with("HTTP/1.1 200"), "expected a 200 OK, got: {resp}");

    let _ = shutdown.send(true);
}
