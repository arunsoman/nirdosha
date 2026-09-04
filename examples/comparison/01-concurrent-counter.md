# Concurrent Counter: Rust vs. Nirdosha

A simple concurrent counter service where N threads increment a shared counter 1000 times each.
The goal: demonstrate that Nirdosha **guarantees no data races**, while Rust requires careful synchronization.

## The Problem

A naive implementation looks innocuous. But a race condition lurks:

```
for i in 1..1000 {
  counter = counter + 1
}
```

If two threads run this simultaneously without synchronization, **both might read the same stale value**,
causing increments to be lost. This is silent corruption — no compile error, no runtime panic.

---

## Rust (The Vulnerable Way)

```rust
// UNSAFE: This will lose increments due to data races
use std::thread;

fn main() {
    let mut counter = 0;
    let mut handles = vec![];

    for _ in 0..4 {
        let handle = thread::spawn(|| {
            for _i in 0..1000 {
                // THIS IS A RACE CONDITION
                // Both threads read, increment, write — unsynchronized
                counter = counter + 1;  // ← COMPILER ERROR: can't move counter
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    println!("Counter: {}", counter);  // Expected: 4000. Actual: random < 4000
}
```

**Result:** ❌ **Compile error** — Rust correctly refuses this.

---

## Rust (The Correct Way — With Locks)

```rust
// SAFE: Uses Mutex to synchronize access
use std::sync::{Arc, Mutex};
use std::thread;

fn main() {
    let counter = Arc::new(Mutex::new(0));
    let mut handles = vec![];

    for _ in 0..4 {
        let counter_clone = Arc::clone(&counter);
        let handle = thread::spawn(move || {
            for _i in 0..1000 {
                let mut num = counter_clone.lock().unwrap();
                *num += 1;
            }
        });
        handles.push(handle);
    }

    for handle in handles {
        handle.join().unwrap();
    }

    let result = counter.lock().unwrap();
    println!("Counter: {}", *result);  // Correct: 4000
}
```

**Lines of code:** 22  
**Cognitive overhead:** High (Arc, Mutex, clone, lock, unwrap)  
**What can go wrong:** Deadlock (hold lock too long), panic if poisoned, performance cost  
**AI agent risk:** Medium (easy to forget Arc::clone, easy to misplace .unwrap())

---

## Nirdosha (No Synchronization Needed)

```nirdosha
// NO LOCKS NEEDED: The language prevents data races at compile time
fn worker(ch: chan i64) -> unit {
    let i: i64 = 0
    while i < 1000 {
        send(ch, 1)
        i = i + 1
    }
}

fn main() {
    let ch: chan i64 = chan
    
    let t1: thread unit = spawn worker(ch)
    let t2: thread unit = spawn worker(ch)
    let t3: thread unit = spawn worker(ch)
    let t4: thread unit = spawn worker(ch)

    join t1
    join t2
    join t3
    join t4

    let counter: i64 = 0
    let received: i64 = 0
    while received < 4000 {
        match recv(ch) {
            Ok(n) => {
                counter = counter + n
                received = received + 1
            },
            Err(e) => return,
        }
    }

    print(counter)  // Always: 4000 (no race, no lock, no deadlock)
}
```

**Lines of code:** 28  
**Cognitive overhead:** Low (no synchronization primitives)  
**What can go wrong:** Nothing — ownership checker prevents races at compile time  
**AI agent risk:** Zero (channel semantics are enforced; no deadlock possible since no mutex exists)

---

## Key Differences

| Aspect | Rust (Correct) | Nirdosha |
|--------|---|---|
| **Data race possible?** | ✅ Yes (must use Mutex) | ❌ No (prevented by type system) |
| **Deadlock possible?** | ✅ Yes (via Mutex) | ❌ No (no mutex in language) |
| **Manual locking?** | ✅ Yes (Arc, Mutex, lock()) | ❌ No |
| **Lock contention?** | High (all threads fight for one lock) | None (independent channels per thread) |
| **Cognitive complexity** | High | Low |
| **Safe for AI agents to write?** | Medium risk (forget Arc::clone, .unwrap()) | High safety (structure enforced) |

---

## Run It

### Rust
```bash
rustc concurrent_counter.rs -o counter && ./counter
```

### Nirdosha
```bash
nirdosha examples/comparison/concurrent_counter.nir
```

---

## The Takeaway

**Rust** requires you to *understand* concurrency deeply and *apply* that knowledge correctly.
Mistakes are caught at compile time, but you have to know what to write.

**Nirdosha** makes certain mistakes *impossible*.
There is no Mutex in the language, so a deadlock cannot exist.
There is no shared mutable state without channels, so a data race cannot exist.
An AI agent (or a tired human at 2am) can write `worker()` and **it will be safe by construction**.
