# `screen`/`dashboard` DSL — build tracker

Same discipline as `examples/trade-finance/todo.md`: every capability
from the published design doc ([Nirdosha UI Engine](https://claude.ai/code/artifact/5f7928fb-8268-4f10-94fc-b59c10a90ce7),
Option 4 + its full v1 spec + the ten enterprise features + FAPI-strict)
gets an explicit entry here — BUILT, or tracked as NOT YET BUILT with a
reason — nothing silently dropped.

## BUILT (this session)

- **Grammar/AST/typeck** (`token.rs`, `ast.rs`, `parser.rs`, `typeck.rs`):
  real `screen`/`dashboard` keywords, contextual `field`/`action`/
  `paginate`/`tile`/`chart`, `paginate { ... }` folded into
  `"paginate.<key>"`-prefixed entries, every value slot reusing
  `parse_expr()`. Existence/shape typeck: struct/field/fn resolution,
  `view`/`edit` must be `role(...)`/`claim(...)` with string-literal
  args, dashboard tile/chart targets resolve. 11 tests in
  `tests/screen_dsl.rs`.
- **`ui_gen.rs` wiring**: declared `title` overrides the struct name;
  `field <name> { label: "..." }` attaches a `displayLabel` (a new,
  separate concept from `FieldSpec.label`, which already meant a
  readonly field's *type* label); `list`/`create`/`update`/`delete`
  entries override which fn backs each CRUD slot; declared `action`
  blocks become `kind: "custom"` actions carrying `label`/`style`/
  `confirm`, appended after the inferred CRUD set. 4 new tests in
  `tests/emit_ui.rs`, plus the pre-existing 6 kept green unchanged
  (progressive-fallback regression guard).
- **Client template** (`ui_gen_template.html`): `fieldLabel(f)` helper
  (`displayLabel` when set, else the raw name) used everywhere a field
  label is shown (form labels, nested-struct legends, table headers);
  `screen.title` used everywhere a screen's display name is shown (nav,
  heading, toasts) in place of the raw struct name; custom actions
  render as extra per-row buttons (`btn-filled`/`btn-outlined` from
  `style`, `window.confirm(...)` gate from `confirm`, disabled/gated the
  same way update/delete already are).
- **Live end-to-end proof**: `examples/store.nir` — `screen Product`
  declaring `title: "Catalog"`, `field name { label: "Product Name" }`,
  and `action "Restock +10" -> restock_product` (a new fn, `requires(role:
  "admin")`, real parameterized `UPDATE ... SET stock = stock + 10`).
  Verified via `nirdosha serve` + curl (admin login → create product →
  call `restock_product` → stock 5→15, persisted in SQLite) + a real
  browser screenshot showing "Catalog", the "PRODUCT NAME" column header,
  and the "Restock +10" button all rendering from the declared block.

## BUILT (later session) — pagination/sort/search/filter, by a different
## mechanism than originally envisioned below

Pagination, sorting, free-text search, and per-column filtering are now
real and **on by default for every screen**, but **not** via the
`paginate{}`/`field { searchable, sortable }` DSL keys this doc
originally proposed — those three keys still parse and typecheck but
remain genuinely unwired (see the still-open note at the bottom of this
section). Instead: `nirdosha serve --db <path>` exposes a new, generic
`/_nirdosha/table/<snake>` route (`serve.rs::dispatch_table_query`) that
serves every struct's table directly via real, allowlist-validated
`ORDER BY`/`LIMIT`/`OFFSET`/`WHERE ... LIKE` SQL — no per-screen
annotation needed, no enforced `list_*(page, page_size, ...)` signature
shape on the author's own function at all. `ui_gen_template.html`'s
`renderListScreen` uses this automatically (sortable `<th>`, a search
box, per-enum-column filter dropdowns, prev/next paging) whenever
`SERVER_TABLE_API` is true; with no `--db` flag (the default), every
table renders exactly as before this feature existed — one unpaginated
fetch, no controls shown. See `LANGUAGE.md` SS12 and `examples/
trade-finance/todo.md` for the full design and verification trail.

**Still genuinely open**: `paginate{}`/`searchable`/`sortable` themselves
remain parsed-but-inert — a program declaring `field x { sortable: true
}` today gets no different behavior than one that doesn't, since the new
mechanism is unconditional per-struct rather than opt-in per-field. Real
limitation, disclosed not hidden: the generic route always runs a plain
`SELECT * FROM <table>`, so a hand-written `list_<struct>` doing a join
or a computed column is invisible to it — the client's fallback to that
function's own unpaginated call (when `SERVER_TABLE_API` is false, or by
the author's own screen design) is the intended escape hatch, not a bug.

## BUILT (later session) — form modes (primary-key handling)

An edit form no longer renders an editable input for a field literally
named `id` — its value is still correctly included in the submitted
payload, read directly from the already-known row object at submit time
rather than from a (would-be) disabled input, which the HTML form-
submission spec would otherwise silently drop
(`ui_gen_template.html::buildForm`/`buildFieldControl`'s `isEdit`
parameter). No new grammar needed, exactly as anticipated below — no
`field <pk> { primary_key: true }` marker was needed either; a field
named literally `id` was already unambiguous in every real example.

## BUILT (later session) — field-level RBAC, real client + server enforcement

`field x { view: role(...)/claim(...), edit: role(...)/claim(...) }`
already parsed/typechecked; now actually enforced on both sides:

- **`ui_gen.rs`**: `FieldSpec` carries `view_roles`/`view_claim`/
  `edit_roles`/`edit_claim` (empty/`None` = ungated), computed by a new
  `kv_gate` helper (mirrors `kv_str`, but extracts a `role(...)`'s full
  role list — any-of, unlike `requires()`'s single role — or a `claim`
  pair) and applied via `apply_field_overrides` to *both* a screen's
  top-level `fields` (list/detail view) *and* every action's struct-
  typed param's `nested` fields (the create/update form's own, entirely
  separate `FieldSpec` tree — without this second pass, a gate declared
  in a `screen` block would only ever reach the list view, never the
  form, since `build_action` builds `params` via `build_field` with no
  knowledge of the screen block at all). Three new `pub` functions
  shared with `serve.rs`: `field_gates_for_fn` (by fn name — any CRUD
  slot), `field_gates_for_struct` (by struct name — for the generic
  table route, which has no `fn_name` to resolve from), `update_gates_
  for_fn` (the `update`-slot-only, edit-gates-only sibling the write
  check needs).
