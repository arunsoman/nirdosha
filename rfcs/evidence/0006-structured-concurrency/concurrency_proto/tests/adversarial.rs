//! The "Thread/Channel/Sandbox" brief's Phase 6 adversarial list, run
//! for real against this prototype and classified per its own Phase 6
//! scheme: Compile-time rejection / Runtime-safe failure / Runtime
//! panic / Undefined behavior. Every classification below is the
//! actual observed result of running this exact test, not a prediction
//! -- see each test's own comment for the literal `rustc` error text
//! where the classification is "compile-time rejection" (those cases
//! are necessarily commented-out snippets, verified once by actually
//! uncommenting and compiling them, with the real error pasted back
//! in, matching this whole project's own "verify before trusting"
//! discipline).

use concurrency_proto::{mailbox, receive, send, try_send, Closed, Froze, Iso, SendResult};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

// ---- 1. Double send -- CLASSIFICATION: compile-time rejection ------------
//
// let (tx, _rx) = mailbox::<Iso<i64>>();
// let v = Iso(42);
// send(&tx, v).unwrap();
// send(&tx, v).unwrap();   // <-- real rustc output, verified by
//                          //     actually compiling this exact
//                          //     snippet as a standalone example,
//                          //     not guessed:
//
// error[E0382]: use of moved value: `v`
//  --> examples/verify_double_send.rs:6:15
//   |
// 4 |     let v = Iso(42);
//   |         - move occurs because `v` has type `Iso<i64>`, which does not implement the `Copy` trait
// 5 |     send(&tx, v).unwrap();
//   |               - value moved here
// 6 |     send(&tx, v).unwrap();
//   |               ^ value used here after move
//
// No test function needed for a case whose entire point is that it
// must not compile -- the comment above *is* the evidence.

// ---- 2. Use-after-send -- CLASSIFICATION: compile-time rejection ---------
//
// Identical mechanism to #1 -- `send` takes `T` by value, so "use after
// send" and "double send" are the exact same `rustc` E0382, not two
// different bugs to test separately.

#[test]
fn single_send_then_receive_works_normally() {
    // The positive control for #1/#2: exactly one send, exactly one
    // receive, is not just "not rejected" but actually correct.
    let (tx, rx) = mailbox::<Iso<i64>>();
    send(&tx, Iso(42)).unwrap();
    let Iso(v) = receive(&rx).unwrap();
    assert_eq!(v, 42);
}

// ---- 3. Double receive -- CLASSIFICATION: runtime-safe (not a bug) -------
//
// Unlike double-send, double-receive is not inherently wrong -- a
// mailbox is a queue, and receiving twice is the ordinary way to drain
// two messages. The real question is "receive on an EMPTY, still-open
// mailbox" (blocks, tested via timeout below) vs. "receive on an
// empty, CLOSED mailbox" (#6).

#[test]
fn receiving_more_messages_than_were_sent_blocks_not_panics() {
    let (tx, rx) = mailbox::<i64>();
    send(&tx, 1).unwrap();
    assert_eq!(receive(&rx), Ok(1));
    // A second receive with nothing sent and the sender still alive
    // must block, not panic or return garbage -- verified via a bounded
    // wait on a background thread rather than actually hanging this
    // test forever if the claim were false.
    let (done_tx, done_rx) = mailbox::<()>();
    std::thread::spawn(move || {
        let _ = receive(&rx); // blocks here, forever, since nothing more is ever sent
        let _ = send(&done_tx, ());
    });
    let timed_out = done_rx.recv_timeout(Duration::from_millis(200)).is_err();
    assert!(timed_out, "a receive on an empty, still-open mailbox must block, not return early");
}

