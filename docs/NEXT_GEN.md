# Nirdosha — next-generation language & UI architecture (design discussion)

**Status: discussion only. Nothing in this document is implemented.**
This is the spec `docs/ROADMAP.md` Track F points to and will execute
against, written *before* the first line of it lands — the same order
`docs/MOBILE.md`/`docs/WORKFLOW.md`/`docs/TRANSACT.md` were written in for their own
features, not after.

## Why this came up

Grew out of a direct 2026-09-03 conversation, prompted by two real bugs
found and fixed the same session in `examples/ctms/ctms.nir`
(`docs/ROADMAP.md` Track E, entry E8 and its `IntegrityScan` follow-up):
`create_audit_log_entry` and `create_integrity_scan` both let a human's
own form submission assert something only the system itself should
ever be allowed to assert — a fabricated audit trail entry; a
"verified, no tampering" result for a scan that never ran. Fixing them
raised a real question: is Nirdosha's current UI-generation model —
naming-convention inference + escape-hatch `screen`/`workspace`/
`dashboard`/`action` blocks, one fixed web renderer, one fixed
CRUD-shaped action vocabulary — the right foundation to keep building
90+-screen apps on, or is it time to generalize it. Two adjacent
threads came up in the same conversation and are folded in here rather
than split across files: a real module/package system, and Hoare-style
per-function contracts.

## F1 — Target-independent UI manifest, multiple renderers (web / TUI / mobile)

### Where things actually stand today

`ui_gen.rs::build_screens` already walks the typechecked AST and emits
a target-agnostic JSON manifest — `Screen`/`FieldSpec`/`Action`/
`Metric`, zero HTML in it (the same finding `docs/MOBILE.md`'s own research
pass already established for Track D). `ui_gen_template.html` is the
**only** renderer of that manifest that exists — one browser-only
interpreter, with three real narrowings baked into it:

1. **A fixed action vocabulary.** Exactly 6 `Action.kind` values —
   `list`/`get`/`create`/`update`/`delete`/`custom` — hardcoded via
   `.find(a => a.kind === "...")` in `ui_gen_template.html` (confirmed
   directly: ~line 879-887, ~1241-1242, ~1924). A `custom` action can
   call any gated fn, but its *reaction* is fixed too (call, then show
   a result or refresh) — there is no way for a `.nir` author to
   declare "call this fn, then navigate to screen X" or "call this fn,
   then open a confirm dialog templated on its own return value,"
   short of a new Rust match arm plus new hand-written JS per new
   reaction shape.
2. **One fixed style** — Material Design 3, overridable only by a
   `--theme` JSON (colors/branding), never per-element.
3. **One consumer**: a browser. Nothing today reads the manifest
   except that one HTML/JS file.

### What generalizing this would take

- **A bounded interaction-verb vocabulary**, replacing the current 6
  fixed `Action.kind`s with something an app author can actually pick
  from and extend within limits — e.g. `call -> show_result`,
  `call -> navigate(<screen>)`, `call -> confirm(<template>)`,
  `call -> refresh_list`. Deliberately *not* a general scripting
  language embedded in the UI layer — that cuts against every other
  Nirdosha design choice (no string concatenation, affine handles,
  static safety proofs); the verb set stays small and enumerable, the
  same discipline `field { render: "countdown" }`'s own fixed
  `MetricRender` enum already uses.
- **A style/token layer instead of literal CSS** — color role,
  density, emphasis, not raw stylesheet rules — because CSS is
  meaningless outside a browser; a TUI renderer needs ANSI colors and
  box-drawing, a native mobile renderer needs its own platform design
  tokens. The existing `--theme` JSON is the right shape to grow, not
  replace.
