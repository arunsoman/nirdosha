# Nirdosha: A Numerical, Agent-Native, Mission-Critical Language
## Unified Development Plan

**Version:** 2.1 — Amended after go/no-go review
**Date:** 2026-08-20
**Status:** Draft

**Changelog (2.0 → 2.1):**
1. Renamed "Phase 0" to **Phase 0.5** to stop colliding with `PHASE0.md`, which already documents a shipped, different Phase 0 (the static type checker and ownership/move checker, 43/43 tests passing).
2. Rewrote §4.2.3 (grammar export) to stop implying a push-button `lalrpop` → GBNF converter exists. It doesn't; this is now scoped as a manual/semi-mechanical translation with its own fidelity tests.
3. Swapped the order of Hardware Performance and Mission-Critical Runtime. The runtime phase never depended on codegen — it's pure interpreter work, same as the existing `chan`/`sandbox`/`tcp` precedent — so it no longer waits behind an LLVM effort it doesn't need. Mission-Critical Runtime is now **Phase 3**; Hardware Performance is now **Phase 4**.

---

## 1. Vision: Why These Three Belong Together

Nirdosha is being shaped into a language for a specific, high-stakes workflow: **AI agents generating verified numerical code for mission-critical systems.**

The three capabilities — Julia-style dense linear algebra, compiler surfaces that machines can read and agents can target, and deterministic runtime primitives for simulation — are not three separate features. They are three layers of the same stack:

1. **Numerical Core:** Without `f64`, `Vector`, and `Matrix`, Nirdosha cannot express the algorithms that AI agents would be asked to write, nor the sensor-fusion geometry that defense simulations require.
2. **Agent-Native Surface:** Without structured diagnostics, AST export, and grammar artifacts, an agent cannot reliably generate Nirdosha code, and a human cannot validate that generation at scale.
3. **Mission-Critical Runtime:** Without deterministic RNG, hardware-isolated sandboxes, and reproducible execution, the code — however well-generated — cannot be trusted in a defense context where a non-deterministic Monte Carlo run or a memory-corrupted drone firmware is unacceptable.

This plan presents **one timeline** with six phases. Every phase is load-bearing for what follows. Nothing is an afterthought.

---

## 2. Architectural Principles

These principles govern every phase and justify every scope decision:

| Principle | What It Means | What It Excludes |
|---|---|---|
| **Sized by Default** | Every array, vector, and matrix has its shape in the type. `Matrix(f64, 3, 3)` and `Matrix(f64, 4, 4)` are different types. | Dynamic arrays, generic dimensions (`Matrix{T,N}`), resizable collections. |
| **Builtins Are Native Rust** | `det`, `inv`, `solve`, `rand_gaussian`, `kf_predict` are Rust functions inside the interpreter, exposed via a registry. They are not limited by Nirdosha's lack of `for` loops or generics. | Re-implementing LAPACK in Nirdosha, generic algorithms written in user code. |
| **Structured by Design** | Every error, AST node, and diagnostic is serializable. The compiler speaks JSON to agents by default. | Plain-text error scraping, regex-based code generation. |
| **Ownership Applies Universally** | `box Container`, `box MicroVM`, `box Channel<T>`, and eventually `box RngState` all obey the same affine move-checking. When the handle drops, the resource cleans up. | GC-managed sandbox handles, leaked simulation entities. |
| **Determinism Is Default** | Randomness is seeded and reproducible. Simulation runs are byte-for-byte replayable from a seed + binary hash. | OS-entropy RNG as default, non-reproducible builds. |
| **Honest Scope** | We ship what we can prove and test. We do not commit to GPU-cluster HPC, sparse matrices, or LAPACK factorization objects until the foundation is real. | Undated promises for generics, complex numbers, or exascale simulation. |

---

## 3. Unified Phase Timeline

```
Phase 0.5 ─► Phase 1 ──► Phase 2 ──► Phase 3 ──► Phase 4 ──► Phase 5
(Language    (Numerical  (Compute    (Mission    (Hardware   (Assurance
Infra-       Surface)    + Agent     Critical    Speed)      & Proof)
structure)               Interface)  Runtime)
```

