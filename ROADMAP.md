# Nirdosha — Roadmap

The single tracking file for what's done, what's pending, and when —
across the whole project, in one place.

**Why the other planning/spec docs (`Nirdosha_Unified_Plan.md`,
`goal.md`, `TRANSACT.md`, `SANDBOXING.md`, `PROTOLANG_PORT.md`,
`nirdosha_row11_amendment.md`, `nirdosha_row12_functions_identity.md`,
`nirdosha-agent-api.md`, `PHASE0.md`, `MOBILE.md`, ...) are not folded
in here and deleted:** they're technical *specifications* (grammar, semantics,
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

Same discipline `examples/trade-finance/todo.md` already uses:
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
- **DB connectivity** — `db_connect`/`db_query`/`db_execute`
  (interpreter-only): SQLite layer 1, Postgres layer 2 (dispatched by
  connection-string scheme, `dbconn.rs`).
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
- **2026-08-24 — Security review of interpreter/typeck/ownership/
  serve paths.** Fixed: HTTP/HTTPS request-line/header injection;
  constant-time `validate_api_key` comparison; propagation of
  `transact` terminal-log write failures; `O_EXCL` sandbox temp-source
  creation to block symlink races; 10 MiB HTTP response cap; 1 MiB
  `nirdosha serve` request-body cap; `127.0.0.1` default bind with new
  `--host` flag; `catch_unwind` around the generic table-query route.
  Known risks deferred to future Track-A work: sandbox Unix-socket peer
  authentication, CORS defaults, and table-route function-level auth
  alignment.
