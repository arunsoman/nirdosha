//! Evidence prototype for `nirdosha_concurrency_spec.md`'s Pillars 1-4
//! (capability-typed boxes, non-blocking send, blocking receive/select,
//! structured concurrency). Deliberately a *runtime-semantics* spike in
//! plain Rust, not a change to Nirdosha's real grammar/typechecker —
//! the point is to get real evidence that the mechanisms are soundly
//! implementable and to run the adversarial test list from the other
//! document (the "Thread/Channel/Sandbox" brief) against something
//! real, before committing to an RFC.
//!
//! The central finding this file's design leans on: Rust's own
//! ownership/move semantics and standard library already provide most
//! of Pillars 1-4 natively --
//!
//! - `Iso<T>` needs no enforcement machinery of its own: a non-`Copy`
//!   Rust value moved into a channel send is already exactly R4
//!   ("sending iso is a move; the compiler marks the source binding
//!   unusable") -- `rustc`'s own borrow checker *is* the proof
//!   obligation here, for free.
//! - `Froze<T>` is `Arc<T>` with no `DerefMut` -- shared, immutable,
//!   `Send + Sync` when `T` is, matching R3 exactly.
//! - Non-blocking `send`/bounded `try_send` (Pillar 2) is
//!   `std::sync::mpsc::channel()` (unbounded, `send` never blocks) and
//!   `sync_channel(n)` (`try_send` returns `Full` immediately) --
//!   already in `std`, not invented here.
//! - Structured concurrency (Pillar 4's "no orphan threads") is
//!   `std::thread::scope` -- stable since Rust 1.63, and it already
//!   enforces C1 ("a scope block cannot exit until every thread started
//!   inside it has terminated") as a real, safe-Rust guarantee, not
//!   something this prototype has to build.
//!
//! What Nirdosha's own compiler would still need to add, that this
//! prototype doesn't have to prove (Rust already proves it): affine/
//! move tracking equivalent to what `ownership.rs` already does for
//! `box`/`thread`/`chan` today, extended to a capability-typed `Iso`/
//! `Froze`/`Lend` distinction. This file's job is narrower: prove the
//! *runtime* mechanics are sound and fast, and produce real adversarial-
//! test evidence — the type-checking-side question is a `rustc`-proven
//! fact here (a real compile error), which is the strongest evidence
//! available that the analogous Nirdosha-side check is buildable.

use crossbeam_channel::{Receiver, Sender, TrySendError};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

/// R1/R4: the *only* thing that moves between threads. A plain newtype
/// — Rust's own move semantics are the entire enforcement mechanism.
/// No `Clone`, no `Copy`: attempting to use an `Iso<T>` after moving it
/// into `Mailbox::send` is `rustc`'s own E0382 ("use of moved value"),
/// not a runtime check this prototype adds.
#[derive(Debug)]
pub struct Iso<T>(pub T);

/// R3: immutable, freely shareable, `Send`/`Sync` whenever `T` is.
/// `Clone`-able (cheap `Arc` bump) and has no mutable-access method at
/// all — the type itself makes "many threads read, none mutate" the
/// only expressible usage.
pub struct Froze<T>(Arc<T>);

impl<T> Froze<T> {
    pub fn new(value: T) -> Self {
        Froze(Arc::new(value))
    }
}

impl<T> Clone for Froze<T> {
    fn clone(&self) -> Self {
        Froze(Arc::clone(&self.0))
    }
}

impl<T> std::ops::Deref for Froze<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

// SAFETY: `Froze<T>` only ever hands out shared (`&T`) access, from
// any number of threads, which is exactly what `Send + Sync` requires
// of `T` itself -- this is the same bound `Arc<T>` already carries;
// no unsafe reasoning beyond what `Arc` itself already relies on.
unsafe impl<T: Send + Sync> Send for Froze<T> {}
unsafe impl<T: Send + Sync> Sync for Froze<T> {}

/// Pillar 2/3: an unbounded mailbox whose `send` **never blocks**, and
/// whose `Receiver` (unlike `std::sync::mpsc`, which is send-many/
/// receive-one) can be legally cloned and pulled from by *multiple*
/// consumer threads at once -- `crossbeam_channel::unbounded` already
/// provides both, a mature, widely-used implementation, not
/// reinvented here. Real multi-consumer support (S3's "round-robin
/// among mailboxes" fairness story needs multiple receivers to be
/// meaningful) and the real `select!` macro (S2) both come from this
/// same crate, for the same reason.
pub type Mailbox<T> = (Sender<T>, Receiver<T>);

pub fn mailbox<T>() -> Mailbox<T> {
    crossbeam_channel::unbounded()
}

#[derive(Debug, PartialEq)]
pub enum Closed {
    Closed,
}

/// S1: blocks while empty; returns `Closed` once every sender has been
/// dropped (S6).
pub fn receive<T>(rx: &Receiver<T>) -> Result<T, Closed> {
    rx.recv().map_err(|_| Closed::Closed)
}

/// M1: enqueues and returns immediately, always -- the actual Pillar-2
/// claim. `Err` only if every receiver has already been dropped (the
/// payload comes back, since nothing was lost).
pub fn send<T>(tx: &Sender<T>, msg: T) -> Result<(), T> {
    tx.send(msg).map_err(|e| e.0)
}

/// Pillar 2's `try_send` bounded variant -- backpressure that is
/// itself still non-blocking (M2): `Full` is a real, explicit result
/// the caller decides what to do with, never a suspension.
pub enum SendResult<T> {
    Ok,
    Full(T),
    Disconnected(T),
}

pub fn bounded_mailbox<T>(capacity: usize) -> Mailbox<T> {
    crossbeam_channel::bounded(capacity)
}

pub fn try_send<T>(tx: &Sender<T>, msg: T) -> SendResult<T> {
    match tx.try_send(msg) {
        Ok(()) => SendResult::Ok,
        Err(TrySendError::Full(m)) => SendResult::Full(m),
        Err(TrySendError::Disconnected(m)) => SendResult::Disconnected(m),
    }
}

/// S4: a level-triggered cancel token -- once fired, every subsequent
/// check sees it, forever (no "missed" cancellation).
#[derive(Clone)]
pub struct CancelToken(Arc<AtomicBool>);

impl CancelToken {
    pub fn new() -> Self {
        CancelToken(Arc::new(AtomicBool::new(false)))
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::SeqCst);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }
}

impl Default for CancelToken {
    fn default() -> Self {
        Self::new()
    }
}

pub mod scope_ext {
    //! Pillar 4: structured concurrency is `std::thread::scope`,
    //! unchanged -- re-exported under this crate's own naming for
    //! parity with the spec's `scope { ... }` syntax, not reimplemented.
    //! `std::thread::Scope::spawn` already refuses to let a spawned
    //! thread's handle (or anything borrowing `'scope` data) escape the
    //! closure, and the scope function itself does not return until
    //! every spawned thread has been joined -- C1 and C3 are therefore
    //! `rustc`-proven facts about this prototype, not new claims it
    //! introduces.
    pub use std::thread::scope;
}
