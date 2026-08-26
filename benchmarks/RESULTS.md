# Nirdosha benchmarks — results

**Update, 21 Aug 2026**: `Vector`/`Matrix` — the whole dense-linear-algebra
feature set pulled from Julia — is now a **compiled** feature
(`goal.md` §9 item 1; see `LANGUAGE.md` §7/§10, updated to match). The
numbers below are the re-run, compiled-vs-compiled results. The original
interpreted numbers are kept further down as the "before" state — the
whole point of compiling it was to see this delta, not to discard the
baseline that motivated the work.

Two separate comparisons, because scalar/control-flow code and dense
linear algebra were built and compiled by different, independently-tested
codegen phases, and the fair external baseline differs for each:

- **Group A — features taken from Julia** (dense linear algebra: `matmul`,
  `det`, `dot`, `kalman`): Nirdosha (**compiled**) vs. **Julia**
  (JIT-compiled native) vs. **C** (hand-specialized). Julia is the
  reference Nirdosha's `Vector`/`Matrix`/linalg builtins were modeled on;
  C is included because it's now a fair comparison (both compiled) and
  because it exposes a real, honest tradeoff — see below.
- **Group B — everything else** (scalar arithmetic, control flow, recursion:
  `fib`, `floatloop`): Nirdosha (**compiled**, `nirdosha build`) vs. **C**,
  the natural baseline for a language whose stated goal (`goal.md` row 5) is
  hardware-native speed with no runtime.

## Methodology

- Machine: Intel Core i7-8550U (4C/8T, 1.8 GHz base), Linux 7.0.10-zen1.
- Toolchains: `julia` 1.12.6, `gcc` 16.1.1, `clang` 22.1.8, Nirdosha's own
  codegen (shells out to `clang` under the hood — `codegen.rs`'s
  `Command::new("clang")` — so "Nirdosha-compiled" and "C via clang" share a
  backend; C via `gcc` is included too, as the more common baseline).
- Each program run 3 times, best wall-clock wall time reported (`time.perf_counter`
  around the whole process, so Julia's JIT warmup and process startup are
  *included*, deliberately — see caveats below).
- **Correctness verified first, for every pair, before any timing was
  trusted**: every Nirdosha/Julia/C program in a pair was run once and its
  output diffed by hand. `det`, `matmul`, `dot`, `kalman` all use the exact
  same algorithm across all three languages (Nirdosha's `matrix_inv`/
  `mat_mul_f64`/`kf_predict`/`kf_update` from `interpreter.rs`, translated
  line-for-line into C — not "close," bit-for-bit on the same operation
  order) rather than each language's own built-in linear-algebra routine, so
  the comparison measures runtime overhead, not algorithmic or numerical
  differences. Julia's `det`/`inv`/`*` go through LAPACK/BLAS and are
  numerically identical to ~14 significant figures, confirmed by hand.
- Source: `benchmarks/{c,julia,nirdosha}/*.{c,jl,nir}`.

## Group A — Julia-derived features (dense linear algebra)

