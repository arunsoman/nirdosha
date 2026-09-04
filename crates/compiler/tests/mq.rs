//! Tests for `mq_connect`/`mq_publish`/`mq_consume` (`Ty::Mq`'s doc
//! comment) — layer 1: Redis only, via the `redis` crate. Unlike
//! `db_connect(":memory:")` (`tests/db.rs`), a queue is a real network
//! service, so these need an actual Redis reachable at
//! `127.0.0.1:6379` (started for this session via
//! `docker run -d --name nirdosha-redis -p 6379:6379 redis:7-alpine`) —
//! not something to fake in-process, consistent with `Ty::Mq`'s own
//! doc comment on why `mq_*` gets `Effect::Network`, not `Effect::Io`.

use nirdosha::interpreter::Value;
use nirdosha::run;

// A per-test-run unique queue name so parallel `cargo test` runs (and
// leftover messages from a previous failed run) can't cross-contaminate
// each other on the same shared Redis instance.
fn unique_queue(tag: &str) -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let nanos = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    format!("nirdosha_test_{tag}_{nanos}")
}

#[test]
fn publish_then_consume_round_trips_the_message() {
    let queue = unique_queue("roundtrip");
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            return match mq_connect("127.0.0.1", 6379) {{
                Ok(conn) => match mq_publish(conn, "{queue}", "hello from nirdosha") {{
                    Ok(u) => match mq_consume(conn, "{queue}", 5) {{
                        Ok(msg) => Text(msg),
                        Err(e) => Text(e),
                    }},
                    Err(e) => Text(e),
                }},
                Err(e) => Text(e),
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "hello from nirdosha"),
            other => panic!("expected Text(Str(\"hello from nirdosha\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"hello from nirdosha\")), got {other:?}"),
    }
}

#[test]
fn consume_on_an_empty_queue_times_out_as_err_not_a_hang() {
    let queue = unique_queue("empty");
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            return match mq_connect("127.0.0.1", 6379) {{
                Ok(conn) => match mq_consume(conn, "{queue}", 1) {{
                    Ok(msg) => Text(msg),
                    Err(e) => Text(e),
                }},
                Err(e) => Text(e),
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert!(s.contains("no message"), "expected a timeout error, got {s:?}"),
            other => panic!("expected Text(Str(..timeout message..)), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(..timeout message..)), got {other:?}"),
    }
}

// `mq` is affine, same as `db`/`tcp`/`file`/`sandbox` (`Ty::Mq`'s doc
// comment) -- using a connection again after `stop`ping it is a
// use-after-move, caught by `check_ownership` before the program ever
// runs, the same static guarantee every other affine handle here already
// gets (mirrors how `tests/db.rs` never needs to exercise `db_query`'s
// "already stopped" runtime message either -- it's an unreachable
// defensive check under ordinary static ownership checking, not
// something a normal program can trigger).
#[test]
fn using_a_connection_after_stop_is_an_ownership_error() {
    let queue = unique_queue("afterstop");
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn after_stop(conn: mq) -> Text {{
            stop conn
            return match mq_publish(conn, "{queue}", "too late") {{
                Ok(u) => Text("should not reach here"),
                Err(e) => Text(e),
            }}
        }}
        fn main() -> Text {{
            return match mq_connect("127.0.0.1", 6379) {{
                Ok(c) => after_stop(c),
                Err(e) => Text(e),
            }}
        }}
    "#
    );
    match run(&src) {
        Err(msg) => assert!(msg.contains("ownership"), "expected an ownership error, got: {msg}"),
        other => panic!("expected a static ownership error (use of `conn` after `stop`), got {other:?}"),
    }
}