// ---- 4. Dangling references -- CLASSIFICATION: compile-time rejection ---
//
// fn send_a_borrow(tx: &crossbeam_channel::Sender<Iso<&i64>>) {
//     let local = 5;
//     send(tx, Iso(&local)).unwrap();
// }   // <-- real rustc output (verified by actually compiling this
//         exact snippet as a standalone example, not guessed):
//
// error[E0597]: `local` does not live long enough
//  --> examples/verify_dangling_ref.rs:4:18
//   |
// 2 | fn send_a_borrow(tx: &crossbeam_channel::Sender<Iso<&i64>>) {
//   |                                                     - let's call the lifetime of this reference `'1`
// 3 |     let local = 5;
//   |         ----- binding `local` declared here
// 4 |     send(tx, Iso(&local)).unwrap();
//   |     -------------^^^^^^--
//   |     |            |
//   |     |            borrowed value does not live long enough
//   |     argument requires that `local` is borrowed for `'1`
// 5 | }
//   | - `local` dropped here while still borrowed
//
// This is exactly R3's "`lend` never crosses a channel" rule, enforced
// today by `rustc`'s own lifetime checker on a bare `&T` with no
// `'static` bound -- Nirdosha's own `Ty::Ref` would need the equivalent
// check for a `Lend` capability specifically, but the *mechanism*
// (lifetime tracking through a generic channel type) already exists
// and already works, here, for free.

// ---- 5/6. Channel closure during send / during receive -------------------
// CLASSIFICATION: runtime-safe failure (a real, catchable `Result`,
// never a panic or UB) for both.

#[test]
fn send_after_every_receiver_is_dropped_is_a_clean_err_not_a_panic() {
    let (tx, rx) = mailbox::<i64>();
    drop(rx);
    let result = send(&tx, 99);
    assert_eq!(result, Err(99), "the payload must come back, not be lost");
}

#[test]
fn receive_after_every_sender_is_dropped_returns_closed_not_a_panic() {
    let (tx, rx) = mailbox::<i64>();
    drop(tx);
    assert_eq!(receive(&rx), Err(Closed::Closed));
}

#[test]
fn receive_on_a_mailbox_that_still_has_buffered_messages_drains_them_before_closing() {
    // Closing (dropping every sender) doesn't discard what's already
    // queued -- a real, observable ordering guarantee, not assumed.
    let (tx, rx) = mailbox::<i64>();
    send(&tx, 1).unwrap();
    send(&tx, 2).unwrap();
    drop(tx);
    assert_eq!(receive(&rx), Ok(1));
    assert_eq!(receive(&rx), Ok(2));
    assert_eq!(receive(&rx), Err(Closed::Closed));
}

// ---- 7. Thread termination during ownership transfer ---------------------
// CLASSIFICATION: runtime-safe (the value is already queue-owned the
// instant `send` returns; the sending thread's own lifetime afterward
// is irrelevant).

#[test]
fn a_value_survives_correctly_even_if_the_sending_thread_exits_immediately_after_send() {
    let (tx, rx) = mailbox::<Iso<String>>();
    std::thread::scope(|s| {
        s.spawn(move || {
            send(&tx, Iso("hello".to_string())).unwrap();
            // thread ends here, immediately -- `tx` (the last sender
            // clone) drops with it, but the *message* already queued
            // is unaffected.
        });
        let Iso(v) = receive(&rx).unwrap();
        assert_eq!(v, "hello");
    });
}

// ---- 8. Multiple producers -- CLASSIFICATION: runtime-safe, by design ---

#[test]
fn multiple_producer_threads_all_deliver_correctly() {
    let (tx, rx) = mailbox::<i64>();
    let total = std::thread::scope(|s| {
        for i in 0..8 {
            let tx = tx.clone();
            s.spawn(move || {
                for _ in 0..100 {
                    send(&tx, i).unwrap();
                }
            });
        }
        drop(tx);
        let mut sum = 0i64;
        let mut count = 0;
        while let Ok(v) = receive(&rx) {
            sum += v;
            count += 1;
        }
        assert_eq!(count, 800, "every one of 8*100 sends must be received exactly once");
        sum
    });
    assert_eq!(total, (0..8).sum::<i64>() * 100);
}

