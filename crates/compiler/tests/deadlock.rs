//! Real, sound runtime deadlock detection for `chan`/`thread`
//! (`interpreter::DeadlockRegistry`) — `README.md`'s "no deadlock is
//! expressible" claim was only ever true for *lock-ordering* deadlock
//! (no mutex primitive exists in the language at all); a `recv` with no
//! corresponding `send`, or two threads mutually `join`-ing each other,
//! both typechecked and ran fine before this existed, hanging the
//! process forever with zero diagnostic. These tests are run through a
//! bounded-time harness (`run_with_timeout`) on purpose, not because a
//! passing run is expected to be slow — as defense in depth against
//! exactly the failure mode being fixed: a regression that reintroduces
//! a silent hang should fail this suite loudly (a clear panic naming the
//! timeout), not wedge the whole `cargo test` process forever.

use nirdosha::interpreter::Value;
use nirdosha::run;

/// Runs `src` on a background thread and waits at most `timeout_secs` —
/// `Ok`/`Err` from a real, prompt result; `Err("timed out...")` if
/// nothing came back in time (this harness's own signal that the
/// interpreter hung, not the program's own result).
fn run_with_timeout(src: &str, timeout_secs: u64) -> Result<Value, String> {
    let (tx, rx) = std::sync::mpsc::channel();
    let src = src.to_string();
    std::thread::spawn(move || {
        let _ = tx.send(run(&src));
    });
    match rx.recv_timeout(std::time::Duration::from_secs(timeout_secs)) {
        Ok(result) => result,
        Err(_) => Err(format!("timed out after {timeout_secs}s — the interpreter hung instead of detecting a deadlock")),
    }
}

fn expect_deadlock(src: &str) {
    match run_with_timeout(src, 10) {
        Err(message) => assert!(message.contains("deadlock"), "expected a deadlock error, got: {message}"),
        Ok(v) => panic!("expected a deadlock error, program completed with: {v:?}"),
    }
}

#[test]
fn recv_with_no_corresponding_send_is_a_detected_deadlock_not_a_hang() {
    expect_deadlock(
        r#"
        fn main() -> i64 {
            let c: chan i64 = chan
            return recv(c)
        }
    "#,
    );
}

#[test]
fn two_threads_that_mutually_join_each_other_are_a_detected_deadlock() {
    expect_deadlock(
        r#"
        fn worker_a(to_a: chan thread i64, outcome: chan bool) -> i64 {
            let hb: thread i64 = recv(to_a)
            let r: i64 = join(hb)
            send(outcome, true)
            return r
        }
        fn worker_b(to_b: chan thread i64) -> i64 {
            let ha: thread i64 = recv(to_b)
            return join(ha)
        }
        fn main() -> bool {
            let c_a: chan thread i64 = chan
            let c_b: chan thread i64 = chan
            let outcome: chan bool = chan
            let ha: thread i64 = spawn worker_a(c_a, outcome)
            let hb: thread i64 = spawn worker_b(c_b)
            send(c_b, ha)
            send(c_a, hb)
            return recv(outcome)
        }
    "#,
    );
}

// ---- false-positive guards: entirely ordinary concurrent programs must
// still run to completion, never falsely flagged --------------------------

#[test]
fn send_then_recv_in_the_same_thread_is_not_a_false_positive() {
    // The value is already queued by the time `recv` runs -- this must
    // never touch the deadlock registry at all, let alone trap.
    assert_eq!(
        run_with_timeout(
            r#"
        fn main() -> i64 {
            let c: chan i64 = chan
            send(c, 42)
            return recv(c)
        }
    "#,
            10
        ),
        Ok(Value::Int(42))
    );
}

#[test]
fn spawn_then_recv_a_value_the_child_sends_is_not_a_false_positive() {
    // The classic producer/consumer shape -- `main` blocking briefly on
    // `recv` while the child hasn't sent yet must not be mistaken for a
    // permanent deadlock just because, at the instant of the check,
    // nothing had been received yet.
    assert_eq!(
        run_with_timeout(
            r#"
        fn producer(c: chan i64) -> unit {
            send(c, 41)
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let h: thread unit = spawn producer(c)
            let v: i64 = recv(c)
            join h
            return v + 1
        }
    "#,
            10
        ),
        Ok(Value::Int(42))
    );
}

#[test]
fn two_independent_spawns_joined_in_sequence_is_not_a_false_positive() {
    // Regression case: this intermittently reported a false deadlock
    // before the parent-side synchronous `spawn_started` registration
    // and the `is_finished()`/`try_recv()` race guards existed — run
    // several times in one process to make a reintroduced race likely to
    // surface, not just a single lucky pass.
    for _ in 0..20 {
        assert_eq!(
            run_with_timeout(
                r#"
            fn double(n: i64) -> i64 { return n * 2 }
            fn main() -> i64 {
                let h1: thread i64 = spawn double(10)
                let h2: thread i64 = spawn double(20)
                let r1: i64 = join h1
                let r2: i64 = join h2
                return r1 + r2
            }
        "#,
                10
            ),
            Ok(Value::Int(60))
        );
    }
}

#[test]
fn a_long_lived_worker_that_eventually_sends_is_not_a_false_positive() {
    // Exercises the poll loop for real: the child deliberately sleeps
    // past several `DEADLOCK_POLL_INTERVAL` cycles before sending, so
    // `main`'s `recv` genuinely polls-and-rechecks more than once before
    // the real value arrives — must still resolve correctly, not time
    // out and not falsely trap.
    assert_eq!(
        run_with_timeout(
            r#"
        fn slow_producer(c: chan i64) -> unit {
            sleep_ms(120)
            send(c, 7)
            return
        }
        fn main() -> i64 {
            let c: chan i64 = chan
            let h: thread unit = spawn slow_producer(c)
            let v: i64 = recv(c)
            join h
            return v
        }
    "#,
            10
        ),
        Ok(Value::Int(7))
    );
}
