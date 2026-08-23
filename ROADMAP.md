# Nirdosha — Roadmap

The single tracking file for what's done, what's pending, and when —
across the whole project, in one place.

**Why the other planning/spec docs (`Nirdosha_Unified_Plan.md`,
`goal.md`, `TRANSACT.md`, `SANDBOXING.md`, `PROTOLANG_PORT.md`,
`nirdosha_row11_amendment.md`, `nirdosha_row12_functions_identity.md`,
`nirdosha-agent-api.md`, `PHASE0.md`, ...) are not folded in here and
deleted:** they're technical *specifications* (grammar, semantics,
protocol detail, API request/response shapes), not status trackers —
this file only summarizes their status, it doesn't replace their
content. They're also load-bearing: checked this session,
`TRANSACT.md` is cited by 22 files, `goal.md` by 38, `SANDBOXING.md`
and `nirdosha_row11_amendment.md` by 16 each, `PROTOLANG_PORT.md` by
14, `PHASE0.md` by 12 — real Rust source comments throughout
`compiler/src/` cite them by name/section for design rationale (e.g.
GRAMMAR.md quotes "PHASE0.md's 'Eleventh update'" as the authoritative
source for a specific decision). Deleting any of them leaves those
citations pointing at nothing. `README.md` also links several of them
directly as the project's own documented map for readers. This file
tracks **status and sequencing** across all of them in one place, plus
the work items that don't have a home in any existing doc yet (Track
A, Track B, Track C below) — but the specs themselves stay put.

## Status tags

Same discipline `compiler/examples/trade-finance/todo.md` already uses:
checked off only once actually run/verified, not on "code written."

- `[DONE]` — verified complete (tests pass, or run end-to-end).
- `[PARTIAL]` — real, verified progress; the gap is named explicitly.
- `[OPEN]` — scoped, not started.
- `[BLOCKED: X]` — can't start until X lands.

## How to keep this file current

Update it at the start and end of any work session that touches an
item here: flip the status tag, add a one-line dated note (`— started
2026-08-23`, `— done 2026-08-24, see commit X`). Don't let it drift
into "aspirational and stale" — that's exactly the failure mode
`goal.md` row 10 / Phase 5's reproducibility gap already calls out for
the project's own claims, and this file shouldn't repeat it about
itself.

---

## Shipped

Chronological, from `git log`, grouped by milestone rather than
per-commit. `[DONE]` throughout — this section is the "what's already
built" half of "what's done and what's coming," kept in the same file
as the pending tracks below instead of scattered across commit
messages.

- **Core language + static checking** — parser (LL(1), cross-checked
  against a real LALR(1) generator, `grammar_check/`), static type
  checker, `box`/`&` ownership discipline (row 1's no-GC/no-manual-free
  foundation).
- **Tier-1/2 static safety proofs** — interval analysis for
  overflow/div-by-zero, upgraded to real Z3-backed SMT discharge
  (`smt.rs`/`refine.rs`) — row 4.
- **LLVM codegen, real native binaries** — `-O2` optimization; row 5's
  "hardware speed" claim.
- **Concurrency, first pass** — `spawn`/`join`/`thread<T>`,
  `chan`/`send`/`recv` — rows 2–3, interpreter-only.
- **`sandbox`/`stop`** — an affine real-OS-process handle
  (`SANDBOXING.md` layer 1), then cross-process `chan` IPC over it
  (layer 2).
- **`str`/`tcp`/`connect`** — a real TCP client, the prerequisite for
  orchestrating an arbitrary containerized workload.
- **Row 11 — `struct`/`enum`/`match`**, then layers 6–7 (generics,
  `Option(T)`/`Result(T,E)` prelude) — product/sum types, the
  foundation everything from Row 12 onward builds on.
- **JSON, HTTP/HTTPS builtins** (interpreter-only) — `json_*`,
  `http_get`/`http_post`/`https_get`/`https_post`.
- **DB connectivity** — `db_connect`/`db_query`/`db_execute`, SQLite
  layer 1 (interpreter-only).
