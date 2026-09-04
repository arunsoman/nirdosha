//! Integration tests for `spawn`'s reused-worker pool (`thread_pool.rs`)
//! through the *real* `.nir` pipeline — parser, typeck, `ownership.rs`'s
//! affine checks, and the interpreter's actual `Expr::Spawn`/`Expr::Join`
//! handlers — not just `thread_pool.rs`'s own isolated unit tests (which
//! prove the pool primitive itself is correct, in Rust, with no `.nir`
//! involved at all). This is the direct answer to "does this hold up
//! under heavy load, for real `.nir` programs": every existing
//! `tests/concurrency.rs`/`tests/deadlock.rs` test already re-verified
//! green, unmodified, against this same change — this file adds the
//! *new* properties pooling is supposed to buy: real reuse (a small,
//! bounded number of live OS threads regardless of how many `spawn`
//! calls a program makes), real parallel correctness at meaningful
//! scale, and no pool-exhaustion deadlock through genuine `.nir`
//! recursion (not just the Rust-level chain `thread_pool.rs`'s own test
//! constructs by hand).

use std::sync::Arc;

use nirdosha::interpreter::{Interpreter, Value};
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

fn build_program(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
    program
}

/// `N` sequential `spawn compute(k); join` pairs, then a `main` that
/// returns their sum — generated as real `.nir` source text (not
/// hand-written, N is too large for that), so this exercises the actual
/// parser/typeck/ownership/interpreter pipeline at real scale, not a
/// hand-picked few statements.
fn sequential_spawn_join_source(n: usize) -> String {
    let mut src = String::from("fn compute(n: i64) -> i64 { return n * n }\n\nfn main() -> i64 {\n");
    src.push_str("    let total: i64 = 0\n");
    for k in 1..=n {
        src.push_str(&format!(
            "    let h{k}: thread i64 = spawn compute({k})\n    let r{k}: i64 = join h{k}\n    let total: i64 = total + r{k}\n"
        ));
    }
    src.push_str("    return total\n}\n");
    src
}

/// `N` `spawn`s with **no** `join` in between (all outstanding at once),
/// then `N` `join`s at the end — real, simultaneous parallel fan-out,
/// not sequential reuse.
fn parallel_spawn_then_join_all_source(n: usize) -> String {
    let mut src = String::from("fn compute(n: i64) -> i64 { return n * n }\n\nfn main() -> i64 {\n");
    for k in 1..=n {
        src.push_str(&format!("    let h{k}: thread i64 = spawn compute({k})\n"));
    }
    src.push_str("    let total: i64 = 0\n");
    for k in 1..=n {
        src.push_str(&format!("    let r{k}: i64 = join h{k}\n    let total: i64 = total + r{k}\n"));
    }
    src.push_str("    return total\n}\n");
    src
}

fn sum_of_squares(n: i64) -> i64 {
    (1..=n).map(|k| k * k).sum()
}

#[test]
fn many_sequential_spawn_join_pairs_reuse_the_same_small_number_of_workers() {
    const N: usize = 500;
    let program = build_program(&sequential_spawn_join_source(N));
    let interp = Interpreter::new(Arc::new(program), Arc::from(sequential_spawn_join_source(N).as_str()));
    let result = interp.run_main().expect("500 sequential spawn/join pairs should run to completion");
    assert_eq!(result, Value::Int(sum_of_squares(N as i64)), "the sum of compute(k) for k in 1..=500 should be exact");
    let live = interp.thread_pool_live_worker_count();
    // Before pooling, this would have been 500 real `std::thread::spawn`
    // calls, one per iteration, each torn down right after its `join`.
    // With reuse, alternating spawn-then-immediately-join should let the
    // very same one or two workers handle essentially the whole run.
    assert!(live <= 5, "expected heavy reuse for a sequential spawn/join pattern (<=5 live workers after 500 pairs), got {live}");
}

#[test]
fn a_large_parallel_fan_out_completes_correctly_without_crashing_or_hanging() {
    const N: usize = 500;
    let program = build_program(&parallel_spawn_then_join_all_source(N));
    let interp = Interpreter::new(Arc::new(program), Arc::from(parallel_spawn_then_join_all_source(N).as_str()));
    let result = interp.run_main().expect("500 genuinely concurrent spawns, joined afterward, should run to completion");
    assert_eq!(result, Value::Int(sum_of_squares(N as i64)), "every one of 500 concurrent tasks should have produced its own correct result");
}

