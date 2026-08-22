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

## NOT YET BUILT — tracked for future sessions

- **Pagination** (`paginate { page_size, total }`) — parses and
  typechecks (folded into `entries` as `paginate.page_size`/
  `paginate.total`), but `ui_gen.rs`/the client don't act on it yet: no
  enforced `list_*(page: i64, page_size: i64) -> ...` signature shape,
  no page-size-aware fetch, no page control in the table UI.
- **Per-field search** (`field x { searchable: true }`) — parses; not
  wired. Needs `interpreter.rs`'s `sql_bind_params` extended to unwrap
  `Option(str)` → NULL/value first (today's bind params are all
  required), typeck enforcing one added `Option(str)` param per
  searchable field on the backing `list_*` fn, and client-side debounced
  per-field search inputs.
- **Sortable fields** (`field x { sortable: true }`) — parses; not
  wired. Needs typeck to enforce `sort_field: str, sort_dir: str` params
  on `list_*`, and — since SQL can't parameterize a column name and
  Nirdosha can't concatenate one — a static per-column/per-direction
  query-branch pattern on the Nirdosha side (verbose, but structurally
  SQL-injection-proof by construction, the same shape `trade_finance.nir`
  already uses for its own sortable views).
- **Form modes** (auto-hide the primary key on `create`, read-only PK on
  `update`) — not yet expressed in the DSL at all; likely a pure
  `ui_gen.rs`-side change (infer from the struct's own first `i64`
  field, or a new `field <pk> { primary_key: true }` marker) with no new
  grammar needed beyond what already exists.
- **Field-level RBAC** (`field x { view: role(...), edit: role(...) }`)
  — parses and typechecks (shape-validated: must be a well-formed
  `role`/`claim` call). Not yet enforced anywhere: the security-critical
  half is server-side (`serve.rs` must redact ungated-view fields from
  every response and silently drop unauthorized writes), which hasn't
  been touched; the client hiding a field is cosmetic only and isn't
  built either.
- **The ten enterprise "lifestyle" features** from the design doc
  (loading skeletons already exist client-side from before this session;
  the rest — optimistic updates, bulk actions, column visibility toggle,
  export-to-CSV, keyboard shortcuts, undo-toast, saved filters, empty/
  error-state illustrations, inline validation messages — are
  undesigned, not just unbuilt) — each still needs its own grammar/
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