**Dependency rule:** A phase may only start after all previous phases in this ordering are complete and tested — **except Phase 3 and Phase 4, which are mutually independent** (see §6: Mission-Critical Runtime needs Phases 0.5–2, not codegen; Hardware Performance needs Phases 1–2, not the runtime primitives). They are sequenced 3-then-4 here so the defense/mission-critical payoff — the project's stated primary motivation (§1) — lands as soon as the numerical and agent surface are ready, rather than waiting on an LLVM codegen effort it does not need. A team with spare capacity could legitimately run them in parallel instead; nothing in either phase's spec assumes serial execution.

---

## 4. Phase Specifications

---

### Phase 0.5: Language Infrastructure
**Theme:** *You cannot build what you cannot inspect.*

**Naming note:** This is not the repo's original Phase 0. `PHASE0.md` documents that work — the static type checker and the ownership/move checker — already shipped and tested (43/43 tests passing at time of writing). This phase is a later increment building on top of it, numbered 0.5 specifically so it stays distinguishable from the shipped work in commit history, docs, and conversation, rather than implying it precedes or replaces it.

This phase makes the compiler honest, inspectable, and extensible. It unblocks both the numerical surface (by adding floats and a clean builtin registry) and the agent surface (by adding serialization to structured types that already exist).

#### 4.0.1 Floats & Indexing
- **`Ty::F64`** in `ast.rs` alongside existing integer types. Add `is_numeric()` helper (`is_integer() || is_float()`), used in `typeck.rs::infer_binary` so `+ - * /` accept floats.
- **Float literal lexing** in `token.rs`: decimal digits + `.` + digits (`3.14`). No scientific notation in v1.
- **`Expr::Float(f64, Span)`** in `ast.rs`, mirroring `Expr::Int`/`Expr::Str`.
- **Float semantics in `refine.rs`/`smt.rs`:** Floats do not overflow like integers; they saturate to `inf` or produce `NaN`. Tier-1 overflow proofs are scoped to integers only. Add `Expr::Float => Interval::unknown()` (same pattern as `Str`).
- **`[`, `]` tokens and indexing grammar** — `Tok::LBracket`/`RBracket`, `Expr::Index(Box<Expr>, Vec<Expr>, Span)` supporting `v[i]` and `m[i, j]`. Parsed as a new postfix step between `parse_call` and `parse_primary`.

#### 4.0.2 Builtin Registry Refactor
Replace the repeated `if name == "print"` special-casing across `typeck.rs`, `interpreter.rs`, `codegen.rs` with a single table:

```rust
struct Builtin {
    name: &'static str,
    arg_shape: fn(&[Ty]) -> Result<Ty, TypeError>,
    eval: fn(&[Value]) -> Result<Value, RuntimeError>,
}
```

Migrate `print` onto it first. All future builtins (math, RNG, geometry, KF) register here. No more copy-pasted `if name == "..."` across four files.

#### 4.0.3 Structured Diagnostics (Agent Surface)
- Add `serde` + `serde_json` to `compiler/Cargo.toml`.
- `#[derive(Serialize)]` on `Span`, `TypeErrorKind`, `OwnershipErrorKind`, `ErrorKind` (interpreter). Use `#[serde(tag = "kind")]` for stable variant names.
- Introduce unifying `enum Diagnostic { Type(TypeError), Ownership(OwnershipError), Runtime(RuntimeError) }` — one shape across all three compiler stages.
- New CLI flag `--format=json` in `main.rs`. Default stays plain text (humans first); agents opt in to structure.
- **Metric this closes:** `goal.md` §7 `m_ai-native` — fraction of diagnostics that are machine-parseable becomes 100% under `--format=json`.

#### 4.0.4 Files Touched
`compiler/Cargo.toml`, `compiler/src/ast.rs`, `compiler/src/token.rs`, `compiler/src/parser.rs`, `compiler/src/typeck.rs`, `compiler/src/interpreter.rs`, `compiler/src/refine.rs`, `compiler/src/smt.rs`, `compiler/src/main.rs`.

#### 4.0.5 Verification
- `cargo test` green.
- `--format=json` output round-trips through `serde_json::from_str` for at least one error from each stage (type, ownership, runtime).
- Float arithmetic example runs end-to-end via interpreter.

---

### Phase 1: Numerical Surface
**Theme:** *You cannot simulate what you cannot express.*

This phase gives Nirdosha the type system surface to write linear algebra. Fixed-size types mean shape mismatches are compile-time errors for free — no generics system needed.