// ---- 9. Multiple consumers -- CLASSIFICATION: runtime-safe, by design ---
// (a real, disclosed *design choice* this prototype makes differently
// from plain `std::sync::mpsc`, which supports many senders but only
// ever one receiver -- `crossbeam_channel`'s `Receiver` is `Clone`,
// and many threads calling `.recv()` on clones of the same receiver
// race safely for each message, each message going to exactly one
// winner.)

#[test]
fn multiple_consumer_threads_never_duplicate_or_drop_a_message() {
    let (tx, rx) = mailbox::<i64>();
    for i in 0..1000 {
        send(&tx, i).unwrap();
    }
    drop(tx);
    let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
    std::thread::scope(|s| {
        for _ in 0..4 {
            let rx = rx.clone();
            let seen = Arc::clone(&seen);
            s.spawn(move || {
                while let Ok(v) = receive(&rx) {
                    seen.lock().unwrap().push(v);
                }
            });
        }
    });
    let mut seen = seen.lock().unwrap().clone();
    seen.sort();
    assert_eq!(seen, (0..1000).collect::<Vec<_>>(), "every message exactly once, across all 4 consumers combined");
}

// ---- 10. Nested thread spawning -- CLASSIFICATION: runtime-safe --------

#[test]
fn nested_scopes_join_correctly_inner_before_outer() {
    let outer_done = AtomicUsize::new(0);
    let inner_done = AtomicUsize::new(0);
    std::thread::scope(|outer| {
        outer.spawn(|| {
            std::thread::scope(|inner| {
                inner.spawn(|| {
                    inner_done.fetch_add(1, Ordering::SeqCst);
                });
                // `inner` (the `std::thread::scope` call) cannot
                // return until its own child above has joined -- C1,
                // recursively, for free.
            });
            assert_eq!(inner_done.load(Ordering::SeqCst), 1, "inner scope must have fully joined before this line runs");
            outer_done.fetch_add(1, Ordering::SeqCst);
        });
    });
    assert_eq!(outer_done.load(Ordering::SeqCst), 1);
}

// ---- 11. Large ownership transfers -- see bench.rs's own #5/#6 note ----
// (real timing evidence lives in the benchmark, not here; the
// *correctness* half is this test: a large value's contents survive
// the move through a channel intact, byte for byte.)

#[test]
fn a_large_payload_survives_transfer_with_full_content_intact() {
    let (tx, rx) = mailbox::<Iso<Vec<u8>>>();
    let payload: Vec<u8> = (0..1_000_000u32).map(|n| (n % 256) as u8).collect();
    let expected = payload.clone();
    send(&tx, Iso(payload)).unwrap();
    let Iso(received) = receive(&rx).unwrap();
    assert_eq!(received, expected);
}

// ---- 12. Rapid thread creation/destruction -- CLASSIFICATION: runtime-safe

#[test]
fn rapid_scope_creation_and_destruction_is_stable() {
    for _ in 0..2000 {
        std::thread::scope(|s| {
            s.spawn(|| {});
        });
    }
}

// ---- 13. Resource exhaustion -- honestly scoped, not actually exhausted -
// Deliberately bounded well below any real OS limit: this environment
// is a shared machine, and the honest, useful claim here isn't "we
// crashed the OS," it's "a large-but-reasonable number of concurrent
// mailboxes/threads behaves predictably, with no corruption" --
// genuinely exhausting OS thread/fd limits is a real, separate test
// this prototype does not attempt, disclosed rather than implied.

#[test]
fn a_large_number_of_concurrent_mailboxes_behaves_predictably() {
    let mailboxes: Vec<_> = (0..10_000).map(|_| mailbox::<i64>()).collect();
    for (tx, _rx) in &mailboxes {
        send(tx, 1).unwrap();
    }
    for (_tx, rx) in &mailboxes {
        assert_eq!(receive(rx), Ok(1));
    }
}

// ---- 14. Panic while owning a value -- CLASSIFICATION: runtime panic,
// propagated -- not swallowed, not UB.