- **A second renderer, chosen to be cheap to build first: TUI.** No
  native toolchain, no app-store distribution, no biometric/push
  complexity (`docs/MOBILE.md`'s own D2/D4 concerns) — just a second
  consumer of the same manifest, in-process, fast to iterate. Proving
  the manifest carries enough information for a genuinely different
  rendering paradigm (terminal, not pixels) is the cheapest real test
  of whether "manifest as target-independent IR" actually holds,
  before committing further engineering to Track D's native mobile
  renderer.
- **Interacts directly with Track D.** `docs/MOBILE.md`'s own D1 already
  frames a native renderer as "a second renderer of `ui_gen.rs`'s
  existing manifest" — this item is the generalization Track D would
  benefit from before or alongside D1, not a replacement for it.

### Real risk this creates, not present today

Once more than one renderer exists, and once one of them (a compiled,
installed mobile app, Track D) can't hot-reload the way a browser tab
does, **the manifest becomes a real versioned contract**, not an
ephemeral thing regenerated fresh every server start. Nothing today
gives it a schema version or a compatibility story. This needs solving
*before* Track D ships a first renderer, not discovered after an
already-installed app breaks against a reshaped manifest. See the risk
register below, R1.

## F2 — Real module/package system

**Status: `[DONE]`, shipped 2026-09-03, same session this section was
first written in.** All three pieces below — namespacing, visibility,
separate compilation — are real, tested end-to-end
(`crates/compiler/tests/modules.rs`, 12 tests), not stubbed, and both
documented collision bugs (`struct Pair` vs. the prelude's own; two
enums sharing a variant name, the `CurrencyCode::SAR`-shaped case) are
fixed and each covered by a real test that reproduces the exact
collision and confirms it now compiles *and runs correctly*, not just
typechecks. The original problem statement/design reasoning below is
kept unedited, since it's still accurate context for *why* the
implementation landed the way it did; this note records what actually
shipped and where it differs from the original sketch.

**Design decisions the implementation made, not fully anticipated by
the sketch below:**

- **The legacy `module "Display Name" { ... }` string form is left
  completely untouched**, rather than repurposed into the real
  namespace as the sketch's "recommend splitting nav from namespace"
  paragraph first framed it. Instead, `module` gained a *second*,
  additive form dispatched on the very next token (`Tok::Str` vs.
  `Tok::Ident`): `module Ident { ... }` is the real namespace: `nav:
  "Display Name"` inside it is the override the sketch wanted, but the
  legacy string form needed zero migration in either example app (19
  existing blocks between `ctms.nir`/`trade_finance.nir`), which a
  breaking-syntax-change approach would have forced.
- **No implicit same-module bare visibility** — the sketch didn't
  settle this either way. The shipped rule (`ast::scope_key`'s doc
  comment): a bare (unqualified) reference *only ever* resolves against
  a non-namespaced declaration, full stop — never a namespaced one,
  *even one in the very same module a reference textually sits inside*.
  Every cross-item reference into a namespace, sibling declarations
  included, has to spell out `Mod::Name`. This was the key
  simplification that let the whole feature ship with **zero**
  "which module is the interpreter currently executing" runtime
  context-tracking anywhere (`interpreter.rs` resolves every name
  exactly as it always did, purely from the literal call-site string,
  since a qualified string and a bare string can now never be
  ambiguous with each other by construction) — the alternative (real
  same-module implicit lookup) would have needed a call-stack-threaded
  "current namespace" through the whole tree-walking interpreter, a
  much larger and riskier change for an ergonomic nicety.
- **`::`-qualified names needed no new AST node.** `Ty::Named(String,
  _)`/`Expr::Call(String, _, _)`/`MatchArm.variant: String` all already
  just carry a `String` — a qualified reference is simply that string
  with `::` in it (`parser::parse_qualified_name`, one new helper reused
  at the 3-4 existing name-reference call sites), never a `Path`/
  `Vec<String>` type. `ast::scope_key(ns, name) -> String` is the one
  new function this whole design turns on: every namespace-aware
  registration map (`ast::TypeRegistry`, `typeck.rs`'s `type_names`/
  `callable_names`/`sigs`, `interpreter.rs`'s `fn_index`) keys by it
  instead of the bare declared name, and a written reference — bare or
  qualified — is used as-is as the lookup key, so resolution needs no
  extra logic beyond "look this exact string up."
- **A real, non-hypothetical implementation bug found and fixed along
  the way**: `Value::Enum`'s own runtime variant tag (and the analogous
  `match`-arm/`check_ty` comparisons in `interpreter.rs`) initially
  carried the *reference's own spelling* (bare or fully qualified)
  rather than the *declaration's* bare `variant.name` — harmless for
  every non-namespaced enum (`Option`/`Result`/etc., where the two
  always coincide), but broke both construction and `match` for a
  namespaced enum's own variants until three separate call sites
  (`interpreter.rs`'s `Expr::Call` construction arm, its `Value`-vs-
  `Ty` `check_ty`, and `Expr::Match`'s own evaluation) were fixed to
  consistently store/compare the bare variant name plus a separately-
  tracked canonical enum key. Caught by the manual end-to-end CLI test
  that exercised a real namespaced `match`, not by typechecking alone —
  a reminder that "typechecks" and "the interpreter agrees with typeck
  about what a value *is*" are genuinely separate claims for anything
  that changes a value's own runtime representation.
- **Piece 3 (`use`) is wired into every CLI command that loads a
  `.nir` file from disk** (`crates/compiler/src/loader.rs`, ~140 lines: read →
  parse → recursively resolve each `use` → typecheck each imported file
  standalone (own source, own diagnostics — this is also what keeps
  every error's `line:col` correct with *no* multi-file source map
  anywhere, since neither `TypeError` nor `ParseError` ever re-quotes a
  source snippet, checked directly) → merge only its `pub`, namespaced
  declarations) — `build`/`emit-llvm`/`emit-ast`/`emit-ui`/`serve`/
  `--sandbox-worker`, and plain `nirdosha <file.nir>` interpretation, all
  go through it (`lib.rs` gained one new `run_program_with_tracer_
  transact_and_workflow_log`, taking an already-loaded `Program` instead
  of lexing/parsing `src` itself, factored out of the existing
  `run_with_tracer_transact_and_workflow_log` without changing that
  function's own signature or behavior at all). The one disclosed gap:
  `nirdosha <file> --format=json`'s structured-diagnostic path still
  lexes/parses `src` directly, not through the loader — `use` in a
  `--format=json` run doesn't resolve yet (a program with no `use` is
  completely unaffected either way).
- **The compiled path (`nirdosha build`/`emit-llvm`) explicitly rejects
  any program containing a real-namespace declaration**, with a clear
  named error, rather than silently risking an LLVM symbol collision —
  a namespaced declaration's own `name` field is deliberately left
  unmangled (every pre-F2 consumer, `codegen.rs` included, reads
  `.name` directly), so two same-named fns in different modules would
  otherwise emit as the exact same unmangled LLVM symbol. Same
  incremental-porting posture Track B already uses for `transact`/`db`/
  `json`/etc.
- **Still genuinely `[OPEN]`, not silently unsupported**: `screen`/
  `dashboard`/`workflow`/`workspace` blocks can't reference a namespaced
  struct in this pass (they still resolve struct names bare-only,
  unchanged) — R1 (manifest versioning) and the rest of the risk
  register below are otherwise unaffected by this work landing.

### Where things actually stood before this (original problem statement)

`module "X" { ... }` is real syntax (`parser::parse_module_decl`) but
— verbatim from that function's own doc comment — "pure nav-grouping
sugar, not a real scoping construct... there is no separate
`Program`-shaped nested item list anywhere, no new namespace, nothing
else in the checker even knows this block existed." Every struct/fn/
enum in a `.nir` program lives in one single flat global namespace,
prelude types included, and there is no `::` token anywhere in the
lexer, no `import`/`use`/`include` keyword, and no way to compile more
than one file into one program. Every real app is therefore
necessarily one file: `examples/ctms/ctms.nir` is 3,766 lines (10
`module` blocks, all nav-only), `examples/trade-finance/
trade_finance.nir` is 3,977 lines (9 `module` blocks).

This already caused two real, independently-hit bugs, not a
hypothetical: a user-declared `struct Pair` colliding with the
prelude's own `Pair` (`examples/generics.nir`, `tests/generics.rs`, 6
failures — `docs/ROADMAP.md`'s 2026-08-27 entries); a user-declared
`enum ReportType { SAR, ... }` colliding with the prelude's
`CurrencyCode::SAR` (`examples/ctms/ctms.nir`, found and worked around
during the Track E6 rebuild). Both are the same root cause: one flat
name pool with no way to scope around a clash, made worse as the
prelude itself grows (row 12's `Money`/`Measure`/`CurrencyCode`/
`UnitCode` promotion already flagged this exact collision risk as
"reintroduced, still unfixed generally" — `docs/ROADMAP.md`, 2026-08-27).

### What a real module system needs — three separable pieces

1. **Namespacing** — qualified names (`Audit::Entry` vs
   `Ingestion::Entry`), so a user type and a prelude type, or two user
   types in different modules, can share a short name without
   colliding. Needs a `::` token in the lexer, which doesn't exist
   today, plus every pass that resolves a name by string
   (`typeck.rs`, `ast.rs::TypeRegistry`, `ui_gen.rs`'s own
   `find_screen_decl`/`find_fn` lookups) to become namespace-aware.
2. **Visibility** — something private to a module vs. exported to the
   rest of the program (and, separately, whether `ui_gen`'s
   naming-convention screen inference should even look inside a
   private module at all — an open design question, not an obvious
   default).
3. **Separate compilation / file-splitting** — an actual `import`/
   `use` mechanism, so a module can live in its own file. Changes a
   standing architectural assumption throughout the compiler: one
   `Program` today always comes from parsing exactly one file
   (`main.rs`'s own entry points all take one path).

### Real risk this creates for F1

`module` is *currently the only thing* deciding UI nav grouping
(`s.module` feeds straight into `Screen.module` in the generated
manifest). A real module system needs to decide up front whether "code
module" (namespace/visibility boundary) and "nav group" (which sidebar
section a screen appears under) stay the same concept or split into
two — every example so far happens to want them identical, which is
exactly the kind of coincidence that's dangerous to assume permanent.
Recommend splitting them: a `nav:` override inside the future
namespaced-module block, defaulting to the module's own name, the same
"override defaults to inferred" pattern `screen { title: "..." }`
already establishes.

## F3 — Hoare-style per-function contracts ("validators")

**Status: `[DONE]`, shipped 2026-09-03, same session this section was
first written in — then followed up the same day ("plz fix these
first") to close two of its own three disclosed gaps for real.** Real
`.nir` syntax (`validate <fn_name> { pre: ... post: ... }`), a
build-time static gate reusing the existing prover below, and a
separate runtime backstop for everything that prover can't reach — all
real, all tested end-to-end (`crates/compiler/tests/validate_contracts.rs`, 19
tests), not stubbed. A real, previously-undiscovered soundness bug in
the Tier-1 walker itself (early-return unreachability) was found and
fixed along the way. The follow-up round: `pre`/`post` are now real
type-checked against the target fn's actual signature (not just
existence-checked); a `Call` inside a provable function's body can now
be resolved via a real, sound, bounded interprocedural pass — a
callee's *independently-proven* contract becomes a usable axiom for its
caller, never an unproven one. The third disclosed gap
(`grammar_check`'s LALR build) was investigated in real depth and found
to be a materially bigger, genuine pre-existing finding — nirdosha's
own real grammar (no statement separator + a valid unary-minus prefix)
provably isn't expressible as unambiguous LALR(1), independent of
`validate` — not something to force a fix onto; left open, honestly,
not silently claimed done. Full writeup for all of this: `docs/ROADMAP.md`'s
F3 entry — this section is kept below as the original problem
statement/design reasoning, unedited, since it's still accurate context
for *why* each piece was built the way it was.

### Where things actually stand today

The hard part already exists and is real, not aspirational:
`crates/compiler/src/contract_check.rs::check_fn_contract` (`docs/ROADMAP.md` A12,
`[DONE]`) takes a real Hoare pair (`pre_logic`/`post_logic`), a named
`.nir` function, parses both predicates with the *same* grammar every
`.nir` expression already uses (`parser::parse_standalone_expr`, no
separate predicate language), asserts the precondition as a Z3
hypothesis, and either proves the postcondition for every input the
function's declared param types admit, or returns a real counterexample
naming exactly which clause failed. Demonstrated end-to-end against a
real function (`required_eyes_for_amount`), not a synthetic fixture.

Three real gaps between that and what "validators" would need:

1. **No `.nir` syntax reaches it at all.** The Hoare pair only ever
   comes from an external JSON extraction file
   (`scratch/extracted_typed_v1.json`, the trade-finance
   PRD-extraction pipeline) — a `.nir` author has no way to write a
   contract inline in their own source today.
2. **Scoped to one pure, loop-free, integer-only function, zero
   interprocedural reasoning.** Disclosed honestly in the code itself
   (`smt.rs`'s own module doc: "no per-function pre/postcondition
   inference exists yet"). The moment a function touches `db`/`json`/
   `http`, calls another function, or has a `while` loop — true of
   nearly every real function in `ctms.nir`/`trade_finance.nir` —
   `check_fn_contract` returns an honest `Unsupported`, never a wrong
   answer, but also never a proof.
3. **Nothing runs it automatically.** Invoked today from tests against
   a scratch file, not from `nirdosha build`/`typecheck`'s normal
   pipeline — there is no "self-check and fail" gate wired up anywhere
   yet.

### What "validators" would need to add

- **Real syntax**, feeding straight into the existing prover — e.g. a
  `validate <fn_name> { pre: <expr>; post: <expr>; }` block (mirrors
  `screen <Struct> { ... }`'s own "separate top-level declaration
  referencing an existing item" shape, rather than new
  per-parameter annotation syntax on the `fn` line itself). **Not**
  `requires(...)` — that keyword already means the role/claim auth
  gate throughout `.nir` today; reusing it for a Hoare precondition
  would collide with an existing, load-bearing meaning. Needs its own
  keyword.
- **A decided failure mode for the provable minority**: does
  `nirdosha build` hard-fail when Z3 finds a real counterexample
  against a declared contract? (Matches the project's existing Tier-1
  ethos — genuinely proved facts, checked, not asserted.)
- **A runtime-checked fallback for the `Unsupported` majority** —
  since gap #2 above means most real functions (anything touching
  `db`/`json`/`http`, anything with a loop or a call) can never be
  statically proven with the prover's current scope, "self-check and
  fail" for those needs the predicate asserted at the actual call
  boundary instead, the same place `serve.rs` already enforces
  `requires(role/claim:...)` server-side today. Static proof and
  runtime guard catch genuinely different classes of function, not two
  versions of one check — both are needed, not either/or.
- **Solving gap #2 properly (interprocedural summaries) is a much
  bigger, separately-scoped problem** — not a prerequisite for
  shipping the syntax + runtime-fallback path above, which can land
  first and cover the majority of real functions on day one.

## F4 — Composable UI layout & widget catalog

**Status: Phase A `[DONE]`, shipped 2026-09-04. Phases B and C `[OPEN]`,
not silently implied by A.** Grew out of a separate, later conversation
than F1-F3 above (the user asking directly for a fuller UI-element
catalog, real composability, and per-element styling) — related to F1's
own ambition but a genuinely different axis: F1 is about *renderers*
(one manifest, many targets) and the *action* vocabulary; F4 is about
*screen content* — arranging fields/widgets on one screen, and what
widgets exist to arrange. Kept as its own numbered item rather than
folded into F1, since F1's own scope (bounded interaction-verb
vocabulary, style/*token* layer, a TUI renderer) is untouched by any of
this and stays entirely `[OPEN]` — see the Sequencing note below for
the real, if narrow, way the two now do connect (R1's manifest-
versioning risk).

### The problem

`screen`/`dashboard`/`workspace` were flat: one struct's fields/actions
rendered as one implicit top-to-bottom list, with no container element
and no way to group/tab/arrange them, and no per-element style hook
beyond one app-wide `--theme` JSON. Confirmed by a three-agent
exploration pass before any code landed: **no recursive DSL construct
existed anywhere in this grammar** (every `parse_*_decl` fn is called
from exactly one site, none call themselves — `workspace`'s own `panel`
came closest but is hard-coded to exactly one level, no self-reference
in its own AST node) and **no generic "render this manifest node"
dispatcher existed in the template** (every visual concept — dashboard
tiles, workspace panels — is its own named `render*` function,
hardcoded to specific call sites, not a registry). Building a real
composability primitive meant introducing both of these mechanisms for
the first time, not generalizing something half-built.

### What Phase A actually shipped

- **`layout { ... }` inside `screen`** — a new, optional, additive
  field (`ScreenDecl.layout: Option<LayoutNode>`) holding a real tree:
  `row`/`column`/`grid`/`group`/`tabs` containers, `field`/`action`
  leaves that *reference* (never own) the screen's existing flat
  `fields`/`actions` lists by name, and a `Widget { kind, entries }`
  leaf for anything else (`divider`, `card`, `timeline` this phase).
  **Deliberately a reference tree, not an owning one** — this is the
  one design choice everything else depends on: `ui_gen.rs::
  field_gates_for_fn`/`update_gates_for_fn`/`field_validations_for_fn`
  all key purely by field name over the existing flat `ScreenDecl.
  fields`, so keeping overrides there instead of moving them inside the
  layout tree meant those three RBAC/validation functions needed *zero*
  changes and can't have regressed. A screen with no `layout` block
  renders exactly as before this shipped.
- **The first real recursive parser function** (`parser::
  parse_layout_node`, calling itself for a container's children, capped
  by a new `MAX_LAYOUT_DEPTH` — the same adversarial-input stack-
  overflow reasoning `MAX_PARSE_DEPTH` already has for expressions,
  sized far smaller since a hand-authored screen has no reason to nest
  anywhere near that deep). A container's own config (`gap`, `columns`,
  `title`, `collapsible`) is parsed as ordinary leading `kv_entry`s
  before any nested item — resolved by one new `peek2()` (this
  grammar's first two-token lookahead), not a special positional
  syntax, keeping every container the same uniform `{ kv_entry*
  layout_node* }` shape.
- **`typeck::check_screen_layout`** — walks the tree, confirms every
  `field`/`action` reference resolves (reusing `UnknownScreenField`; a
  new `UnknownLayoutAction` for actions, accepting either a custom
  action's own label or one of the five inferred CRUD kind names), and
  validates each `Widget`'s `kind` against a closed vocabulary
  (`UnknownRenderValue`, the same check `visual`/`panel`'s own `render`
  keys already share).
- **The generic dispatcher this was all building toward**:
  `renderLayoutNode(node, ctx)` in the web template — the first "take
  an arbitrary manifest node, render it by its own `type`" registry in
  this codebase. Every container wraps and recurses; a `field` leaf
  still goes through the *exact same* `buildFieldControl` every flat
  `buildForm` already used, an `action` leaf mirrors the existing row/
  custom-action click handler (`confirm`/`showResult` honored
  identically); nothing about field/action rendering was reimplemented,
  only arranged differently.
- **Three widgets pulled forward from Phase B on direct request**,
  proving the mechanism against real needs, not just its own structure:
  - **`searchable_select`** (`field <name> { render: "searchable_select"
    source: <Struct|fn> }`) — the scroll-paginated, search-as-you-type
    dropdown asked for by name. `source` naming a struct reuses the
    *already-existing* `/_nirdosha/table/<table>` route
    (`serve.rs::dispatch_table_query`) exactly as the main list
    screen's own pagination already does — real search plus scroll-
    triggered "load next page and append," with **zero new backend
    work**. `source` naming a fn instead falls back to one unpaginated
    `callFn` (a real, disclosed narrowing for the no-`--db` case, not a
    stub).
  - **`timeline`** — a `Widget` leaf that reuses the *already-
    implemented* `renderTimelineList` template function (previously
    reachable only from dashboard/workspace-panel contexts), now
    placeable directly in a layout.
  - **`badge`** — extends `FieldSpec.render`'s vocabulary past
    `"countdown"` (the exact extension `docs/LANGUAGE.md` had already
    flagged as a "candidate sibling, not designed yet") to a zero-
    payload-enum field, shown as a colored pill (a fixed 6-hue palette,
    deterministically hashed from the variant name — no per-variant
    config surface added this pass).
- **The `css:` escape hatch decision, made explicit rather than left
  ambiguous**: raw CSS, full power, **web-renderer-only** — a future
  TUI/native-mobile renderer simply ignores it, the same disclosed-
  narrowing pattern already used for `db`/`json`/`http` on the compiled
  path. This directly overrides F1's own original "style/token layer
  instead of literal CSS" framing for *this* escape hatch specifically
  (F1's reasoning — CSS means nothing outside a browser — is still
  correct and still applies to F1's own eventual TUI renderer; the
  user's explicit choice here was to ship the web-only power now rather
  than wait on a portable token vocabulary). **Not yet implemented in
  Phase A** — no per-element scoped class exists yet on any manifest
  node; this is Phase C's own first task, tracked separately below.

**Two real bugs found and fixed by manually testing in a real browser
(`nirdosha serve --db`), not caught by typechecking alone** — logged
here since both are the same class of lesson: a value's *shape*
(string literal vs. bare identifier) and a widget's *initial state*
matter beyond what type-level checking sees.
- `ui_gen.rs`'s `kv_str` (string-literal extraction) was used to read
  `source:`'s value for both `timeline` and `searchable_select` — but
  `source` always names a real fn/struct, a bare `Expr::Ident`, never a
  string literal, so it silently extracted `None` every time despite
  `typeck`'s own `check_fn_ref`/`check_searchable_select_source_expr`
  correctly requiring exactly that shape. Fixed by adding `kv_ident`
  (`ast.rs`'s `Expr::Ident` sibling of `kv_str`) and using it at both
  call sites; the type-level check was right all along; the value-level
  *extraction* on the manifest-building side was the actual bug.
- `buildSearchableSelect`'s pre-filled value (the raw stored id, e.g.
  `"7"` — there's no cheap way to resolve "id 7 is which row" without
  an extra fetch this phase doesn't do) sat in the input with the
  cursor merely placed after it; typing then *appended* (`"7carl"`)
  instead of replacing it, exactly the everyday browser-native-input
  behavior a real user would also hit. Fixed with `input.select()` on
  focus whenever a value is pre-filled — the next keystroke now
  replaces it.

### What's still genuinely `[OPEN]`

- **Phase B — the rest of the widget catalog**: `progress`,
  `multi_select`, `tag_input`, date/datetime pickers, `checkbox_group`/
  `radio_group`, `toggle`, `slider`, `breadcrumb`, `stepper`, plus
  Tier-2 items (`file_upload` blocked on R5's missing file/blob type;
  `calendar`/`gantt`/`org_chart`/`comment_thread`). Each is meant to be
  a small slice of the exact same `FieldSpec.render`-vocabulary-plus-
  typeck-check pattern `badge`/`searchable_select` already established,
  sequenced by which screens in `examples/ctms/ctms.nir`/
  `examples/trade-finance/trade_finance.nir` would actually use them —
  not built speculatively ahead of a real need.
- **Phase C — the `css:` per-element escape hatch itself**: no
  `FieldSpec`/`LayoutNode`/`Action` carries a generated scoped CSS
  class yet; `theme_value_is_safe`'s existing sanitizer (rejects
  `< > { }` outright) is too strict to reuse for real CSS rule bodies
  and needs its own real validation (balanced braces, no `<script`/
  `</style` breakout, no `@import`/`url(javascript:...)`) since this
  would splice server-side into the page's one `<style>` block.
- **A `list_<Struct>` fn not backed by the generic table route** (custom
  join/computed-column logic, `serverTableApi: false`) still has no
  `searchable_select` search/pagination story — would need a new
  `.nir`-level `search: str` parameter convention the fn author opts
  into, deliberately not solved this phase.
- **No scroll-pagination widget test exists beyond manual verification**
  — confirmed by hand against a real seeded SQLite table (search
  narrowing correctly, debounce, the append-vs-replace fix), but
  `compiler/tests/layout_dsl.rs` has no automated coverage of the
  *client-side* JS at all (this codebase has no JS test harness of any
  kind yet, for anything — not a gap unique to this feature).

## Risk register — surfaced the same session, not yet scoped as their own items

Found while discussing F1-F3 above; listed here so a future pass
doesn't have to re-discover them. Not duplicated into `docs/ROADMAP.md`
proper — cross-referenced where an existing track already owns the
fix.

- **R1 — Manifest versioning.** No schema version on the JSON manifest
  `ui_gen.rs` emits; becomes a real compatibility problem the moment a
  compiled, non-hot-reloading consumer (Track D mobile, or F1's TUI
  renderer) exists. Needs solving as part of F1, before Track D ships
  a first renderer. **F4 makes this a bigger surface, not a new risk**:
  `layout`'s own `LayoutNode` JSON shape (container/field/action/widget
  nodes) is now part of what any future non-web renderer would need to
  understand, on top of everything R1 already named — still owned by
  F1's own eventual fix, not scoped separately here.
- **R2 — Field-level write authorization is convention-matched, not
  declared.** `serve.rs::check_edit_gates` recognizes exactly one
  shape (`update_<S>` taking the whole struct positionally, comparing
  against stored values) and provides zero protection for `create_<S>`
  at all — confirmed directly this session fixing
  `create_audit_log_entry`/`create_integrity_scan` (`docs/ROADMAP.md` Track
  E, E8 and its follow-up). Every new action shape F1's generalized
  verb vocabulary adds reopens this same hole in a new place unless
  authorization becomes a declared property of an action, not a
  guessed one from its function's shape.
- **R3 — `requires(role: "X")` takes exactly one role.** No
  OR-of-roles. Every `SCREENS.md` row in `examples/ctms/` lists
  multiple actors ("Investigator, Compliance Officer, Admin"); the
  established workaround, repeated dozens of times across two
  ~4,000-line apps, is silently gating by just the first one. Worth
  fixing alongside F3 if the permission-modeling story is getting
  touched anyway.
- **R4 — No string concatenation anywhere in the expression grammar.**
  Already blocks ad-hoc search/query builders (`examples/ctms/
  SCREENS.md` Module 5, Global Search — both explicitly scoped out for
  this reason, `docs/ROADMAP.md` Track E6). Will also block F1's
  click-behavior vocabulary the moment it wants to compose a dynamic
  message ("Created case #42") rather than show a static string.
- **R5 — No file/blob/attachment type.** Blocks every real
  export/upload screen today (Case Export dossier, Audit export both
  return a JSON summary instead of a real signed bundle — `docs/ROADMAP.md`
  Track E6) and directly blocks Track D3 (camera/document capture,
  currently `[BLOCKED]` on this exact gap). F1's richer visual-element
  vocabulary will want an upload/attachment widget sooner or later
  with nowhere to attach it.
- **R6 — One `dashboard{}` block per program, parser-enforced.**
  Already forced every CTMS module's own "home" dashboard to merge
  into one shared block during the Track E6 build. Will get more
  cramped as more apps/screens are built, not less.
- **R7 — `db`/`json`/`http`/`transact`/identity are interpreter-only;
  no compiled path exists.** Pre-existing, tracked (`docs/ROADMAP.md` Track
  B), mentioned here only because F1/F2/F3 all add more surface area
  that stays permanently interpreter-bound as a result.

## Sequencing note

F1 and F2 were independent of each other (different subsystems:
rendering vs. name resolution) and could proceed in either order or in
parallel. F3 was independent of both — it touched `typeck`/a new
prover-facing syntax, not `ui_gen`/`serve`'s manifest or module
resolution at all — and shipped first, without needing F1 or F2 to
exist. F2 shipped second, also without needing F1: name resolution
(`ast::scope_key`, `typeck.rs`'s registration maps, `interpreter.rs`'s
`fn_index`) never touches `ui_gen.rs`'s manifest generation at all, and
F2's own explicit scope boundary (`screen`/`dashboard`/`workflow`/
`workspace` stay bare-name-only, can't reference a namespaced struct)
kept it that way deliberately — confirming the independence claim a
second time. R1-R7 above are the only real cross-dependencies among
what's left (R1 depends on F1, R3 relates to F1's action-vocabulary
work; R2, listed as related to F3 when this was first written, turned
out to be exactly the class of bug F3's own build closed for
`validate`-declared functions — it remains open for ordinary
`create_<S>`/`update_<S>` actions with no `validate` block at all; R4
blocks part of F1's click-behavior ambition specifically). None of
R1-R7 depend on F2, and F2's own landing didn't close or reshape any of
them.

F4 (Phase A) shipped third, also independent of F1 — it touches
`ui_gen.rs`'s manifest and `ui_gen_template.html`'s renderer, which F1
would eventually touch too, but F4 added a genuinely new tree
(`layout`) alongside the existing flat shapes rather than changing
anything F1's own bounded-action-vocabulary/style-token/TUI-renderer
plan already described — no rework needed on either side. **F1 remains
the only item still fully `[OPEN]`**, with F4's own Phases B and C
open alongside it (see F4's "What's still genuinely `[OPEN]`" above).
R1 is the one real link forward: whenever F1's own manifest-versioning
fix lands, it now has to account for `layout`'s tree shape too, not
just the flat `fields`/`actions` shapes that existed when R1 was first
written.