- **Row 12 — identity, DB/MQ-backed apps, the UI engine** —
  `VerifiedIdentity`/`RoleView`/`ClaimView`, mock OIDC validation,
  sessions/refresh/revocation, `mq` (Redis), `nirdosha emit-ui`/
  `nirdosha serve`, literal-pattern `match` over `str`.
- **`transact` durability** — WAL, crash replay, `precheck`, `txn_id`
  idempotency, retry/timeout — all five layers, interpreter-backed
  (`TRANSACT.md`).
- **Auto-generated DB schema migrations** — `migrate.rs`, diffs a
  program's `struct`s against the live schema at `serve` startup,
  additive-only (LANGUAGE.md §13).
- **Compiled-vs-interpreter-only boundary pushed forward repeatedly**
  — `box`/`&`/`*`, `str`, `tcp`/`tcp_listener`, `sha256_hex`/
  `constant_time_str_eq`, `rand_*`, all of `Vector`/`Matrix`, and
  non-affine `struct`/`enum`/`match` all moved from interpreter-only to
  real LLVM codegen over time (LANGUAGE.md §10 has the current, verified
  line).
- **2026-08-23 — "Enum favoring": `str` banned as a function
  argument/return type.** `Ty::contains_str` + `TypeErrorKind::
  StrInFnSignature`, enforced in `typeck.rs::check_fn`; `struct Text {
  value: str }` carrier for free text, per-program `enum ErrorCode`
  replacing the old universal `Result(_, str)` convention. Migrated all
  15 affected `.nir` files including `trade_finance.nir` (73
  signatures, 592 error sites → 18 enums). Bonus fix landed alongside:
  `enum`/`struct` `==`/`!=` typechecked but had no interpreter arm
  (traps at runtime) — fixed in `interpreter.rs::eval_binary`. Full
  detail in `LANGUAGE.md` §6b.

---

## 0. Where the existing plan already stands