#[test]
fn a_child_thread_panicking_while_holding_an_iso_value_propagates_through_the_scope() {
    let result = std::panic::catch_unwind(|| {
        std::thread::scope(|s| {
            s.spawn(|| {
                let _held = Iso(vec![1, 2, 3]); // owned, never sent, never returned
                panic!("deliberate panic while still owning `_held`");
            });
        });
    });
    assert!(result.is_err(), "std::thread::scope must propagate a child's panic to the caller, not swallow it");
}

#[test]
fn one_child_panicking_does_not_prevent_other_children_from_completing_first() {
    // C2's real claim isn't tested to full fidelity here (this
    // prototype has no supervisor/cancellation-on-sibling-failure
    // wired up yet -- a disclosed gap, not silently assumed done) --
    // but the floor claim, "a sibling's panic doesn't corrupt or lose
    // another sibling's already-completed, already-sent work," is
    // real and checked.
    let (tx, rx) = mailbox::<i64>();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::thread::scope(|s| {
            let tx2 = tx.clone();
            s.spawn(move || {
                send(&tx2, 7).unwrap();
            });
            s.spawn(|| {
                panic!("a sibling panics");
            });
        });
    }));
    assert!(result.is_err());
    assert_eq!(receive(&rx), Ok(7), "the non-panicking sibling's send must still have gone through");
}

// ---- 15. Panic while sending a value -------------------------------------
// CLASSIFICATION: not applicable as stated -- `send` on a healthy
// mailbox cannot itself panic (it's an infallible enqueue or a clean
// `Err`, never a panicking operation in this design). The nearest real
// question -- does a panic *immediately after* a successful send still
// let the message be received -- is exactly
// `a_value_survives_correctly_even_if_the_sending_thread_exits_immediately_after_send`
// (#7) with a panic substituted for a normal return; verified below to
// close that specific gap rather than assume it.

#[test]
fn a_message_already_sent_survives_even_if_the_sender_then_panics() {
    let (tx, rx) = mailbox::<i64>();
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        std::thread::scope(|s| {
            s.spawn(|| {
                send(&tx, 42).unwrap();
                panic!("panics immediately after a successful send");
            });
        });
    }));
    assert!(result.is_err());
    assert_eq!(receive(&rx), Ok(42));
}

// ---- Bonus: `Froze<T>` sharing, and the classic two-channel deadlock -----
// (not in the brief's own list, but directly relevant to Pillar 5's
// own honesty section: Pillars 1-4 alone do NOT prevent this.)

