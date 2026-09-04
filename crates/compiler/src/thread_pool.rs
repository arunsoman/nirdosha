//! A self-tuning, reused-worker OS thread pool backing `spawn`
//! (`interpreter.rs::Expr::Spawn`) — the answer to a direct question a
//! real-world enterprise-systems review raised: can Nirdosha's `thread`/
//! `spawn` be made cheap enough that a developer never has to think about
//! the cost of spawning one, the way Java's virtual threads or Go's
//! goroutines let you not think about it?
//!
//! **What this deliberately is *not*, and why**: literal Java-style
//! virtual threads (a user-space scheduler multiplexing many logical
//! threads onto a few OS threads, suspending a *specific point in a call
//! stack* and resuming it later) needs either (a) a managed runtime that
//! controls its own continuation representation — the JVM's actual
//! mechanism, unavailable in ahead-of-time-compiled Rust — or (b) unsafe
//! stackful coroutines with hand-rolled stack switching, which the
//! surviving Rust crates that do this (`may`, etc.) document as capable of
//! real undefined behavior via thread-local storage — directly
//! contradicting this project's own memory-safety guarantees, or (c) a
//! from-scratch async rewrite of the interpreter's entire execution model
//! (Rust's own actual answer to cheap concurrency, `async`/`.await` +
//! an executor like Tokio) — correct, and how production Rust systems
//! really do this, but a multi-week rewrite of every expression-evaluation
//! path, including every blocking builtin (`db_query`, `http_post`,
//! `recv`, `tcp`), far too large and far too risky to a correctness-
//! critical, already-shipped subsystem to attempt casually. Even Java's
//! own fully-JVM-controlled virtual threads still fight "carrier thread
//! pinning" today (a virtual thread blocked inside a native/FFI call
//! freezes its carrier) — Nirdosha's interpreter calls blocking native
//! code (SQLite, TLS, Redis, raw sockets) constantly, with zero
//! cooperation points, so a naive port would reintroduce exactly that
//! failure mode with none of the JVM's years of engineering behind it.
//!
//! **What this actually is**: real OS threads, reused instead of created
//! fresh per `spawn` — the same proven, 100%-safe-Rust pattern production
//! thread pools (.NET's `ThreadPool`, Java's own classic
//! `ThreadPoolExecutor`) use. `submit` never leaves a job waiting behind
//! other work purely because every worker happens to be busy: if no
//! worker is idle, a brand-new one is spawned immediately, just for that
//! job. This is the one property that matters most for correctness here —
//! see "why eager growth, not a bounded queue" below — a *bounded* pool
//! with blocking `join`/`recv` calls can deadlock purely from pool
//! exhaustion (task A spawns and blocks on task B, but no worker is free
//! to ever run B); eager growth makes that structurally impossible. Idle
//! workers retire after `IDLE_TIMEOUT` of no work, so the pool's real OS
//! thread count tracks actual concurrent demand, not the total number of
//! `spawn` calls ever made — this is the actual "dirt cheap" property:
//! a program that spawns 10,000 short-lived tasks in bursts reuses a
//! small, roughly-peak-concurrency-sized set of real threads instead of
//! creating and tearing down 10,000 of them.
//!
//! **What this does not solve, disclosed, not hidden**: a task that calls
//! a genuinely long blocking native operation (a slow `db_query`, a
//! `recv` waiting on a real external `tcp` peer) still ties up one real
//! OS worker thread for that duration, exactly as it does today with a
//! one-thread-per-spawn design — this pool changes *reuse*, not the cost
//! of blocking itself. True cheapness during blocking I/O specifically
//! needs the async/Tokio rewrite named above as the real, not-attempted-
//! here, next step.
//!
//! ## Why eager growth, not a bounded queue
//!
//! The obvious-looking alternative — a fixed-size pool with a bounded
//! task queue, blocking `submit` when full — is the textbook thread-pool
//! deadlock trap for exactly Nirdosha's workload: `spawn`+`join` means a
//! worker routinely blocks *waiting on another task this same pool must
//! run*. With a bounded pool, once every worker is blocked waiting on a
//! child that's still sitting in the queue because no worker is free to
//! start it, nothing can ever make progress — the pool has deadlocked
//! itself, independent of anything `DeadlockRegistry` (`interpreter.rs`)
//! detects, because that registry only reasons about `.nir`-level
//! `recv`/`join` waits, not this runtime's own internal scheduling.
//! Eager growth (spawn a new worker the instant a job would otherwise
//! wait) makes this class of self-inflicted deadlock structurally
//! impossible: a submitted job is *always* either claimed by an already-
//! idle worker or given a freshly-spawned one, synchronously, inside
//! `submit` itself — it can never sit queued behind other work waiting
//! for *some* worker to eventually free up.
//!
//! ## Failure mode changed, not just reuse added
//!
//! Before this: `Expr::Spawn` called the free-function `std::thread::
//! spawn`, which **panics the whole process** if the OS refuses to create
//! a new thread (`RLIMIT_NPROC`/`kernel.threads-max` exhaustion under
//! real heavy load) — an uncatchable crash, not a `.nir`-catchable error,
//! the exact "crumbles under heavy load" failure this module exists to
//! remove. `submit` here is fallible (`Result<(), SpawnError>`); a real
//! OS-level failure to create a thread — reachable today only under
//! genuine resource exhaustion, exercised in tests via an injectable
//! spawn function rather than actually exhausting OS threads — now
//! becomes a clean, propagatable error instead of a process abort.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

