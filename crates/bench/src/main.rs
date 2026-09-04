//! Thin CLI wrapper — the actual harness logic lives in `lib.rs` so
//! `tests/harness.rs` can exercise it directly (`cargo test` coverage,
//! not just a manually-run report).

use nirdosha_bench::{load_corpus, score_all, MockModel, RealModel, SelfRepairMockModel};

const MAX_ATTEMPTS: usize = 3;

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_else(|| "mock".to_string());
    let tasks = load_corpus();

    let results = match mode.as_str() {
        "mock" => score_all(&mut MockModel, &tasks, MAX_ATTEMPTS),
        "self-repair" => score_all(&mut SelfRepairMockModel::default(), &tasks, MAX_ATTEMPTS),
        "real" => {
            // Not a silent fallback to mock -- a missing key is a hard
            // error, since a `real` run that quietly scored a mock model
            // would misreport what it measured.
            let mut model = RealModel::from_env().unwrap_or_else(|e| {
                eprintln!("cannot run --mode real: {e}");
                std::process::exit(2);
            });
            score_all(&mut model, &tasks, MAX_ATTEMPTS)
        }
        other => {
            eprintln!(
                "unknown mode `{other}` -- use `mock` (always-correct), `self-repair` (wrong once, then \
                 fixed), or `real` (an actual OpenAI-compatible chat-completions API -- see \
                 nirdosha_bench::real_model's doc comment for the env vars it reads)"
            );
            std::process::exit(2);
        }
    };

    for r in &results {
        if r.passed {
            println!("PASS  {} (attempt {}/{})", r.id, r.attempts_used, MAX_ATTEMPTS);
        } else {
            println!("FAIL  {} (exhausted {} attempts)", r.id, MAX_ATTEMPTS);
        }
    }

    let total = results.len();
    let pass_at_1 = results.iter().filter(|r| r.passed && r.attempts_used == 1).count();
    let failed_first = results.iter().filter(|r| r.attempts_used > 1 || !r.passed).count();
    let repaired = results.iter().filter(|r| r.passed && r.attempts_used > 1).count();

    println!();
    println!("pass@1: {pass_at_1}/{total}");
    if failed_first > 0 {
        println!(
            "self-repair rate (of {failed_first} that failed on attempt 1): {repaired}/{failed_first} recovered within {MAX_ATTEMPTS} attempts"
        );
    } else {
        println!("self-repair rate: n/a (nothing failed on attempt 1)");
    }
}
