# Benchmark harness

Unified development plan §4.5.2 — "can a model write Nirdosha" scoring
plumbing. This is the harness, not a claim about any real model's score.

## What's here

- `corpus.json` — 23 `{id, prompt, expected_nir, expected_value}` tasks
  spanning the language's shipped features: arithmetic, control flow
  (`if`, `while`, recursion), ownership (`box`, `&`), `str`, `f64`,
  `Vector`/`Matrix` literals and builtins (`dot`, `det`), concurrency
  (`spawn`/`join`, `chan`), deterministic RNG, `audited`, and the linear
  Kalman filter / WGS84 geometry builtins.
- `src/lib.rs` — the scoring loop: `score_task` runs a `Model` up to N
  times against one task, feeding each failure's structured diagnostic
  (`nirdosha::run_diagnostic`'s `Diagnostic` JSON — goal.md row 9) back
  in as the next attempt's context, exactly the re-prompt loop a real
  self-repair integration would use.
- `src/main.rs` — a CLI (`cargo run -- mock` or `cargo run -- self-repair`)
  printing a pass@1 / self-repair-rate report.
- `tests/harness.rs` — `cargo test` coverage proving the loop itself
  works (unified plan §4.5.5's verification bullet), independent of the
  CLI.

## What's *not* here — stated precisely, not silently implied

**No real model integration.** `Model` is a two-method trait
(`generate(&mut self, task, prior_failure) -> String`); this crate ships
two mock implementations (`MockModel` — always right; `SelfRepairMockModel`
— wrong once, then right) that exist only to prove the scoring loop
itself is correct. Wiring in a real LLM API is a distinct, separate piece
of work — this phase built the plumbing it would plug into, not the
integration (unified plan §4.5.2's own scoping).

**Scoring is against `run()`'s return value, not printed stdout.**
`print`'s builtin (interpreter.rs's `eval_builtin`) writes straight to
the process's real stdout with no capture hook — building one is a
separate, real piece of work this harness doesn't block on. Every
corpus task's `main()` returns a plain scalar (`i64`/`f64`/`bool`/unit)
instead, directly comparable in-process via `value_matches`. A future
revision that wants to score *printed* output could do so by shelling
out to the real `nirdosha` binary and capturing its stdout — the same
technique `compiler/tests/codegen.rs` already uses for compiled
binaries — rather than the in-process `nirdosha::run_diagnostic` call
this harness uses today.

**No `sandbox` task in the corpus.** `sandbox`-spawned processes re-exec
via `std::env::current_exe()` (`interpreter.rs`'s `spawn_sandbox`,
documented on `Interpreter::sandbox_exe`), which only resolves correctly
when the calling binary *is* the real `nirdosha` CLI — not a generic
library embedder like this harness. `lib.rs::run()`/`run_diagnostic()`
don't expose a way to override it. A `sandbox` task was written and
removed once this was discovered running the corpus for the first
time — a real, found-not-assumed limitation, not an oversight.

## Running it

```sh
cargo run -- mock          # every task should pass on attempt 1
cargo run -- self-repair   # every task fails once, recovers on attempt 2
cargo test                 # the same two runs, as assertions
```
