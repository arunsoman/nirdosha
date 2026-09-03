# Nirdosha — Public Roadmap

A scannable, external-facing summary of what's shipped and what's next.
This is a distillation for readers deciding whether to try Nirdosha or
contribute — the full internal tracker, with verification detail and
session-by-session notes, is [`ROADMAP.md`](./ROADMAP.md).

Status tags: `[DONE]` (verified — tests pass or run end-to-end),
`[PARTIAL]` (real progress, gap named), `[OPEN]` (scoped, not started).

---

## Shipped

**Language core**
- [DONE] LL(1) grammar — hand-written parser, cross-verified against an
  independent LALR(1) generator (`grammar_check/`)
- [DONE] Static type checker
- [DONE] Ownership/affine types (`box`/`&`) — no GC, no manual `free()`
- [DONE] Concurrency primitives (`spawn`/`thread`, `chan`) — no mutex in
  the language, so a deadlock isn't expressible
- [DONE] `struct`/`enum`/`match`, generics, `Option(T)`/`Result(T, E)`
- [DONE] SMT-backed integer/buffer-overflow proofs (Z3), tiered with a
  runtime-guard fallback
- [DONE] Native codegen via LLVM (`-O2`) for the compiled subset —
  within 1.4× of `gcc -O2` on scalar benchmarks

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

**LLM integration**
- [DONE] LL(1) grammar exported to GBNF for constrained decoding
  (`compiler/nirdosha.gbnf`)
- [DONE] Structured `Diagnostic` JSON on every error (`--format=json`)
- [DONE] `emit-ast`/`validate_fragment` for typed AST/fragment tooling
- [PARTIAL] `bench/` pass@1 + self-repair-rate harness — scaffold and
  corpus exist, ships with mock models today; real LLM wiring is
  separate work

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

**Track B — Full compilation** (currently 100% interpreter-only for
`db`/`json`/`http`/`mq`/identity/`transact`/concurrency; native codegen
only covers the numeric/control-flow subset today)
- [OPEN] `transact` → `db`/`json` → `mq` → identity → `http`/`https` →
  concurrency/sandboxing → first-class functions → compiled `serve` mode,
  roughly in that order

**Track C — Agent-facing HTTP API** (the spec exists —
[`nirdosha-agent-api.md`](./nirdosha-agent-api.md) — about half the
underlying capability already ships; the `/v1/*` server itself is 0% built)
- [OPEN] The HTTP server and its 20 endpoints across code generation,
  execution, introspection, benchmarking, and provenance

**Track D — Mobile app generation** (a second renderer of the existing
UI manifest, independent of Tracks A–C — see [`MOBILE.md`](./MOBILE.md))
- [OPEN] `emit-mobile` codegen scaffold — native iOS/Android from the
  same `struct`/`screen` declarations that drive the web UI today

**Track E — Enterprise UI constructs** (extends the same UI manifest/
DSL with what a dense enterprise app needs beyond CRUD+dashboard,
independent of Tracks A–D — see
[`examples/ctms/UI_CONSTRUCTS.md`](./examples/ctms/UI_CONSTRUCTS.md))
- [DONE] Screen inventory + construct design — an 89-screen inventory
  and a 5-construct design proposal, worked against a real
  Counter-Terrorism Financing & Transaction Monitoring System spec
- [DONE] `workspace`/`panel` — composite multi-pane screens composing
  fields/lists from several structs onto one page (`LANGUAGE.md` §15)
- [DONE] `visual`/`render` — graph, heatmap, and timeline views on a
  dashboard or inside a panel, on top of the existing bar-chart-only
  `chart` (`LANGUAGE.md` §11c)
- [DONE] `field { render: "countdown" }` — a live SLA countdown chip on
  a table field, ticking client-side with zero added network traffic
  (`LANGUAGE.md` §11)
- [DONE] `action { show_result: true }` — a "Simulate"/"Preview" action
  shows its own JSON return value in a modal instead of just refreshing
  the row (`LANGUAGE.md` §11)
- [OPEN] A workflow stage stepper — see `ROADMAP.md` Track E

---

## How to help

Pick an `[OPEN]` item above, comment on its GitHub issue (or open one
if it doesn't exist yet), and say what you're picking up before
starting on anything non-trivial. See
[CONTRIBUTING.md](./CONTRIBUTING.md).

Docs, examples, and `.nir` test cases are just as valuable as compiler
work and are the fastest way to make a first contribution.
