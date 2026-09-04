//! The benchmark harness (unified plan §4.5.2): pass@1 and self-repair-
//! rate scoring plumbing for "can a model write Nirdosha," built against
//! `corpus.json`'s ~20-30 prompt -> expected-program tasks spanning the
//! language's features (arithmetic, control flow, recursion, ownership,
//! `f64`/`Vector`/`Matrix`, concurrency, RNG, `audited`, `sandbox`).
//!
//! **What this does and doesn't include, stated precisely (unified plan
//! §4.5.2):** this is the harness *plumbing* -- corpus format, a scoring
//! loop, and the re-prompt-with-diagnostics mechanism (docs/goal.md row 9's
//! actual payoff: a failing attempt's structured `Diagnostic` JSON,
//! from `nirdosha::run_diagnostic`, is what a real re-prompt would feed
//! back to a model). `Model` is a small trait; `MockModel` and
//! `SelfRepairMockModel` exist only to prove the loop itself works end
//! to end (4.5.5's verification bullet) against no live model at all.
//! `real_model::RealModel` is a real integration: an HTTP client for any
//! OpenAI-compatible `/chat/completions` endpoint (DeepSeek, Kimi/
//! Moonshot, GLM/Zhipu -- configurable via `NIRDOSHA_BENCH_API_BASE`/
//! `_API_KEY`/`_MODEL`, see `real_model`'s module doc), selected with
//! `--mode real`. It has not been run against a live provider in this
//! development environment -- no API key is set here -- but its request
//! building and response parsing are covered by real unit tests
//! (`real_model::tests`) that don't need one.
//!
//! **Scoring is against `run()`'s return *value*, not printed stdout.**
//! `print`'s builtin writes straight to the process's real stdout
//! (`eval_builtin`, interpreter.rs) with no capture hook of its own --
//! building one is a separate, real piece of work this harness doesn't
//! need to block on. Every corpus task's `main()` returns a plain
//! scalar instead, which is directly comparable in-process. A future
//! revision that wants to score printed output can do so by shelling
//! out to the real `nirdosha` binary and capturing its stdout, the same
//! technique `crates/compiler/tests/codegen.rs` already uses for compiled
//! binaries.

use std::collections::HashSet;

use serde::Deserialize;
use serde_json::Value as Json;

pub mod real_model;
pub use real_model::RealModel;

#[derive(Deserialize, Clone)]
pub struct Task {
    pub id: String,
    // Unread by both mock `Model`s (neither actually "generates" from
    // the prompt) -- a real `Model::generate` implementation is the
    // consumer; kept on the struct since it's real corpus data, not
    // dead weight.
    #[allow(dead_code)]
    pub prompt: String,
    pub expected_nir: String,
    pub expected_value: Json,
}

/// A source of candidate programs for one task. In real use: an LLM API
/// call seeded with `task.prompt` (and, on retry, `prior_failure` --
/// the previous attempt's structured diagnostic JSON, or a parse/lex
/// error string for the two stages that aren't yet part of `Diagnostic`
/// -- see `lib.rs`'s doc comment for why those two stay plain text).
pub trait Model {
    fn generate(&mut self, task: &Task, prior_failure: Option<&str>) -> String;
}

/// Always returns the task's own known-good reference program on the
/// first attempt -- proves the scoring loop works end to end against a
/// mock response (4.5.5's verification bullet), nothing more.
pub struct MockModel;

impl Model for MockModel {
    fn generate(&mut self, task: &Task, _prior_failure: Option<&str>) -> String {
        task.expected_nir.clone()
    }
}

/// Deliberately wrong on the first attempt (a syntactically broken
/// program -- the closing brace of the last function is missing) and
/// correct on the second, regardless of what `prior_failure` actually
/// says -- exercises the re-prompt loop *itself* running a second
/// generation and re-scoring it, not just the trivial always-right case
/// `MockModel` is. A real model would use `prior_failure`'s content to
/// decide the fix; this one doesn't need to, since its "fix" is fixed.
#[derive(Default)]
pub struct SelfRepairMockModel {
    already_failed_once: HashSet<String>,
}

impl Model for SelfRepairMockModel {
    fn generate(&mut self, task: &Task, _prior_failure: Option<&str>) -> String {
        if self.already_failed_once.insert(task.id.clone()) {
            let mut broken = task.expected_nir.clone();
            broken.truncate(broken.trim_end().len().saturating_sub(1));
            broken
        } else {
            task.expected_nir.clone()
        }
    }
}

fn value_matches(v: &nirdosha::interpreter::Value, expected: &Json) -> bool {
    use nirdosha::interpreter::Value;
    match (v, expected) {
        (Value::Int(n), Json::Number(m)) => m.as_i64() == Some(*n),
        (Value::Float(f), Json::Number(m)) => m.as_f64().map(|e| (e - f).abs() < 1e-6).unwrap_or(false),
        (Value::Bool(b), Json::Bool(m)) => b == m,
        (Value::Unit, Json::Null) => true,
        _ => false,
    }
}

pub struct TaskResult {
    pub id: String,
    pub passed: bool,
    pub attempts_used: usize,
}

/// Runs `model` against `task` for up to `max_attempts` generations,
/// feeding each failure's structured diagnostic back in as
/// `prior_failure` for the next one -- the actual self-repair loop.
pub fn score_task(model: &mut dyn Model, task: &Task, max_attempts: usize) -> TaskResult {
    let mut prior_failure: Option<String> = None;
    for attempt in 1..=max_attempts {
        let candidate = model.generate(task, prior_failure.as_deref());
        match nirdosha::run_diagnostic(&candidate) {
            Ok(v) if value_matches(&v, &task.expected_value) => {
                return TaskResult { id: task.id.clone(), passed: true, attempts_used: attempt };
            }
            Ok(_) => {
                prior_failure = Some("the program ran to completion but returned the wrong value".to_string());
            }
            Err(nirdosha::RunFailure::Diagnostics(diags)) => {
                prior_failure = Some(serde_json::to_string(&diags).unwrap_or_default());
            }
        }
    }
    TaskResult { id: task.id.clone(), passed: false, attempts_used: max_attempts }
}

pub fn load_corpus() -> Vec<Task> {
    let path = concat!(env!("CARGO_MANIFEST_DIR"), "/corpus.json");
    let text = std::fs::read_to_string(path).unwrap_or_else(|e| panic!("failed to read {path}: {e}"));
    serde_json::from_str(&text).unwrap_or_else(|e| panic!("failed to parse {path}: {e}"))
}

pub fn score_all(model: &mut dyn Model, tasks: &[Task], max_attempts: usize) -> Vec<TaskResult> {
    tasks.iter().map(|task| score_task(model, task, max_attempts)).collect()
}