- **`ui_gen_template.html`**: `canViewField`/`canEditField` (mirror
  `canRun`'s any-of role check). A view-gated field the identity can't
  see is hidden from the list/detail view, the create form, and the
  update form — but, critically, still **passed through unchanged** in
  a submitted update payload (`buildForm`/`buildFieldControl`'s
  `passthrough`, generalizing the pre-existing `id`-field-hiding
  pattern) rather than merely omitted: Nirdosha's `create_<S>`/
  `update_<S>` take the *whole* struct positionally, so a caller who
  can't see one field still has to submit a value for every other
  field, and their own view of that hidden field is exactly what came
  back from the server (already redacted to `null`) — see the matching
  server-side pass-through-on-omit step below for why this round-trips
  correctly instead of blocking the rest of the update. An edit-gated
  field the identity can see but can't change renders `disabled`
  (still visible, its current value still correctly read via `.value`
  since this template's `getValue()` closures are plain JS property
  reads, never native `FormData` serialization).
- **`serve.rs` (the actual security boundary)**: `dispatch`'s response
  path redacts every view-gated field the caller isn't authorized for
  (`redact_gated_fields`, a generic walk over `{"ok":...}`/`{"err":...}`
  /array/object shapes — every shape a `list_`/`get_`/`create_`/
  `update_` fn in this codebase actually returns) to `null`, after
  `encode_value`, before the response is sent. The **write** side
  (`check_edit_gates`, only for a struct's `update` slot specifically,
  never `create` — see its own doc comment) reads the row's *currently
  stored* value for each edit-gated column via `--db`'s connection
  (same table/column-name convention `migrate.rs` already relies on)
  and rejects (403) only if the submitted value genuinely *differs* and
  the caller isn't authorized — comparing against "is the field
  present" alone would reject every update from an unauthorized caller
  outright, since the whole struct is always resubmitted, changed or
  not. The matching **pass-through-on-omit** step (right before
  decoding, at the JSON level) substitutes the currently stored value
  for any view-gated field the caller can't see whose submitted value
  is missing/`null` — otherwise a required, non-`Option` field decoding
  a `null` the client dutifully echoed back would itself be a decode
  error, blocking that caller from ever updating *any* field on the
  struct. The generic `/_nirdosha/table/<name>` route (`dispatch_table_
  query`) previously had **zero identity awareness at all** — it now
  extracts/validates the bearer token the same way `dispatch` does and
  applies the same redaction, closing a real gap (a view-gated field
  would otherwise have leaked straight through that route regardless of
  what `dispatch` enforced).
- **Worked example**: `examples/trade-finance/trade_finance.nir`'s
  `screen Counterparty { field risk_rating { view: role("compliance_
  officer", "bank_ops") edit: role("compliance_officer") } }` — verified
  live (curl matrix): `seller1`/`buyer1` see the rest of a counterparty
  with `risk_rating: null`; `bank1`/`comp1` see the real value, via both
  `/api/list_counterparty` and `/_nirdosha/table/counterparty`;
  `seller1` can freely `update_counterparty` other fields (status
  `draft` → `active` succeeded) while a `risk_rating` change from the
  same caller gets a clean 403 naming the field; `comp1`'s own
  `risk_rating` change succeeds and persists.
- New tests: `src/ui_gen.rs`'s own `#[cfg(test)]` module and a new
  `tests/field_rbac.rs` integration suite (real `--db` tempfile, no
  `tiny_http` — pure-function calls into `dispatch`, mirroring `tests/
  migrate.rs`'s style) covering redaction, the write-reject/-accept
  split, and the pass-through-on-omit round-trip.

## BUILT (later session) — field-level format validation: `pattern`/`format`/`min`/`max`

`field <name> { pattern: "<regex>" }` / `{ format: "email"|"phone"|
"date"|"url"|"uuid" }` / `{ min: ... }` / `{ max: ... }`, real client +
server enforcement, same architecture the field-level RBAC pass above
established (typecheck the declaration's shape, carry a resolved value
through `ui_gen.rs`, enforce for real in `serve.rs`, mirror cosmetically
on the client):

- **`ast.rs`**: `well_known_format_pattern(name) -> Option<&'static
  str>` — the single source of truth for `format`'s fixed, closed
  vocabulary (`email`/`phone`/`date`/`url`/`uuid`), shared by `typeck.rs`
  (validates the name) and `ui_gen.rs` (expands it into the same
  `pattern` slot an explicit `pattern:` would fill, so client and server
  enforcement are the literal same regex string either way).
- **`typeck.rs`**: `check_pattern_expr`/`check_format_expr`/
  `check_min_max_expr` — `pattern` must be a string literal that
  compiles as a valid regex (via the new `regex` crate dependency) and
  only apply to a `str` field; `format` must name a known format and
  only apply to a `str` field; `min`/`max` must be an int/float literal
  and only apply to a numeric field (`Ty::is_numeric`); `pattern` and
  `format` together on the same field is rejected as ambiguous. New
  `TypeErrorKind` variants: `InvalidFieldValidationExpr`,
  `FieldValidationTypeMismatch`, `InvalidRegexPattern`,
  `UnknownFieldFormat`, `ConflictingPatternAndFormat`.
- **`ui_gen.rs`**: `FieldSpec` carries `pattern: Option<String>`/
  `min`/`max: Option<f64>` (mirroring `view_roles`/`edit_roles`'s
  role in the RBAC pass), resolved by `apply_field_overrides` via new
  `kv_num` (numeric sibling of `kv_str`) and `resolve_pattern`
  (`pattern` directly, or `format` expanded through
  `well_known_format_pattern`). New `pub` `ValidatedField` struct +
  `field_validations_for_fn` — unlike RBAC's `update_gates_for_fn`,
  matches EITHER a struct's `create` OR `update` slot, since a format
  constraint applies just as much to a brand-new row as a changed one
  (no "currently stored" comparison needed at all, unlike edit gates).
  `field_json` emits `pattern`/`min`/`max` into the manifest.
- **`ui_gen_template.html`**: `buildFieldControl`'s final (text/number)
  branch sets the input's native HTML5 `pattern`/`min`/`max`
  attributes (plus a `title` on `pattern` so the browser's built-in
  validation tooltip actually names the constraint) — real, standard
  browser inline validation UI, no custom widget needed. Cosmetic only,
  same split as `canEditField`: a client with JS disabled, or a direct
  API call, is caught server-side regardless.
- **`serve.rs` (the actual enforcement boundary)**: `check_field_
  validations`, wired into `dispatch` right after args decode (before
  RBAC's edit-gate check) for any fn `field_validations_for_fn`
  resolves — needs no `--db` at all (checks the *incoming* value only,
  never a stored one), so unlike edit-gate enforcement this works
  without `--db` being passed. Rejects the first violating field with a
  `400` naming it (`field \`Struct.field\` does not match the required
  pattern` / `must be >= N` / `must be <= N`) — a plain string `err`
  body, not a nested object, so the client's existing `String(body.err)`
  toast renders it correctly.
- New tests: `tests/screen_dsl.rs` (9 new typecheck shape tests),
  `src/ui_gen.rs`'s own `#[cfg(test)]` module (7 new unit tests for
  `kv_num`/`resolve_pattern`/`validations_from_screen_decl`),
  `tests/emit_ui.rs` (1 new manifest-propagation test, plus 3
  pre-existing exact-substring assertions updated for the 3 new JSON
  keys landing between `label`/`name` alphabetically), and a new
  `tests/field_validation.rs` integration suite (real `tiny_http`
  server, no `--db`/auth needed) covering pattern/format/min/max
  rejection and acceptance on both `create_` and `update_`. Full
  compiler test suite (`cargo test`, every `tests/*.rs` file) reverified
  green after this feature landed.

## BUILT (later session) — identity role-mapping cache (`ROADMAP.md` Track A item A6)

The role-mapping half of A6's "identity admin console" — real, not just
the admin-editable table (which alone would've been inert). `RoleMapping
{ app_role, idp_role }` is an ordinary struct+CRUD-screen fixture (same
convention `EmailProviderConfig` established); the real work is
`serve.rs` actually consulting it:

- **`RoleMappingCache`**: `by_app_role: HashMap<String, Vec<String>>`
  (app_role -> every idp_role synonym) + `loaded_at`. Loaded eagerly
  once at `serve::run` startup (right after `migrate.rs` runs, so
  `role_mapping` is guaranteed to exist if declared) — not just lazily
  on first request, which would've left a mapping already in the DB
  inert for a full TTL window after every server restart. Refreshed at
  most once per `role_mapping_ttl()` (30s in production; overridable via
  `NIRDOSHA_TEST_ROLE_MAPPING_TTL_MS` for `tests/role_mapping.rs`,
  `#[cfg(test)]` isn't visible to integration-test crates so this was
  the only available seam), checked once per request — never a fixed
  background timer.
- **`identity_has_mapped_role`**: the translated check every
  `requires(role: ...)`/`view`/`edit` enforcement point now goes
  through instead of a bare `interpreter::identity_has_role` call —
  literal match first (so "no mapping configured" is exactly as fast
  and correct as before this feature existed), falling back to "does
  the identity have any `idp_role` this cache maps to the requested
  `app_role`." Threaded through `dispatch`'s own `requires` check,
  `identity_satisfies_gate` (and so every `view`/`edit` gate site:
  `redact_gated_fields`, `check_edit_gates`, `dispatch_table_query`),
  and `run`'s per-request loop — `Option<&Mutex<RoleMappingCache>>`,
  `None` without `--db`, the same all-or-nothing posture every other
  `--db`-gated feature in this file already takes.
- New tests: `tests/role_mapping.rs` (4 real-server integration tests —
  literal-role fallback still works with an empty/no `RoleMapping`
  table, an unmapped raw IdP role is rejected, a mapped IdP role is
  accepted only after the TTL refreshes (proving bounded staleness is
  real, not instant), and a mapping already in the DB before a fresh
  server start is live immediately, not after one TTL window). Full
  `cargo test` (50 test binaries) reverified green.
- **Table/form section headers**: unrelated small UX gap noticed and
  fixed alongside this work — a list screen's table had no heading of
  its own (only a generic "Records" subtitle up near the page title),
  ambiguous against the create form's "New `<Title>`" heading sitting
  directly above it on the same page. `renderListScreen` now also
  renders "Existing `<Title>`" right above the table.

## BUILT (later session) — general-purpose design-token theming + live reload

Mission-scoped: generalize the UI DSL beyond CRUD+dashboard, with a real
design-token system (animations, hover/press states, layout variants)
driven by `--theme`, tightly integrated with protobox's existing
`DesignSpec`/`resolve_design_tokens()` rather than a competing
invention. Full design in `LANGUAGE.md` §11b.

- **`ui_gen::Theme` redesigned as a 1:1 mirror of `resolve_design_
  tokens()`'s JSON shape** (`brand`/`neutral` 11-step ramps, `fonts`,
  `radius`, `shadow_card`, `density`, `motion`, `dark_mode`, `layout`,
  `type_scale`), replacing the old narrow 12-field color/radius/font
  subset. `theme_override_css` rewritten to emit the full custom-
  property set plus a fixed semantic-role → ramp-step mapping
  (`RampRoleStep`) deriving the existing `--md-*` roles from the raw
  ramps, so the template's 1000+ lines of `var(--md-*)` references
  needed no rewrite. `dark_mode` strategy dispatch (`media`/`class`/
  `always`/`none`) is real, not just color-value plumbing.
- **`ui_gen_template.html`**: went from 1 `:hover` rule and 0
  transitions/animations/keyframes to a real interaction system — 4
  named `@keyframes` (fixed vocabulary matching protobox's own "no
  other animate-* names exist" contract), `transition`/`:hover`/
  `:focus-visible`/`:active`/`:disabled` on every button/input/select/
  nav-item, screen-entrance + per-row staggered-list-entrance animation
  (`replayAnimation` helper forces a CSS animation to restart on every
  render), unconditional global `prefers-reduced-motion` honoring,
  CSS-only `app_shell`/`content_width` layout variants via a static
  `<html class="...">` computed once at generation time
  (`theme_html_class`) — `"auto"`/absent is byte-for-byte today's shell,
  unchanged, for every pre-existing `.nir` app.
- **Live reload (`serve.rs::ThemeCache`)**: `--theme` used to be read
  once at startup; now re-read on `GET /` at most once per 30s TTL
  (`theme_ttl()`, env-overridable via `NIRDOSHA_TEST_THEME_TTL_MS` for
  tests — the same reason `role_mapping_ttl` needed its own env seam:
  `#[cfg(test)]` isn't visible to an integration-test crate). A
  malformed/missing `theme.json` on reload is tolerated (logged,
  last-good page kept serving), never a crash.
- **protobox integration (`nirdosha.py::_theme_json_from_design_spec`)**:
  replaced the old hand-picked 8-field mapping (plus its now-dead
  `_relative_luminance`/`_on_color` WCAG-luminance helpers) with a
  direct `return resolve_design_tokens(spec)` — zero translation layer,
  can't drift, by construction. Verified with real protobox code (not
  mocked): ran `resolve_design_tokens(DEFAULT_DESIGN_SPEC)` through
  `be-v2`'s own `.venv`, fed the real output straight into `nirdosha
  emit-ui --theme`, confirmed the exact hex/font/easing/shadow values
  landed correctly in the generated CSS. Updated `tests/plugins/
  languages/test_nirdosha.py`'s theme assertions for the new JSON shape
  (`theme["brand"]["600"]` not `theme["primary_light"]`, etc.) — full
  18-test file green via `be-v2/.venv/bin/python3 -m pytest`.
- New tests: `tests/emit_ui.rs` (dark-mode-strategy tests +
  `theme_overrides_only_the_sections_it_sets`, rewritten for the new
  shape), a new `tests/theme_reload.rs` (3 real-server integration
  tests: not-yet-stale, stale-and-refreshed, malformed-reload
  resilience). Full `cargo test` (51 test binaries) reverified green.
- **Live browser + curl verification**: a real playful-motion/class-
  dark-mode/boxed-content theme served against `b2b.nir`, confirmed via
  screenshot (dark class applied, brand-ramp-derived button color,
  themed radius) and direct HTML inspection (keyframe/hover/transition/
  focus-visible counts, exact custom-property values for both the
  baked-in defaults and the theme override, the dark-mode bootstrap
  script, the `content-boxed` html class).

## NOT YET BUILT — tracked for future sessions
- **Eight of the ten enterprise "lifestyle" features** from the design
  doc. Two are now real, both from earlier sessions' work: loading
  skeletons (client-side, pre-dates the field-level-RBAC session), and
  "inline validation messages" (native HTML5 constraint validation from
  the `pattern`/`format`/`min`/`max` work above — not a custom-styled
  component, but a real inline message naming the violated constraint).
  The remaining eight — optimistic updates, bulk actions, column
  visibility toggle, export-to-CSV, keyboard shortcuts, undo-toast,
  saved filters, empty/error-state illustrations — remain undesigned,
  not just unbuilt — each still needs its own grammar/
  ui_gen/client design pass before it's buildable, not just a coding
  pass.
- **`--fapi-strict` serve flag** — proposed in the design doc (short
  token TTL, explicit CORS allow-list instead of today's wildcard `*`,
  mandatory idempotency keys, fresh step-up tokens for gated actions,
  minimal structured errors). None of it exists in `serve.rs` yet. Real
  FAPI sender-constrained tokens (mTLS/DPoP) and PAR/JARM stay
  structurally out of reach without an external IdP — Nirdosha's own
  stance (matching `oidc_validate_token`) is to be a correct relying
  party, never the IdP itself.

## Investigated, found out of scope: `grammar_export`/`grammar_check`

Per this session's plan, checked what these two repo-root crates
actually do before touching them:

- **`grammar_check`** — an independent LALR(1) conflict-freeness proof
  for `compiler/nirdosha.gbnf`'s twin, `src/nirdosha.lalrpop` (`lalrpop`
  refuses to generate a parser table for an ambiguous grammar, so a
  clean build *is* the proof).
- **`grammar_export`** — a second, independent GBNF interpreter plus
  `tests/fidelity.rs`, which parses every shipped `.nir` example with
  the *real* lexer/parser and separately checks it against
  `compiler/nirdosha.gbnf`, asserting both agree.

Running `tests/fidelity.rs` today (before any of this session's changes
were even a factor) already fails on `examples/transact.nir` — "accepted
by the real parser but rejected by nirdosha.gbnf". Inspecting
`nirdosha.gbnf` (119 lines) and `nirdosha.lalrpop` (182 lines) directly:
neither file has a `struct`/`enum`/`match`/`transact` rule at all — they
predate Row 11 entirely. This drift is pre-existing and far larger than
this session's own `screen`/`dashboard` addition; catching both files up
to the compiler's actual current grammar (structs, enums, match,
generics, `mq`, `json`, `http`/`https`, `db`, `sha256_hex`, `transact`,
`audited`, `effect`, `requires`/`acquire`, and now `screen`/`dashboard`)
is effectively a from-scratch rewrite of both grammar files, not an
incremental sync — out of scope for this session, tracked here rather
than silently left unmentioned or attempted piecemeal (which would leave
both files in a worse, inconsistent half-updated state).

## Docs still owed once the above lands

Each doc update below should land with the phase it documents, not be
batched to the end — see `LANGUAGE.md`/`GRAMMAR.md` for what's already
been updated this session (the v1 grammar landed above) versus what's
still owed as pagination/search/sort/RBAC/FAPI actually get built.