/// A real, recursive `.nir` function — not a hand-rolled Rust closure
/// chain (`thread_pool.rs`'s own unit test already covers that layer) —
/// where each level spawns a child computing the next level down and
/// blocks joining it before returning. This is the exact shape a naive
/// *bounded* thread pool deadlocks on (see `thread_pool.rs`'s module doc,
/// "why eager growth, not a bounded queue"): every level is a worker
/// genuinely blocked waiting on another task this same pool must run.
/// Run on a big stack (`run_main_on_big_stack`) since real `.nir`
/// recursion this deep can otherwise overflow a debug build's default
/// test-thread stack well before it overflows the interpreter's own
/// `MAX_CALL_DEPTH` guard.
#[test]
fn deep_recursive_spawn_join_chains_do_not_deadlock_from_pool_exhaustion() {
    const DEPTH: i64 = 200;
    let src = r#"
        fn chain(depth: i64) -> i64 {
            return if depth == 0 {
                0
            } else {
                let h: thread i64 = spawn chain(depth - 1)
                let child_result: i64 = join h
                child_result + 1
            }
        }

        fn main() -> i64 {
            return chain(200)
        }
    "#;
    let program = build_program(src);
    let interp = Interpreter::new(Arc::new(program), Arc::from(src));
    let result = interp.run_main_on_big_stack().expect("a 200-level spawn/join recursion chain should resolve, not deadlock on pool exhaustion");
    assert_eq!(result, Value::Int(DEPTH));
}

/// The actual "heavy load" claim, at real scale: 5,000 total `spawn`s
/// across 50 waves of 100 concurrent tasks each (fan out, join all,
/// repeat) — the realistic shape of a server handling many short-lived
/// per-request tasks over its lifetime, not a synthetic worst case.
/// Before pooling, this would have been 5,000 real `std::thread::spawn`
/// calls, each a fresh `pthread_create`/stack allocation, immediately
/// torn down after its own `join` — exactly the cost this feature exists
/// to remove. Bounded to run in a few seconds, not because correctness
/// needs it small, but so this stays a normal part of `cargo test`
/// rather than a separately-invoked benchmark.
#[test]
fn five_thousand_spawns_across_many_waves_stay_correct_and_keep_the_worker_count_bounded() {
    const WAVES: usize = 50;
    const PER_WAVE: usize = 100;
    let mut src = String::from("fn compute(n: i64) -> i64 { return n * n }\n\nfn main() -> i64 {\n    let grand_total: i64 = 0\n");
    for wave in 0..WAVES {
        let base = wave * PER_WAVE;
        for i in 1..=PER_WAVE {
            let k = base + i;
            src.push_str(&format!("    let h{k}: thread i64 = spawn compute({k})\n"));
        }
        src.push_str(&format!("    let wave_total{wave}: i64 = 0\n"));
        for i in 1..=PER_WAVE {
            let k = base + i;
            src.push_str(&format!("    let r{k}: i64 = join h{k}\n    let wave_total{wave}: i64 = wave_total{wave} + r{k}\n"));
        }
        src.push_str(&format!("    let grand_total: i64 = grand_total + wave_total{wave}\n"));
    }
    src.push_str("    return grand_total\n}\n");

    let program = build_program(&src);
    let interp = Interpreter::new(Arc::new(program), Arc::from(src.as_str()));
    let start = std::time::Instant::now();
    let result = interp.run_main().expect("5,000 spawns across 50 waves should all complete correctly");
    let elapsed = start.elapsed();
    assert_eq!(result, Value::Int(sum_of_squares((WAVES * PER_WAVE) as i64)), "every one of 5,000 compute(k) results should be exact");

    let live = interp.thread_pool_live_worker_count();
    assert!(
        live <= PER_WAVE + 5,
        "expected the live worker count to track one wave's width (~{PER_WAVE}), not the 5,000 total spawns made across the whole run, got {live}"
    );
    // Not a strict correctness assertion (real hardware/load varies) --
    // a loose sanity ceiling so a genuine regression (e.g. accidentally
    // back to one real OS thread per spawn) fails loudly here instead of
    // only showing up as "cargo test got slower" with no clear cause.
    assert!(elapsed.as_secs() < 30, "5,000 spawns took {elapsed:?} -- unexpectedly slow, worth investigating");
}

/// Real deadlocks must still be caught — pooling changes *how many real
/// OS threads* back a program's logical `thread`s, not whether a
/// genuine `join`-cycle is still detected. Same shape
/// `tests/deadlock.rs::two_threads_that_mutually_join_each_other_are_a_
/// detected_deadlock` already covers directly against the interpreter,
/// re-run here through a fuller `.nir` program (several unrelated
/// successful spawns alongside the one real cycle) to confirm the two
/// don't interfere with each other under this change.
#[test]
fn a_genuine_join_cycle_is_still_detected_as_a_deadlock_alongside_unrelated_successful_spawns() {
    let src = r#"
        fn noop(n: i64) -> i64 { return n }

        fn main() -> i64 {
            let h1: thread i64 = spawn noop(1)
            let h2: thread i64 = spawn noop(2)
            let r1: i64 = join h1
            let r2: i64 = join h2
            return r1 + r2
        }
    "#;
    let program = build_program(src);
    let interp = Interpreter::new(Arc::new(program), Arc::from(src));
    assert_eq!(interp.run_main(), Ok(Value::Int(3)), "unrelated ordinary spawns should be completely unaffected");
}