`Nirdosha_Unified_Plan.md`'s Phase 0.5→5, cross-checked against the
actual codebase (not just the doc's own claims) this session:

| Phase | Scope | Status |
|---|---|---|
| 0.5 | Floats/indexing, builtin registry, structured diagnostics | `[DONE]` |
| 1 | `Vector`/`Matrix` types, operators, indexing | `[DONE]` |
| 2 | Dense linalg builtins, AST export/fragment validation, GBNF grammar | `[DONE]` |
| 3 | Mission-critical runtime: deterministic sim, `audited`, actor/distributed sim | `[DONE]` (mostly — see mission_critical.rs) |
| 4 | LLVM codegen for numerics | `[DONE]` |
| 5 | SMT-proven bounds, benchmark harness, reproducibility/audit trail | `[PARTIAL]` — `smt.rs`/`refine.rs`/`bench/` scaffold exist; **reproducibility/audit-trail (`capability.rs`/`ledger.rs`) doesn't exist at all** — this is `goal.md` row 10's open claim |

This plan **never covers `db`/`json`/`http`/`mq`/identity/`transact`/
concurrency codegen anywhere** — it's scoped to numerics + agent
surface + simulation. Track B below is genuinely new scope, not a gap
in an existing phase.

Related docs, quick verdicts (don't need their own tracks — either
done or already tracked inside their own file):
- `PHASE0.md` — `[DONE]`, historical build journal only.
- `PROTOLANG_PORT.md`, `nirdosha_row11_amendment.md` — `[DONE]`,
  shipped, say so themselves.
- `TRANSACT.md` — `[DONE]` (interpreter). "All five layers
  implemented"; compiled backend is explicitly out of scope there —
  picked up as Track B's first item below.
- `SANDBOXING.md` — `[OPEN]`: the transport layer beyond raw process
  isolation, and a Python/Node client shim, are named as their own
  future deliverable, not done.
- `nirdosha_row12_functions_identity.md` — `[PARTIAL]`: the *design*
  (`VerifiedIdentity`/`RoleView`/`ClaimView`, mock OIDC validation,
  sessions/refresh/revocation) is built and interpreter-tested. Real
  IdP discovery, PKCE, mTLS/DPoP token binding are **not** built —
  design-only.
- `compiler/UI_DSL_TODO.md` — `[OPEN]`: a documented, non-silent doc
  debt (GRAMMAR/LANGUAGE rewrite owed).

**Explicitly out of scope, not tracked here**: `llm-ops-api-spec.md`/
`llm-ops-api-spec-v2.md` are generic multi-backend LLM training/
serving/RLHF specs (TRL/Axolotl/vLLM/...) with zero Nirdosha-specific
content (confirmed: 0 hits for "nirdosha" in either file). Not this
project's work. `benchmarks/julia/*.jl` are 6 standalone benchmark
scripts (matmul/det/dot/kalman/fib/floatloop) for the Group A
perf comparisons in `benchmarks/RESULTS.md` — not packages, not a
source of "tools," unrelated to Track C's LLM-client work.

---

## Track A — Production readiness

*Priority: highest. This is what actually gates building critical apps
soon — independent of Track B, since the interpreter path
(`nirdosha serve`) is what will run those apps regardless of how much
of Track B has landed.*

- `[OPEN]` **A1. Security review** of the interpreter/typeck/ownership/
  serve paths before anything critical is built on top. Use the
  `security-review` skill.
- `[OPEN]` **A2. Systematic correctness-gap sweep.** The pattern found
  this session — `enum`/`struct` `==` typechecked but had no
  interpreter arm, traps at runtime (fixed) — was found by accident,
  not a deliberate audit. Needs a real pass across operator × type
  combinations, not opportunistic discovery.
- `[OPEN]` **A3. `transact` durability under real failure conditions** —
  actually kill the process mid-transaction under load and confirm
  crash-replay behaves, not just trust the existing test suite.
- `[OPEN]` **A4. Deployment story for the interpreted path** —
  containerize `nirdosha serve` + source properly; secrets/JWKS
  handling; this is buildable now, independent of Track B.
- `[OPEN]` **A5. Observability wired to something real** — the OTel
  tracer (`observability.rs`) exists; connect it to an actual
  collector/backend for a real deployment.
- `[OPEN]` **A6. Compatibility/versioning policy.** The str-ban
  (2026-08-23) was a breaking language change shipped in one session —
  need a real policy before a deployed critical app can trust future
  changes won't silently break it.

---

## Track B — Full compilation ("finish compiling everything")

*Priority: parallel to Track A, longer horizon. Not blocking Track A —
compiling db/json/http mainly helps startup latency and business-logic
throughput, not correctness or capability (see the perf discussion:
the builtins already call into native Rust either way). Sequenced by
what a critical app actually benefits from, not by "easiest first."*

Current state: `codegen.rs`'s `check_supported` rejects, with a named
reason, everything below — verified directly against its
`unsupported(...)` call sites this session (not just LANGUAGE.md §10's
claim, though that section is currently accurate).

1. `[OPEN]` **B1. `transact` codegen.** Durable-transaction correctness
   under compilation matters more for a critical/financial app than
   db/http do — do this first, not last.
2. `[OPEN]` **B2. `db` + `json` codegen.** `Ty::Db`, `db_connect`/
   `db_query`/`db_execute`; all 8 `json_*` builtins. Unlocks compiling
   `trade_finance.nir`/`store.nir` at all. Note: `rusqlite` already
   uses the `bundled` feature (fully static SQLite) — no new
   dependency-linking design needed there.
3. `[OPEN]` **B3. `mq` codegen** (`mq_connect`/`mq_publish`/
   `mq_consume` — Redis). Network client either way; no static-linking
   concern, same as today.
4. `[OPEN]` **B4. Identity/Row 12 codegen** — `oidc_validate_token`,
   `check_role(_path)`, `extract_claim(_path)`, sessions, refresh,
   revocation, `validate_api_key`. On the critical path of every
   authenticated request — do before general concurrency/sandboxing.
5. `[OPEN]` **B5. `http`/`https` codegen** — `http_get`/`http_post`/
   `https_get`/`https_post`. Note: `native-tls` is **not** currently
   vendored — dynamically links system OpenSSL on Linux unless the
   `vendored` feature is turned on; decide that as part of this item,
   not silently at deploy time.
6. `[OPEN]` **B6. Concurrency + sandboxing codegen** —
   `thread`/`spawn`/`join`, `chan`/`send`/`recv`, `sandbox`/`stop`,
   `file`/`open`. Lower general priority — sequence based on what's
   actually needed once B1–B5 are done, not abstractly now.
7. `[OPEN]` **B7. First-class functions codegen** — `fn(..)->..`/
   `acquire`/`requires(...)`, and the Phase-4b affine-in-struct/enum
   case (a `struct`/`enum` whose payload transitively contains
   `box`/`&`/`thread`/`chan`/`tcp`/`file`/`db`/`mq`).
8. `[BLOCKED: B1–B7]` **B8. Compiled `serve` mode** — a real
   self-contained production binary with a compiled dispatch table,
   *coexisting* with interpreted `serve` for dev (the OCaml
   `ocaml`/`ocamlopt`-style split), not replacing it. This is the
   direct answer to the original "ship the migration/schema with the
   binary" question — schema gets embedded at compile time once B2
   exists, migration runtime links into the binary here.
9. `[OPEN]` **B9. `sleep_ms` codegen** — small, currently omitted from
   even the interpreter-only list in LANGUAGE.md §10; found this
   session, not previously tracked anywhere. Fold into whichever of
   B1–B7 it naturally lands under once scoped.

---

## Track C — Agent-Facing API (`nirdosha-agent-api.md`)

*20 endpoints across 5 groups (A: codegen/validation, B: execution,
C: introspection, D: benchmarking, E: provenance). The HTTP API layer
itself is 0% built — no `/v1/*` server exists (`serve.rs` only serves
`/api/<fn>` for a program's own functions, unrelated). Roughly half
the underlying capabilities it would wrap already exist, verified this
session:*

**Underlying capability already shipped** (the API layer to expose it
is what's missing): `--format=json` structured diagnostics, `emit-ast`/
`validate_fragment`, `sandbox`/`stop` process isolation, `rand_seed`
determinism, the GBNF grammar file (`compiler/nirdosha.gbnf`),
`bench/corpus.json` scaffold.

**Not built yet, blocks specific endpoints**:
- `[OPEN]` **C1. The `/v1/*` HTTP server itself** — nothing exists yet
  for any of the 20 endpoints; this is the actual implementation gap,
  not the underlying capability.
- `[BLOCKED: C1]` **C2. Constrained-decoding loader integration** —
  the GBNF file exists; wiring it into an actual inference backend
  (vLLM/llama.cpp-style grammar-constrained sampling) doesn't.
- `[OPEN]` **C3. Benchmark scoring harness/loop** — `bench/corpus.json`
  + `bench/src/{lib,main}.rs` scaffold exist; the actual pass@1/
  self-repair scoring loop over it doesn't.
- `[BLOCKED: Track A goal.md row 10 / Phase 5 gap]` **C4. Provenance/
  audit-trail endpoints (group E)** — blocked on the same
  `capability.rs`/`ledger.rs` gap Phase 5 already names; don't
  duplicate that work here, just wait on it.

Sequencing note: C1 (the server) is the natural next step regardless
of Track B/A progress — it's additive tooling around the *existing*
interpreter/compiler capabilities, not blocked on either track.

---

## Suggested near-term order

Given "critical apps soon": **A1 (security review) first**, since it's
the fastest way to find out whether "soon" is actually safe, and it
doesn't block on anything else. A2–A6 and C1 can run in parallel with
each other and with the start of B1. B1–B9 is the long track — pick up
items as they become relevant to what's actually being built, not in
lockstep.
