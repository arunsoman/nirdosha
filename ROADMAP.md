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
- **2026-08-25 — UI DSL "Deliberate non-goals" documented.** No code
  change: the three closed sets (one inline-SVG bar chart type, four
  fixed `@keyframes` animations, seven fixed form-control kinds) already
  existed exactly this way in `ui_gen.rs`/`ui_gen_template.html`; they
  just weren't stated anywhere as *intentional* boundaries versus
  unbuilt gaps. Added a "Deliberate non-goals" section to
  `compiler/UI_DSL_TODO.md` (the source of truth, same heading MOBILE.md
  already uses), with pointers/summaries in `LANGUAGE.md` §11,
  `README.md`'s screen/dashboard section, and `MOBILE.md`'s own non-goals
  list (native inherits the same closed sets, not a separate mobile
  gap).

- **2026-08-27 — `dec128`: a real 128-bit fixed-point decimal type.**
  `LANGUAGE.md` §2/§5, plus the `Money`/`Measure` conventions in §6c/§6d
  (`dec128` + a closed `CurrencyCode`/`UnitCode` enum) replacing the
  "money is a naming convention" gap — no `From<f64>`, no implicit
  conversion to/from `i64`/`f64`, native `+`/`-`/`*`/`/`/comparisons via
  `rust_decimal::Decimal`, five builtins (`dec_from_str`/`dec_from_i64`/
  `dec_to_str`/`dec_round`/`dec_scale`). Wired end to end: `ast.rs`'s
  `Ty`/lexer/parser, `typeck.rs`, `interpreter.rs` (`Value::Dec128`,
  `eval_binary`, `sql_bind_params`), `serve.rs` (JSON encode/decode as a
  *string*, not a JSON number — the drift `dec128` exists to prevent),
  `ui_gen.rs` (renders as a text field, not a number spinner),
  `migrate.rs` (`TEXT` column), `transact_log.rs` (durability-log
  round-trip). Interpreter-only for now — `codegen.rs::check_supported`
  rejects it by name, joining `json`/`db`/`mq`, not silently mis-compiled.
  Verified with two real `nirdosha serve` apps, not just unit tests:
  `examples/money_invoice.nir` (currency-safe invoice line totals) and
  `examples/measure_shipment.nir` (unit-safe shipment quantities + a
  UCUM-code bridge) — full CRUD, DB persistence, and the currency/unit-
  mismatch guard exercised live over real HTTP.

- **2026-08-27 — Default web-app style: subtle hover/entrance motion,
  everywhere.** `ui_gen_template.html`'s shared design-token motion
  system (already real: `--motion-*`/`--hover-lift-px`/`--stagger-ms`,
  keyframes, staggered row entrance, global `prefers-reduced-motion`)
  extended to two spots that had no hover treatment at all: `.card`
  (table wrapper/form/dashboard-tile — subtle lift on hover) and
  `tbody tr` (background highlight, scoped off the header). Applies to
  every `.nir` app automatically — it's the one shared template, not a
  per-app opt-in.