#[test]
fn froze_is_freely_shared_read_only_across_many_threads() {
    let shared = Froze::new(vec![1, 2, 3, 4, 5]);
    let sum = std::thread::scope(|s| {
        let handles: Vec<_> = (0..5)
            .map(|_| {
                let shared = shared.clone();
                s.spawn(move || shared.iter().sum::<i32>())
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).sum::<i32>()
    });
    assert_eq!(sum, 15 * 5);
}

/// **A genuine, surprising finding from actually running this, not
/// assumed in advance**: the brief's Phase 7 classic deadlock, taken
/// completely literally --
///   A -> channel1.send(); A -> channel2.receive()
///   B -> channel2.send(); B -> channel1.receive()
/// -- does **not** reproduce under Pillars 1-4. First version of this
/// test asserted it would hang and failed (the send/receive pair
/// completed in well under a millisecond). The reason, once traced
/// through: Pillar 2 makes `send` unconditional and immediate -- it
/// never waits for a receiver to be ready. So by construction, neither
/// thread's `receive` is ever waiting on a message that depends on the
/// *other* thread reaching some later statement first; both sends have
/// already landed before either thread's `receive` call even begins.
/// A cycle of blocking waits requires a blocking op to depend on
/// something gated behind *another* blocking op -- two independent,
/// already-fired sends don't create that. Kept here, inverted, as a
/// real regression test for that specific (real, literally-stated, but
/// not actually dangerous) case.
#[test]
fn the_literal_two_independent_channel_send_receive_case_does_not_deadlock() {
    let (tx1, rx1) = mailbox::<i64>();
    let (tx2, rx2) = mailbox::<i64>();
    let (done_tx, done_rx) = mailbox::<()>();

    std::thread::spawn(move || {
        std::thread::scope(|s| {
            s.spawn(|| {
                send(&tx1, 1).unwrap();
                let _ = receive(&rx2);
            });
            s.spawn(|| {
                send(&tx2, 1).unwrap();
                let _ = receive(&rx1);
            });
        });
        let _ = send(&done_tx, ());
    });

    let completed = done_rx.recv_timeout(Duration::from_millis(300)).is_ok();
    assert!(completed, "this specific pattern should NOT deadlock under Pillar 2 -- see this test's own doc comment");
}

/// The deadlock Pillar 5 actually targets: a **nested reply-obligation**,
/// not two independent one-way sends. A sends a request to B and parks
/// in exactly one blocking `receive` for B's *reply*. To compute that
/// reply, B needs an answer from A -- but A is not running any code
/// that could ever service that request (it is blocked, waiting only
/// on the reply channel). Neither thread has an independent trigger
/// that fires regardless of the other, unlike the case above: B's
/// question to A sits in a queue A is never listening on, and A's
/// reply-wait can never be satisfied because B can never finish
/// computing it. This **does** hang, confirming Pillars 1-4 alone do
/// not prevent the class of deadlock Pillar 5's "reply-obligation must
/// strictly ascend levels" rule (R5.3) exists to rule out at compile
/// time -- exactly the honest boundary
/// `nirdosha_concurrency_spec.md`'s own "what we are NOT claiming"
/// section already names, now demonstrated rather than assumed.
#[test]
fn nested_reply_obligation_deadlocks_without_pillar_5() {
    let (a_to_b, b_recv_request) = mailbox::<i64>(); // A's initial request, to B
    let (b_reply_to_a, a_recv_reply) = mailbox::<i64>(); // B's final reply, to A -- A's ONLY receive
    let (b_ask_a, a_recv_ask) = mailbox::<i64>(); // B's own question back to A -- A never reads this
    let (_a_answer_to_b, b_recv_answer) = mailbox::<i64>(); // would carry A's answer -- never sent
    let (done_tx, done_rx) = mailbox::<()>();

    std::thread::spawn(move || {
        std::thread::scope(|s| {
            // Thread A: send the request, then block for exactly one
            // thing -- the reply. No further code runs until it
            // arrives, so B's incoming question (on a_recv_ask) is
            // never serviced.
            s.spawn(|| {
                send(&a_to_b, 1).unwrap();
                let _ = receive(&a_recv_reply);
            });
            // Thread B: receive A's request, then discover it needs an
            // answer from A before it can reply -- asks, then blocks.
            s.spawn(|| {
                let _ = receive(&b_recv_request).unwrap();
                send(&b_ask_a, 99).unwrap();
                let answer = receive(&b_recv_answer); // blocks forever: A never answers
                let _ = answer.map(|a| send(&b_reply_to_a, a * 2));
            });
        });
        let _ = send(&done_tx, ());
        // `a_recv_ask`'s single queued message, never drained, is the
        // observable trace of the cycle -- kept unread on purpose.
        let _ = a_recv_ask;
    });

    let timed_out = done_rx.recv_timeout(Duration::from_millis(300)).is_err();
    assert!(
        timed_out,
        "this SHOULD hang with only Pillars 1-4 -- a nested reply-obligation cycle is exactly what Pillar 5 exists to reject at compile time"
    );
}

// Silence an unused-import warning for `SendResult`/`try_send`, used
// only by the Pillar-2 backpressure test below.
#[test]
fn bounded_try_send_never_blocks_and_reports_full_explicitly() {
    let (tx, _rx) = concurrency_proto::bounded_mailbox::<i64>(1);
    assert!(matches!(try_send(&tx, 1), SendResult::Ok));
    // Capacity 1, already full -- try_send must return immediately
    // with `Full`, never block.
    let start = std::time::Instant::now();
    let result = try_send(&tx, 2);
    assert!(start.elapsed() < Duration::from_millis(50), "try_send must never block");
    assert!(matches!(result, SendResult::Full(2)));
}