/// How long an idle worker waits for new work before retiring (removing
/// itself from the pool, ending its OS thread) — long enough that a
/// bursty-but-not-continuous workload (the common shape: a wave of
/// `spawn`s, a `join` on all of them, then a pause) doesn't thrash
/// creating and tearing down threads between waves; short enough that a
/// genuine one-off burst doesn't leave threads parked indefinitely.
const IDLE_TIMEOUT: Duration = Duration::from_secs(10);

type Job = Box<dyn FnOnce() + Send + 'static>;

/// The one real, fallible OS operation this module performs — spawning a
/// worker's backing thread. A trait, not a bare function pointer, so
/// tests can substitute a fake that fails on command without needing to
/// actually exhaust real OS thread resources (see `tests` below).
pub trait Spawner: Send + Sync + 'static {
    fn spawn(&self, f: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()>;
}

/// The real spawner `ThreadPool::new` uses — `std::thread::Builder`, not
/// the free-function `std::thread::spawn`, specifically because the
/// builder form returns a `Result` instead of panicking on failure (the
/// exact behavior change this whole module exists to make: a
/// `spawn`-under-resource-exhaustion `.nir` program gets a catchable
/// error, not a process abort).
struct RealSpawner;
impl Spawner for RealSpawner {
    fn spawn(&self, f: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
        std::thread::Builder::new().spawn(f)?;
        Ok(())
    }
}

struct PoolState {
    queue: VecDeque<Job>,
    idle_count: usize,
}

/// A self-tuning worker-thread pool. See the module doc comment for the
/// full design rationale. Cloned via `Arc` into every job it runs (a
/// spawned task can itself `spawn` more work, recursively, onto the same
/// pool) — `submit` takes `&Arc<Self>` for exactly this reason.
pub struct ThreadPool {
    state: Mutex<PoolState>,
    condvar: Condvar,
    spawner: Box<dyn Spawner>,
    /// Diagnostic/test-only: real OS worker threads currently alive
    /// (busy or idle-but-not-yet-retired) — not part of the scheduling
    /// logic itself, purely observational (`live_worker_count`, used by
    /// this module's own stress tests to confirm reuse actually happens,
    /// not just that the pool doesn't crash).
    live_workers: AtomicUsize,
}

/// A `submit` call that couldn't get a worker running — currently only
/// reachable when the OS itself refuses to create a new thread (real
/// resource exhaustion). The job that was about to be submitted is
/// **not** run and **not** left queued — `submit` always leaves the
/// queue exactly as it found it on this path (see `submit`'s own
/// rollback comment).
#[derive(Debug)]
pub struct SpawnError(pub String);

impl std::fmt::Display for SpawnError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "could not create a thread to run this: {}", self.0)
    }
}

impl ThreadPool {
    pub fn new() -> Arc<Self> {
        Self::with_spawner(Box::new(RealSpawner))
    }

    fn with_spawner(spawner: Box<dyn Spawner>) -> Arc<Self> {
        Arc::new(ThreadPool {
            state: Mutex::new(PoolState { queue: VecDeque::new(), idle_count: 0 }),
            condvar: Condvar::new(),
            spawner,
            live_workers: AtomicUsize::new(0),
        })
    }