- **2026-08-24 — `workflow { ... }`: durable state machines with
  notification actions.** New top-level construct (`workflow`/`state`
  reserved keywords), desugared by `workflow_lower.rs` into ordinary
  `fn`/`enum`/`struct` declarations right after parsing — every existing
  pass (typeck, interpreter, `serve.rs`'s automatic RPC exposure) handles
  the result unchanged. New durable store (`workflow_log.rs`, modeled on
  `transact_log.rs`): instance state, append-only history, single-use
  magic-link tokens (constant-time compared), `identity_directory`
  (`Recipient::ByRole` resolution — the first reverse role→subjects
  lookup in this codebase), `identity_presence`. New builtins
  `send_email`/`send_sms`/`send_push`/`notify` — a generic authenticated-
  HTTPS-POST transport reading an admin-editable provider-config `struct`
  (the "communication control," an ordinary CRUD screen, no new UI work);
  `notify`'s online path is a Redis `PUBLISH` bridge (`nirdosha:push:
  <subject>`) for an external WS gateway, gated behind new
  `--presence-token`/`POST /api/_presence_connect`/`_disconnect` — this
  repo terminates no WebSocket connections itself (verified absent
  before building this, not assumed). `nirdosha build`/`emit-llvm`
  cleanly reject `workflow`-using programs via `check_supported`, same
  as `transact`. `on_entry`/`on_exit` actions are crash-durable
  (`WorkflowLog::begin_pending_action`/`Interpreter::
  replay_pending_workflow_actions`, replayed at `nirdosha serve` startup
  alongside `transact`'s own replay) — added same-day after the first cut
  shipped without it, closing the one gap that had been disclosed rather
  than silently left. Full design, runtime protocol, and remaining
  disclosed non-goals (`payload` not yet threaded into action bindings;
  no real WebSocket termination, by design) in `WORKFLOW.md`. 6 new
  end-to-end tests (`tests/workflow.rs`, including two dedicated replay
  tests); full existing suite (400+ tests) still green.
- **2026-08-24 — Systematic correctness-gap sweep (Track A1).**
  Added compiled structural `==`/`!=` for `struct`, `enum`,
  `Vector`/`Matrix` of `bool`, and recursive `box`/`&` payloads in
  `codegen.rs::emit_deep_eq`; fixed duplicate switch-case emission in the
  enum branch. Four new `compiler/tests/codegen.rs` tests cover `bool`
  vectors, struct, enum, and nested struct-in-enum equality against the
  interpreter. Full `cargo test` (400+ tests) green.
- **2026-08-24 — DB layer 2: Postgres, alongside SQLite.**
  `db_connect`/`db_query`/`db_execute`'s Nirdosha-facing surface is
  unchanged (`Ty::Db`'s doc comment already named this as the intended
  shape); new `compiler/src/dbconn.rs` dispatches purely off
  `db_connect`'s connection-string scheme — `postgres://`/`postgresql://`
  selects Postgres (`postgres`/`postgres-native-tls` crates, TLS opt-in
  via `sslmode`), anything else (a bare path, `:memory:`) is unchanged
  SQLite behavior, so no existing `.nir` program's behavior moves. The
  SQLite-era `?` bind-placeholder convention is rewritten to Postgres's
  `$1, $2, ...` internally, so the same call site and the same `sql`
  string work against either backend. Closed a real soundness gap this
  otherwise would have opened: `effects.rs`'s static classification
  tagged all of `db_connect`/`db_query`/`db_execute` as `Effect::Io`
  (right when SQLite, a local file, was the only backend) — a function
  declaring only `effect(io)` could now silently reach the network via a
  `postgres://` `db_connect`. Fixed for `db_connect` itself
  (`effects::db_connect_effect` inspects the call's literal connection-
  string argument, conservatively assuming `Network` too when it isn't a
  literal); `db_query`/`db_execute` on an already-open handle are a
  disclosed, narrower gap (no points-to tracking to trace a `Ty::Db`
  variable back to which `db_connect` opened it — named in `effects.rs`
  next to the identical pre-existing call-through-value limitation, not
  fixed here). Verified against a real, locally-run Postgres server (not
  just unit tests) before landing; `compiler/tests/postgres.rs` covers
  the same ground as an `#[ignore]`d integration suite (opt-in via
  `NIRDOSHA_TEST_POSTGRES_URL` + `cargo test -- --ignored`, since a real
  server can't be part of this project's self-contained-by-default test
  discipline the way SQLite's embedded `:memory:` can). Explicitly out of
  scope, named rather than silently gapped: `nirdosha serve --db`'s
  auto-generated table routes and `migrate.rs`'s schema-diff migrations
  stay SQLite-only — a second SQL dialect and schema-introspection
  mechanism throughout both, materially larger than this addition.
  5 new `compiler/tests/effects.rs` tests plus the 4 `#[ignore]`d
  Postgres integration tests; full non-ignored `cargo test` (660+ tests)
  green. Full design writeup: `PROTOLANG_PORT.md`'s "Locked design 5: DB".
- **2026-08-24 — Noted, not yet designed: data-dictionary-driven
  categorical detection.** User request: automatically treat a `str`
  column/field as enum-like (categorical) rather than free text, without
  re-querying `DISTINCT` values on every access — instead backed by an
  explicit data-dictionary table (temporal/ordinal/categorical/... per
  field), optionally Redis-cached for lookup speed. Touches (at least)
  `migrate.rs` schema diffing, `db_query` result shaping, and `ui_gen.rs`
  form/table rendering. Scoped as `[OPEN]`, deliberately not started this
  session — real schema design (the data-dictionary table shape, cache
  invalidation story) needed first, not something to bolt onto the
  Postgres work above.
- **2026-08-24 — Field-level format validation: `pattern`/`format`/
  `min`/`max` in the `screen` DSL.** `field <name> { pattern: "<regex>"
  }` / `{ format: "email"|"phone"|"date"|"url"|"uuid" }` / `{ min: ...
  }` / `{ max: ... }` — real client + server enforcement, same
  architecture the earlier field-level RBAC work established
  (typecheck the declaration's shape, carry a resolved value through
  `ui_gen.rs`, enforce for real in `serve.rs`, mirror cosmetically via
  native HTML5 input attributes client-side). New `regex` crate
  dependency; new `ast::well_known_format_pattern` (the `format`
  vocabulary's single source of truth, shared by `typeck.rs`/
  `ui_gen.rs`); 5 new `TypeErrorKind` variants; new `ui_gen::
  ValidatedField`/`field_validations_for_fn` (matches EITHER a struct's
  `create` or `update` slot, unlike edit-gate enforcement which only
  applies to `update`); `serve.rs::check_field_validations`, needing no
  `--db` at all (checks only the incoming value, never a stored one).
  20 new tests across `tests/screen_dsl.rs` (9), `src/ui_gen.rs`'s own
  unit tests (7), `tests/emit_ui.rs` (1, plus 3 pre-existing exact-
  substring assertions updated for the new JSON keys), and a new
  `tests/field_validation.rs` real-server integration suite (6). Full
  `cargo test` (every `tests/*.rs` file) reverified green. Full design
  detail in `LANGUAGE.md` §11 and `compiler/UI_DSL_TODO.md`.
- **2026-08-25 — General-purpose design-token theming + live reload.**
  Mission: generalize the UI DSL beyond CRUD+dashboard with a real
  design system (animations, hover/press states, layout variants),
  tightly integrated with protobox's existing `DesignSpec`/
  `resolve_design_tokens()` rather than inventing a competing format.
  `ui_gen::Theme` redesigned as a 1:1 mirror of `resolve_design_
  tokens()`'s JSON shape (was a narrow 12-field color/radius/font
  subset); `ui_gen_template.html` went from 1 `:hover` rule and 0
  animations to a real interaction system (4 named `@keyframes`,
  `transition`/`:hover`/`:focus-visible`/`:active`/`:disabled` on every
  interactive element, screen-entrance + staggered-list-row animation,
  global `prefers-reduced-motion`, CSS-only `app_shell`/`content_width`
  layout variants); `serve.rs::ThemeCache` makes `--theme` reload live
  (30s TTL, same pattern `RoleMappingCache` established) instead of
  requiring a server restart; protobox's `nirdosha.py::_theme_json_
  from_design_spec` now directly returns `resolve_design_tokens(spec)`
  (was a hand-picked, driftable subset) — verified with real protobox
  code through `be-v2`'s own `.venv`, not mocked, including
  `test_nirdosha.py`'s theme assertions updated and passing (18/18).
  Full `cargo test` (51 test binaries) green; live browser + curl
  verification against a real `.nir` app. Full design detail in
  `LANGUAGE.md` §11b and `compiler/UI_DSL_TODO.md`. **Deliberately not
  touched this pass** (explicitly gated by the user until this landed):
  `protobox/be-v2/src/plugins/languages/nirdosha_direct_codegen.py` —
  a real, confirmed-broken file (undefined `NirdoshaStoryCode` name,
  malformed `subprocess` arg, never successfully imported) — tracked as
  the next mission, to delete-and-rewrite from scratch.
- **2026-08-25 — protobox's `nirdosha_direct_codegen.py`: deleted and
  rewritten (mission phase 2).** The previous file (untracked in
  protobox's own git — never committed) had never once successfully
  imported: `NirdoshaStoryRepairPrompt(Prompt[NirdoshaStoryCode])`
  referenced a name defined nowhere in the file, a `NameError` at
  class-definition (i.e. module-import) time; both `LlmAgent`s were
  constructed with `output_type=None` despite the code accessing
  `out.code`/`repaired.code`; `_compile_check` invoked `emit-ast -o `
  (`-o` isn't even a real `emit-ast` flag) as one malformed, never-split
  argument. Read `docs/code-gen-repair-design.md` in full before
  rewriting — deliberately did NOT adopt its PassInfo/EditBlock/
  CodebaseSnapshot machinery (that design targets the classic multi-
  file, multi-language, brownfield-capable pipeline; nirdosha's lane has
  no equivalent shape — one file, one language, append-only, the real
  compiler as ground truth) — the one piece that *does* generalize,
  plateau detection, was adopted. `_compile_check` switched from
  `emit-ast` (lex+parse only — confirmed by reading `main.rs` directly:
  the previous check would have silently accepted real type errors,
  `str`-signature violations, and ownership mistakes as "success") to
  `emit-ui` (typecheck + ownership too, no extra z3/clang toolchain
  dependency). Verified for real: the rewritten module now imports and
  constructs both agents correctly; `_compile_check` demonstrated
  catching a genuine type error it would have missed before; a new
  `tests/plugins/languages/test_nirdosha_direct_codegen.py` (11 tests —
  mocked-LLM repair-loop behavior incl. plateau detection, the full
  `generate_all_from_stories` pipeline, and real-compiler `_compile_
  check` cases) all green through `be-v2`'s own `.venv`. Full
  `tests/plugins/languages/` (90 tests) green. Found, and left alone as
  explicitly out of scope, 6 pre-existing failures elsewhere in
  protobox's `tests/forge_repair/` (a different feature area's classic-
  pipeline tests, unrelated to this file — confirmed by direct
  inspection, not assumed) and 2 pre-existing unrelated collection
  errors — none caused by, or fixed by, this rewrite.

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
- `MOBILE.md` — `[OPEN]`, design only: nothing in it is built. Written
  2026-08-24, before Track D's first item, on purpose — see that doc's
  own status line.

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

- `[OPEN]` **A1. `transact` durability under real failure conditions** —
  actually kill the process mid-transaction under load and confirm
  crash-replay behaves, not just trust the existing test suite.
- `[OPEN]` **A2. Deployment story for the interpreted path** —
  containerize `nirdosha serve` + source properly; secrets/JWKS
  handling; this is buildable now, independent of Track B.
- `[OPEN]` **A3. Observability wired to something real** — the OTel
  tracer (`observability.rs`) exists; connect it to an actual
  collector/backend for a real deployment. Layer 2a is now done:
  `nirdosha serve --otel-port P --otel-token T` opens a second,
  loopback-only listener that dynamically enables/disables tracing based
  on whether an APM client is actually connected (`Tracer::enabled()`,
  gated one atomic-load check past layer 1's existing `Option` check) —
  zero-overhead when nobody's watching, live JSON-line spans streamed to
  every connected client while someone is. Still open: layer 2b (the
  real OTLP/collector wire format over that transport — today's feed is
  this project's own JSON-lines shape, not OTLP), layer 3 (real
  metrics), layer 4 (blocking-op watchdog) — see `observability.rs`'s
  module doc, "Rollout layers 2-4" section, for the full breakdown.
- `[OPEN]` **A4. Compatibility/versioning policy.** The str-ban
  (2026-08-23) was a breaking language change shipped in one session —
  need a real policy before a deployed critical app can trust future
  changes won't silently break it.
- `[OPEN]` **A5. `workflow`'s real-time presence gateway.** `notify()`
  (`WORKFLOW.md`) publishes to `nirdosha:push:<subject>` (Redis) and
  reads `identity_presence` — both real — but nothing in this repo
  terminates a live browser WebSocket/SSE connection, and that's the
  only thing that could ever legitimately call the two routes that
  populate `identity_presence` (`_presence_connect`/`_presence_disconnect`)
  or subscribe to those Redis channels. Net effect today: `identity_presence`
  never has an "online" row, so `notify()` always silently takes the
  offline (`send_email`) path — it doesn't error, it just never does the
  "push it live" half of its job. Needs a small standalone service
  (outside `compiler/`) that: terminates the real client connections,
  calls `_presence_connect`/`_presence_disconnect` as they open/close
  (bearer-token-authenticated via `--presence-token`), and subscribes to
  each `nirdosha:push:<subject>` channel to relay to the right live
  connection. `send_email`/`send_sms`/`send_push` and every other part
  of `workflow` are unaffected and fully functional without this.
- `[PARTIAL]` **A6. Identity admin console: multi-IdP registry, role
  mapping + cache, roles/ACL introspection, opt-in scaffolding.**
  Prompted by a real gap: `requires(role: "compliance_officer")` and a
  `screen` field's `view`/`edit` role gates only worked, historically,
  if the string literal in `.nir` source was byte-identical to whatever
  the connected IdP actually puts in the token's roles claim — no
  translation layer, and a renamed IdP group silently broke every check
  it gated (no error, the check just stopped matching).
  - **2026-08-24 — Role mapping + in-memory cache: `[DONE]`.** A
    per-project, admin-editable `RoleMapping { app_role: str, idp_role:
    str }` table (same "ordinary struct, free CRUD screen" convention
    `EmailProviderConfig` already established for the communications
    panel — both now standing fixtures in `scratch/
    nirdosha_llm_prompt.md`, emitted once per generated project rather
    than hand-typed per app), translating the app's canonical role
    vocabulary into whatever the connected IdP actually emits. Loaded
    once into a long-lived, shared `RoleMappingCache` at `serve::run`
    startup (eagerly, not just lazily on first request — a mapping
    already in the DB before the process started needs to be live
    immediately, not after one TTL window), refreshed on a 30s TTL
    rather than re-queried per auth check — bounded staleness (an
    admin's edit takes up to one TTL window to take effect) is an
    accepted, disclosed tradeoff, not a correctness bug, the same
    category of real-clock/real-world exception `resolve_identity`'s
    own token-`expires_at` check already is. Every `requires(role:
    ...)` check and every `screen` field's `view`/`edit` gate now goes
    through `identity_has_mapped_role` (literal match first, so a
    program with no mapping configured is unaffected; falls back to the
    cache otherwise). Verified live end-to-end (curl, not just unit
    tests: a raw-IdP-role-only token is rejected before any mapping
    exists, still rejected within the TTL window right after the
    mapping is created, then accepted once the TTL refreshes) plus 4
    new `tests/role_mapping.rs` integration tests (real server, TTL
    overridable via `NIRDOSHA_TEST_ROLE_MAPPING_TTL_MS` so the boundary
    is proven with a real short wait, not a 30s tax per test run or a
    faked clock). Full detail in `LANGUAGE.md` §11a. **Not fixed
    alongside this**, still a real, disclosed inefficiency: the
    unrelated `identity_directory` table still reopens a fresh SQLite
    connection on every single `resolve_identity` call — this session's
    cache only covers `role_mapping` reads, not that.
  - `[OPEN]` **Multi-IdP registry** — today `nirdosha serve` takes exactly one
    fixed `--jwks-file`/`--issuer`/`--audience` triple (`AuthConfig`).
    An admin-editable `IdentityProviderConfig` list (mirroring the
    provider-config struct pattern again) would let `resolve_identity`
    pick the right provider by the token's own issuer claim.
  - `[OPEN]` **Roles → functions/fields report** — pure static analysis, no new
    runtime concept: walk `program.fns`' `requires(role: ...)` and
    `ui_gen::field_gates_for_struct`'s already-computed table/field ACL
    gates (that data already exists, just isn't surfaced as a page),
    group by role name.
  - **On-demand activation is already solved, not a new problem** — a
    program that declares none of these marker structs renders none of
    this UI today, the same way a hello-world script that never
    declares `EmailProviderConfig` gets no communications panel:
    `ui_gen`/`serve` only render screens for structs that exist. A
    proposed `nirdosha init <project-name>` scaffolding command would
    only be solving *ergonomics* (not hand-typing the marker structs),
    not cost — worth keeping scoped as a text-generation convenience,
    not a new "project manifest" concept the compiler itself needs to
    understand (Nirdosha has no notion of "a project" beyond a source
    file today, and this shouldn't quietly become the thing that
    introduces one).

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
   dependency-linking design needed there. The Postgres backend added
   2026-08-24 (`dbconn.rs`) is a real, separate wrinkle for this item:
   `postgres`/`postgres-native-tls` are *not* statically bundled the way
   `rusqlite` is, so a compiled binary using a Postgres `db_connect`
   would need real dynamic-linking/deployment design (a system TLS
   library at minimum) — not just "port the interpreter's dispatch to
   LLVM IR" the way the SQLite path is.
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

## Track D — Mobile app generation (`MOBILE.md`)

*Priority: independent of Tracks A–C — a second renderer of `ui_gen.rs`'s
existing manifest, not a change to the interpreter/compiler/agent-API
work above. D1 has zero new server-side dependencies and can start any
time; D2–D5 each stand alone (no ordering constraint between them),
picked up in proportion to which example app actually needs the
capability, per `MOBILE.md`'s own archetype ranking.*

- `[OPEN]` **D1. `emit-mobile` codegen scaffold + Standard profile.**
  New `mobile_gen.rs` (`generate_ios`/`generate_android`), consuming
  `ui_gen.rs::build_screens`'s existing `Screen`/`FieldSpec`/`Action`/
  `Metric` IR unchanged; a checked-in Swift/Kotlin runtime library
  (generic per-`control`-kind field views, list/singular/dashboard/login
  screens, networking client, `Theme` mapper) embedded via `include_str!`
  the same way `codegen.rs`'s `RUNTIME_KERNELS_LIB` is; per-app generated
  code is one typed struct per `Screen`, not per-struct logic. No new
  `ScreenDecl` grammar, no new builtins, no new `serve.rs` routes.
- `[OPEN]` **D2. Device-bound biometric step-up.** New credential
  artifact a native app can hold in Keychain/Keystore and unlock via
  Face ID/Touch ID/BiometricPrompt before presenting — layered on
  `nirdosha_row12_functions_identity.md`'s `RefreshTokenHandle` shape,
  since nothing today (`VerifiedIdentity`/`TokenReference`/
  `ApplicationSession`) is client-holdable. New `action { step_up:
  biometric }` `ScreenDecl` key.
- `[BLOCKED: a new file/blob/attachment type, itself undesigned]`
  **D3. Camera/document capture on upload-shaped fields.** Nothing
  mobile-specific — Nirdosha has no file/blob/attachment type at all
  today (confirmed absent, `trade-finance/todo.md` names it explicitly).
  That type needs its own design pass before this item can move.
- `[OPEN]` **D4. Real push adapter (APNs/FCM) + device-token
  registration.** `send_push`/`notify` (`WORKFLOW.md`) exist but their
  transport is the same generic authenticated-POST adapter every
  channel shares — needs a real provider-specific adapter and a new
  way for a native app to register a device token against a subject.
  Sidesteps Track A5's presence-gateway gap entirely (no live-connection
  routing needed for push).
- `[OPEN]` **D5. RPC-layer idempotency key for offline action queues.**
  `txn_id` (`TRANSACT.md`) is scoped to a `transact` block's own
  `network` slot, not exposed on the ordinary `POST /api/<fn>` RPC
  layer. Needs an optional client-supplied idempotency key at
  `serve.rs::dispatch` plus a durable "seen keys" table, so a mobile
  app can safely replay queued calls after reconnecting. At-least-once,
  not exactly-once — same disclosed limit `TRANSACT.md`/`WORKFLOW.md`
  already carry.

---

## Suggested near-term order

Given "critical apps soon": the security review (now `[DONE]`) and the
systematic correctness-gap sweep (now `[DONE]`) have both landed;
**A1 (`transact` durability under real failure conditions) is the next
concrete blocking work**, since it closes the largest remaining
interpreted-path correctness risk. A2–A4 and C1 can run in parallel with
each other and with the start of B1. B1–B9 is the long track — pick up
items as they become relevant to what's actually being built, not in
lockstep. Track D runs independently of all of the above — D1 can start
whenever native app delivery actually becomes a priority, without waiting
on A/B/C.
