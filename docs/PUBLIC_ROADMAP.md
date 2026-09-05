# Nirdosha — Public Roadmap

A scannable, external-facing summary of what's shipped and what's next.
This is a distillation for readers deciding whether to try Nirdosha or
contribute — the full internal tracker, with verification detail and
session-by-session notes, is [`docs/ROADMAP.md`](./docs/ROADMAP.md).

Status tags: `[DONE]` (verified — tests pass or run end-to-end),
`[PARTIAL]` (real progress, gap named), `[OPEN]` (scoped, not started).

---

## Shipped

**Language core**
- [DONE] LL(1) grammar — hand-written parser, cross-verified against an
  independent LALR(1) generator (`crates/grammar_check/`)
- [DONE] Static type checker
- [DONE] Ownership/affine types (`box`/`&`) — no GC, no manual `free()`
- [DONE] Concurrency primitives (`spawn`/`thread`, `chan`) — no mutex in
  the language, so a deadlock isn't expressible
- [DONE] `struct`/`enum`/`match`, generics, `Option(T)`/`Result(T, E)`
- [DONE] SMT-backed integer/buffer-overflow proofs (Z3), tiered with a
  runtime-guard fallback
- [DONE] Native codegen via LLVM (`-O2`) for the compiled subset —
  within 1.4× of `gcc -O2` on scalar benchmarks
- [DONE] `validate <fn_name> { pre: ... post: ... }` — real Hoare
  contracts on a function: a Z3-backed static proof that hard-fails the
  build on a genuine counterexample where it can reach one, plus an
  unconditional runtime check on every actual call as the backstop for
  everything it can't (most real functions) — see `docs/ROADMAP.md` Track F,
  F3

**Backend/services**
- [DONE] `db` (SQLite + Postgres), `json`, `http`/`https`, `mq` (Redis)
  — interpreter-only today, see Track B below
- [DONE] Identity — OIDC/JWT validation, roles/claims,
  `requires(role:...)`, an admin-editable role-mapping cache (IdP role
  names → app role names)
- [DONE] `transact` — durable transactions (WAL, crash replay, retry/
  timeout, idempotency)
- [DONE] `workflow` — durable state machines with email/SMS/push
  notification actions
- [DONE] Auto-generated, additive-only DB schema migrations

**UI engine**
- [DONE] Zero-syntax CRUD + dashboard inference from `struct`/fn naming
  conventions — no UI code needed for the common case
- [DONE] `screen`/`dashboard`/`module` DSL for the cases naming
  conventions can't express
- [DONE] Field-level RBAC (`view`/`edit` role/claim gates) and format
  validation (`pattern`/`format`/`min`/`max`) — enforced server-side,
  not just hidden in the client
- [DONE] Design-token theming (`--theme`) with live reload — color
  ramps, motion, dark-mode strategy, layout shell, all optional
- [DONE] `workspace`/`panel` — composite multi-pane screens composing
  fields/lists from several structs onto one page (`docs/LANGUAGE.md` §15)
- [DONE] `visual`/`render` — graph, heatmap, and timeline views on a
  dashboard or inside a panel, on top of the existing bar-chart-only
  `chart` (`docs/LANGUAGE.md` §11c)
- [DONE] `field { render: "countdown" }` — a live SLA countdown chip on
  a table field, ticking client-side with zero added network traffic
  (`docs/LANGUAGE.md` §11)
- [DONE] `action { show_result: true }` — a "Simulate"/"Preview" action
  shows its own JSON return value in a modal instead of just refreshing
  the row (`docs/LANGUAGE.md` §11)
- [DONE] A workflow stage stepper — a real `●━●━○━○` progress stepper
  on a workflow queue row instead of a bare state-name badge, no syntax
  change (`docs/LANGUAGE.md` §14)
- [DONE] `examples/ctms/ctms.nir` — all of the above proven together
  against a real 89-screen enterprise app spec (a Counter-Terrorism
  Financing & Transaction Monitoring System), not just in isolation —
  see `docs/ROADMAP.md` Track E6

**LLM integration**
- [DONE] LL(1) grammar exported to GBNF for constrained decoding
  (`crates/compiler/nirdosha.gbnf`)
- [DONE] Structured `Diagnostic` JSON on every error (`--format=json`)
- [DONE] `emit-ast`/`validate_fragment` for typed AST/fragment tooling
- [PARTIAL] `crates/bench/` pass@1 + self-repair-rate harness — scaffold,
  corpus, and a real `Model` (`--mode real`, any OpenAI-compatible
  `/chat/completions` endpoint) all exist; not yet run against a live
  provider for lack of an API key in this environment

---

## In progress / next

