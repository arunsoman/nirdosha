//! Per-subject connection ref-counting.
//!
//! One subject can legitimately have more than one live WebSocket
//! connection at once (two browser tabs, a phone and a laptop) — presence
//! must go "online" on the *first* connection and "offline" only once the
//! *last* one closes, never on every individual connect/disconnect. A
//! naive one-connect-one-disconnect call per WebSocket would mark a
//! subject offline the moment they close one of two open tabs, even
//! though the other tab is still live — a real, silent correctness bug
//! this registry exists specifically to avoid.

use std::collections::HashMap;
use std::sync::Mutex;

pub struct PresenceRegistry {
    counts: Mutex<HashMap<String, u32>>,
}

impl PresenceRegistry {
    pub fn new() -> Self {
        Self { counts: Mutex::new(HashMap::new()) }
    }

    /// Records one more live connection for `subject`. Returns `true`
    /// exactly on the 0 -> 1 transition — the caller should call
    /// `PresenceClient::connect` only then, never on every call.
    pub fn increment(&self, subject: &str) -> bool {
        let mut counts = self.counts.lock().unwrap();
        let count = counts.entry(subject.to_string()).or_insert(0);
        *count += 1;
        *count == 1
    }

    /// Records one fewer live connection for `subject`. Returns `true`
    /// exactly on the 1 -> 0 transition — the caller should call
    /// `PresenceClient::disconnect` only then. Removes the entry entirely
    /// once it reaches zero so long-running processes don't accumulate an
    /// ever-growing map of zero-count subjects.
    pub fn decrement(&self, subject: &str) -> bool {
        let mut counts = self.counts.lock().unwrap();
        let Some(count) = counts.get_mut(subject) else {
            // Decrementing a subject with no recorded connection should
            // never happen (every decrement is paired with a prior
            // increment on the same connection's own task) — defensively
            // treat it as already-offline rather than underflowing or
            // panicking.
            return false;
        };
        *count -= 1;
        let reached_zero = *count == 0;
        if reached_zero {
            counts.remove(subject);
        }
        reached_zero
    }

    /// Every subject currently tracked as online — used only for graceful
    /// shutdown (`main.rs`'s SIGTERM handler), so every subject this
    /// process itself marked online gets a best-effort `disconnect` call
    /// before the process exits, rather than leaving a stale "online" row
    /// behind for `notify()` to keep trusting after nobody's listening.
    pub fn subjects(&self) -> Vec<String> {
        self.counts.lock().unwrap().keys().cloned().collect()
    }
}

impl Default for PresenceRegistry {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_connection_transitions_online_later_ones_do_not() {
        let reg = PresenceRegistry::new();
        assert!(reg.increment("alice"));
        assert!(!reg.increment("alice"));
        assert!(!reg.increment("alice"));
    }

    #[test]
    fn last_disconnect_transitions_offline_earlier_ones_do_not() {
        let reg = PresenceRegistry::new();
        reg.increment("alice");
        reg.increment("alice");
        reg.increment("alice");
        assert!(!reg.decrement("alice"));
        assert!(!reg.decrement("alice"));
        assert!(reg.decrement("alice"));
    }

    #[test]
    fn independent_subjects_do_not_interfere() {
        let reg = PresenceRegistry::new();
        assert!(reg.increment("alice"));
        assert!(reg.increment("bob"));
        assert!(reg.decrement("alice"));
        assert!(!reg.increment("bob")); // bob's first connection still live
    }

    #[test]
    fn decrementing_an_untracked_subject_is_a_safe_no_op() {
        let reg = PresenceRegistry::new();
        assert!(!reg.decrement("nobody-ever-connected"));
    }

    #[test]
    fn subjects_reflects_only_currently_online_ones() {
        let reg = PresenceRegistry::new();
        reg.increment("alice");
        reg.increment("bob");
        reg.decrement("bob");
        assert_eq!(reg.subjects(), vec!["alice".to_string()]);
    }
}