#### 4.1.1 Vector & Matrix Types
- `Ty::Vector(Box<Ty>, usize)`, `Ty::Matrix(Box<Ty>, usize, usize)` in `ast.rs`.
- **Literal syntax:** `[1.0, 2.0, 3.0]` for vectors; `[[1.0, 2.0], [3.0, 4.0]]` (row-major list-of-rows) for matrices. Deliberately **not** Julia's space-sensitive `[1 2; 3 4]` — that grammar isn't LL(1)/LALR-parseable and would violate `GRAMMAR.md`'s row-7 discipline.
- Dimensions inferred from literal element counts at typeck time.

#### 4.1.2 Runtime Representation
- `Value::Vector(Arc<[Value]>)`, `Value::Matrix(Arc<[Value]>, usize, usize)` in `interpreter.rs`. `Arc`-backed for cheap clone-on-read, following `Value::Str(Arc<str>)`.
- **Declared non-affine** in `ast.rs::is_affine()` for this phase. No `ownership.rs` changes needed. Move-semantics for mutable buffers are deferred to a later planning pass.

#### 4.1.3 Operators
New arms in `typeck.rs::infer_binary` + `interpreter.rs::eval_binary`:

| Operator | Semantics | Shape Rule |
|---|---|---|
| `+`, `-` | Elementwise | Exact same shape |
| `*` | Linear algebra | scalar×matrix, matrix×vector, matrix×matrix (inner dims match) |
| `.*`, `./` | Hadamard (elementwise) | Exact same shape |

`Vector * Vector` is a **type error** — use `dot()` or transpose explicitly, matching Julia's own requirement.

New `TypeErrorKind::ShapeMismatch` for inner-dimension failures in matmul.

#### 4.1.4 Indexing
- `Expr::Index` evaluation with **runtime bounds check** (Tier-2 style, traps on out-of-range). SMT-proven bounds are Phase 5.
- `v[i]` and `m[i, j]` both supported.

#### 4.1.5 Files Touched
`compiler/src/ast.rs`, `compiler/src/token.rs`, `compiler/src/parser.rs`, `compiler/src/typeck.rs`, `compiler/src/interpreter.rs`.

#### 4.1.6 Verification
- `cargo test` green.
- `compiler/examples/matrices.nir` + `compiler/tests/matrices.rs` — end-to-end example and grouped tests, following the `strings.nir`/`tests/strings.rs` pattern.
- Static-rejection tests for shape mismatch (`[1.0] + [1.0, 2.0]`), `Vector * Vector`, and out-of-bounds index.

---

### Phase 2: Computational Depth + Agent Interface
**Theme:** *You cannot automate what you cannot validate.*

This phase delivers two things in parallel: (a) the dense linear algebra builtins that make Nirdosha useful for real numerical work, and (b) the compiler surfaces that let agents and tools generate and validate Nirdosha code programmatically.

#### 4.2.1 Dense Linear Algebra Builtins
Implemented as native Rust in the interpreter, registered via the Phase-0.5 builtin table:

- `transpose`, `dot`, `cross` (3-vectors only)
- `zeros(n)`, `zeros(r, c)`, `ones(...)`, `identity(n)`
- `sum`, `len`, `norm` (2-norm)
- `trace`, `det` (Gaussian elimination with partial pivoting), `inv` (Gauss-Jordan)
- `solve(A, b)` — Julia's `A \ b` semantics
- `rank`, norm variants (1-norm, ∞-norm, Frobenius)
- Predicates: `is_symmetric`, `is_diag`, `is_square`

New `TypeErrorKind::NotSquare` where required.

#### 4.2.2 AST Export & Fragment Validation
- `#[derive(Serialize, Deserialize)]` on `Ty`, `Expr`, `Stmt`, `Program` in `ast.rs`.
- `--emit-ast[=json]` CLI flag.
- **Fragment validation entry point** — the load-bearing new piece for row 9:
  ```rust
  fn validate_fragment(
      json: &str,
      expected_ty: &Ty,
      env: &Env
  ) -> Result<Expr, Vec<Diagnostic>>
  ```
  Parses and typechecks a single expression fragment against a caller-supplied expected type and variable environment. Reuses `parser.rs`'s expression entry point and a new environment-seeding path into `typeck.rs::Checker`.