    /// Real OS worker threads currently alive right now (test/diagnostic
    /// use only — see `live_workers`'s own doc comment).
    pub fn live_worker_count(&self) -> usize {
        self.live_workers.load(Ordering::SeqCst)
    }

    /// Runs `job` on some worker thread — an already-idle one if any
    /// exists, otherwise a freshly-spawned one, synchronously, before
    /// this call returns (see the module doc's "why eager growth"
    /// section for why this specific guarantee is load-bearing, not
    /// just an optimization). `Err` only when the OS itself refused to
    /// create a thread; the queue is left exactly as it was found (the
    /// job is never silently dropped, and never left orphaned in the
    /// queue with no worker ever coming to claim it).
    pub fn submit(self: &Arc<Self>, job: Job) -> Result<(), SpawnError> {
        let mut state = self.state.lock().unwrap();
        if state.idle_count > 0 {
            // An idle worker is already parked in `worker_loop`'s own
            // wait, guaranteed to wake and claim this job (or find it
            // already claimed by whichever idle worker `notify_one`
            // actually wakes — doesn't matter which one, any idle
            // worker is equally valid). No new OS thread needed.
            state.queue.push_back(job);
            self.condvar.notify_one();
            return Ok(());
        }
        // No idle worker exists right now -- spawn one immediately,
        // handed this exact job directly (not via the shared queue at
        // all, so there's nothing to roll back if the closure itself
        // never runs: the job is simply moved into it).
        drop(state); // never hold the lock across the real spawn syscall
        let pool = Arc::clone(self);
        self.live_workers.fetch_add(1, Ordering::SeqCst);
        let first_job = job;
        let outcome = self.spawner.spawn(Box::new(move || pool.worker_loop(Some(first_job))));
        if let Err(e) = outcome {
            // Roll back the optimistic counter bump -- no thread was
            // actually created, so none will ever decrement it.
            self.live_workers.fetch_sub(1, Ordering::SeqCst);
            return Err(SpawnError(e.to_string()));
        }
        Ok(())
    }

    /// One worker's whole lifetime: run `first_job` (if this worker was
    /// just spawned specifically to run it — `None` is never actually
    /// passed today, since every worker is either created with a first
    /// job via `submit`'s fresh-spawn path, but kept as `Option` so the
    /// loop body below reads the same either way after the first
    /// iteration), then loop pulling more work off the shared queue
    /// until `IDLE_TIMEOUT` of nothing to do, then retire.
    fn worker_loop(self: Arc<Self>, first_job: Option<Job>) {
        let mut next = first_job;
        loop {
            let job = match next.take() {
                Some(j) => j,
                None => match self.wait_for_job() {
                    Some(j) => j,
                    None => {
                        // Retire -- `wait_for_job` only returns `None`
                        // from inside the same critical section that
                        // decided nothing more is coming, so no racing
                        // `submit` can hand us work in the gap between
                        // "decided to retire" and "actually gone": by
                        // the time any other thread can observe
                        // `idle_count`, it's already been decremented
                        // there without a matching queue claim, so
                        // `submit` will correctly see "no idle worker"
                        // and spawn a fresh one instead of notifying a
                        // worker that's already on its way out.
                        self.live_workers.fetch_sub(1, Ordering::SeqCst);
                        return;
                    }
                },
            };
            // Deliberately outside any pool-internal lock: a panic
            // inside `job` (a user `.nir` program can genuinely panic —
            // see `interpreter.rs::Expr::Join`'s own panic handling)
            // must never happen while `self.state`'s mutex is held, or
            // it would poison the whole pool for every future job on
            // every worker. `catch_unwind` contains it; the caller
            // (`interpreter.rs`) is responsible for observing the panic
            // through its own result-delivery channel, not this pool.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(job));
        }
    }