**Track A — Production readiness** (highest priority: gates building
critical apps on the interpreted path)
- [OPEN] `transact` durability under real kill-mid-transaction conditions
- [OPEN] A deployment story for `nirdosha serve` (containerization,
  secrets/JWKS handling)
- [PARTIAL] Observability — a local OTel-shaped tracer exists; wiring
  to a real collector (OTLP) is open
- [OPEN] A compatibility/versioning policy before the next breaking
  language change
- [PARTIAL] Identity admin console — role-mapping cache is done;
  multi-IdP registry and a roles→functions/fields report are open
- [OPEN] Real Windows verification — the compiled `tcp`/`tcp_listener`
  runtime was ported to Windows' `RawSocket` API (v0.1.0-alpha.3) but
  has never run on a real Windows machine; needs an actual test pass
- [OPEN] macOS binaries link system Z3 instead of vendoring it —
  `z3-src` 416.0.2 fails to build against current AppleClang (a real
  upstream incompatibility); revisit once a fixed `z3`/`z3-src` release
  ships

**Track B — Full compilation** (`db`/`json`/`http`/`mq`/identity/
`transact`/concurrency/sandboxing remain interpreter-only; native
codegen covers the numeric/control-flow subset, `tcp`/`tcp_listener`,
and, as of this pass, `file` and scalar-only native plugin calls)
- [DONE] `file` (`open`/`send`/`recv`/`stop`) — linked `nir_file_*`
  kernels, the same "declare + link a staticlib" pattern `tcp` already
  used; `examples/file_io.nir` compiles and runs as a native binary
  unchanged, verified against the interpreter's own output
  (`crates/compiler/tests/codegen.rs`)
- [DONE] Scalar-only native plugin calls (Kind A plugins) —
  `plugin::NativePluginBuiltin`/`codegen::build_with_native_plugins`
  (`rfcs/0005-plugin-boundary-safety-and-performance.md` §3), ~250x
  faster than interpreted plugin dispatch for this subset; `str`/
  aggregate-typed plugin builtins still interpreter-only
- [OPEN] `transact` → `db`/`json` → `mq` → identity → `http`/`https` →
  concurrency/sandboxing → first-class functions → compiled `serve` mode,
  roughly in that order. `db`/`json`/`mq` share `file`'s "linked
  handle-based kernel" shape but need a real dynamically-typed value
  representation for query results first (`Ty::Handle`'s own affine
  fix, `rfcs/0005` §1, generalizes to a `db`/`mq` connection handle for
  free once that representation exists). Concurrency/sandboxing are a
  materially harder, separate design question (a native thread-spawn +
  cross-thread `box`-ownership model, not a kernel to link) — see
  `rfcs/0005` §0's own difficulty ranking for the fuller breakdown.

**Track C — Agent-facing HTTP API** (the spec exists —
[`docs/nirdosha-agent-api.md`](./docs/nirdosha-agent-api.md) — about half the
underlying capability already ships; the `/v1/*` server itself is 0% built)
- [OPEN] The HTTP server and its 20 endpoints across code generation,
  execution, introspection, benchmarking, and provenance

**Track D — Mobile app generation** (a second renderer of the existing
UI manifest, independent of Tracks A–C — see [`docs/MOBILE.md`](./docs/MOBILE.md))
- [OPEN] `emit-mobile` codegen scaffold — native iOS/Android from the
  same `struct`/`screen` declarations that drive the web UI today

**Track F — Next-generation language & UI architecture** (design
discussion, independent of every track above — see
[`docs/NEXT_GEN.md`](./docs/NEXT_GEN.md))
- [OPEN] A target-independent UI manifest with multiple renderers
  (web/TUI/mobile), not just today's one fixed web template
- [DONE] A real module/package system — `module Ident { ... }`
  namespacing, `pub` visibility, and `use "path.nir"` splitting a
  program across files, all real and tested — see `docs/ROADMAP.md` Track
  F, F2. The legacy `module "Display Name" { ... }` nav-label form
  (still just a nav label, no scoping) is untouched and still works.
- [OPEN] A composable UI layout system — Phase A [DONE]: `screen
  <Struct> { layout { row { column { group "..." { field x } } } } }`,
  real containers (row/column/grid/group/tabs), plus a searchable +
  scroll-paginated dropdown, a live timeline widget, and colored status
  badges — see `docs/ROADMAP.md` Track F, F4. A per-element `css: "..."`
  styling override and the rest of the widget catalog are still open.

---

## How to help

Pick an `[OPEN]` item above, comment on its GitHub issue (or open one
if it doesn't exist yet), and say what you're picking up before
starting on anything non-trivial. See
[CONTRIBUTING.md](./CONTRIBUTING.md).

Docs, examples, and `.nir` test cases are just as valuable as compiler
work and are the fastest way to make a first contribution.
