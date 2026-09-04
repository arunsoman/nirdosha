//! Tests for observability layer 2a (`observability.rs`'s "Rollout
//! layers 2-4" section): the dynamic, client-gated `--otel-port`
//! mechanism — `Tracer::new_dynamic`/`spawn_otel_port_listener`,
//! `Tracer::enabled()`, the token handshake, the debounce-on-disconnect,
//! and real spans actually reaching a connected client. Talks to the
//! listener with a raw `TcpStream`, the same technique `tests/serve.rs`
//! already uses for `nirdosha serve` itself — no client library needed
//! for a protocol this small.

use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::{Duration, Instant};

use nirdosha::interpreter::{Interpreter, Value};
use nirdosha::observability::{spawn_otel_port_listener, Tracer};
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("should typecheck cleanly");
    program
}

/// Connects, sends the token handshake line, and returns the connected
/// stream once the server has replied `ok\n` -- panics (via the caller's
/// own assertion) rather than silently treating a rejection as success.
fn connect_and_auth(port: u16, token: &str) -> (TcpStream, String) {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect to the APM port");
    stream.write_all(format!("Bearer {token}\n").as_bytes()).expect("write handshake line");
    let mut reader = BufReader::new(stream.try_clone().expect("clone stream"));
    let mut reply = String::new();
    reader.read_line(&mut reply).expect("read handshake reply");
    (stream, reply)
}

