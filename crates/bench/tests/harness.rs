//! `cargo test` coverage for the benchmark harness itself (unified plan
//! §4.5.5's verification bullet: "harness runs end-to-end against a
//! mock model response to prove scoring loop works"). Not a claim about
//! any real model's pass@1/self-repair rate — see `lib.rs`'s module doc.

use nirdosha_bench::{load_corpus, score_all, MockModel, SelfRepairMockModel};

#[test]
fn the_corpus_loads_and_has_a_reasonable_number_of_tasks() {
    let tasks = load_corpus();
    assert!(tasks.len() >= 20, "expected at least 20 tasks, found {}", tasks.len());
    assert!(tasks.len() <= 30, "expected at most 30 tasks, found {}", tasks.len());
}

#[test]
fn every_task_id_is_unique() {
    let tasks = load_corpus();
    let mut ids: Vec<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
    ids.sort_unstable();
    let mut deduped = ids.clone();
    deduped.dedup();
    assert_eq!(ids.len(), deduped.len(), "duplicate task id found");
}

/// The corpus's own self-check: every `expected_nir` really does
/// evaluate to its `expected_value` — if this ever fails, either the
/// reference program or the expected value drifted, and the corpus
/// itself is now lying about what "correct" means for that task.
#[test]
fn mock_model_passes_every_task_on_the_first_attempt() {
    let tasks = load_corpus();
    let results = score_all(&mut MockModel, &tasks, 3);
    let failed: Vec<&str> = results.iter().filter(|r| !r.passed).map(|r| r.id.as_str()).collect();
    assert!(failed.is_empty(), "expected every task to pass with the reference program, but these failed: {failed:?}");
    let not_first_try: Vec<&str> = results.iter().filter(|r| r.attempts_used != 1).map(|r| r.id.as_str()).collect();
    assert!(not_first_try.is_empty(), "MockModel always returns the correct program -- these should have passed on attempt 1: {not_first_try:?}");
}

/// Exercises the actual re-prompt loop, not just the trivial always-
/// right case above: every task is deliberately wrong on attempt 1 and
/// must recover by attempt 2.
#[test]
fn self_repair_model_recovers_every_task_by_the_second_attempt() {
    let tasks = load_corpus();
    let results = score_all(&mut SelfRepairMockModel::default(), &tasks, 3);
    let failed: Vec<&str> = results.iter().filter(|r| !r.passed).map(|r| r.id.as_str()).collect();
    assert!(failed.is_empty(), "expected every task to eventually recover, but these failed: {failed:?}");
    let wrong_attempt_count: Vec<&str> = results.iter().filter(|r| r.attempts_used != 2).map(|r| r.id.as_str()).collect();
    assert!(wrong_attempt_count.is_empty(), "SelfRepairMockModel is wrong on attempt 1, correct on attempt 2 -- these didn't match that shape: {wrong_attempt_count:?}");
}

/// A program that's wrong forever (never matches `expected_value`,
/// regardless of attempt) must be reported as a failure once attempts
/// are exhausted, not silently dropped or falsely marked passed.
#[test]
fn a_model_that_never_produces_the_right_answer_is_reported_as_failed() {
    struct AlwaysWrongModel;
    impl nirdosha_bench::Model for AlwaysWrongModel {
        fn generate(&mut self, _task: &nirdosha_bench::Task, _prior_failure: Option<&str>) -> String {
            "fn main() -> i64 { return -1 }".to_string()
        }
    }
    let tasks = load_corpus();
    // Only tasks whose expected value genuinely isn't -1 are a fair
    // test of "this should fail" -- exclude any accidental match.
    let relevant: Vec<_> = tasks.into_iter().filter(|t| t.expected_value != serde_json::json!(-1)).collect();
    assert!(!relevant.is_empty());
    let results = score_all(&mut AlwaysWrongModel, &relevant, 2);
    assert!(results.iter().all(|r| !r.passed), "AlwaysWrongModel should never pass");
    assert!(results.iter().all(|r| r.attempts_used == 2), "should exhaust all attempts before giving up");
}