- **2026-08-27 — `Money`/`Measure`/`CurrencyCode`/`UnitCode` promoted
  from documented convention to real prelude types.** `LANGUAGE.md`
  §6c/§6d. Injected into every `.nir` program via `ast.rs::
  prelude_structs`/`prelude_enums`, the same mechanism `Option`/`Result`/
  `HttpResponse` already use — no more pasting the ~180-entry
  `CurrencyCode`/34-entry `UnitCode` list into each program that needs
  one. `Measure`'s field is `unit_code`, not `unit` — `unit` lexes as
  `Ty::Unit`'s own keyword regardless of position (`token.rs::
  TYPE_NAMES`), so `m.unit` would be a parse error, not a field access.
  `PRELUDE_STRUCT_NAMES` (three separate copies: `migrate.rs`/`serve.rs`/
  `ui_gen.rs`) extended with `Money`/`Measure` so auto-migration/table-
  catalog/screen-derivation keep skipping them, same as every other
  prelude struct. `examples/money_invoice.nir`/`measure_shipment.nir`
  updated to stop pasting their own copies and construct real `Money`/
  `Measure` values directly (`combine_lines`/`combine_shipments` now
  return `Result(Money, ErrorCode)`/`Result(Measure, ErrorCode)`) —
  re-verified live over real HTTP after the change, full test suite
  (600+ tests) green.
  - **Real bug found and fixed along the way:** `serve.rs::encode_value`'s
    `Value::Enum` arm encoded *every* enum value as `{"variant":...,
    "payload":[...]}`, including zero-payload ones — asymmetric with
    `decode_value`/`decode_enum_value`/the DB round-trip, which all
    require/produce a bare variant-name *string* for a zero-payload
    variant. Surfaced by `combine_lines` returning a real `Money` (whose
    `currency` field hit this exact case) — `{"err":"CurrencyMismatch"}`
    now, not `{"err":{"variant":"CurrencyMismatch","payload":[]}}`. Was
    also silently producing `"[object Object]"` in `ui_gen_template.html`'s
    error snackbar (`String(body.err)`) for any endpoint returning
    `Result(_, SomeZeroPayloadEnum)` — a real, user-visible UI bug, not
    just a wire-format nicety. Fixed; `tests/workflow_ownership.rs`'s two
    assertions pinning the old (wrong) shape updated to match.
  - **Known gap, found but *not* fixed (out of scope for this pass):**
    the compiler has no duplicate struct/enum name detection at all —
    not new here, and not specific to prelude collisions (two
    user-declared `struct Foo { .. }` in one file also silently pass
    typeck today, first one wins). This makes the new prelude promotion
    slightly riskier to adopt than it should be: a program that still
    pastes its *own* `Money`/`CurrencyCode`/etc. (old habit, or hasn't
    seen this note) gets silently shadowed, not a clear error — directly
    against this project's own "reject, don't silently mis-compile"
    stance (§1). Worth a real fix (a name-collision pass over
    `Program.structs`/`Program.enums`, prelude entries included) — not
    attempted here; flagging so it doesn't quietly stay unowned.

- **2026-08-27 — `dec128` onto real LLVM codegen: investigated, not
  shipped — a genuine architectural blocker, not a time-boxing punt.**
  `runtime_kernels.rs` (the freestanding kernel library `codegen.rs`
  links compiled `.nir` binaries against) is deliberately a *dependency-
  free* `rustc --crate-type staticlib` compilation, entirely separate
  from the main `nirdosha` crate's own dependency graph (`build.rs`'s own
  doc comment: "no circular-dependency risk from a sub-crate"). Reusing
  `rust_decimal` there means locating and `--extern`-linking its compiled
  `.rlib` from Cargo's own build output — checked empirically in this
  environment: `target/debug/deps/` didn't even exist at the point
  `build.rs` would need it, meaning that lookup is genuinely racy, not
  just theoretically fragile. The alternative — hand-rolling decimal
  add/sub/mul/div/cmp directly against `rust_decimal`'s documented
  128-bit `serialize()`/`deserialize()` bit layout, no crate dependency —
  is the same "duplicate the algorithm, catch divergence with parity
  tests" pattern the existing f64 linalg kernels already use
  (`runtime_kernels.rs`'s own module doc), so it's not unprecedented, but
  it's real numeric code (mantissa rescaling, overflow, rounding) with
  real correctness stakes for the one type whose entire point is "no
  silent rounding drift" — not something to rush in the same pass as
  everything else above. Left interpreter-only, unchanged from how it
  shipped (§10's list) — nothing regressed, nothing silently half-built.
  Needs a deliberate decision on which path (fragile-but-reuses-proven-
  logic vs. self-contained-but-newly-written) before landing.

- **2026-08-27 — Default web-app style: real Nirdosha brand, not generic
  Material Design 3.** `ui_gen_template.html`'s token *values* replaced
  (names kept, so a per-project `--theme` override still works
  unmodified) — navy/amber derived from `assets/brand/nirdosha-logo.png`
  (sampled: navy `#1f1a52`, amber `#f89a1c`, the one accent that stays
  byte-identical light/dark), IBM Plex Sans + IBM Plex Mono (Google
  Fonts — the one deliberate network dependency this template now takes
  on, degrading to a system sans-serif stack if the fetch fails), radii
  pulled from Material's 28px pill down to 6-10px echoing the logo's own
  squared brace glyph, brand icon embedded in the app-bar
  (`ui_gen.rs::logo_data_uri`, same `include_bytes!`-at-compile-time
  pattern `favicon_data_uri` already used). Signature element: the
  active nav item gets a solid amber left-border bracket instead of a
  filled pill, and every `:focus-visible` ring switched from primary-navy
  to the amber accent specifically (a distinct color for "this is
  interactive" was a real legibility improvement, not just brand
  matching). Verified live via real screenshots on both demo apps (dark
  mode; light mode reasoned through the same token structure, not
  separately screenshotted), not just reading the CSS.
  - **Real bug found and fixed along the way:** both demo apps'
    `db_connect("money_invoice.db")`/`db_connect("measure_shipment.db")`
    calls used a bare relative filename — resolved against the *server
    process's* cwd at connect time, not the app's own source location.
    Restarting either server from a different working directory (which
    happened repeatedly this session) silently opened a **different**
    SQLite file than the one `--db`'s auto-migration touched — the app's
    own CRUD functions and the `/_nirdosha/table/<name>` browse route
    disagreed on row count as a result, caught by actually checking that
    route's response rather than trusting the CRUD API alone worked.
    Fixed by hardcoding the absolute path in both `.nir` files.
  - **Environment note, not a code issue:** mid-verification, `cargo
    test --release` started failing with real compile errors
    (`src/pool.rs`, `E0015`/`E0308`) that trace to an r2d2 connection-
    pooling change neither authored nor requested this session
    (`compiler/Cargo.toml`'s `r2d2`/`r2d2_sqlite`/`r2d2_postgres`
    additions, `src/pool.rs` itself untracked, both modified after this
    session's own last clean full-suite run) — concurrent work landing
    in the same working tree from elsewhere, mid-edit and currently
    broken. Not touched here (not this session's to fix); this session's
    own changes were full-suite-verified *before* that landed, plus
    live-verified after via running servers built from that clean state.
    Re-run `cargo test` once that pooling work finishes or is reverted.

- **2026-08-27 — JSON API boundary: struct nesting depth cap removed,
  replaced with real cycle detection.** `serve.rs::decode_value`/
  `ui_gen.rs::build_field` used a flat `depth >= 2` ceiling that rejected
  *any* fourth level of struct nesting unconditionally — a legitimate
  `Order -> LineItem -> Product -> Category` schema exactly as hard as an
  actually-cyclic one. Replaced with `DecodeGuard`/`visiting` — a set of
  struct names on the current expansion path; a name reappearing is a
  real cycle (clear error / `readonly` fallback, not a crash), anything
  else expands as deep as the schema genuinely goes.
  `serve.rs`'s side additionally keeps `MAX_DECODE_DEPTH` (64) as an
  independent backstop, since JSON *input* depth is caller-controlled
  regardless of whether the `.nir` program's own struct graph is cyclic
  (a deeply-nested plain JSON body doesn't need a cyclic type to exist).
  Verified live: a real 4-level `Order`/`LineItem`/`Product`/`Category`
  round-trips through `nirdosha serve` now; full test suite green.
  - **Real, more serious bug found while testing the fix, and fixed
    separately:** `ast.rs::TypeRegistry::is_affine` had its *own*
    unguarded recursion into struct/enum field types, with a doc comment
    explicitly dismissing the cyclic case ("no well-typed program could
    ever reach this path" — reasoning that only holds for *constructing
    a value*, not for merely *declaring a function whose signature
    mentions the type*, which needs no value at all).
    `struct A { b: B } struct B { a: A }` typechecks today (no cycle
    check at struct-declaration time) and a bare `fn echo(a: A) -> A` is
    all it takes to reach `is_affine` on it — confirmed empirically:
    `nirdosha emit-ui`/`serve` on exactly that shape aborted with a real
    stack overflow, not a hang or a clean diagnostic, reachable by any
    author who wrote a mutually-recursive struct pair, no adversarial
    input required. Fixed the same way (`is_affine_visiting`, an
    internal cycle-guarded helper behind the unchanged public
    `is_affine` signature — no ripple through its 10 external call
    sites in `codegen.rs`/`ownership.rs`/`typeck.rs`).
  - **Adjacent finding, fixed same day:** `nirdosha build` on the same
    cyclic-struct shape didn't crash, but surfaced a raw `clang` error
    ("identified structure type 'CycleA' is recursive") instead of a
    clean nirdosha-level diagnostic. Fixed with a `check_supported` guard
    (`codegen.rs::has_cyclic_layout`) that walks every struct/enum
    declaration's fields/payloads through direct (non-pointer) `Ty::Named`
    containment — `box`/`&`/`chan`/etc. fields break the walk on purpose
    (fixed-size handle regardless of what's behind them), so a genuine
    cons-list shape (`struct Node { next: box Node }`) still compiles;
    only a truly infinite-size cycle is rejected, with the offending type
    name and a "wrap it in `box`/`&`" fix in the message — same "reject,
    don't leak the backend's own error text" standard every other
    `check_supported` rejection holds itself to.
    - **Second bug found while verifying the fix, fixed in the same
      pass:** once box-indirected self-reference was correctly let
      through, it hit a *second*, pre-existing unguarded recursion —
      `codegen.rs::affine_codegen_supported` walks a struct's fields to
      decide if its affine leaves are all `nir_free`/`nir_tcp_stop`-able,
      with no cycle guard of its own; `struct Node { next: box Node }`
      sent it into `Node -> box Node -> Node -> ...` forever, a real
      stack overflow confirmed empirically (not a hang or clean error)
      the moment the first guard stopped masking it. Fixed the same
      `_visiting`-parameter shape as `is_affine_visiting`
      (`affine_codegen_supported_visiting`, unchanged public signature,
      its one external call site untouched); a repeat on the path
      returns "supported" here (not "unsupported" the way the cyclic-type
      guard does), since every step across the repeat was a `box` — the
      one affine leaf already torn down — so a second pass can only
      re-confirm the first, never surface a new unsupported leaf.
    - Verified live: `nirdosha build` on the exact `CycleA`/`CycleB`
      shape now fails with the clean nirdosha diagnostic (no `clang`/LLVM
      text in it); the same shape rewritten with `box` back-references
      compiles and runs correctly; three new regression tests
      (`structs_enums.rs`: a direct struct cycle rejected, a direct enum
      cycle rejected, a box-indirected self-reference still compiles and
      emits IR); full suite green.

- `[OPEN]` **A16. `json` onto real LLVM codegen — scoped, not started.**
  Requested directly (`nirdosha` chat, 2026-08-27). Structurally a
  different problem than `dec128`'s own outstanding codegen gap
  (A-numbered entry above): `dec128` is a flat 128-bit scalar; `json` is
  a dynamically-shaped, arbitrarily-nested heap tree (`Value::
  Json(Arc<serde_json::Value>)`) — nothing else in the compiled subset
  has that shape (`struct`/`enum`/`Vector`/`Matrix` are all sized at
  compile time; `json` never is).

  Three real pieces:
  1. **Representation** — low risk. `Ty::Json` lowers to a plain `ptr`,
     the same "opaque, never freed" treatment `str` already gets (`json`
     is non-affine — freely readable, no cleanup point — same as `str`).
  2. **A hand-written JSON parser + tree + accessors in
     `runtime_kernels.rs`** — hits the identical wall `dec128`'s own
     entry does: that file is a dependency-free freestanding `rustc
     --crate-type staticlib` build (`build.rs`'s own doc comment: "no
     circular-dependency risk from a sub-crate"), so it can't `use
     serde_json` any more than it could `use rust_decimal`. Lower
     correctness risk than hand-rolled decimal math, though — JSON
     parsing is a bounded, well-known algorithm. ~9 runtime functions
     (`nir_json_parse`/`_get`/`_get_str`/`_get_i64`/`_get_f64`/
     `_get_bool`/`_array_len`/`_array_get`/`_set_str`), each mirroring
     `interpreter.rs`'s own logic (same "duplicate it, catch divergence
     with parity tests" precedent the f64 linalg kernels already use).
  3. **The actual pivotal piece: a genuinely new codegen capability —
     constructing a `Result` value from a fallible runtime call.** 7 of
     the 8 real JSON accessors return `Result(_, str)`. Nothing in
     `codegen.rs` today can build an `Ok`/`Err` enum value from a runtime
     call's success/failure flag (confirmed: no `construct_result_from_
     call`-shaped function anywhere in `codegen.rs`) — this is the exact
     same unresolved gap already blocking `dec_from_str`'s own compiled
     path. **Solve this once, first, as its own primitive — it unblocks
     both `dec_from_str` and every fallible `json_*` accessor**, not
     json-specific work.

  **Disclosed scope caveat:** `json`'s real producers — `db_query`/
  `db_execute`, `http_get`/`http_post`, `nirdosha serve` itself — are all
  still interpreter-only too. Compiling bare `json_parse`/accessors alone
  only unlocks a literal `json_parse("...")` on a hardcoded string (an
  embedded-config case); the "real app" case needs `db`/`http` compiled
  too, a separate, much bigger lift not scoped here.

  Not started this session — the depth-cap fix and the `is_affine` crash
  it surfaced (this file's own entry just above) took the remaining time
  budget instead. Next actual step: prototype the `Result`-from-runtime-
  call codegen primitive on `dec_from_str` (smaller, already-fully-
  designed surface — see the `dec128` codegen entry above) before
  building the JSON parser on top of it.

---

## Standards & compliance posture

Added 2026-08-25, after a launch-prep session surfaced several claims
("we support FAPI," "audit trail included," "JWT support") that turned
out to be overstated once checked against real code — see that
session's core-review notes for the methodology. Same discipline as
this file's `[DONE]`/`[PARTIAL]`/`[OPEN]` tags above, plus one more:
`[N/A]` for a standard that's organizational/certification-level (an
ISMS, a legal compliance program, a business-continuity plan) rather
than something a compiler or language runtime can itself implement —
marking those `[OPEN]` would misleadingly imply "buildable, not yet
built," when the honest answer is "not the kind of thing this project's
code would ever contain." Every `[DONE]`/`[PARTIAL]`/`[OPEN]` row below
was checked against real source (file:line) or an actual grep/run, not
assumed from a doc comment.

| Area | Standard/protocol | Status | Evidence |
|---|---|---|---|
| Secure development | OWASP Top 10 | `[PARTIAL]` | Real: injection prevented structurally (`db_execute`/`db_query`'s `?`-bound params, no string-built SQL is even possible — `str` has no concatenation), broken access control mitigated by `requires(role/claim:...)` (server-enforced, `serve.rs`) and field-level `view`/`edit` gates (`ui_gen.rs:61-121`). Open: no rate limiting anywhere in `serve.rs` (checked, zero matches) — a real security-misconfiguration/DoS-adjacent gap. |
| Secure development | OWASP ASVS | `[N/A]` | A verification checklist you self-assess or get audited against — no formal ASVS pass has been run. |
| Secure development | OWASP SAMM | `[N/A]` | An org-level SDLC maturity model, not a codebase property. |
| Secure development | ISO/IEC 27034 | `[N/A]` | Application-security process standard, not code-assessable. |
| Information security | ISO/IEC 27001 | `[N/A]` | ISMS certification — an organizational program, not something a repo has. |
| Information security | ISO/IEC 27002 | `[N/A]` | Companion controls catalogue to 27001, same reasoning. |
| Information security | NIST CSF | `[N/A]` | Organizational risk-management framework. |
| Information security | CIS Controls | `[N/A]` | Organizational hardening/controls checklist. |
| Cloud security | ISO/IEC 27017 | `[N/A]` | Cloud-provider/tenant responsibility standard — depends on who's hosting `nirdosha serve`, not on the language. |
| Cloud security | CSA CCM | `[N/A]` | Cloud controls matrix, organizational. |
| Cloud security | CSA STAR | `[N/A]` | A registry/attestation program, organizational. |
| Cloud security | SOC 2 | `[N/A]` | Third-party audit of an organization's controls over time — nothing a codebase alone satisfies. |
| Privacy | ISO/IEC 27701 | `[N/A]` | Privacy-information-management extension to 27001, organizational. |
| Privacy | GDPR | `[N/A]` | Law, not a code property — no built-in "right to erasure"/consent-management tooling exists today (additive-only schema migrations, `migrate.rs`, is the closest adjacent primitive, and it's about schema evolution, not data-subject rights). |
| Privacy | India DPDP Act | `[N/A]` | Same reasoning as GDPR. |
| Privacy | CCPA/CPRA | `[N/A]` | Same reasoning as GDPR. |
| Identity and access | OAuth 2.0 | `[PARTIAL]` | No authorization-code/client-credentials grant flow (zero matches for `/authorize`, `redirect_uri`, `grant_type`). What's real: a session layer on top of OIDC token validation — `create_application_session`/`session_cookie`/`new_refresh_token`/`exchange_refresh_token`/`check_revocation` (`interpreter.rs:2580+`), unpredictable session IDs (`interpreter.rs:1474`), real revocation checking (`:2649`). This is "validate a token and manage sessions," not "be an OAuth2 authorization server or client." |
| Identity and access | OpenID Connect | `[PARTIAL]` | Real ID-token validation (`validate_oidc_token`, `interpreter.rs`, real JWKS lookup) — **2026-08-26: RS256/ES256 signature verification added** (Track A11, real RSA/EC via `ring`, alongside the original HS256), closing the single largest gap here. Still missing: no discovery endpoint, no `/userinfo`, one fixed JWKS/issuer/audience per `serve` process (Track A6's "Multi-IdP registry" is still `[OPEN]`). |
| Identity and access | SAML 2.0 | `[OPEN]` | Zero matches repo-wide. |
| Identity and access | SCIM | `[OPEN]` | Zero matches repo-wide (no user/group provisioning protocol). |
| Identity and access | FIDO2/WebAuthn | `[OPEN]` | Zero matches repo-wide (no passkey/phishing-resistant login). |
| Network security | TLS 1.2/1.3, HTTPS (outbound) | `[DONE]` | `https_get`/`https_post` (`interpreter.rs:2503-2509`) use real `native_tls::TlsConnector` (`:1022`, `:1765`). |
| Network security | TLS 1.2/1.3, HTTPS (`nirdosha serve` itself) | `[OPEN]` | `serve.rs:267` calls `tiny_http::Server::http(...)` — plain HTTP only. Production HTTPS needs a reverse proxy in front; the server doesn't terminate TLS itself. |
| Network security | SSH | `[N/A]` | Not something an application-level language runtime provides. |
| Network security | IPsec, DNSSEC | `[N/A]` | OS/network-layer concerns, out of scope for a language runtime regardless of implementation state. |
| Network security | mTLS | `[OPEN]` | Confirmed absent — also the FAPI blocker (`compiler/UI_DSL_TODO.md:353-357` lists it as "still owed"). |
| API security | OpenAPI | `[OPEN]` | No spec generation anywhere for `nirdosha serve`'s `POST /api/<fn>` routes. |
| API security | OAuth 2.0, JWT, mTLS | — | See Identity/access and Network security rows above. |
| API security | OWASP API Security Top 10 | `[PARTIAL]` | Real: parameterized queries everywhere, a real 1 MiB request-body cap (`serve.rs:82-99`, `MAX_BODY_BYTES`), field-level object authorization via `view`/`edit` gates (not just whole-endpoint gating). Open: no rate limiting, no OpenAPI contract to validate requests against. |
| Logging | Syslog | `[OPEN]` | Zero matches repo-wide. |
| Logging | OpenTelemetry | `[PARTIAL]` | Real local tracer with a console/file exporter, zero-cost-when-disabled design, fail-open channel (`observability.rs`). The module's own doc comment says real OTLP export to an actual collector, and real metrics, aren't built yet (Track A3 above tracks this). |
| Logging | CEF, ECS | `[OPEN]` | Zero matches repo-wide. |
| Availability | ISO 22301 | `[N/A]` | Business-continuity-management certification, organizational. |
| Availability | SRE practices | `[PARTIAL]` | Real durability primitives exist and are load-bearing (`transact`'s WAL + crash replay + retry/timeout, `workflow`'s durable state machine) — genuinely SRE-adjacent reliability engineering, not just a claim. No formal SLOs/error budgets/on-call tooling. |
| Availability | RTO/RPO | `[PARTIAL]` | `transact`'s durability log gives a real, low RPO for in-flight transactions specifically — but no formal RTO/RPO targets are defined anywhere for a `nirdosha serve` deployment as a whole, and no DR runbook exists. |
| Availability | Backup standards | `[OPEN]` | No built-in backup/restore tooling — a SQLite-backed deployment's backups are entirely the operator's own responsibility today. |
| Accessibility | WCAG 2.2 | `[OPEN]` | `ui_gen.rs`/`ui_gen_template.html`: zero `aria-` attributes anywhere. Whatever accessibility exists is incidental to using native HTML form elements plus Material CSS, not an explicit feature, audit, or test. |
| Accessibility | EN 301 549 | `[OPEN]` | Largely references WCAG; same gap. |
| Accessibility | Section 508 | `[OPEN]` | Same — references WCAG-equivalent criteria. |
| Accessibility | India GIGW | `[OPEN]` | Same — includes WCAG-aligned accessibility criteria. |
| Quality management | ISO 9001 | `[N/A]` | Organizational quality-management-system certification. |
| Quality management | ISO/IEC 25010 | `[N/A]` | No formal characteristic-by-characteristic assessment has been run — this file's own `[DONE]`/`[PARTIAL]`/`[OPEN]` discipline plus the compiler's test suite are the project's actual (informal) substitute today. |

### Domain applicability

Added 2026-08-25, same session. All of these are *product/process*
certifications — they certify a specific shipped product's full
development, verification, and traceability process (often including
the toolchain used to build it), not a language or compiler in the
abstract. So **every row below is `[N/A]` for "Nirdosha the project
holds this certification"** — that will always be true, regardless of
how the language matures, because a compiler isn't the kind of thing
these standards certify. What *can* change over time is which of
Nirdosha's real properties (SMT-proven overflow/bounds safety,
ownership-proven memory safety, structural deadlock-freedom, structured
machine-readable diagnostics) would count as useful supporting evidence
for a team pursuing one of these for an actual product built with it —
that's what the notes below capture.

| Domain | Relevant standards | Status | Notes |
|---|---|---|---|
| General SaaS | ISO 9001, ISO 27001, SOC 2, ISO/IEC 29119, ISO/IEC 25010 | `[N/A]` | Covered individually above (Information security / Quality management rows) — ISO/IEC 29119 (software testing standard) is new here: no formal 29119-structured test-process documentation exists; the project has a real, substantial test suite (`cargo test` across `compiler/`, `grammar_export/`, `bench/`) but it isn't mapped to 29119's process vocabulary. |
| Medical software | IEC 62304, ISO 13485, ISO 14971, IEC 81001-5-1 | `[N/A]` | None built or claimed. IEC 62304 (medical device software lifecycle) and ISO 14971 (risk management) are both about the *development process* for a specific device, not a language — using Nirdosha wouldn't satisfy either on its own. Real supporting evidence a team could cite: SMT-discharged overflow/bounds proofs and ownership-checked memory safety are exactly the class of property IEC 62304's risk-control expectations care about, but citing them isn't the same as having the certification. |
| Automotive | ISO 26262, Automotive SPICE, ISO/SAE 21434 | `[N/A]` | Same reasoning — ISO 26262 (functional safety) has ASIL-level tool-qualification requirements for anything in the safety-critical toolchain; Nirdosha's compiler has never gone through that qualification process. Structural deadlock-freedom and proven overflow safety are relevant *properties*, not a substitute for ASIL tool qualification. |
| Aerospace | DO-178C, DO-278A, AS9100 | `[N/A]` | Sharpest caveat of this whole table: DO-178C explicitly requires separate **tool qualification (DO-330)** for any tool (including a compiler) used in the verification process — Nirdosha's LLVM-based backend has no DO-330 qualification, and "the language has safety properties" doesn't substitute for that requirement. |
| Industrial control | IEC 61508, IEC 62443 | `[N/A]` | IEC 61508 (functional safety, SIL levels) has the same tool-qualification expectation as automotive/aerospace above. IEC 62443 is industrial-control *cybersecurity* — see the Network security/API security rows above (`[OPEN]` mTLS, no rate limiting) for what's concretely missing if this mattered today. |
| Banking and payments | PCI DSS, ISO 20022, secure SDLC and audit controls | `[N/A]` | PCI DSS is an assessed compliance program for whoever handles cardholder data, not a language property. Real gap already surfaced this session: no built-in audit-trail feature exists (`ROADMAP.md`'s own earlier note, confirmed absent in code) — PCI DSS requirement 10 (track/monitor all access) would need that built at the application layer today, same as the trade-finance example already does by hand with `sha256_hex`. |
| Government software | NIST, Common Criteria, FIPS 140-3 where cryptography is involved | `[N/A]` | NIST CSF already covered above. Common Criteria is a per-product security evaluation. **FIPS 140-3 specifically does not hold**: Nirdosha's cryptography (`hmac`/`sha2` crates, `Cargo.toml`) are standard RustCrypto software implementations, not a NIST CMVP-validated cryptographic module — a government deployment requiring FIPS-validated crypto could not use Nirdosha's built-in `sha256_hex`/HMAC as-is. |
| AI and ML | ISO/IEC 42001, ISO/IEC 23894, NIST AI RMF, AI-testing practices | `[N/A]` | These govern an AI *management system* or *risk process* around a product, not a compiler. Adjacent and real: the `bench/` harness (pass@1 + self-repair rate, 23 tasks) is a genuine piece of "AI-testing practice" infrastructure for evaluating a model's Nirdosha-generation quality — but it's evaluating *models writing Nirdosha*, not Nirdosha itself as an AI system, and (per this session's earlier check) it's only ever been run against mock models, never a real one. |
| Accessibility | WCAG 2.2, EN 301 549, Section 508 | `[OPEN]` | Duplicate of the Accessibility rows above — kept `[OPEN]` here (not `[N/A]`) since, unlike the rest of this table, accessibility genuinely *is* a property the generated UI could have; it just doesn't yet (zero `aria-` attributes in `ui_gen_template.html`, confirmed this session). |
| Cloud service | ISO 27001, ISO 27017, ISO 27018, SOC 2, CSA CCM | `[N/A]` | ISO 27017/SOC 2/CSA CCM already covered above (Cloud security rows). ISO 27018 (PII protection in public clouds) is new here — same reasoning as the Privacy rows: no built-in data-subject/PII-handling tooling exists, so this would depend entirely on the deploying operator, not on Nirdosha. |

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

- `[DONE]` **A1. `transact` durability under real failure conditions** —
  actually kill the process mid-transaction under load and confirm
  crash-replay behaves, not just trust the existing test suite.

  **2026-08-26 — done, and it found a real bug.** New `tests/
  transact_process_kill.rs`: spawns a real `nirdosha serve` child process
  (not the in-process simulation `tests/transact_durability.rs` already
  had), throws real concurrent HTTP load at it (12 client threads, 240
  `transact`-wrapped requests per round), `SIGKILL`s it mid-flight
  (`Child::kill`, a real signal to a real PID) twice across two
  restart-and-reload cycles, and confirms afterward that: the durability
  log has zero unresolved rows; the real business side effect (a separate
  SQLite "ledger" table standing in for whatever a real `commit` durably
  writes) has exactly one row per committed transaction (no lost writes,
  no double-applies); and every response a client actually saw as `true`
  before the kill is durably reflected in the ledger. Verified across
  repeated runs, not just once.

  This surfaced a real, previously-undiscovered gap on the very first run
  (not a hypothetical): a crash landing between `record_verify` and
  `mark_commit_pending` left rows unconditionally `Stuck`, even when
  `commit`'s arguments were exactly `network`/`txn_id` — the same
  always-safe shape every worked example in `TRANSACT.md` already uses.
  Fixed same-session (`TRANSACT.md`'s "recoverability boundary" section
  has the full writeup): `commit`/`compensate`'s arguments are now
  classified per-argument at `begin_pending` time
  (`commit_arg_kinds`/`compensate_arg_kinds`), and `replay_one`
  reconstructs them from `network`/`txn_id` when the durably-captured
  arguments are missing, falling back to `Stuck` only for a genuine
  outer-scope reference (the gap that's still honestly open). Includes an
  `ALTER TABLE` backfill in `TransactLog::open` so a pre-existing log file
  from before this fix still opens correctly after a binary upgrade — a
  real concern this same session's full-suite run actually hit (a
  temp-file durability log from an older test binary, reused via
  OS port-number reuse, failed to open with a "no such column" error
  before the backfill was added). Two new in-process reproduction tests
  in `tests/transact_durability.rs` plus the two now-passing existing
  negative controls. Full `cargo test` (700+ tests) reverified green.
- `[PARTIAL]` **A2. Deployment story for the interpreted path** —
  containerize `nirdosha serve` + source properly; secrets/JWKS
  handling; this is buildable now, independent of Track B. The simple
  case (copy a folder to a machine, run it, no orchestration) is now
  covered by **A6**'s `nirdosha init` below — a bundled executable +
  `run.sh`/`run.bat` launcher. **Horizontal scaling specifically is no
  longer just a missing-tooling gap here** — see **A17** below:
  `workflow`/`transact`'s durability logs had no multi-instance story at
  all (a real correctness wall, since fixed for the Postgres case,
  `[DONE]`).
  **2026-08-27 — full Kubernetes-specific breakdown done, then P0/P2/P3
  and most of P1 actually implemented and locally verified: see
  `KUBERNETES.md`.** Source-verified compliance matrix (container/image,
  12-factor config, health/lifecycle probes, state/horizontal-scaling,
  networking, observability, security posture, deployment manifests)
  plus a dependency-ordered P0→P3 remediation plan, then the plan itself
  executed same day: repo-root `Dockerfile` (non-root, read-only-rootfs-
  ready, multi-arch build via `.github/workflows/docker.yml`, cosign
  signing + SBOM), `/healthz`/`/readyz`/`/metrics` added to `serve.rs`
  with real integration tests, `SIGTERM`-triggered graceful shutdown
  verified against a real subprocess + real signal, structured JSON
  logs (`NIRDOSHA_LOG_FORMAT=json`), a Helm chart AND a Kustomize
  base + Postgres-multi-replica overlay (`deploy/helm/nirdosha/`,
  `deploy/kustomize/`), and protobox's own `kubernetes.py` deploy
  target. Verified live end-to-end, not just built: `docker run` boots
  the built image, `/healthz`/`/readyz`/`/metrics`/`POST /api/ping` all
  answer correctly from the host, `docker stop` exits cleanly in ~0.4s,
  UID 10001 non-root confirmed inside the container — this also caught
  and fixed a real build/runtime base-image mismatch (`rust:1-slim-
  trixie` build stage vs. a first `bookworm` runtime stage that failed
  to even boot, `GLIBC_2.39'/`GLIBCXX_3.4.31' not found; both stages now
  pinned to `trixie`).
  What's still genuinely open, per `KUBERNETES.md`'s own remediation-
  order section: (1) **P1's one disclosed gap** — `serve.rs`'s `--db
  <path>` auxiliary layer (the generic table-browser route +
  `RoleMappingCache`) opens a bare `rusqlite::Connection` directly,
  bypassing `dbconn.rs`'s Postgres-capable `DbConn` abstraction entirely
  (`serve.rs:218`) — this stays single-instance even after A17's
  durability-log fix and even if a project's own `db_connect(...)`
  literal is pointed at Postgres; judged too large a rewrite to fold into
  this pass, so `--db postgres://...` now fails fast with a clear error
  instead of being silently misused, rather than left silently broken;
  (2) the built image has never actually been pushed to a real
  `ghcr.io/protobox/nirdosha-runtime` registry from CI — the workflow
  lints clean but that leg is unexercised; (3) P2's OTel Layer 2b (real
  OTLP export) — tracked separately under **A3** below, never claimed
  done here; (4) P3's mTLS via service-mesh sidecar — deliberately not
  built into nirdosha itself, by design, consistent with TLS-for-`serve`
  already being deferred to a reverse proxy, not a gap. Companion
  positioning doc, same date: `KUBERNETES_ADVANTAGE.md` — the case for
  nirdosha over a mainstream k8s-targeted language (Go/Java/Node/Python)
  once this item's remaining gaps close: built-in kill-tested `transact`
  durability, a `workflow` audit trail with no extra stateful service,
  one-process UI+API footprint, compiled/enforced RBAC, and proven
  memory/overflow safety with no GC.
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
- `[DONE]` **A5. `workflow`'s real-time presence gateway.** `notify()`
  (`WORKFLOW.md`) publishes to `nirdosha:push:<subject>` (Redis) and
  reads `identity_presence` — both real — but nothing in this repo
  terminated a live browser WebSocket/SSE connection, and that's the
  only thing that could ever legitimately call the two routes that
  populate `identity_presence` (`_presence_connect`/`_presence_disconnect`)
  or subscribe to those Redis channels. Net effect before this: `identity_presence`
  never had an "online" row, so `notify()` always silently took the
  offline (`send_email`) path — it didn't error, it just never did the
  "push it live" half of its job.

  **2026-08-28 — built: `presence-gateway/` (repo root, its own crate —
  `README.md` there has the full protocol/design writeup, `WORKFLOW.md`'s
  presence-bridge section has the pointer).** A small standalone
  service, deliberately kept out of `compiler/` (own `Cargo.toml`'s doc
  comment: `nirdosha` is a `[dev-dependencies]`-only dependency, used
  purely by its own tests, so the shipped binary stays free of
  z3/postgres/rusqlite) that: terminates real browser WebSocket
  connections; independently verifies each one's identity token against
  its own `--jwks-file`/`--issuer`/`--audience` (a real `jsonwebtoken`-
  based verifier, not a second hand-rolled one — deliberately not
  reusing `interpreter.rs`'s `pub(crate)` verifier, `src/jwt.rs`'s own
  doc comment has the reasoning); calls `_presence_connect`/
  `_presence_disconnect` as connections open/close, correctly
  ref-counted per subject (`src/registry.rs`) so a second open tab
  neither re-announces nor is the one that marks a subject offline when
  it alone closes; and subscribes to each `nirdosha:push:<subject>`
  channel to relay to the right live connection — with the
  subscribe-before-ack ordering that actually matters enforced
  deliberately (an earlier version of this code acked "connected" before
  subscribing, and a `notify()` call issued right after could be
  silently lost to Redis pub/sub's no-persistence-for-late-subscribers
  semantics; caught by this crate's own integration test, not left as a
  latent bug).

  Verified live end-to-end, repeatedly, not just built: a real `nirdosha
  serve`, a real Redis, a real browser-shaped `WebSocket` client (Node's
  own global `WebSocket`, not a mocked stand-in), and a real `notify()`
  call round-trip correctly — as a plain release binary, and again as a
  built Docker image (own `Dockerfile`, confirmed non-root/UID 10002,
  90.7MB), including `docker stop`'s graceful-SIGTERM path: the client
  actually receives a clean WS close frame, not a connection reset, and
  a subsequent `notify()` call correctly falls back offline afterward
  (proving `_presence_disconnect` really ran, not just that the socket
  dropped). `cargo test`: 5 unit tests (ref-counting) + 5 real-service
  integration tests (`compiler/tests/mq.rs`/`serve.rs`'s own "verify
  against something real" discipline — a real in-process `nirdosha
  serve`, real Redis, real `mock_issue_token`-minted tokens), all green,
  `cargo clippy --all-targets` clean.

  **2026-08-28, same day — wired into the deployment story too, not left
  as a Dockerfile nobody actually deploys:** `deploy/helm/nirdosha/`
  bumped to `0.2.0`; `presence.enabled: true` now deploys this crate's
  image as a sidecar in the same Pod as `nirdosha serve` (`_pod.tpl`),
  reusing the main container's `auth.jwksSecretName`/`issuer`/`audience`
  and `presence.tokenSecretName` (no new Secret needed — same identity
  provider, same tokens), with a new `presence.redis.host` value for
  `notify()`'s live-push transport. This closed a real, freshly-created
  trap: `presence.enabled: true` alone already wired
  `--presence-token-file` onto the main container (so the routes stopped
  404ing) but deployed no actual gateway — indistinguishable from
  `presence.enabled: false` except for a token file nobody read.
  `presence.enabled` without `auth.enabled` or `presence.redis.host` now
  `fail`s at Helm render time (`nirdosha.validatePresence`), the same
  posture `validateReplicaMode` already takes for `db.mode`. Verified
  live under the *exact* `securityContext` this chart applies (arbitrary
  non-owning UID, read-only rootfs, no writable mount at all) — a real
  `notify()` round-trip still worked. `deploy/kustomize/`'s two static
  renders regenerated from the bumped chart per each file's own
  "regenerate, don't hand-edit" comment (`helm template`); no dedicated
  Kustomize overlay for presence specifically, consistent with `auth`/
  `otel` (the main chart's other optional toggles) also having none.
  `.github/workflows/docker.yml` matrixed to also multi-arch-build+sign+
  SBOM this image on the same trigger as the runtime image.

  **What's disclosed, not silently left out** (this crate's own README,
  "What's not here" section): no TLS termination (same `[N/A]`,
  delegate-to-the-platform posture `nirdosha serve` itself takes); one
  dedicated Redis `SUBSCRIBE` per WebSocket connection rather than a
  shared/fan-out subscription (simple and correct, `O(connections)` — a
  real thing to revisit only if it becomes an actual bottleneck at a
  scale this hasn't been tested against); neither this image nor the
  main runtime one has actually been pushed to the real registry from a
  live tag push yet (workflow lints clean, unexercised for real).
  `send_email`/`send_sms`/`send_push` and every other part of `workflow`
  were already, and remain, unaffected and fully functional without any
  of this.
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
    `ui_gen`/`serve` only render screens for structs that exist.
  - **2026-08-25 — `nirdosha init <project-name>` scaffolding: `[DONE]`.**
    Solves exactly the ergonomics this section named (not hand-typing the
    marker structs), kept scoped as a text-generation convenience, not a
    new "project manifest" concept the compiler itself needs to
    understand — `typeck`/`codegen`/`serve` still only ever know about
    one `.nir` file; `cmd_init`/`nirdosha::init` (`compiler/src/init.rs`)
    just write one to disk. Emits `EmailProviderConfig`/`RoleMapping`
    (default on) and `SmsProviderConfig`/`PushProviderConfig` (opt-in via
    `--sms`/`--push`) verbatim from `scratch/nirdosha_llm_prompt.md`'s
    standing-fixtures section, plus the `struct Text { value: str }`
    wrapper their `Result(_, Text)` signatures need (the str-ban's
    documented convention). Went one step further than "just a file,"
    per a direct ask: `init` also writes a self-contained, runnable
    project folder — `<name>.nir`, a bundled copy of the running
    `nirdosha` executable (`std::env::current_exe()`, same-OS/arch only),
    a `run.sh`/`run.bat` launcher wired to `nirdosha serve` with
    placeholder `--jwks-file`/`--issuer`/`--audience` (visible/
    discoverable rather than silently absent — every `requires(role:
    ...)` route honestly 401s until real IdP values replace them), and a
    placeholder `jwks.json` (`{"keys": []}`) so that launcher runs with
    zero manual setup. This is the simple, self-contained-folder answer
    to **A2**'s "deployment story for the interpreted path" below —
    containerization for a real production deployment is still open.
    Verified: `compiler/tests/init.rs` (generator-half lex/parse/
    typecheck/ownership-check on every fixture combination, plus a
    CLI-half spawning the real binary to check the written folder,
    the overwrite guard, and `--dest` directory creation).
- `[DONE]` **A7. Real Windows verification.** The `v0.1.0-alpha.1`
  release run (2026-08-25) found `runtime_kernels.rs`'s `tcp`/
  `tcp_listener` codegen backend used Unix-only `std::os::fd`
  (`RawFd`/`IntoRawFd`/`FromRawFd`) unconditionally — fails to compile
  at all on Windows. Ported (`v0.1.0-alpha.3`) to a `#[cfg(unix)]`/
  `#[cfg(windows)]` split using `std::os::windows::io::{RawSocket,
  IntoRawSocket, FromRawSocket, OwnedSocket}` on the Windows side, with
  the existing Unix-path integration tests
  (`compiled_connect_send_recv_stop_round_trips_real_bytes`,
  `compiled_listen_accept_serves_a_real_client`,
  `connecting_to_a_closed_port_traps_at_runtime` in
  `compiler/tests/codegen.rs`) re-verified green after the change.

  **2026-08-26 — a second real gap found and fixed the same way, plus
  the CI job this entry asked for.** `interpreter.rs`'s sandbox-channel
  transport used `std::os::unix::net::{UnixListener, UnixStream}`
  unconditionally too — also uncompilable on Windows (`AF_UNIX` isn't
  wrapped there), and also missed by the alpha.3 pass since it's a
  different subsystem than `runtime_kernels.rs`'s TCP path. Fixed the
  same way: a real `#[cfg(windows)]` leg, a loopback TCP socket instead
  of a Unix domain socket (`write_value`/`read_value` generalized over
  `Read`/`Write` so the wire format itself is unchanged). Same commit
  also fixed `codegen.rs`'s unconditional `-lm` clang link flag, which
  broke native codegen on Windows too (MSVC has no `m.lib`; libm lives
  in the C runtime there) — now Unix-only. New `build-windows` job in
  `.github/workflows/build.yml` (commit `4b535b0`) is the CI job this
  entry named as still needed: runs on a real `windows-latest` GitHub
  Actions runner, builds with `--features dist` (vendored Z3, no system
  `libz3` install needed on Windows), then runs the tests that actually
  exercise the ported code end to end — `tcp`/`sandbox`/
  `sandbox_channels`/`channels` for the interpreter paths, plus the
  `compiled_connect`/`compiled_listen`/`compiled_recv`/
  `connecting_to_a_closed_port_traps_at_runtime`/`tcp_client_example`
  subset of `codegen.rs` for the native-codegen `RawSocket` path.
  **2026-08-26 — the `build-windows` job ran for real (merged to
  `main`, run `32973021409`) and found two more real bugs, both
  fixed.** `tcp`/`channels` and most of `sandbox` passed outright; two
  `sandbox.rs` tests failed on real Windows:
  - `stopping_a_still_running_sandbox_kills_it_and_returns_negative_one`
    got `Int(1)` instead of the documented `Int(-1)`. Root cause:
    `SandboxChild::stop` inferred "was this process killed by us" from
    `status.code().is_none()` — true on Unix (`SIGKILL` termination has
    no exit code), but false on Windows, where `Child::kill()` is
    `TerminateProcess(handle, 1)`, a *real* exit code of `1`
    indistinguishable from a process that legitimately called
    `exit(1)`. Fixed by tracking `killed_by_us` explicitly at the call
    site instead of inferring it from the exit status afterward
    (`interpreter.rs::SandboxChild::stop`) — returns `-1`
    unconditionally when this call is the one that killed the process,
    on both platforms.
  - `dropping_a_sandbox_handle_without_stopping_it_still_kills_the_process`
    panicked with "the sandboxed process should be running before
    drop" — immediately after a real spawn, not a timing race. Root
    cause: the test's own `process_exists` helper shelled out to
    `kill -0 <pid>`, a Unix-only command with no Windows equivalent, so
    it always failed to even run there and silently read as "not
    running." Fixed with a `#[cfg(windows)]` counterpart using
    `tasklist /FI "PID eq <pid>"` (`compiler/tests/sandbox.rs`) — same
    "shell out, no new dependency" approach the Unix version already
    documents.

  Both fixes verified locally (`cargo test` green on Linux) and pushed;
  the resulting `build-windows` run (`32975984692`) confirmed the
  sandbox-channel fix — `tcp`/`sandbox`/`sandbox_channels`/`channels`
  all green — but surfaced a **third**, independent real bug in the
  same job's next step: `clang: error: linker command failed with exit
  code 1120` (unresolved externals) on all 4 of the compiled-TCP
  `codegen.rs` tests. Root cause: `runtime_kernels.rs`'s `nir_tcp_*`
  kernels (added for the TCP codegen path, `RUNTIME_KERNELS_LIB`) call
  into `std::net`, which needs `ws2_32.lib` on Windows — but
  `codegen.rs::build()` links that staticlib with a bare `clang`
  invocation, not `rustc`, so none of the OS-level libraries `rustc`
  would normally supply automatically (`ws2_32.lib` and friends) were
  ever being passed. The existing Unix fix for the same *class* of gap
  (`-lm`, needed for `atan2`) was a one-off hardcoded flag; Windows
  needs a whole list, and guessing it wasn't necessary — `rustc
  --print=native-static-libs` (`build.rs`) captures the real list at the
  exact moment `rustc` already knows it, for whichever platform the
  build is actually running on, written to `OUT_DIR/
  native_static_libs.txt` and threaded into `clang`'s link line via a
  new `#[cfg(windows)]` arm (`codegen.rs::NATIVE_STATIC_LIBS`) — Unix's
  existing `-lm` arm is untouched. Verified locally: `cargo build
  --release` + full `cargo test`/`--test codegen` (142 tests) still
  green on Linux with this change (the captured Unix list is `-lgcc_s
  -lutil -lrt -lpthread -lm -ldl -lc`, a superset of the old hardcoded
  `-lm`, applied only under `cfg(unix)` so behavior there is unchanged).
  That push (run `32979448313`) hit a **fourth** real bug, in the fix
  itself: `clang: error: no such file or directory: 'kernel32.lib'` —
  the real captured list (`kernel32.lib ntdll.lib userenv.lib
  ws2_32.lib dbghelp.lib /defaultlib:msvcrt`) confirmed the mechanism
  works, but Clang preflight-checks any *positional* argument as a
  literal path relative to the working directory, even though a bare
  `foo.lib` token is exactly what MSVC's linker resolves through its
  own library search path, never by looking in the cwd. Fixed by
  stripping each `.lib` suffix and passing `-lfoo` instead (Clang's
  ordinary library flag, which skips that preflight check and does
  reach the linker's search path); the one non-`.lib` token
  (`/defaultlib:msvcrt`, already a raw linker flag) is forwarded
  verbatim via `-Xlinker`.

  **2026-08-26 — `build-windows` run `32982218064` came back fully
  green**: `Build`, the `tcp`/`sandbox`/`sandbox_channels`/`channels`
  suite, and all 5 compiled-TCP `codegen.rs` tests (the ones that
  actually exercise the `-lfoo` fix) all passed for real on a Windows
  runner. Flipping to `[DONE]` now — per this file's own rule, on an
  observed-green run, not a believed-correct port. Four real,
  independent Windows-only bugs were found and fixed to get here, none
  hypothetical, each caught by this same CI job on a real push: (1)
  `SandboxChild::stop` returning `Int(1)` instead of `-1` for a killed
  process (`Child::kill()`'s Windows semantics are `TerminateProcess`
  with a real exit code, unlike Unix's signal termination); (2) the
  Unix-only `kill -0` test helper always reading "not running" on
  Windows; (3) the compiled `tcp` path failing to link at all
  (`ws2_32.lib` and friends never passed to `clang`, since `codegen.rs`
  links with a bare `clang` call, not `rustc`) — fixed by capturing
  `rustc --print=native-static-libs` at build time; (4) that fix's own
  bare `.lib` tokens rejected by Clang's positional-argument preflight
  check — fixed by passing them as `-lfoo` instead. Still open, a
  narrower gap than before: this proves the *compiler and its test
  suite* build and run on Windows CI, not that a shipped end-user
  release binary has been run on someone's own Windows machine outside
  CI.
- `[OPEN]` **A8. macOS Z3 vendoring.** `z3-src` 416.0.2 (pulled by the
  current `z3` 0.20.2 crate) fails to compile against the AppleClang on
  GitHub's `macos-13`/`macos-14` runners — a real `obj_hashtable.h`
  constructor-strictness incompatibility, confirmed via the
  `v0.1.0-alpha.1` release run's build log, not a config mistake.
  Worked around (`v0.1.0-alpha.2`) by linking system Z3 via Homebrew on
  macOS instead of vendoring it — the macOS release binary needs
  `brew install z3` on the machine running it, unlike Linux/Windows.
  Revisit once a `z3`/`z3-src` release ships that fixes the upstream
  incompatibility (latest known `z3-src` is 500.0.0, but the `z3` crate
  itself hard-pins `z3-sys = "0.11.0"`, which itself hard-pins
  `z3-src = "416"` — upgrading requires either a new `z3` crate release
  or a `[patch]` override, not just a version bump in this repo's own
  `Cargo.toml`).
- `[OPEN]` **A9. Business-rule parameters (thresholds, boundary
  operators, currency) have no elicitation or config-store path.**
  Found via `examples/trade-finance/trade_finance.nir`'s
  `required_eyes_for_amount` (Module 4's Maker-Checker/6-Eyes
  governance routing) against its own source user story
  (`US-TRDPAY-002`, `scratch/extracted_userstories_v2.json`):
  - **Threshold value drift** — the story's acceptance criteria use
    "$1,000,000" (the PRD's own "e.g." hedge); the shipped rule uses
    $50,000 (5,000,000 cents), self-disclosed in its own comment as "a
    fixed illustrative cutoff... no per-tenant config store exists
    yet." Two different sources of truth, neither actually authoritative.
  - **Boundary-operator drift** — the story's `post_logic`
    (`routed_to_six_eyes == (payment_amount > high_value_threshold)`)
    and its second acceptance criterion ("at or below the threshold ->
    Maker-Checker") both imply strict `>`; the shipped rule uses `>=`
    — exactly-at-threshold is six-eyes in code, Maker-Checker in the
    spec. Documented, not silently "fixed," by
    `compiler/tests/trade_finance_governance_routing.rs`'s
    `boundary_case_at_exact_threshold_is_six_eyes_per_shipped_code`.
  - **Currency-blind comparison** — `submit_trade_payment`/
    `required_eyes_for_amount` take a raw `amount_cents: i64` and never
    consult `currency` at all, even though `Currency` already exists as
    an enum and `TradePayment` carries a `currency` field: a
    5,000,000-cent JPY payment (~$33) and a 5,000,000-cent USD payment
    ($50,000) route identically today.

  None of these are LLM-generation bugs fixable at the prompt level —
  they're missing *inputs*: nobody with real business authority was
  ever asked what the threshold, the boundary, or the per-currency
  conversion should be, so an LLM (or a developer) filled in a
  placeholder. Proposed direction, not started: `nirdosha init` grows a
  domain-aware question phase — e.g. "does this project have
  monetary threshold-routing rules? if so, for each one: what's the
  threshold per currency/corridor, and is the boundary inclusive or
  exclusive?" — with answers landing in a runtime-editable config
  table (the same "ordinary struct, free CRUD screen" convention
  `EmailProviderConfig`/`RoleMapping` already use, per **A6**), not
  inlined as `.nir` literals. Needs two things that don't exist yet:
  (1) a structured, domain-specific elicitation schema (a flat "what
  domain?" question doesn't surface "inclusive or exclusive boundary?"
  on its own), and (2) the config-store primitive itself, generalized
  beyond identity/provider settings to arbitrary business-rule data.
- `[DONE]` **A10. `serve.rs`'s dispatcher is default-open, not
  default-deny.** Found red-teaming `API_TRUST_MODEL.md`, verified
  directly: `dispatch` (`compiler/src/serve.rs:1030-1063`) only runs an
  authorization check `if let Some(req) = &f.requires` — a function with
  no `requires(...)` annotation skips the block entirely and is callable
  by anyone, with or without a Bearer token. Counted against the shipped
  `examples/trade-finance/trade_finance.nir`: 246 functions, 34 declare
  `requires(...)`, **79 are reachable with no token at all**, including
  mutating ones (`issue_letter_of_credit`, `clear_sanctions_override`,
  `update_counterparty`). This directly contradicts how the project
  describes its own output — `PROTOBOX_INTEGRATION.md`'s own Purpose
  line calls the generated app "a real, running, **role-gated** web
  app."

  **2026-08-26 — fixed via direction (a): a new `requires(public)`
  marker plus a typeck warning, not a runtime behavior change.** Direction
  (b) (a project-level default-deny `serve` flag) was scoped out for this
  pass — it would force triaging all 79 already-open
  `trade_finance.nir` functions before that flag could ever be turned on
  for it, real follow-up work distinct from this fix. What shipped:
  - `requires(public)` (`ast::FnDecl::explicit_public`,
    `parser.rs::parse_requires_annotation`, `GRAMMAR.md`/
    `compiler/nirdosha.gbnf` updated) — an explicit "this fn is
    intentionally callable with no token" marker. Deliberately **not** a
    `Requirement` variant: it does not gate direct calls or `Ty::Fn`
    references the way `requires(role/claim: ...)` does — a
    `requires(public)`-marked fn stays exactly as directly callable as an
    unannotated one (`requires` stays `None`).
  - `typeck::ungated_fn_warnings` (new, non-fatal — never blocks
    `typecheck`/a build, unlike a real `TypeErrorKind`) walks every
    declared `fn` and warns on one with no `requires(...)`, no
    `requires(public)`, no `VerifiedIdentity` parameter, and no `db`/`mq`
    parameter (the last two already 400 at `serve.rs::decode_value`
    regardless, excluded so the count matches what's actually reachable).
    Wired into `nirdosha serve`/`emit-ui` (`main.rs::
    print_ungated_fn_warnings`), the two commands where HTTP reachability
    is the actual question; `run`/`build`/`emit-llvm` are unaffected.
  - `workflow_lower.rs`'s synthesized `<event>_via_link` functions are
    marked `explicit_public: true` — the pre-existing, already-documented
    deliberate carve-out (`API_TRUST_MODEL.md` §1) — so the new warning
    doesn't fire on the one shape that's supposed to be open; synthesized
    `start_*`/`advance_*` are not marked, so they do warn today (an
    honest gap: the `workflow` DSL has no syntax yet to attach
    `requires(...)` to those generated functions).
  - Verified end-to-end against the real flagship example: `nirdosha
    emit-ui examples/trade-finance/trade_finance.nir` now prints exactly
    **79** `UngatedFnReachableWithNoToken` warnings — an exact match with
    this entry's own hand-counted figure above, confirming the
    reachability logic is right, not just plausible.
  - New `compiler/tests/ungated_fn_warning.rs` (8 tests: plain-fn warns;
    `requires(role/claim)`, a `VerifiedIdentity` param, a `db`/`mq`
    param, and `requires(public)` each silence it; `requires(public)`
    confirmed to *not* gate a direct call; an unknown `requires(...)` kind
    is still a parse error naming `public` as a valid option). Full
    `cargo test` reverified green.
  - Full design and the runtime invariant this closes: `API_TRUST_MODEL.md`
    §4, `LANGUAGE.md` §6a.
- `[DONE]` **A11. JWKS validation is symmetric-only — no mainstream IdP
  can be plugged in today.** Found in the same red-team pass, verified
  directly: `jwks_key` (`compiler/src/interpreter.rs:1168-1178`) reads
  only a JWKS key's `k` member (a base64 symmetric/HMAC secret) — there
  is no RSA (`n`/`e`) or EC (`x`/`y`/`crv`) key-material path anywhere
  in the validator, and `validate_oidc_token`
  (`compiler/src/interpreter.rs:1085-1184`) never inspects the token
  header's `alg` at all. `mock_issue_token`
  (`compiler/src/interpreter.rs:1241`) hardcodes `alg: HS256` to match.
  Auth0, Okta, Keycloak, and Azure AD all sign with RS256/ES256 by
  default — none of their JWKS documents are consumable by this code
  path as it stands. `LANGUAGE.md` §5 already hedges this correctly ("a
  **mock** OIDC/JWT ID token"); `PROTOBOX_INTEGRATION.md`'s "replace the
  three placeholder flags in `run.sh` with a real IdP's `--jwks-file`"
  (§4) currently cannot be completed for any mainstream IdP without an
  RSA/EC signature-verification path first.

  **2026-08-26 — fixed: real RS256/ES256 verification via `ring`.**
  New `ring = "0.17"` dependency (pure-Rust, no dynamic system-crypto
  link, same "just works" posture `rusqlite`'s `bundled` feature already
  gives this project). `jwks_key` now returns a `JwksKeyMaterial` enum
  (`Symmetric`/`Rsa { n, e }`/`Ec { crv, x, y }`) keyed off each JWKS
  key's own `kty` (`oct`/`RSA`/`EC`), instead of unconditionally reading
  `k`. `validate_oidc_token` now reads the JWT header's `alg` (previously
  never inspected at all) and dispatches to a new `verify_jwt_signature`:
  `HS256` keeps the existing constant-time HMAC path; `RS256` verifies
  via `ring::signature::RsaPublicKeyComponents`/
  `RSA_PKCS1_2048_8192_SHA256`; `ES256` via `ring::signature::
  UnparsedPublicKey`/`ECDSA_P256_SHA256_FIXED` over the raw uncompressed
  SEC1 point. Closes algorithm confusion as a side effect, not an
  afterthought: `alg` and the resolved key's `kty` must agree (there is
  no match arm accepting `HS256` against an `Rsa`/`Ec` key), so an
  attacker can no longer replay a JWKS's public RSA/EC key bytes as an
  HS256 HMAC secret. `mock_issue_token` is intentionally unchanged — it
  stays HS256-only (its own doc comment's documented scope; it now
  errors clearly if pointed at a non-symmetric `kid` instead of silently
  misusing the key material).

  Verified with real key material, not just unit-level mocks: new
  `compiler/tests/oidc_jwt_algorithms.rs` (5 tests) signs real JWTs with
  a freshly generated 2048-bit RSA keypair (RS256) and a `ring`-generated
  P-256 keypair (ES256), round-trips both through the real
  `oidc_validate_token` builtin (parser/typeck/interpreter, not a
  Rust-level unit test), confirms a tampered signature is rejected for
  each algorithm, and confirms the algorithm-confusion forgery (an
  RS256 JWKS's public `n` bytes reused as an HS256 HMAC secret) is
  rejected rather than silently HMAC-verified. All 5 existing JWKS-based
  test suites (`row12_identity`, `claim_path`, `privileged_fn`,
  `role_mapping`, `field_rbac`, `serve` — every fixture already declared
  `"kty":"oct"`) re-verified green with no changes needed. Full
  `cargo test` green. Full design and remaining scope (mobile identity,
  a multi-IdP registry) unchanged: `API_TRUST_MODEL.md` §3.
- `[DONE]` **A12. Verbatim, mathematical verification of a PRD
  extraction against real `.nir` code** — `API_TRUST_MODEL.md` §7.5's
  Tier 1 (an SMT obligation channel for a human/extractor-written
  predicate) and a new sibling structural construct for `workflow`
  shape, both built and demonstrated against a real extraction file,
  `scratch/extracted_typed_v1.json`, not a synthetic fixture.
  - **`contract_check::check_fn_contract`** (new
    `compiler/src/contract_check.rs`) — Tier 1, for real: takes a real
    Hoare pair (`pre_logic`/`post_logic`, straight out of the extraction
    JSON's own shape) and a real named `.nir` function, parses each
    predicate with the exact same grammar every `.nir` expression gets
    (`parser::parse_standalone_expr`, one new entry point — no separate
    predicate mini-language), asserts `pre_logic` as a hypothesis, then
    either proves every `post_logic` clause for **every** input the
    function's declared param types admit or returns a real
    counterexample pulled from Z3's own model, naming exactly which
    clause it violates. Deliberately a separate module from `smt.rs`
    rather than an extension of it — same "duplicate a focused walker
    rather than couple two independently-evolving analyses" precedent
    `smt.rs`'s own module doc already sets. Scoped exactly as §7.5
    proposed: one named pure, loop-free, integer-only function, no
    interprocedural reasoning — anything outside that (a `Call`, a
    `while`, a non-integer type) is an honest `Unsupported`, never a
    silent wrong answer (approximating an unmodelable sub-expression
    would be sound for a universal proof but **unsound for a
    counterexample**, so the walker aborts on both sides instead).
    Demonstrated end-to-end: `required_eyes_for_amount`'s real body
    (`if amount_cents >= 5000000 { 2 } else { 1 }`) is proved to satisfy
    `WF-TRDPAY-001.routing_fn.post_logic`'s real biconditional,
    `(result == 2) == (amount_cents >= high_value_threshold)`, for every
    `i64` `amount_cents` — once told `high_value_threshold`'s actual
    value. That threshold is exactly §7.1a's "the spec references a
    quantity the code doesn't parameterize on" case:
    `required_eyes_for_amount` takes no such parameter, the real code
    hardcodes 5,000,000 (`ROADMAP.md` A9), so `check_fn_contract`
    requires it as an explicit `extra_bindings` input — omitted, it
    returns `UnboundIdentifier` rather than a misleading answer;
    supplied wrong (6,000,000), it correctly returns a real
    `Counterexample`. A `bool_expr` case `smt.rs`'s own didn't need
    (nothing it synthesizes itself is shaped this way) had to be added
    to get the biconditional right: the predicate's outer `==` is
    boolean equality between two comparisons, not integer equality
    between two numbers.
  - **`workflow_conformance::check_workflow_conformance`** (new
    `compiler/src/workflow_conformance.rs`, plus new
    `compiler/src/extraction_schema.rs` — a typed `serde::Deserialize`
    mirror of the extraction JSON's whole shape) — no solver needed: a
    `workflow`'s states/transitions/data fields are a finite, fully-known
    structure the moment `.nir` source parses, so checking the real
    `workflow { ... }` against an extraction's version is ordinary
    set/relation equality, always a real match or a real, named diff.
    Narrower than the SMT construct in one way: verifies shape, not
    behavior (`on_entry`/`on_exit` compared by count, not by matching
    prose against a real `notify(...)` call's actual arguments — a
    natural-language-to-call binding deliberately not attempted here).
  - Both verified against all three of `scratch/extracted_typed_v1.json`'s
    `workflows[]` entries — new `compiler/tests/
    extracted_typed_v1_verification.rs` (8 tests): 3 exact-match
    conformance checks against `.nir` snippets mirrored verbatim from
    `compiler/tests/trade_payment_approval_workflow_check.rs`; 2 tests
    that deliberately mutate the real `.nir` source (drop a transition;
    flip a `terminal` flag) and confirm the *specific* mismatch is
    reported, not just that some diff exists; 3 tests proving/
    counterexampling the routing_fn contract as described above. Full
    `cargo test` reverified green.
  - **What this doesn't close, named rather than silently implied:** a
    user story's own `pre_logic`/`post_logic` (e.g. `US-COMM-006`'s
    `withdrawal_amount > 0`) isn't checkable yet — the extraction schema
    has no field binding a story to the real function(s) that implement
    it (`ExtractedUserStory::implements` exists as a `#[serde(default)]`
    placeholder, always empty until the extraction prompt emits it); per
    §7.1a most user-story postconditions are Tier 2b's shape anyway
    (end-to-end DB state after several functions, not one pure
    function's return value) — Tier 2b itself, the repair loop, and
    row-level ACL remain exactly as `[OPEN]` as before this item. Full
    design detail: `API_TRUST_MODEL.md` §7.5.
- `[DONE]` **A12a. Extend the extraction schema with `implements:
  [fn_name, ...]` on a user story** — the concrete next step A12
  identified, now shipped. `scratch/prompt_v2.txt`'s `UserStory` schema
  gained `implements` (bound to a real `.nir` `fn`), plus, folded into
  the same pass since A13 below needed related fields anyway:
  `required_role` (a literal role token distinct from
  `required_permission`'s prose label) and `input_fields` (typed
  `{field, type}` entries — what makes a story renderable as an actual
  form, mirroring `Workflow.data`'s existing shape).
  `extraction_schema::ExtractedUserStory` updated to match, all three
  fields `#[serde(default)]` so `scratch/extracted_typed_v1.json` (which
  predates every one of them) still deserializes unchanged — verified by
  `compiler/tests/extraction_schema_new_fields.rs`'s dedicated
  backward-compatibility test, plus the existing
  `extracted_typed_v1_verification.rs` suite reverified green with no
  changes needed. Still `[OPEN]`, unchanged: nothing in `contract_check.rs`
  consumes `implements` yet — the field exists and validates, but no
  extraction has populated it with real data yet, so user-story-level
  Tier 1 checking (§7.5) isn't exercised end-to-end.
- `[DONE]` **A13. Workflow state ownership + a generated "my queue" UI.**
  Surfaced by a direct question: does today's `workflow`/extraction
  schema even carry *who owns a state* (e.g. which users are the "two
  eyes" in six-eyes) or *where the UI is* for a user to see/act on their
  pending items? Verified directly: no — `ast::StateDecl` had no owner
  field, `ui_gen.rs` had zero references to `WorkflowDecl`, and the one
  hand-written approval screen `trade_finance.nir` actually ships is a
  read-only list gated to a single fixed role with no decide action at
  all (it doesn't even use `workflow{}` — a separate, older, hand-rolled
  mechanism).

  **2026-08-26 — built**, against the design `WORKFLOW.md`'s "State
  ownership + a generated queue UI" section wrote up (now updated in
  place to describe what shipped, not kept as a stale proposal):
  `state { owner: role(...)/claim(...), label: "..." }` grammar
  (`StateDecl.entries`, reusing `screen`'s own `view`/`edit` shape check);
  `advance_<workflow>` gains a real, disclosed breaking-change leading
  `identity: VerifiedIdentity` param, checked in `interpreter.rs::
  workflow_advance_inner` against the *current instance's* live state,
  not a static per-function gate (the magic-link path stays deliberately
  un-owner-checked — a consumed single-use token is its own
  authorization); a new synthesized `list_<workflow>_pending_for_me` read
  side; and `ui_gen.rs`'s first new screen archetype beyond `screen`'s
  fixed-action-set table — a generated **"Workflows"** nav section where
  each row's button set is that row's own current state's own outgoing
  events, from a real per-row server response, not anything static.
  New non-fatal `TypeWarningKind::WorkflowStateHasNoOwner` (A10's
  "default open, but tell you" posture, for a state instead of a fn).
  Full detail, including the exact new fns/builtins/manifest shape and a
  bug fix found in passing (`serve.rs::decode_value` had no `Ty::Json`
  arm at all, so any `json`-typed fn param 400'd over real HTTP,
  unconditionally): `compiler/UI_DSL_TODO.md`'s own new "workflow state
  ownership" section.

  **Not solved, disclosed**: `owner` alone models a single decider
  (Maker-Checker), not a quorum (six-eyes' "2 *distinct* holders of a
  role" — the first qualifying caller's decision would fire the
  transition immediately). The extraction schema's
  `owner_role`/`owner_claim`/`label`/`required_decisions` (per `state`,
  `ExtractedState`, shipped earlier: `scratch/prompt_v2.txt`,
  `extraction_schema.rs`) are the data half; `required_decisions` is
  still metadata only, not enforced anywhere in the runtime — six-eyes
  needs either new transition-level grammar or a hand-rolled
  decision-count table layered on top of this.

  **Extended same session, from a direct enterprise-systems question
  ("does this handle every real-world approval pattern?")**: two more
  near-universal enterprise expectations, both fully built, plus an
  honest catalog of what still isn't. **Who submitted this** (every
  system from ServiceNow to Concur puts "my requests" one click from the
  homepage): `start_<workflow>` gains a leading `identity:
  Option(VerifiedIdentity)` param — optional, unlike `advance_<workflow>`'s
  required one, since starting a workflow is legitimately anonymous in
  real programs today (`kyc_onboarding.nir`'s own public intake). This
  needed a genuinely new, general `serve.rs::dispatch` capability, not
  workflow-specific: a fn param typed `Option(VerifiedIdentity)` is
  injected `Some(id)`/`None` depending on whether a valid bearer token
  was presented, never a 401 either way — useful for any "personalize
  when signed in, still work when not" endpoint. `workflow_instance`
  gains `started_by_subject`; a new `list_<workflow>_submitted_by_me`
  read fn backs a second, read-only "My Requests" tab in the generated
  UI. **Audit trail** (who/when/why, SOX/banking-regulation territory):
  `workflow_history` (already durable from this feature's first version)
  gains `actor_subject`/`via_link`/`comment`; a new
  `get_<workflow>_history` read fn backs a per-row "History" button.
  Found and fixed in passing: `serve.rs::decode_value` had no `Ty::Json`
  arm at all, so `advance_<workflow>`'s own pre-existing `payload: json`
  param unconditionally 400'd over real HTTP, regardless of what a
  caller sent — nothing had exercised it end to end before now.
  **Disclosed, not built**: quorum (unchanged from above); a real
  per-viewer history ACL (today: any signed-in identity may view any
  instance's history); delegation/out-of-office reassignment; SLA/
  escalation timers (structurally impossible without a scheduler
  primitive Nirdosha doesn't have); bulk actions; a unified cross-
  workflow inbox; an in-app notification *bell/inbox UI* convention
  (the persistence itself is already possible today via an ordinary
  `on_entry`-called `fn` + struct, no gap there) — full table with
  what each would take, `WORKFLOW.md` §9.

  Verified: full `cargo test` green (including four pre-existing test
  files/examples updated for the `advance_<workflow>` signature change,
  plus a new `compiler/tests/workflow_ownership.rs`, 5 real-server
  integration tests covering owner enforcement across all three levels,
  `pending_for_me`/`submitted_by_me`/`history`, and the
  `Option(VerifiedIdentity)` capability); a real 3-level sequential
  purchase-order approval (`examples/purchase_approval.nir`) served via
  `nirdosha serve`, driven through all three levels via curl with three
  distinct mock-issued role tokens, plus real browser screenshots of the
  generated queue and the requester's own "My Requests"/History view.
- `[DONE]` **A14. Real runtime deadlock detection for `chan`/`thread`
  (`interpreter::DeadlockRegistry`) — closes a real gap between what
  README.md/goal.md claimed ("no deadlocks... proof by construction...
  an agent literally cannot generate a deadlock") and what the compiler
  actually did.** Found and verified directly, not assumed: a fully
  well-typed, cleanly-typechecking program —
  `fn main() -> i64 { let c: chan i64 = chan; return recv(c) }` — hung
  the process forever, with zero diagnostic, before this landed.
  `PHASE0.md`'s own "Twelfth update" already disclosed this honestly
  internally ("the *proof-by-construction* claim isn't fully earned
  yet") — README.md's/goal.md's user-facing claims didn't carry that
  caveat.
  - **What's real now**: a `join`-cycle (two or more threads mutually
    `join`-ing each other) is detected *precisely* — an exact wait-for
    graph over `join` edges, since `join`'s argument always names one
    specific target thread. A `recv` with no possible sender gets a
    coarser, still-sound fallback: if *every* thread this run knows
    about (`main` plus every currently-live `spawn`ed thread) is
    simultaneously blocked on `recv`/`join`, none of them can ever run
    code again, so none could ever call `send` — the same condition
    Go's own runtime deadlock detector checks (`"fatal error: all
    goroutines are asleep"`), generalized here to also catch a
    same-process `join`-cycle mid-program, which Go's whole-process-only
    check misses. Either case traps with a clear, structured
    `ErrorKind::Deadlock` instead of hanging — `serve.rs`/`main.rs`
    surface it exactly like any other runtime error, no special-casing
    needed.
  - **What's still honestly open, named rather than implied solved**: a
    `recv` blocked forever while some *other*, unrelated live thread
    stays busy on its own work (never touches that channel, never
    finishes) is invisible to the coarse check — real detection there
    would need points-to tracking of channel handles (freely copyable,
    per `SANDBOXING.md`), not attempted. README.md/`WORKFLOW.md`'s
    "Proposed, not built" framing elsewhere in this file is the model
    for how this gap is disclosed, not silently dropped.
  - **Two real correctness bugs found and fixed during construction, both
    caught by this file's own test suite going red, not by inspection**:
    (1) registering a spawned thread as "live" *inside its own closure*
    raced against the parent immediately calling `recv`/`join` — fixed
    by registering synchronously in the *parent*, right after
    `std::thread::spawn` returns (a `JoinHandle`'s `ThreadId` is valid
    the instant `spawn` returns, before the child's closure has
    necessarily started). (2) a single check-before-blocking design
    could still miss a deadlock that only finished forming *after* a
    thread had already committed to a real, unstoppable OS wait (no
    timed variant exists for `JoinHandle::join`) — fixed by converting
    both `recv` and `join` into short poll loops (`DEADLOCK_POLL_
    INTERVAL`, 10ms) that periodically re-check instead of blocking
    unconditionally forever, with `try_recv`/`is_finished` fast paths so
    an ordinary, already-resolved `recv`/`join` never touches the
    registry at all (verified necessary: without the fast paths, a
    same-thread `send` then `recv` was itself falsely flagged).
  - Verified: new `compiler/tests/deadlock.rs` (6 tests — the two real
    deadlock shapes, both resolving in a bounded-time harness rather
    than risking a hung `cargo test`; four false-positive guards
    including a 20-iteration repeat of the exact race this session hit
    and a genuinely slow producer that legitimately spans several poll
    cycles). Full existing concurrency suite (`concurrency.rs`,
    `channels.rs`, `sandbox_channels.rs`) reverified green, repeated 15x
    with no flakiness after the fix (the pre-fix version reliably
    reintroduced the exact intermittent failures described above). Full
    `cargo test` reverified green, repeated 4x. `README.md`'s deadlock-
    freedom claims (the row-3 requirements table, the "no mutex" pitch
    paragraph, the comparison-matrix row, and the concurrency section)
    all corrected to the precise, now-true claim instead of the
    overstated one — `goal.md`/`PHASE0.md` deliberately left as-is
    (frozen design/historical-journal docs, not status trackers — this
    file is where current status belongs, per this file's own stated
    convention).
- `[OPEN]` **A15. SLA/escalation timers for `workflow` states — no
  scheduler primitive exists; `owner`/`on_entry` alone can't express
  "escalate after N hours of silence."** Surfaced by the enterprise-
  catalog review `WORKFLOW.md` §9 did against real systems (ServiceNow,
  SAP Business Workflow, Concur, banking maker-checker all have this).
  Verified directly: `on_entry`/`on_exit` only ever fire in response to
  a transition that already happened — nothing in the language calls
  back into a running instance after a time delay with no human action,
  and `WORKFLOW.md`'s own "Deliberate non-goals" section already
  discloses "no scheduling/cron primitive in Nirdosha at all." A state
  sitting in `PendingManagerApproval` for a week with nobody acting is
  invisible today: no re-notification, no auto-escalation to a
  substitute approver, no visibility that an SLA was even breached.

  **Proposed design, not started** — two independent pieces, drawing
  the same "in-language vs. external infrastructure" line `notify()`'s
  own realtime path (an external WS gateway relays its Redis `PUBLISH`)
  already draws:
  1. **In-language**: a new `state { sla: "<duration>" }` kv-entry — no
     new grammar needed, just a new key on the same open-ended
     `StateDecl.entries` slot `owner`/`label` already use (`ROADMAP.md`
     A13). `workflow_instance` gains an `sla_deadline_at` column,
     computed at entry time (`now + duration`) the same way
     `record_transition`/`create_instance` already stamp `updated_at`. A
     new synthesized read fn, `list_<workflow>_overdue() ->
     Result(json, WorkflowActionError)`, queries every instance whose
     current state declares an `sla` and whose deadline has passed —
     pure SQL, no scheduler needed for the query itself.
  2. **External** (the part Nirdosha genuinely cannot do alone, disclosed
     not hidden): something has to actually *call* that query
     periodically and act on what it finds (re-notify, auto-reassign, or
     fire a new `Escalated` event via `advance_<workflow>`) — a cron job
     or orchestrator polling `list_<workflow>_overdue()`, the same "the
     schedule itself is always external" posture `WORKFLOW.md`'s own
     "nightly workflow" note already establishes for a purely time-
     triggered `workflow`. `nirdosha serve` itself needs no new
     subsystem — this is a client of the existing RPC surface, not a new
     runtime concept.

  **Not solved by this**: what "escalate" actually *does* (e.g.
  auto-reassign `owner` to someone's manager) needs the delegation
  feature (also `[OPEN]`, same `WORKFLOW.md` §9 review) or a hand-
  written `Escalated` transition in the workflow's own states — left to
  the app author, not something `sla` alone would automate.
- `[DONE]` **A16. `spawn` created a brand-new real OS thread on every
  single call, unconditionally — no reuse, and a real OS-level failure
  to create one (`RLIMIT_NPROC`/`kernel.threads-max` exhaustion under
  heavy load) was an uncatchable process panic, not a `.nir`-catchable
  error.** Surfaced by a direct question: could `spawn` be made "dirt
  cheap," the way Java's virtual threads or Go's goroutines let a
  developer not think about the cost of spawning one? Verified directly:
  `Expr::Spawn` (`interpreter.rs`) called the free-function
  `std::thread::spawn` — which panics the whole process on failure,
  Rust's own documented behavior — with no pooling, no reuse, one fresh
  OS thread (default stack, real `pthread_create` cost) per `spawn`,
  forever.

  **Researched properly before building anything** (real prior art, not
  guessed): Java's virtual threads work because the JVM controls its own
  continuation representation at the bytecode level — a mechanism
  unavailable to ahead-of-time-compiled Rust. Rust itself *had* green
  threads before 1.0 and removed them deliberately (blocking I/O still
  blocked the whole carrier thread; FFI/embedding got hard; stacks made
  big enough to be safe lost their whole size advantage). The unsafe
  stackful-coroutine crates that survive in the Rust ecosystem today
  (`may`, etc.) are explicitly documented as capable of real undefined
  behavior via thread-local storage — directly contradicting this
  project's own memory-safety guarantees. Even Java's own fully-
  resourced implementation still fights "carrier thread pinning" today
  (a virtual thread blocked in a native/FFI call freezes its carrier) —
  and this interpreter calls blocking native code (SQLite, TLS, Redis,
  raw sockets) constantly, with zero cooperation points anywhere, so a
  naive port would reintroduce exactly that failure mode with none of
  the JVM's years of engineering behind it. Modern Rust's own real
  answer to cheap M:N concurrency is `async`/`.await` + an executor like
  Tokio (stackless task scheduling, not stackful green threads) — correct,
  and how production Rust systems actually do this, but a from-scratch
  rewrite of the interpreter's entire execution model (every blocking
  builtin becoming a cooperative yield point), far too large and far too
  risky to a correctness-critical, already-shipped subsystem to attempt
  in one pass.

  **2026-08-27 — built**, the safe, proven, 100%-Rust middle path: a
  self-tuning, reused-worker OS thread pool (`thread_pool.rs`, new
  module) backing `spawn` — `.nir`-visible behavior byte-for-byte
  unchanged (no new grammar; `thread`/`spawn`/`join`/`chan` mean exactly
  what they always meant), only the underlying resource cost changes.
  `submit` never leaves a job waiting behind other work purely because
  every worker is busy — no idle worker means a brand-new one is spawned
  immediately, just for that job — the one property that structurally
  rules out the classic bounded-thread-pool self-deadlock (a worker
  blocked in `join` waiting on a child that can never get a worker to
  run it). Idle workers retire after 10s of no work, so real OS thread
  count tracks actual concurrent demand, not the lifetime total of
  `spawn` calls. `thread_pool.rs`'s own module doc has the full design
  rationale, including the exact deadlock-shaped bug this specific eager-
  growth property avoids.

  **The one genuinely delicate part, and a real bug found and fixed
  along the way**: `DeadlockRegistry` (the existing, already-shipped
  deadlock detector `ROADMAP.md` doesn't have its own entry for because
  it long predates this file) was keyed by `std::thread::ThreadId` —
  sound only when one OS thread's whole lifetime corresponds to exactly
  one logical `spawn`, which stops being true the moment a worker is
  reused for a second, later, unrelated task. Re-keyed to a new logical
  `TaskId`, decoupled from whichever physical thread executes it at any
  moment (a clean simplification in passing: the old `main_thread_id`
  field's `OnceLock`-captured-on-whichever-thread-calls-`run_main`-first
  fragility, documented at length in its own doc comment, is gone
  entirely — `TaskId::MAIN` is just `0`, a real constant now). Found via
  a new, real 200-level-deep recursive `.nir` spawn/join chain test (the
  exact shape a naive bounded pool deadlocks on, run through the *actual*
  parser/typeck/ownership/interpreter pipeline, not a synthetic Rust-only
  check): a task must stay registered as "live" for as long as any
  joiner might still be mid-check for it, not just until its own closure
  finishes computing a result — unregistering too early (either right
  after computing a result, or via the poll loop's old "unblock after
  every timeout, re-register next iteration" flicker) let a finished-
  but-not-yet-joined task transiently vanish from the coarse "is
  everyone blocked" check's universe, producing an intermittent false
  deadlock report under real scheduling pressure. Fixed by (1) moving
  unregistration to the *joining* side (`PooledTaskHandle`'s own `Drop`
  impl, firing only once a result has actually been consumed, or a
  handle is abandoned) and (2) keeping a waiter continuously registered
  across poll iterations instead of flickering on/off — the second half
  of the fix applies equally to `Expr::Recv`'s identically-shaped loop,
  a latent soundness gap in the *pre-existing* detector this session's
  own new deep-chain test is what actually exposed, not something the
  pooling change introduced on its own.

  **What changed for the worse, on purpose, and why it's better**: a
  real OS-level thread-creation failure is now `ErrorKind::
  ThreadSpawnFailed` — a clean, catchable `.nir`-level `Err`, exercised
  via an injectable-spawner unit test (not by actually exhausting real
  OS threads, slow and flaky) — instead of the prior uncatchable process
  panic. This is the literal "doesn't crumble under heavy load" fix: hitting
  the OS thread ceiling used to kill the whole server; now it's a
  recoverable error a `.nir` program can `match` on.

  **What this deliberately does not solve, disclosed, not hidden**: a
  task that calls a genuinely long *blocking* operation (a slow
  `db_query`, a `recv` waiting on a real external `tcp` peer) still ties
  up one real OS worker thread for that duration — pooling changes
  *reuse between tasks*, not the cost of blocking itself. True cheapness
  during blocking I/O specifically needs the async/Tokio rewrite named
  above, a real, scoped, explicitly-not-attempted next step, not
  papered over as already solved.

  **Verified**: every pre-existing `tests/concurrency.rs` (9)/
  `tests/deadlock.rs` (6)/`tests/sandbox_channels.rs` (8) test re-run
  green, unmodified — proving the language-level contract is untouched.
  New `thread_pool.rs` unit tests (7: reuse across sequential bursts,
  idle-timeout shrink, the exact bounded-pool-deadlock shape proven
  impossible via a 50-level Rust-level chain, injected spawn-failure
  handling, panic containment). New `tests/thread_pool_reuse.rs` (5,
  through the real `.nir` pipeline): 500 sequential spawn/join pairs
  reuse ≤5 live workers; 500 genuinely concurrent spawns resolve
  correctly; a real 200-level recursive `.nir` spawn/join chain resolves
  without a false deadlock (repeated 28 times clean during development,
  after being the exact test that first caught the false-positive race
  above); a genuine `join`-cycle is still correctly detected alongside
  unrelated successful spawns; **5,000 total spawns across 50 waves of
  100 concurrent tasks complete correctly in well under a second, with
  the live worker count staying near one wave's width instead of
  growing with the lifetime total** — the direct, at-scale demonstration
  of the actual "dirt cheap" claim. Full `cargo test` (all 64 test
  binaries) reverified green.
- `[DONE]` **A17. `workflow`/`transact`'s durability logs had no
  multi-instance story at all — a correctness wall, not just missing
  tooling.** A2 (above) filed "horizontal scaling for a workflow/transact
  app" as a missing-tooling bullet, next to containerization and secrets
  management — that undersold it. `workflow_log.rs`/`transact_log.rs`
  each unconditionally opened a local `rusqlite::Connection` to a file
  path: run two `nirdosha serve` replicas behind a load balancer and each
  has its own independent, diverging workflow-state file —
  `workflow_instance.id` (SQLite `AUTOINCREMENT`) collides across
  replicas, and a retried request that lands on a different replica than
  its first attempt sees no row for its `txn_id` at all, silently
  defeating `transact`'s idempotency guarantee. Fixed in three phases,
  same session:

  - **Phase 0 — fail fast (`src/instance_lock.rs`).** A second process
    pointed at the exact same SQLite durability file now refuses to
    start, loudly, instead of silently diverging — an OS-level exclusive
    lock (`PRAGMA locking_mode=EXCLUSIVE` on a tiny sidecar `<path>.lock`
    file, held for `serve::run`'s whole process lifetime, acquired once
    up front rather than inside `WorkflowLog`/`TransactLog::open` itself
    — those stay callable any number of times per process, which
    `interpreter.rs`'s per-request `Interpreter` construction and
    `tests/transact_process_kill.rs`'s own live-inspection-while-serving
    pattern both depend on; a real bug this session's own full test run
    caught before landing). **Explicitly does not** solve the actual
    cross-machine case (two replicas, two independent files, never
    touching the same filesystem at all — nothing for a local lock to
    contend on) — only the same-host "started twice" accident (a stuck
    old process during a restart, an operator running `serve` twice by
    hand). Verified live, not just by unit test: two real `nirdosha
    serve` processes pointed at the same `--transact-log` file — the
    second refuses to start with a clear error, the first keeps serving
    unaffected.
  - **Phase 1 — real multi-instance via Postgres (`src/durability.rs`).**
    `--transact-log`/`--workflow-log` accept a `postgres://`/
    `postgresql://` value (parsed by the new `LogTarget`, reusing
    `pool.rs`'s existing generic `PoolRegistry` — the exact "any future
    pooled resource gets this for free" case that module's own doc
    comment named) — every replica then shares one real Postgres
    database: `workflow_instance.id` comes from a real `BIGSERIAL`
    sequence (no per-file counter to collide), and `transact_log`'s
    `txn_id`-keyed idempotency guarantee holds fleet-wide instead of
    per-process. `workflow_log.rs`/`transact_log.rs` each dispatch on a
    3-way `Backend` enum (`Sqlite`/`Postgres`/`PostgresTls`, mirroring
    `dbconn.rs::DbConn`'s own shape) — every one of the ~25 query methods
    across both files got a parallel Postgres implementation (real
    `BIGSERIAL`/`BOOLEAN` columns in place of SQLite's `AUTOINCREMENT`/
    0-1 `INTEGER`), not a stub. A local SQLite file is still the default
    and remains explicitly single-instance (an inherent property of an
    embedded, per-file database, not a nirdosha gap) — Phase 0's lock is
    what makes that limit loud instead of silently dangerous. Verified
    live against a real local Postgres server, not just read from docs:
    7 new `#[ignore]`d integration tests
    (`tests/durability_postgres.rs`, run the same way `tests/postgres.rs`
    already documents) including a direct proof that two independent
    `WorkflowLog`/`TransactLog` handles sharing one Postgres database
    never collide on `instance_id` and do see each other's `txn_id` rows;
    plus two real `nirdosha serve` processes on the same Postgres
    database, both starting and serving concurrent requests correctly.
    Full `cargo test` (all 65 test binaries) reverified green throughout.
  - **Phase 2 — SQLite-native replication via rqlite (`src/rqlite.rs`).**
    Real prior art, not reinvented: **rqlite** already replicates SQLite
    correctly via real **Raft** consensus (Ongaro & Ousterhout, *"In
    Search of an Understandable Consensus Algorithm,"* 2014) — every
    write goes through an elected leader and isn't acknowledged until a
    majority of replicas have it in their log. This module is
    deliberately just an HTTP client speaking rqlite's `/db/execute`/
    `/db/query` wire protocol (hand-rolled raw HTTP over `TcpStream`/
    `native_tls`, the same choice `interpreter.rs::http_request`/
    `https_request` already made, for the same "no extra runtime
    dependency" reason) — not a reimplementation of consensus itself;
    see below for why hand-rolling that would have been the wrong call.
    `--transact-log`/`--workflow-log` accept `rqlite://`/`rqlites://`
    (a third `LogTarget` variant); every replica then shares one
    Raft-replicated SQLite database instead of Postgres, for a deployment
    that wants multi-instance without taking on a Postgres dependency.
    Because rqlite *is* SQLite under the hood, every `?`/`?N`-placeholder
    SQL string `Backend::Sqlite`'s own arms already use works here
    verbatim — unlike Postgres, no parallel dialect was needed, only a
    third `Backend::Rqlite(RqliteClient)` arm per method reusing the
    exact same query text. Verified live against a **real 3-node rqlite
    cluster** (`rqlited` built from source, Raft-joined, not simulated):
    the client's leader-redirect-following logic exercised for real by
    pointing `WorkflowLog`/`TransactLog` at a follower node directly (this
    rqlite build transparently proxies rather than issuing an HTTP
    redirect — confirmed by inspecting the raw response before trusting
    it, not assumed); `?N` numbered-placeholder SQL confirmed to work
    unmodified through rqlite's HTTP API (a real, not hypothetical, risk
    given `transact_log.rs`'s SQLite arms use `?1`/`?2`/etc.); two real
    `nirdosha serve` processes, each pointed at a *different* node of the
    same cluster, both starting and serving concurrent requests
    correctly, one of them even correctly surfacing a stale
    cross-replica `transact` row from an earlier test run as `Stuck` on
    crash-replay — direct proof the shared-log/crash-replay path works
    end to end over rqlite, not just the individual query methods in
    isolation. 7 new `#[ignore]`d integration tests
    (`tests/durability_rqlite.rs`, same convention as
    `tests/durability_postgres.rs`), including the same
    never-collide-on-`instance_id`/see-each-other's-`txn_id` proof Phase
    1's own Postgres tests give. Full `cargo test` (66 test binaries)
    reverified green throughout.

    **Why prior art (rqlite), not a hand-rolled Raft implementation:**
    a from-scratch consensus implementation inside nirdosha itself
    (`openraft` + a custom SQLite-backed state machine — essentially
    rebuilding what `dqlite` already is, in Rust) was the other option on
    the table and was deliberately not chosen — a multi-day scope with
    exactly the correctness subtleties the literature below exists to
    warn about, in a subsystem where getting it wrong means silent data
    corruption, not a crash. Real prior art either way:
    - **rqlite** and **dqlite** — the two established SQLite-via-Raft
      systems; rqlite chosen here for its plain HTTP API (fits this
      codebase's existing "hand-rolled raw HTTP, no client SDK" pattern)
      over dqlite's C-library/FFI surface.
    - **cr-sqlite** (vlcn.io) — the alternative design: CRDTs (Shapiro et
      al., *"Conflict-free Replicated Data Types,"* 2011) instead of a
      leader — multiple concurrent writers, merges commutative by
      construction, availability over strong consistency under
      partition. Not chosen: `workflow`/`transact`'s durability
      contract wants linearizable reads (`level=strong`), not
      eventual-consistency merge semantics.
    - **Litestream** is *not* a solution here despite often coming up in
      the same breath — single-writer WAL shipping to object storage for
      backup/point-in-time recovery, not multi-writer replication.
    - Underlying theory: Lamport's Paxos (*"The Part-Time Parliament,"*
      1998), Brewer's CAP theorem (why "just replicate the file" can't
      give strong consistency *and* availability once the network can
      partition, full stop), Google's Chubby (Burrows, 2006) and Spanner
      papers. Kleppmann's *Designing Data-Intensive Applications*
      synthesizes all of it; his *"How to do distributed locking"* post
      specifically debunks naive lock-based fixes (Redlock) via the
      fencing-token problem — relevant because a lock, not consensus,
      is the wrong tool for this specific job, for exactly the reasons
      that post lays out.
    - Postgres (Phase 1) still gets the same guarantee at one more
      remove — a real production Postgres deployment (managed, or
      self-hosted behind Patroni, which itself leans on etcd/Consul for
      leader election) already has this solved too; Phase 2 exists for
      the deployments that specifically don't want that dependency.

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
  `ui_gen.rs::build_screens`'s `Screen`/`FieldSpec`/`Action`/`Metric` IR
  — plus one real addition to that IR, not carried over unchanged: a
  `target: Web|Mobile|All` field on `Screen`/`Metric` (default `All`)
  backing a new optional `target: "web"|"mobile"|"all"` `kv_entry` on
  `screen`/`dashboard`/`tile`/`chart`, so mutually-exclusive per-target
  screens are possible (`MOBILE.md`'s "Per-target screen/dashboard
  exclusion" section). That filtering has to land in `ui_gen.rs`/
  `manifest_json` as part of this item, before `mobile_gen.rs` itself —
  `emit-ui` is the only renderer that exists while D1 is being built, so
  a `target: "mobile"` screen must already disappear from *its* output,
  not just from a native renderer that doesn't exist yet. Otherwise:
  checked-in Swift/Kotlin runtime library (generic per-`control`-kind
  field views, list/singular/dashboard/login screens, networking client,
  `Theme` mapper) embedded via `include_str!` the same way `codegen.rs`'s
  `RUNTIME_KERNELS_LIB` is; per-app generated code is one typed struct
  per `Screen`, not per-struct logic. No new `ScreenDecl` grammar (the
  `target` key reuses the existing generic `kv_entry` production), no
  new builtins, no new `serve.rs` routes.
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

## Track E — Enterprise UI constructs (`examples/ctms/SCREENS.md`, `examples/ctms/UI_CONSTRUCTS.md`)

*Priority: independent of Tracks A–D — this grows `ui_gen.rs`'s existing
manifest/renderer (the same `Screen`/`FieldSpec`/`Action`/`Metric` IR
`MOBILE.md`'s Track D already reuses unchanged), not the
interpreter/compiler/agent-API/mobile work above. Grounded in a real,
dense enterprise spec (CTMS — Counter-Terrorism Financing & Transaction
Monitoring System) worked all the way from a raw doc through a full
89-screen inventory to five concrete, grammar-shaped construct
proposals, exactly the way `MOBILE.md` was written before `mobile_gen.rs`
existed. E1–E5 each stand alone (no ordering constraint between them,
per `UI_CONSTRUCTS.md`'s own leverage ordering) and can be picked up in
priority order or in whatever order actually matches what's being built;
E6 is the one item that waits on all five.*

- `[DONE]` **E0. Screen inventory.** `examples/ctms/SCREENS.md` —
  89 screens across CTMS's 10 modules plus cross-cutting, each with
  actor(s)/purpose/key data/actions/screen-shape, grounded in the CTMS
  doc's own component/actor/event names, not generalized boilerplate.
  — 2026-09-03.
- `[DONE]` **E0b. Construct design.** `examples/ctms/UI_CONSTRUCTS.md` —
  gap analysis of all 89 `SCREENS.md` screens against today's `screen`/
  `dashboard`/`module` DSL, five proposed constructs in priority order
  (`workspace`/`panel`; `visual` dashboard/panel item + `render:
  "graph"|"heatmap"|"timeline"`; `field { render: "countdown" }`;
  `action { show_result: true }`; a workflow stage stepper), each with a
  grammar-shaped syntax sketch, what it lowers to in `ui_gen.rs`/
  `ui_gen_template.html`/`serve.rs`, a worked CTMS example, and an
  explicit "not included" scope note — same level of detail `MOBILE.md`
  uses for its own not-yet-built constructs. Also shows, with two worked
  examples, that most of the 89 screens (report generation/scheduling,
  most config-as-data policy screens, plain CRUD) need **no** new
  construct at all — existing minimalism preserved, not inflated. —
  2026-09-03.

- `[DONE]` **E1. `workspace` / `panel` — composite multi-pane screens.**
  Highest-leverage item: ~18 of the 89 screens directly (Investigation
  Workspace, Alert Detail/Risk-Score Breakdown, Behavioural Profile, ML
  Model Management, Case Collaboration, Evidence Management, Decision
  Panel, Escalation & Regulatory Referral, Case Export/Audit Dossier,
  Fiat–Crypto Correlation, Entity 360, Exchange/Partner FI Portal, RTFDS
  Real-Time Action Console, plus the child-row half of several
  config-as-data screens). New top-level construct — composes fields/
  lists from multiple structs, scoped to one subject-struct instance,
  onto a single screen; today's `screen <Struct> { ... }` is
  fundamentally one-struct-shaped and can't express this. Full design:
  `UI_CONSTRUCTS.md` §1. — 2026-09-03, verified: full `cargo test` green
  (`mq.rs`'s pre-existing Redis-connection-refused failures unrelated,
  present before this change), plus a real `nirdosha serve --db` smoke
  test (create a matter, fetch it as the workspace header, create a
  transaction, read it back through the panel `source` fn, add a note
  through the panel `action`, read it back) and a `node --check` pass on
  the extracted client `<script>` — not just "typechecks."
  - [x] Grammar: `workspace_decl`/`panel_decl` added to `GRAMMAR.md` and
    `compiler/nirdosha.gbnf`, `Tok::Workspace` (real reserved keyword)
    + contextual `panel` in `token.rs`, `parse_workspace_decl`/
    `parse_panel_decl` in `parser.rs`, mirroring `parse_screen_decl`
    production-for-production.
  - [x] Cross-verify against `grammar_check/`'s independent LALR(1)
    generator — **found inapplicable, not silently skipped**: that
    crate's `Item` production is still just `FnDecl` (pre-existing gap
    predating this work — `struct`/`enum`/`screen`/`dashboard`/`module`/
    `workflow` were never added there either, and the crate doesn't
    build clean regardless, per its own README's disclosed statement-
    boundary-ambiguity finding). Adding `workspace`/`panel` alone to a
    crate that models none of the UI DSL would imply a cross-check that
    doesn't exist — left undone, honestly, rather than faked.
  - [x] `compiler/nirdosha.gbnf` updated (hand-maintained, not test-
    verified — no `cargo test` target exercises it, confirmed by
    inspection before assuming otherwise).
  - [x] AST: `ast::WorkspaceDecl`/`PanelDecl` (`PanelActionDecl` is a
    type alias to the existing `ActionDecl`, not a new type);
    `typeck.rs::check_workspace` — `subject` resolves to a real struct
    with an `id: i64` field, every panel's `source` resolves to a real
    fn taking exactly one `i64` param and returning `Result(json, _)`
    (`sig.ret` matched structurally, not just "resolves"), every panel
    `action`'s `->` target resolves via the existing `check_fn_ref`.
  - [x] `ui_gen.rs`: `struct Workspace`/`struct Panel`, `build_workspaces`
    alongside `build_screens`, `workspaces_json`, a new `WORKSPACES`
    top-level array threaded through `generate()` — no new parameter on
    `generate()` itself, since workspaces come straight off the
    already-passed `program`.
  - [x] `ui_gen_template.html`: `#/ws/<snake>` (picker) and
    `#/ws/<snake>/<id>` (the workspace) routes, `renderWorkspaceList`/
    `renderWorkspace`/`renderPanel`, a "Workspaces" nav section. **One
    real deviation from the design doc, disclosed**: the doc proposed
    changing a subject screen's row click target; implemented instead as
    an additive "Open Workspace" per-row button (`renderListScreen`
    gained an optional 3rd `openWorkspace` param) — safer (doesn't
    touch existing Edit/Delete semantics), same end capability. Panel
    actions' param convention (first param = the workspace subject's own
    id, pre-filled/hidden) was undesigned in the doc (its own "Panel
    refresh" open question) — implemented and documented in both
    `ui_gen_template.html` and `LANGUAGE.md` §15, not left silently
    ambiguous.
  - [x] `serve.rs`: confirmed a genuine no-op, as the design predicted —
    not touched.
  - [x] `LANGUAGE.md`: new `§15. workspace/panel` section.
  - [x] `compiler/tests/workspace_dsl.rs` (10 tests: well-formed shape,
    every rejection case — missing/unknown/id-less subject, missing/
    unresolved/wrong-shape source, unresolved action target, and the
    "no workspace block" regression guard) plus 2 real end-to-end cases
    in `tests/emit_ui.rs` (manifest wiring incl. gating, and the
    "no workspace" empty-array case).
  - [x] Applied to `examples/ctms/ctms.nir` (recreated — it only existed
    in git history at `c6d6e3e` before this). **A second real finding,
    disclosed**: the worked example's `Case` struct was renamed
    `Matter` — `CASE` (and, separately, `TRANSACTION`) are reserved SQL
    keywords, confirmed by a real `sqlite3` failure
    (`Parse error ... near "case": syntax error`) before this was
    caught here rather than by a future author. This is deliberately a
    proof-of-concept subset (`Matter`/`Transaction`/`MatterNote`, one
    workspace, two panels) — not the full 89-screen rebuild, which is
    still E6, blocked on E2–E5.
  - [x] Full `cargo test` (whole suite) green — verified above.

- `[OPEN]` **E2. `visual` dashboard/panel item + `render: "graph"|
  "heatmap"|"timeline"`.** Unblocks Case Linking/Entity Graph, Wallet
  Cluster Graph, Graph Network Explorer, Session/Device Linkage View,
  Geo Heatmap directly (~6 screens), plus upgrades an E1 panel from a
  flat table to a timeline. Small grammar extension — one new
  `dashboard_item` keyword plus a closed-vocabulary `render:` key reused
  inside `panel` (no separate mini-language per chart kind). Full
  design: `UI_CONSTRUCTS.md` §2.
  - [ ] Grammar: `dashboard_item ::= ("tile" | "chart") string "->" ident
    | "visual" string "->" ident ("{" kv_entry* "}")?` (`GRAMMAR.md`);
    `parser.rs::parse_dashboard_decl` grows the `visual` arm.
  - [ ] Cross-verify against `grammar_check/`'s independent LALR(1)
    parser.
  - [ ] Update `compiler/nirdosha.gbnf`.
  - [ ] AST/typeck: `render` restricted to the closed set (`"graph"` |
    `"heatmap"` | `"timeline"`, plus `"bar_chart"`'s existing implicit
    default) — new `TypeErrorKind` variant for an unrecognized value,
    `typeck.rs::check_dashboard`'s sibling check next to
    `check_pattern_expr`/`check_format_expr`.
  - [ ] `ui_gen.rs`: `Metric` gains `render: MetricRender` (default
    `BarChart` — every existing `dashboard { chart ... }` unaffected);
    `build_visuals` alongside `build_charts`.
  - [ ] `ui_gen_template.html`: `renderDashboard`'s per-chart loop
    branches on `chart.render`; three new pure render functions
    (`renderForceGraph` — static circular/concentric layout, no physics;
    `renderHeatGrid` — binned density grid, not a real basemap;
    `renderTimelineList` — reuses the existing `row-enter`/
    `--stagger-ms` per-row entrance), same inline-SVG/zero-dependency,
    `var(--md-primary)`-token approach `renderBarChart` already uses.
  - [ ] `LANGUAGE.md`: extend §11's dashboard section (or a new §15b)
    documenting `visual`/`render` and each shape's expected JSON
    contract.
  - [ ] New test coverage: `tests/screen_dsl.rs` (or a new file) for the
    grammar/typeck shape, plus a real end-to-end `emit-ui`/`serve` case
    exercising at least the `"graph"` render kind.
  - [ ] Apply to `examples/ctms/ctms.nir`: Wallet Cluster Graph (Module
    7, `render: "graph"`) and Geo Heatmap (Module 5, `render:
    "heatmap"`), per `UI_CONSTRUCTS.md` §2's worked example.
  - [ ] Full `cargo test` green before `[DONE]`.

- `[OPEN]` **E3. `field { render: "countdown" }` — live-SLA/live-status
  fields.** Unblocks Case Queue, Alert Queue, Compliance Flag Queue,
  RTFDS Session/Fraud Alert Queue, Wallet Sanctions Screening Queue,
  Regulatory Filing Calendar, and the "SLA countdown per case" widgets
  on the Investigator/Supervisor Home dashboards (~9 screens). **No
  grammar change** — `field_override`'s body is already generic
  `kv_entry*`; this is one new closed-vocabulary value, same precedent
  `field { format: "email" }` already set. Full design:
  `UI_CONSTRUCTS.md` §3.
  - [ ] AST/typeck: `typeck.rs::check_screen` gains a
    `check_render_expr` sibling next to `check_pattern_expr`/
    `check_format_expr`/`check_min_max_expr` — `render` must be a string
    literal from the fixed set (`"countdown"` for v1), and only on an
    integer-typed field.
  - [ ] `ui_gen.rs`: `FieldSpec` gains `render: Option<&'static str>`,
    populated by `apply_field_overrides` alongside `pattern`/`min`/`max`.
  - [ ] `ui_gen_template.html`: table-cell renderer branches on
    `f.render === "countdown"` — client-side `Date.now()`-based
    remaining-time text, one shared `setInterval` for every `.countdown`
    node on the page (not one timer per row); an overdue row gets the
    existing semantic error/warning color token, no new theme tokens.
  - [ ] `LANGUAGE.md`: document `render` as a new `field { ... }` key
    (§11 or its own subsection), alongside `pattern`/`format`/`min`/
    `max`.
  - [ ] New test coverage: a `tests/screen_dsl.rs` typeck case (wrong
    field type/value rejected) plus a real end-to-end render check
    (`tests/emit_ui.rs` or a browser-level check that the countdown
    markup/interval is actually emitted).
  - [ ] Apply to `examples/ctms/ctms.nir`: `screen Case { field
    sla_deadline_unix { render: "countdown" } }` on the Case Queue
    screen (Module 3), per `UI_CONSTRUCTS.md` §3's worked example.
  - [ ] Full `cargo test` green before `[DONE]`.

- `[OPEN]` **E4. `action { show_result: true }` — preview/simulate
  actions.** Unblocks the "simulate before apply" half of Rule Engine
  Configuration, Scoring Weights Configuration, Policy Management
  Engine, RBAC/ABAC Policy Editor, Integrity/Tamper-Check, Audit Search
  & Export (~6 screens' worth of preview actions, not new screens on
  their own). **No grammar change** — one new boolean key inside the
  existing `action_decl` body. Full design: `UI_CONSTRUCTS.md` §4.
  - [ ] AST/typeck: `typeck.rs::check_screen`'s `action_decl` check
    requires the target fn's return type be `Result(json, E)` whenever
    `show_result: true` is present.
  - [ ] `ui_gen.rs`: `Action` gains `show_result: bool` (default
    `false`), threaded through `build_custom_action`.
  - [ ] `ui_gen_template.html`: the existing action-button click handler
    opens the existing modal/dialog primitive with the JSON response
    pretty-printed, when `action.showResult` and the call succeeded — no
    new CSS, no new animation.
  - [ ] `LANGUAGE.md`: document `show_result` alongside `style`/
    `confirm` in the `action` `kv_entry` list (§11).
  - [ ] New test coverage: a typeck case (non-`Result(json, E)` target
    rejected), plus a real end-to-end `serve` case confirming the
    response is actually returned to the client for display.
  - [ ] Apply to `examples/ctms/ctms.nir`: `"Simulate" -> 
    simulate_policy_threshold { show_result: true }` on the Policy
    Management Engine screen (Module 6), per `UI_CONSTRUCTS.md` §4's
    worked example.
  - [ ] Full `cargo test` green before `[DONE]`.

- `[OPEN]` **E5. Workflow stage stepper.** Unblocks Case Workflow/Stage
  Tracker (Module 3) — rendering the doc's own 4-stage model as a real
  progress stepper instead of a bare state-name label. **No grammar or
  DSL change at all** — everything the render needs (`workflow`'s
  declared `state` list, in order) is already parsed; this is purely a
  `ui_gen.rs`/`ui_gen_template.html` manifest-enrichment + rendering
  upgrade. Full design: `UI_CONSTRUCTS.md` §5.
  - [ ] `ui_gen.rs`: `WorkflowQueue` gains `all_states: Vec<String>` —
    the declared `workflow`'s own `state` list in declaration order,
    read straight off `ast::WorkflowDecl`.
  - [ ] `ui_gen_template.html`: `renderWorkflowScreen`/
    `renderWorkflowQueue` render a horizontal stepper (`wf.allStates`,
    current index = the row's own `state` position in that list) using
    the existing MD3 primary/on-surface-variant color tokens — no new
    theme tokens.
  - [ ] `LANGUAGE.md`: a short note in §14 (`workflow`) describing the
    generated stepper, since the underlying grammar is unchanged.
  - [ ] New/extended test coverage in `tests/workflow.rs` or
    `tests/emit_ui.rs` confirming `all_states`/current-index appear
    correctly in the manifest for a multi-state workflow.
  - [ ] Apply to `examples/ctms/ctms.nir`: the `CaseLifecycle` workflow
    (Investigation → ComplianceEscalation → Resolution →
    RegulatoryFiling, Module 3), per `UI_CONSTRUCTS.md` §5's worked
    example.
  - [ ] Full `cargo test` green before `[DONE]`.

- `[BLOCKED: E2–E5]` **E6. Rebuild `examples/ctms/ctms.nir` end-to-end.**
  E1 recreated the file as a real, verified `Matter`/`Transaction`/
  `MatterNote` proof-of-concept (one `workspace`, two panels) — still
  nowhere near the 8-struct plain-CRUD-only version `SCREENS.md`'s own
  intro names as this whole initiative's starting point, let alone the
  full 89-screen inventory. Once E2–E5 also land: rebuild it for real
  against the actual CTMS screen inventory (`SCREENS.md`), not just the
  handful of worked-example screens E1–E5 individually touch — then
  verify it actually renders (`nirdosha emit-ui`) and serves (`nirdosha
  serve --db`) end to end, the same "verified, not just written" bar
  this file holds every other `[DONE]` item to.
- `[DONE]` **E7. `PUBLIC_ROADMAP.md` — add a Track E entry.** Brief,
  external-facing mirror of Track D's own entry there — done as part of
  this same session, since it's small. — 2026-09-03.

---

## Suggested near-term order

Given "critical apps soon": the security review, the systematic
correctness-gap sweep, and now A1 (`transact` durability under real
failure conditions) are all `[DONE]` — the largest remaining
interpreted-path correctness risks have been closed and verified against
real process kills, not just trusted from the existing test suite. A2–A4
and C1 can run in parallel with each other and with the start of B1.
B1–B9 is the long track — pick up items as they become relevant to what's
actually being built, not in lockstep. Track D runs independently of all
of the above — D1 can start whenever native app delivery actually becomes
a priority, without waiting on A/B/C. Track E runs independently of
Tracks A–D too — E1 (`workspace`/`panel`, the highest-leverage item) can
start any time, without waiting on Track D's mobile work or anything in
A/B/C; only E6 (rebuilding `examples/ctms/ctms.nir`) waits on E1–E5
actually landing first.
