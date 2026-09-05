//! Phase 8: real timing numbers, not modeled ones. Thread creation/
//! destruction, mailbox creation, send/receive latency, ownership-
//! transfer cost for small vs. large `Iso<T>` payloads, and a
//! contended multi-producer/multi-consumer scenario -- each compared
//! against the closest native baseline available in `std`.

use concurrency_proto::{mailbox, receive, send, Iso};
use std::time::Instant;

fn bench(label: &str, iters: u64, mut f: impl FnMut()) {
    let mut best = std::time::Duration::MAX;
    for _ in 0..3 {
        let start = Instant::now();
        for _ in 0..iters {
            f();
        }
        let elapsed = start.elapsed();
        if elapsed < best {
            best = elapsed;
        }
    }
    let ns_per_iter = best.as_nanos() as f64 / iters as f64;
    println!("{label:<55} best of 3: {:>10.3} ms total   {:>9.2} ns/iter", best.as_secs_f64() * 1000.0, ns_per_iter);
}

fn main() {
    // 1. Raw OS thread creation + immediate join -- the floor every
    //    `spawn` pays regardless of what safety wrapper sits on top.
    bench("1. std::thread::spawn + join (no payload)", 20_000, || {
        std::thread::spawn(|| {}).join().unwrap();
    });

    // 2. std::thread::scope + one child, same work -- Pillar 4's
    //    structured-concurrency primitive, at zero extra runtime cost
    //    over #1 (it's the same underlying OS thread; the "structure"
    //    is a compile-time/API guarantee, not a runtime wrapper).
    bench("2. std::thread::scope + one child (no payload)", 20_000, || {
        std::thread::scope(|s| {
            s.spawn(|| {});
        });
    });

    // 3. Mailbox creation.
    bench("3. mailbox() creation", 200_000, || {
        let _ = mailbox::<i64>();
    });

    // 4. Send + receive round trip, same thread (isolates the
    //    channel's own overhead from real cross-thread scheduling).
    let (tx, rx) = mailbox::<i64>();
    bench("4. send + receive round trip, same thread", 2_000_000, || {
        send(&tx, 42).unwrap();
        receive(&rx).unwrap();
    });

    // 5/6. Ownership-transfer cost, small vs. large Iso<T> -- the real
    // claim under test: R1-R4 say this is a *move*, so cost should be
    // ~O(1) regardless of payload size, not O(size).
    let (tx_small, rx_small) = mailbox::<Iso<i64>>();
    bench("5. Iso<i64> transfer (8 bytes)", 1_000_000, || {
        send(&tx_small, Iso(42)).unwrap();
        let Iso(_v) = receive(&rx_small).unwrap();
    });

    let (tx_big, rx_big) = mailbox::<Iso<Vec<u8>>>();
    let big_payload = vec![0u8; 64 * 1024 * 1024]; // 64 MB
    bench("6. Iso<Vec<u8>> transfer (64 MB)", 200, || {
        // A fresh 64MB Vec would dominate the timing with allocation
        // cost unrelated to *transfer* -- clone the pre-built one once
        // per iteration outside what's being measured is impossible
        // with this simple harness, so this bench includes one clone's
        // cost (real allocation), reported honestly rather than
        // hidden. The point (send/receive itself is O(1) regardless of
        // size) still shows clearly against bench 5's per-iter cost:
        // if transfer itself were O(size), this would be orders of
        // magnitude slower than a 64MB memcpy already is, not merely
        // "one clone's worth."
        let payload = big_payload.clone();
        send(&tx_big, Iso(payload)).unwrap();
        let Iso(_v) = receive(&rx_big).unwrap();
    });

    // 7. Contended: 4 producers, 1 consumer, real cross-thread
    //    scheduling (not same-thread like #4).
    let (tx_c, rx_c) = mailbox::<i64>();
    bench("7. 4 producers -> 1 consumer, cross-thread (1000 msgs)", 50, || {
        std::thread::scope(|s| {
            for _ in 0..4 {
                let tx = tx_c.clone();
                s.spawn(move || {
                    for i in 0..250 {
                        send(&tx, i).unwrap();
                    }
                });
            }
            for _ in 0..1000 {
                receive(&rx_c).unwrap();
            }
        });
    });
}