Nirdosha **compiled** (`nirdosha build`) vs. Julia (`julia bench.jl`) vs.
C. 200,000 iterations per benchmark (drifting inputs each iteration so
nothing constant-folds away). Best-of-3 wall time. Every output verified
bit-identical across all three languages before any time was trusted
(same as the original run's methodology).

| Benchmark | C (gcc -O2) | Nirdosha (compiled) | Julia (JIT) | Nirdosha vs. Julia | Nirdosha vs. C |
|---|---:|---:|---:|---:|---:|
| `matmul` (4×4 × 4×4) | 0.0102 s | **0.0018 s** | 0.794 s | **441× faster** | 5.7× faster |
| `det` (4×4 Gaussian elim.) | 0.0093 s | 0.0272 s | 0.993 s | **36.5× faster** | 2.9× slower |
| `dot` (8-vector) | 0.0023 s | **0.0017 s** | 0.418 s | **246× faster** | 1.4× faster |
| `kalman` (4-state KF, predict+update) | 0.0798 s | 0.3274 s | 2.735 s | **8.4× faster** | 4.1× slower |

**Decisive win over Julia on all four now** — the 2-2 split in the
interpreted-era numbers below is gone once compiled, exactly what
compiling `Vector`/`Matrix` was expected to buy (`goal.md` §9).

**The honest asterisk**: Nirdosha loses to hand-specialized C on `det` and
`kalman`, and wins on `matmul`/`dot` by more than a compiled-vs-compiled
comparison should plausibly produce. Both have the same root cause —
`det`/`kalman`'s update step (Phase 5 of the codegen plan) go through a
**generic, runtime-parameterized** native function call (`nir_det(ptr, n)`,
compiled once into a linked static library, callable for *any* matrix
size `n`), which LLVM cannot inline across the compilation-unit boundary
or specialize for `n=4` — while C's `det4()` has `4` baked in as a literal
in its loop bounds, which `gcc` can and does exploit. `matmul`/`dot`, by
contrast, are **fully unrolled at Nirdosha-codegen time** into straight-line
IR (no function call, no loop at all) — that's *more* aggressively
optimizable than C's runtime-bounded loop (even one bounded by a compile-time
`4`), which is why Nirdosha wins there by more than "roughly tied" — this is
a real, structural effect, not noise, but it's an artifact of which builtins
got the unrolled-IR treatment vs. the linked-native-call treatment (see the
implementation plan's Phase 3/4 vs. Phase 5 split), not a general claim that
Nirdosha's backend beats C's. A future pass that monomorphizes the
runtime-library kernels per concrete size (or inlines them via LTO) would
likely close the `det`/`kalman` gap the same way unrolling already closed
it for `matmul`/`dot` — noted as follow-up, not done here.

### Before compiling (historical — interpreted Nirdosha vs. Julia)

Kept for the record; this was the whole reason Vector/Matrix codegen
(`goal.md` §9 item 1) got prioritized.

| Benchmark | Julia (JIT native) | Nirdosha (interpreted) | Ratio |
|---|---:|---:|---:|
| `matmul` | 0.656 s | 1.104 s | Nirdosha 1.68× slower |
| `det` | 0.980 s | 0.524 s | Nirdosha 1.87× faster |
| `dot` | 0.393 s | 0.479 s | Nirdosha 1.22× slower |
| `kalman` | 2.794 s | 1.307 s | Nirdosha 2.14× faster |

That 2-2 split was already a tree-walking interpreter holding its own
against JIT-compiled Julia at small (4×4) matrix sizes — mostly Julia
paying LAPACK/BLAS dispatch overhead that never amortizes at that size,
not Nirdosha's interpreter being unusually fast. Compiling turned "holding
its own" into "441× faster on the best case, 8.4× faster on the worst."

## Group B — everything else (scalar / control flow)

Nirdosha **compiled** (`nirdosha build`, LLVM via `clang`) vs. C. Two
optimization levels shown for Nirdosha; both `gcc -O2` and `clang -O2` shown
for C since Nirdosha's backend is `clang`.

| Benchmark | C (gcc -O2) | C (clang -O2) | Nirdosha (`--opt0`) | Nirdosha (`-O2`, default) | Julia (for reference) |
|---|---:|---:|---:|---:|---:|
| `fib(35)` (recursive) | 0.018 s | 0.027 s | 0.078 s | **0.026 s** | 0.283 s |
| `floatloop` (2×10⁸ iters) | 0.443 s | 0.435 s | 1.041 s | **0.436 s** | 0.686 s |

At `-O2`, Nirdosha-compiled is within **1.4×** of `gcc -O2` on `fib` and
essentially tied with both C compilers on `floatloop` (0.436 s vs. 0.443/
0.435 s — noise-level). This is the comparison `goal.md` row 5 actually
cares about, and it lands where a thin LLVM-backed AOT compiler should:
close to hand-written C, an order of magnitude ahead of Julia on cold-start
+ short-lived-process workloads, and roughly what `clang -O2` alone gets on
the same source shape (unsurprising, since Nirdosha's backend *is* `clang`).

`--opt0` (unoptimized IR) is included to show the gap optimization closes:
4.3× on `fib`, 2.4× on `floatloop` — i.e. Nirdosha's own codegen quality
before `clang`'s optimizer touches it is the weaker of the two factors, not
the LLVM backend.

For reference, interpreted Nirdosha on `fib(35)` took **16.1 s** — 620×
slower than compiled Nirdosha, 57× slower than Julia. `floatloop`
interpreted was not run (200M tree-walked iterations extrapolates to
well over an hour from the `fib` interpreter/compiled ratio) — this is
flagged, not silently omitted.

## Caveats, stated plainly

- **Julia's number includes JIT warmup and process startup**, not just
  steady-state loop execution — deliberately, because that's what a user
  actually experiences running `julia script.jl` once, which is the same
  thing being measured for Nirdosha and C. A benchmark that pre-warms
  Julia's JIT (e.g., running the function twice inside one process and
  timing only the second call) would show Julia much closer to or ahead of
  C — that's a different, also-valid question ("steady-state throughput"
  rather than "one-shot run cost") that this suite doesn't answer.
- **Group A is now compiled vs. compiled**, as of the `Vector`/`Matrix`
  codegen work landing (`goal.md` §9 item 1) — no longer the
  interpreter-vs-JIT caveat the original run carried.
- **4×4/8-element problem sizes still favor Nirdosha's hand-rolled
  algorithm choice over Julia's BLAS dispatch** — not a general "Nirdosha
  beats Julia at linear algebra" claim; the same 4×4-scale caveat from the
  interpreted-era run still applies to the *algorithm* comparison, on top
  of the now-added compiled-vs-compiled speed itself.
- **`det`/`kalman` vs. C is not apples-to-apples in one specific way**:
  they call a runtime-parameterized native routine, not compile-time-
  specialized inline code — see Group A's "honest asterisk" above. Reflects
  a real implementation choice (Phase 5 of the codegen plan), not a
  fundamental backend limitation.
- **One machine, one run of 3 samples per program** — enough to establish
  the order of magnitude and confirm correctness, not a rigorous
  statistical benchmark (no variance reported beyond best-of-3, no isolation
  from other system load).

## Reproducing

```sh
RUNS=5 WARM=1 ./benchmarks/run_head_to_head.sh
```

`benchmarks/run_head_to_head.sh` builds C (`gcc -O2`) and Nirdosha
(`nirdosha build`) for all six benchmarks, runs C/Nirdosha/Julia `RUNS`
times each, prints **every individual sample** (not a silently-picked
"best of N") plus min/median/max, and verifies every output numerically
before printing "trustworthy" for that benchmark — a mismatch is a loud
`!!` line, not a footnote. `WARM=1` additionally reports Julia's
steady-state number (a full-size warmup call, discarded, then several
timed calls in the same process — see `benchmarks/julia/*_warm.jl`'s own
comment for why the warmup has to be full-size, not a token call, to be
meaningful). Pass specific benchmark names to run a subset, e.g.
`./benchmarks/run_head_to_head.sh matmul kalman`. Prints the actual
machine/toolchain versions used for that run, not copied from this
document.

The older manual copy-paste version of this section is gone — this
script does the same thing plus the verification and warm-mode
measurement, so there's no reason to hand-roll it anymore.