#### 4.2.3 Machine-Readable Grammar Artifact
**Scope correction:** there is no off-the-shelf tool that mechanically converts a `lalrpop` grammar into GBNF (or any constrained-decoding format). `lalrpop`'s precedence/associativity declarations and its LALR table structure don't map 1:1 onto GBNF's plain context-free rules — a naive AST walk over the `.lalrpop` file will produce a grammar that *parses* but doesn't reject the same strings the real parser rejects. Treat this as a semi-manual translation, not a mechanical export:

1. **Spike first, before committing the rest of Phase 2 to this approach:** hand-translate a representative slice of the grammar (expressions with precedence, at minimum) to GBNF and confirm a real constrained-decoding loader (e.g. `llama.cpp`'s grammar sampler) actually constrains generation the way it should.
2. Build the translation as a small program driven by the `lalrpop` definition (reusing its token/rule names so the two can't silently drift), but expect to hand-encode precedence climbing and any construct GBNF can't express directly — this is authored *from* the grammar, not derived automatically by a generic converter.
3. **Fidelity test, not just "it loads":** run a corpus of valid and invalid Nirdosha snippets through both the hand-written parser and the GBNF grammar (via the loader's own matcher, not just eyeballing the file) and assert they agree on accept/reject for every case in the corpus.

Ship the resulting file (e.g., `compiler/nirdosha.gbnf`) and reference it from `GRAMMAR.md`, with the fidelity corpus checked in alongside it so drift between the hand-written parser and the exported grammar is caught by CI, not discovered by an agent hitting a rejected-but-should-be-valid completion in production.

#### 4.2.4 Files Touched
`compiler/src/ast.rs`, `compiler/src/interpreter.rs`, `compiler/src/typeck.rs`, `compiler/src/main.rs`, `compiler/src/lib.rs`, `grammar_check/` (or new `grammar_export/`), `GRAMMAR.md`.

#### 4.2.5 Verification
- `cargo test` green.
- `compiler/examples/linalg.nir` + matching tests — textbook examples with known-good numeric output.
- Fragment validation tests: valid fragment accepted, invalid fragment returns structured `Diagnostic` JSON.
- Exported grammar validated against a real constrained-decoding loader, **and** the accept/reject fidelity corpus from 4.2.3 step 3 passes for both the hand-written parser and the exported grammar.

---

### Phase 3: Mission-Critical Runtime
**Theme:** *You cannot trust what you cannot isolate.*

This phase turns Nirdosha from a numerical language into a runtime for mission-critical simulation. It depends on Phases 0.5–2 (numerics + agent surface) and on the existing concurrency/sandbox infrastructure (`spawn`, `chan`, `sandbox`) already in the codebase. **It does not depend on Phase 4.** Every builtin and primitive here runs on the existing tree-walking interpreter — the same "interpreter now, codegen later" precedent already set by `Ty::Str`, `Ty::Channel`, `Ty::Sandbox`, and `Ty::Tcp`, all of which currently hit `unsupported()` in `codegen.rs` with no loss of runtime functionality. Sequencing this phase before Hardware Performance means the project's defense/mission-critical payoff (§1) lands without waiting on an LLVM effort it doesn't use.

#### 4.3.1 Deterministic Simulation Primitives
- **Seeded PRNG builtins:** `rand_seed(u64)`, `rand_f64()`, `rand_gaussian(mean, stddev)`.
  - **Deterministic by default.** No OS entropy. A wargame run is reproducible from its seed — essential for after-action review and audit.
  - RNG state is carried in the interpreter environment, not a global.
- **Geometry builtins:** `distance`, `bearing`, lat/lon/alt ↔ ECEF ↔ local-ENU conversions — native Rust, operating on Phase-1 `Vector`/`Matrix` types.
- **Linear Kalman filter:** `kf_predict(x, P, F, Q)`, `kf_update(x, P, z, H, R)`.
  - Extended KF and particle filter are flagged as follow-on work once the linear KF is proven.

#### 4.3.2 Actor-Based Simulation Architecture
**No new language primitives needed.** The pattern is a library convention atop existing `spawn`/`chan`:

```nirdosha
// Each simulated entity is an actor
fn sensor_actor(id: i64, inbox: box Channel<Command>, outbox: box Channel<Plot>) {
    // ... sensor logic ...
}

// Central clock broadcasts ticks
fn clock_actor(tick_chans: Vector<box Channel<Tick>>) {
    // ... broadcast ticks ...
}
```

**Honest gap:** Managing hundreds of spawned entities is awkward without dynamically-sized collections. For Phase 3, cap entity counts to fixed-size handle lists (reusing Phase 1's `Vector` of `box Channel<T>` handles). A later generics pass removes this cap.

#### 4.3.3 Distributed Simulation
- **TCP server/listener primitive.** The existing `Ty::Tcp`/`connect` (commit `c9841dd`) is client-only. Add `listen`/`accept` over real network sockets so simulation nodes can talk to each other.
- Transport: same `Channel<T>` semantics over TCP as over Unix sockets/VSOCK.

#### 4.3.4 Tier-3 `audited` Escape Hatch
New syntax: `audited "<justification>" { <block> }`

- Grammar: `Stmt::Audited { justification: String, body: Vec<Stmt>, span }`.
- Inside the block, Tier-1/2 guard emission (`guard_in_range`, SMT proofs) is suppressed.
- Justification string must be non-empty (typeck-time check).
- Every escape hatch is greppable: `grep -rn "audited"` finds all of them.
- **Scope boundary:** The compiler enforces syntax and non-empty justification. Judging justification *content* or gating authorship is a CI/review-process rule, not compiler code.

#### 4.3.5 Files Touched
`compiler/src/interpreter.rs` (RNG state, geometry, KF, TCP server), `compiler/src/ast.rs`, `compiler/src/parser.rs`, `compiler/src/token.rs`, `compiler/src/typeck.rs` (audited justification check), `compiler/examples/sensor_fusion.nir`, `compiler/examples/wargame_agents.nir`.

#### 4.3.6 Verification
- `cargo test` green.
- Determinism test: same seed run twice produces byte-identical output.
- KF unit test with textbook hand-computed expected posterior.
- Actor example: N spawned entities run to completion, all inbox messages consumed.
- Static-rejection test: empty `audited ""` justification rejected.

---

### Phase 4: Hardware Performance
**Theme:** *You cannot deploy what you cannot optimize.*

This phase makes the Phase-1/2 numerical code run at hardware speed. Until this phase, `codegen.rs` returns `unsupported()` for `F64`/`Vector`/`Matrix` — same honest precedent `Str` set. It depends only on Phases 1–2 (numerics); it does not depend on Phase 3, so it could equally run before or in parallel with it (see §3's dependency-rule note) — it's sequenced last here purely because the mission-critical payoff was prioritized to land first.

#### 4.4.1 LLVM Codegen for Numerics
- `Matrix(f64, R, C)` → LLVM `[R x [C x double]]` alloca (fixed size, no dynamic metadata).
- Elementwise ops unrolled for small fixed sizes.
- Matrix multiplication as unrolled/looped IR sequence.
- `f64` scalar ops map directly to LLVM `fadd`, `fmul`, etc.

#### 4.4.2 Files Touched
`compiler/src/codegen.rs`.

#### 4.4.3 Verification
- `cargo test` green.
- Performance smoke test: a `3x3` matmul via compiled code runs measurably faster than interpreter path.

---

### Phase 5: Assurance & Proof
**Theme:** *You cannot improve what you cannot measure.*

This phase closes the loop: proving properties at compile time, measuring the AI-native workflow, and guaranteeing reproducibility.

#### 4.5.1 SMT-Proven Index Bounds
Extend `refine.rs`/`smt.rs` with `Expr::Index` case:
- Prove `index < length` at compile time (Tier 1) where the index expression is provably bounded.
- Fall back to Phase 1's runtime check (Tier 2) otherwise.
- Directly generalizes `goal.md` line 165 (`byte[n] where n < cap`) from bytes to all fixed-size indices.

#### 4.5.2 Benchmark Harness
New `bench/` directory:
- Corpus of ~20–30 prompt → expected-`.nir` tasks spanning all language features.
- **pass@1 metric:** Does a model's generated program parse, typecheck, and run to expected output?
- **Self-repair rate:** Feed Phase-0.5 JSON diagnostics back to the model; measure fix rate within N retries.
  - Requires external LLM API; this phase builds the harness plumbing (corpus format, scoring script, re-prompt loop), not the model integration.

#### 4.5.3 Reproducibility & Audit Trail
Extend `goal.md` row 10's reproducibility ambition to simulation runs:
- Same binary hash + same scenario file + same seed (Phase 3's §4.3.1) → byte-identical outcome log.
- **Honest flag:** `goal.md` repeatedly references `src/capability.rs` and `src/ledger.rs` as existing files. An exhaustive repo search found **no such files**. This phase treats reproducibility infrastructure as aspirational until row 10 gets its own implementation pass. The seed-based determinism in Phase 3 is the concrete, checkable foundation we can build today.

#### 4.5.4 Files Touched
`compiler/src/refine.rs`, `compiler/src/smt.rs`, new `bench/` directory.

#### 4.5.5 Verification
- `cargo test` green.
- SMT proof test: `let v = [1.0, 2.0]; v[0]` compiles with no runtime check; `v[i]` where `i` is unbounded falls back to runtime check.
- Harness runs end-to-end against a mock model response to prove scoring loop works.

---

## 5. Explicitly Out of Scope

These are not forgotten. They are intentionally excluded because they require infrastructure (generics, first-class functions, GPU runtimes) that is not scheduled in this plan.

| Feature | Why Excluded | When It Might Return |
|---|---|---|
| Dynamically-sized arrays | Needs real generics/parametric types | Separate planning pass after Phase 2 ships |
| Generic dimensions (`Matrix{T,N}`) | Needs full generics system | Same as above |
| LAPACK-grade factorization objects (QR/SVD/Eigen/Cholesky as types) | Multi-quarter effort; needs generics + complex numbers | Own planning pass |
| Sparse matrices | Needs dynamic sizing + compressed formats | Post-generics |
| Complex numbers | Needs type system extensions | Post-generics |
| Arbitrary-function broadcasting (`f.(x)`) | Needs first-class functions/closures | Post-first-class-functions |
| GPU/SIMD kernels | Needs GPU runtime + vectorization model | Profiling-driven, after real workloads exist |
| Koka-style algebraic effect system | Large separate design effort; `capability.rs`/`ledger.rs` do not exist in this repo | Reconcile `goal.md` claim with repo reality first |

---

## 6. Dependency Graph

```
                    ┌─────────────────┐
                    │  Existing Code  │
                    │ (ownership,     │
                    │  spawn, chan,   │
                    │  sandbox, tcp)  │
                    └────────┬────────┘
                             │
        ┌────────────────────┼────────────────────┐
        ▼                    ▼                    ▼
   ┌─────────┐         ┌─────────┐          ┌─────────┐
   │Phase 0.5│         │Phase 0.5│          │Phase 0.5│
   │(Floats, │         │(Serde,  │          │(Builtin │
   │Indexing)│         │Diagnostics)│        │Registry)│
   └────┬────┘         └────┬────┘          └────┬────┘
        └────────────────────┼────────────────────┘
                             ▼
                        ┌─────────┐
                        │Phase 1  │
                        │(Vector/ │
                        │ Matrix) │
                        └────┬────┘
                             ▼
                        ┌─────────┐
                        │Phase 2  │
                        │(LA      │
                        │Builtins +│
                        │Agent     │
                        │Surface)  │
                        └────┬────┘
                             │
              ┌──────────────┴──────────────┐
              │   (mutually independent —    │
              │    see §3's dependency note) │
              ▼                              ▼
        ┌─────────────┐              ┌─────────────┐
        │  Phase 3    │              │  Phase 4    │
        │ (RNG,       │              │ (LLVM       │
        │  Geometry,  │              │  Codegen    │
        │  KF, TCP    │              │  for f64/   │
        │  Server,    │              │  Vector/    │
        │  Audited)   │              │  Matrix)    │
        └──────┬──────┘              └──────┬──────┘
               └──────────────┬──────────────┘
                               ▼
                        ┌─────────────┐
                        │   Phase 5   │
                        │ (SMT Proofs,│
                        │  Benchmarks,│
                        │Reproducibility)│
                        └─────────────┘
```

Phase 3 and Phase 4 each depend only on Phase 2, not on each other — the branch above is real, not cosmetic. This plan executes them in the order 3-then-4 (rationale in §3), but a team with two independent workstreams could run them concurrently without changing anything in either phase's spec.

---

## 7. Files Touched (Consolidated)

| File | Phases | Nature of Change |
|---|---|---|
| `compiler/Cargo.toml` | 0.5 | Add `serde`, `serde_json` |
| `compiler/src/ast.rs` | 0.5, 1, 2, 3 | `Ty::F64/Vector/Matrix`, `Expr::Float/Index/Audited`, `Serialize`/`Deserialize` derives |
| `compiler/src/token.rs` | 0.5, 1, 3 | Float literals, `[` `]`, `.+` `.-` `.*` `./`, `audited` keyword |
| `compiler/src/parser.rs` | 0.5, 1, 3 | Postfix indexing, vector/matrix literals, `audited` statement |
| `compiler/src/typeck.rs` | 0.5, 1, 2, 3 | `is_numeric()`, shape rules, `ShapeMismatch`/`NotSquare`, fragment validation env, audited justification check |
| `compiler/src/interpreter.rs` | 0.5, 1, 2, 3 | `Value::Float/Vector/Matrix`, eval arms, builtin registry + implementations, RNG state, TCP server |
| `compiler/src/refine.rs` | 0.5, 5 | Float catch-all, index-bound proofs |
| `compiler/src/smt.rs` | 0.5, 5 | Same as above |
| `compiler/src/codegen.rs` | 4 | LLVM lowering for `f64`, `Vector`, `Matrix` |
| `compiler/src/main.rs` | 0.5, 2 | `--format=json`, `--emit-ast` flags |
| `compiler/src/lib.rs` | 0.5, 2 | Unified `Diagnostic` enum, fragment validation entry point |
| `grammar_check/` or `grammar_export/` | 2 | GBNF/decoder-format export |
| `GRAMMAR.md` | 2 | Reference generated grammar artifact |
| `compiler/examples/matrices.nir` | 1 | End-to-end matrix example |
| `compiler/examples/linalg.nir` | 2 | End-to-end LA example |
| `compiler/examples/sensor_fusion.nir` | 3 | KF example |
| `compiler/examples/wargame_agents.nir` | 3 | Actor simulation example |
| `compiler/tests/*.rs` | 1, 2, 3 | Unit tests per phase |
| `bench/` | 5 | Benchmark corpus and harness |

---

## 8. Verification Strategy (Unified)

Every phase must satisfy:

1. **`cargo test` green** — existing suite plus new tests, no regressions.
2. **Example execution** — each phase's `.nir` example runs end-to-end via `cargo run -- interpret compiler/examples/<name>.nir`.
3. **Static rejection tests** — every illegal operation (shape mismatch, `Vector * Vector`, empty `audited` justification, etc.) has a test asserting the specific `TypeErrorKind`.
4. **Determinism test** (Phase 3+) — same seed + same binary → byte-identical output.
5. **Round-trip test** (Phase 0.5+) — `--format=json` diagnostics deserialize correctly.
6. **Performance smoke test** (Phase 4) — compiled matmul faster than interpreted.

---

## 9. Summary: What Changes vs. the Original Three Plans

| Original Fragment | In This Plan | Why the Change |
|---|---|---|
| Three separate docs with different phase naming (0-5, A-F, G1-G6) | One doc, phases 0.5–5 | Unified timeline, clear dependencies |
| "This plan depends on the math plan above" | Dependencies expressed in the phase graph | No afterthought cross-references |
| Math plan = "Julia-style math bolted on" | Phase 1, 2, 4 = "Numerical Core" (surface, builtins, then codegen — no longer contiguous, since Phase 3 is deliberately sequenced between builtins and codegen; see §3) | Math is the foundation, not a plugin |
| AI plan = "making the compiler agent-friendly" | Phase 0.5, 2, 5 = "Agent-Native Surface" | Structured diagnostics are foundational (Phase 0.5), not an add-on |
| Defense plan = "battlefield sim support" | Phase 3 = "Mission-Critical Runtime" | Defense is the validation domain for the numerical + agent stack, and now lands before codegen rather than after it |
| `audited` in AI plan Phase E | Phase 3, alongside simulation | Escape hatches matter most when running mission-critical code |
| `capability.rs`/`ledger.rs` assumed existing | Flagged as discrepancy, not built upon | Honest scope — don't plan on vaporware |
| Benchmark harness in AI plan Phase D | Phase 5, last | You benchmark a complete system, not a compiler in isolation |