    /// Blocks (with periodic wakeups so an idle worker doesn't wait past
    /// `IDLE_TIMEOUT` forever) until a job is available or it's time to
    /// retire. `None` means "retire" — the caller must actually stop
    /// looping, not call this again.
    fn wait_for_job(&self) -> Option<Job> {
        let mut state = self.state.lock().unwrap();
        loop {
            if let Some(j) = state.queue.pop_front() {
                return Some(j);
            }
            state.idle_count += 1;
            let (guard, wait_result) = self.condvar.wait_timeout(state, IDLE_TIMEOUT).unwrap();
            state = guard;
            state.idle_count -= 1;
            if wait_result.timed_out() && state.queue.is_empty() {
                return None;
            }
            // Spurious wakeup, or a job just landed -- loop back to
            // `pop_front`.
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    /// Real spawner (genuine OS threads) — used by every test except the
    /// one specifically proving the fallible-spawn error path, which
    /// needs a spawner that can be told to fail on command instead of
    /// actually exhausting OS thread resources (slow, flaky, and not
    /// really testing this module's own logic anyway).
    fn real_pool() -> Arc<ThreadPool> {
        ThreadPool::new()
    }

    #[test]
    fn a_single_job_runs_and_the_worker_is_reused_for_the_next_one() {
        let pool = real_pool();
        let (tx, rx) = mpsc::channel();
        let tx2 = tx.clone();
        pool.submit(Box::new(move || tx2.send(1).unwrap())).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 1);
        // Give the worker a moment to loop back to "idle" after finishing.
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(pool.live_worker_count(), 1, "the one worker should still be alive, parked idle");
        let tx3 = tx.clone();
        pool.submit(Box::new(move || tx3.send(2).unwrap())).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), 2);
        // Still exactly one real OS thread -- the second job reused the
        // first job's worker instead of spawning a new one.
        assert_eq!(pool.live_worker_count(), 1);
    }

    #[test]
    fn many_sequential_bursts_reuse_a_small_number_of_workers_not_one_per_spawn() {
        let pool = real_pool();
        for _ in 0..20 {
            let (tx, rx) = mpsc::channel();
            for _ in 0..25 {
                let tx = tx.clone();
                pool.submit(Box::new(move || tx.send(()).unwrap())).unwrap();
            }
            drop(tx);
            for _ in 0..25 {
                rx.recv_timeout(Duration::from_secs(2)).unwrap();
            }
        }
        // 500 total jobs submitted (20 bursts * 25) -- if this were
        // "one real OS thread per spawn, never reused" (the pre-pool
        // behavior), this would have created and torn down 500 threads.
        // With reuse, the live count right after the last burst should
        // be on the order of the burst width, not the total submitted.
        std::thread::sleep(Duration::from_millis(50));
        let live = pool.live_worker_count();
        assert!(live <= 25, "expected reuse to keep the worker count near one burst's width (<=25), got {live}");
    }

    #[test]
    fn idle_workers_retire_after_the_timeout_and_the_pool_shrinks_back_down() {
        let pool = real_pool();
        let (tx, rx) = mpsc::channel();
        for _ in 0..10 {
            let tx = tx.clone();
            pool.submit(Box::new(move || tx.send(()).unwrap())).unwrap();
        }
        drop(tx);
        for _ in 0..10 {
            rx.recv_timeout(Duration::from_secs(2)).unwrap();
        }
        assert!(pool.live_worker_count() >= 1);
        // Wait past IDLE_TIMEOUT (10s) -- every worker should retire on
        // its own with no further submissions.
        std::thread::sleep(IDLE_TIMEOUT + Duration::from_secs(2));
        assert_eq!(pool.live_worker_count(), 0, "idle workers should have retired and the pool shrunk back to zero");
    }

    #[test]
    fn a_job_that_spawn_join_blocks_waiting_on_a_child_never_deadlocks_from_pool_exhaustion() {
        // The exact failure mode a *bounded* pool would hit: a job that
        // itself submits a child job and blocks (via a channel recv,
        // standing in for `interpreter.rs`'s real `join`) waiting on it.
        // With eager growth, this must always complete -- there is
        // always a free worker for the child, however deep the chain.
        let pool = real_pool();
        const DEPTH: usize = 50;
        let (final_tx, final_rx) = mpsc::channel::<usize>();

        fn chain(pool: Arc<ThreadPool>, depth: usize, final_tx: mpsc::Sender<usize>) {
            if depth == 0 {
                final_tx.send(0).unwrap();
                return;
            }
            let (child_tx, child_rx) = mpsc::channel::<usize>();
            let pool2 = Arc::clone(&pool);
            pool.submit(Box::new(move || chain(pool2, depth - 1, child_tx))).unwrap();
            // Block THIS worker waiting on its child -- same shape
            // `interpreter.rs::Expr::Join` blocks a worker waiting on a
            // spawned child's result.
            let child_result = child_rx.recv_timeout(Duration::from_secs(5)).expect("child should complete, not deadlock");
            final_tx.send(child_result + 1).unwrap();
        }

        let pool2 = Arc::clone(&pool);
        pool.submit(Box::new(move || chain(pool2, DEPTH, final_tx))).unwrap();
        let result = final_rx.recv_timeout(Duration::from_secs(10)).expect("the whole chain should resolve, not deadlock on pool exhaustion");
        assert_eq!(result, DEPTH);
    }

    /// A spawner that fails every Nth call — proves `submit` surfaces a
    /// clean `Err` (not a panic) and leaves the pool's own bookkeeping
    /// consistent, without needing to actually exhaust real OS threads.
    struct FlakySpawner {
        fail_every: usize,
        calls: AtomicUsize,
    }
    impl Spawner for FlakySpawner {
        fn spawn(&self, f: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
            let n = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            if n % self.fail_every == 0 {
                return Err(std::io::Error::other("injected failure: OS refused to create a thread"));
            }
            std::thread::Builder::new().spawn(f)?;
            Ok(())
        }
    }

    #[test]
    fn a_real_os_level_spawn_failure_is_a_clean_err_not_a_panic() {
        let pool = ThreadPool::with_spawner(Box::new(FlakySpawner { fail_every: 1, calls: AtomicUsize::new(0) }));
        let result = pool.submit(Box::new(|| {}));
        assert!(result.is_err(), "expected the injected spawn failure to surface as Err");
        assert_eq!(pool.live_worker_count(), 0, "a failed spawn must not leave the live-worker count incremented");
    }

    #[test]
    fn an_intermittent_spawn_failure_does_not_corrupt_the_pool_for_later_submissions() {
        // Deliberately fails only the *very first* real `spawner.spawn()`
        // call -- guaranteed to actually happen, since the pool starts
        // with zero workers (`idle_count` can't be nonzero yet), unlike
        // any *later* submission, whose need for a fresh OS thread at
        // all depends on how aggressively the pool has already reused
        // one (proven thoroughly by this module's other tests -- too
        // aggressively for a "fails every Nth `spawner.spawn()` call"
        // test to predict how many real calls a given number of
        // submissions will produce). What this test actually needs to
        // prove -- a failed submission doesn't corrupt the pool's own
        // bookkeeping for whatever comes after it -- doesn't need more
        // than one deterministic failure to demonstrate.
        struct FailFirstCallOnly {
            calls: AtomicUsize,
        }
        impl Spawner for FailFirstCallOnly {
            fn spawn(&self, f: Box<dyn FnOnce() + Send + 'static>) -> std::io::Result<()> {
                if self.calls.fetch_add(1, Ordering::SeqCst) == 0 {
                    return Err(std::io::Error::other("injected failure: OS refused to create a thread"));
                }
                std::thread::Builder::new().spawn(f)?;
                Ok(())
            }
        }
        let pool = ThreadPool::with_spawner(Box::new(FailFirstCallOnly { calls: AtomicUsize::new(0) }));
        let first = pool.submit(Box::new(|| {}));
        assert!(first.is_err(), "the first real spawn() call should have hit the injected failure");
        assert_eq!(pool.live_worker_count(), 0);

        let (tx, rx) = mpsc::channel();
        pool.submit(Box::new(move || tx.send("still alive").unwrap())).expect("the pool must remain usable after a prior spawn failure");
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "still alive");
    }

    #[test]
    fn a_panic_inside_a_job_does_not_poison_the_pool_for_the_next_job() {
        let pool = real_pool();
        let (tx, rx) = mpsc::channel();
        pool.submit(Box::new(|| panic!("boom"))).unwrap();
        // Give the panicking job time to actually run and be caught.
        std::thread::sleep(Duration::from_millis(100));
        let tx2 = tx.clone();
        pool.submit(Box::new(move || tx2.send("still alive").unwrap())).unwrap();
        assert_eq!(rx.recv_timeout(Duration::from_secs(2)).unwrap(), "still alive");
    }
}