fn wait_until(mut cond: impl FnMut() -> bool, timeout: Duration, what: &str) {
    let deadline = Instant::now() + timeout;
    while !cond() {
        assert!(Instant::now() < deadline, "timed out waiting for: {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

// ---- dormant by default, enabled the moment a client connects --------------

#[test]
fn dormant_until_a_client_connects_and_disabled_again_after_it_leaves() {
    let tracer = Tracer::new_dynamic();
    assert!(!tracer.enabled(), "a freshly built dynamic tracer must start disabled");

    let port = free_port();
    spawn_otel_port_listener(Arc::clone(&tracer), port, "secret-token".to_string()).expect("bind the APM port");

    let (stream, reply) = connect_and_auth(port, "secret-token");
    assert_eq!(reply, "ok\n", "a valid token should be accepted");
    wait_until(|| tracer.enabled(), Duration::from_secs(2), "tracer to enable after first connect");

    drop(stream);
    wait_until(|| !tracer.enabled(), Duration::from_secs(8), "tracer to disable after the last client disconnects (post-debounce)");
}

// ---- reconnecting during the debounce window cancels the disable -----------

#[test]
fn reconnect_during_debounce_keeps_tracing_enabled() {
    let tracer = Tracer::new_dynamic();
    let port = free_port();
    spawn_otel_port_listener(Arc::clone(&tracer), port, "tok".to_string()).expect("bind the APM port");

    let (stream, _) = connect_and_auth(port, "tok");
    wait_until(|| tracer.enabled(), Duration::from_secs(2), "tracer to enable");
    drop(stream);

    // Reconnect well inside the debounce window (documented as ~2.5s) --
    // `enabled()` should never have flipped false in between.
    std::thread::sleep(Duration::from_millis(300));
    let (stream2, reply) = connect_and_auth(port, "tok");
    assert_eq!(reply, "ok\n");
    assert!(tracer.enabled(), "reconnecting during the debounce window must not have let tracing turn off");

    // And it should stay enabled well past when the *original* debounce
    // timer would have fired, since that timer's epoch check should have
    // made it a no-op.
    std::thread::sleep(Duration::from_secs(3));
    assert!(tracer.enabled(), "the stale debounce timer from the first disconnect must not disable a still-connected client");
    drop(stream2);
}

// ---- bad token is rejected, never enables tracing ---------------------------

#[test]
fn wrong_token_is_rejected_and_never_enables_tracing() {
    let tracer = Tracer::new_dynamic();
    let port = free_port();
    spawn_otel_port_listener(Arc::clone(&tracer), port, "correct-token".to_string()).expect("bind the APM port");

    let (_stream, reply) = connect_and_auth(port, "wrong-token");
    assert!(reply.starts_with("err "), "an invalid token should get an err line, got: {reply:?}");

    // Give any (incorrect) enabling a moment to have happened, then
    // confirm it didn't.
    std::thread::sleep(Duration::from_millis(300));
    assert!(!tracer.enabled(), "a rejected handshake must never enable tracing");
}

// ---- N clients: only the last disconnect disables ---------------------------

#[test]
fn tracing_stays_enabled_until_every_client_has_disconnected() {
    let tracer = Tracer::new_dynamic();
    let port = free_port();
    spawn_otel_port_listener(Arc::clone(&tracer), port, "tok".to_string()).expect("bind the APM port");

    let (a, _) = connect_and_auth(port, "tok");
    let (b, _) = connect_and_auth(port, "tok");
    wait_until(|| tracer.enabled(), Duration::from_secs(2), "tracer to enable");

    drop(a);
    // One client remains connected -- must not disable even after well
    // past the debounce window.
    std::thread::sleep(Duration::from_secs(3));
    assert!(tracer.enabled(), "tracing must stay enabled while at least one APM client is still connected");

    drop(b);
    wait_until(|| !tracer.enabled(), Duration::from_secs(8), "tracer to disable once the last client disconnects");
}

// ---- a real span actually reaches a connected client, live -----------------

#[test]
fn a_connected_client_receives_real_spans_as_they_happen() {
    let tracer = Tracer::new_dynamic();
    let port = free_port();
    spawn_otel_port_listener(Arc::clone(&tracer), port, "tok".to_string()).expect("bind the APM port");

    let (stream, reply) = connect_and_auth(port, "tok");
    assert_eq!(reply, "ok\n");
    wait_until(|| tracer.enabled(), Duration::from_secs(2), "tracer to enable before running the traced program");

    let src = r#"
        fn double(n: i64) -> i64 {
            return n * 2
        }
        fn main() -> i64 {
            return double(21)
        }
    "#;
    let program = parse_ok(src);
    let interp =
        Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src)).with_tracer(Arc::clone(&tracer));
    match interp.run_main() {
        Ok(Value::Int(42)) => {}
        other => panic!("expected Ok(Int(42)), got {other:?}"),
    }

    let mut reader = BufReader::new(stream);
    let mut names = Vec::new();
    let deadline = Instant::now() + Duration::from_secs(5);
    // Two `call` spans expected (`double`, `main`) -- read until both
    // have arrived or the deadline passes.
    while names.len() < 2 && Instant::now() < deadline {
        let mut line = String::new();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            break;
        }
        let parsed: serde_json::Value = serde_json::from_str(line.trim_end()).expect("every line should be valid JSON");
        assert_eq!(parsed["type"], "span");
        names.push(parsed["fn_name"].as_str().map(|s| s.to_string()));
    }
    assert!(names.contains(&Some("double".to_string())), "expected a call span for `double`, got {names:?}");
    assert!(names.contains(&Some("main".to_string())), "expected a call span for `main`, got {names:?}");
}

// ---- a fresh tracer with no APM client attached traces nothing -------------

#[test]
fn no_connected_client_means_hook_points_stay_untraced() {
    let tracer = Tracer::new_dynamic();
    assert!(!tracer.enabled());
    let src = r#"
        fn main() -> i64 {
            return 7
        }
    "#;
    let program = parse_ok(src);
    let interp =
        Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src)).with_tracer(Arc::clone(&tracer));
    match interp.run_main() {
        Ok(Value::Int(7)) => {}
        other => panic!("expected Ok(Int(7)), got {other:?}"),
    }
    // Nothing was ever recorded -- `emitted_count` stays at 0 even though
    // a `Tracer` (dormant) was attached the whole time.
    assert_eq!(tracer.emitted_count(), 0, "a dormant tracer (no connected APM client) must not emit any spans");
}
