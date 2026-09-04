# CTMS UI constructs — gap analysis + proposed Nirdosha DSL extensions

**Status: design only. Nothing in this document is implemented.** No
grammar change, no `typeck.rs`/`ui_gen.rs`/`serve.rs` change, no `.nir`
code landed anywhere in this repo as of this writing. This is step 2 of
the CTMS forcing-function initiative — `examples/ctms/SCREENS.md` (step 1,
left untouched by this doc, same `docs/PUBLIC_ROADMAP.md`/`docs/ROADMAP.md`
scannable-summary-vs-full-detail split) inventoried all 89 screens a real
CTMS needs; this doc takes every "hard" screen shape from that inventory
and asks, concretely, what Nirdosha's `screen`/`dashboard`/`module` DSL
(`docs/LANGUAGE.md` §11/§12, `crates/compiler/src/ui_gen.rs`,
`crates/compiler/src/ui_gen_template.html`) would need to grow in order to
generate it — not a wishlist, a specific proposal per construct, at the
same level of detail `docs/MOBILE.md` uses for its own not-yet-built
constructs (problem framing, grammar-shaped syntax, what it lowers to,
a worked example, open questions). A human reviews this design — and
cross-checks any grammar delta against `crates/grammar_check/`'s independent
LALR parser and the `crates/compiler/nirdosha.gbnf` export — before any of it is
built.

## The finding that matters

**Most of the 89 screens need no new construct at all.** Plain CRUD
list/detail (11 screens), report generation/scheduling (4), the calendar/
queue-shaped screens, and the bulk of the "config-as-data" policy/rule
screens (13) are already fully expressible with today's `struct` +
`list_/create_/update_/delete_<struct>` + declared `screen { field / action
}` — versioning is just fields (`version: i64`, `active: bool`,
`effective_from_unix: i64`), an activation timeline is just a date field,
"simulate before apply" is just a custom `action` calling an ordinary fn.
Section 6 below proves this with two worked examples, precisely so this
doc doesn't inflate the language surface for screens that don't need it —
the same restraint `docs/MOBILE.md`'s own "already reusable, unchanged" table
takes, and the same instinct `AGENTS.md`/`docs/LANGUAGE.md`'s existing
minimalism (one chart type, one animation vocabulary, a fixed seven-kind
form-control set) already commits to.

Genuine gaps cluster into three shapes, and one new construct
(`workspace`/`panel`) covers the single largest one *and* the one real
missing piece inside the config-as-data bucket (a parent record's related
child rows, e.g. a policy's list of conditions), rather than needing a
second, separate mechanism for each:

| Screen shape (from `SCREENS.md`) | Verdict |
|---|---|
| Composite multi-pane workspace (~14 screens) | **New top-level construct** — `workspace`/`panel` (§1) |
| Graph/network view (~4), geo/heatmap (~1) | **Small grammar extension** — one new `dashboard_item`/`panel_item` kind (§2) |
| Live-SLA/live-status queue (~9) | **New field-level render hint**, zero grammar change (§3) |
| Config-as-data policy/rule editor w/ versioning + simulate (~13) | **Mostly nothing new** — plain fields + one small `action` extension (§4, §6) |
| Workflow stage tracker (1 screen, embedded in others) | **Nothing new in the grammar** — `ui_gen.rs`-only manifest enrichment of state already parsed (§5) |
| Report generation/scheduling (~4) | **Nothing new** — ordinary struct + CRUD + custom actions (§6) |
| Ad-hoc query builder (~2, "tool" shape) | **Explicitly not designed here** — see §7 |

Sections below are ordered by leverage: which single change unblocks the
most screens across the 89.

---

## §1. `workspace` / `panel` — composite multi-pane screens

**Unblocks (directly or as the master-detail half of a config-as-data
screen):** Investigation Workspace, Alert Detail/Risk-Score Breakdown,
Behavioural Profile, ML Model Management, Case Collaboration & Comment
Thread, Evidence Management, Decision Panel, Escalation & Regulatory
Referral, Case Export/Audit Dossier, Fiat–Crypto Correlation, Entity 360/
Master Entity Profile, Exchange/Partner FI Portal, RTFDS Real-Time Action
Console — plus the child-row half of Policy Management Engine, Consent &
Data Handling Policy, Legal Hold Management, and the Data Profiling/
Quality Scorecard. **~18 of 89 screens**, by far the largest single
bucket in `SCREENS.md`.

### Is this really new?

Yes, honestly. A `screen <Struct> { ... }` block is fundamentally
one-struct-shaped: `build_screens` (`ui_gen.rs:749`) derives one `Screen`
per `struct`, its fields come from that one struct's own field list, and
its actions are that one struct's CRUD functions plus declared
`action`s. The Investigation Workspace needs a `Transaction` timeline, a
`Case`'s own KYC/geo fields, an `Alert` list, and free-text notes *all on
one screen, all scoped to one case instance* — there is no single struct
whose fields are "a case's transactions plus its alerts plus its notes."
Faking this with `module "Investigation" { ... }` groups screens in the
nav sidebar (`docs/LANGUAGE.md` §12) but still renders them as separate pages,
not one composed view a Supervisor can read at a glance. This is a real
gap, not an under-used existing feature.

### Proposed shape

```nirdosha
workspace CaseInvestigation {
    title: "Investigation Workspace"
    subject: Case                       // struct this workspace is opened per-instance-of

    panel "Transactions" {
        source: list_transaction_for_case
    }
    panel "Alerts & Cases" {
        source: list_related_alerts_for_case
    }
    panel "Notes" {
        source: list_case_note
        action "Add Note" -> add_case_note {
            style: "filled"
        }
    }
}
```

**Grammar** (mirrors `screen_decl`/`action_decl` almost exactly — see
`docs/GRAMMAR.md`'s existing `screen_decl ::= "screen" ident "{" screen_item*
"}"` production for the pattern this follows):

```ebnf
item           ::= fn_decl | struct_decl | enum_decl | screen_decl
                  | dashboard_decl | module_decl | workflow_decl
                  | workspace_decl                        // NEW

workspace_decl ::= "workspace" ident "{" workspace_item* "}"
workspace_item ::= panel_decl | kv_entry
panel_decl     ::= "panel" string "{" panel_item* "}"
panel_item     ::= action_decl | kv_entry
```

`workspace` is a new fully-reserved top-level keyword (dispatched on the
first token exactly like `screen`/`dashboard`/`module`/`workflow`
already are — confirmed zero collisions: `workspace` and `panel` are
unused as identifiers anywhere in `crates/compiler/src/*.rs` or
`examples/*.nir` today). `panel` is a **contextual** keyword, the same
"keyword only within this one leading position" treatment `field`/
`action`/`tile`/`chart` already get (`docs/GRAMMAR.md`'s own note on this):
disambiguated from a `kv_entry` the same way `action_decl` already is —
`panel_decl`'s second token is always a `string`, `kv_entry`'s second
token is always `:`, so the two-token dispatch is unambiguous, no
backtracking needed. `action_decl` inside `panel_item` is the *exact
same* production `screen_item` already uses — zero new syntax there.

**Typeck** (`typeck.rs`, mirroring `check_screen` at `968`): `subject`
must name a real `struct` with an `id: i64` field (the same
primary-key convention `update_<S>`/`delete_<S>`'s single-id-param shape
already assumes); every panel's `source` must resolve to a real `fn`
taking exactly one `i64` param and returning `Result(json, E)` for some
enum `E` (same shape check `check_dashboard`'s `chart_` fns already get,
generalized off the fixed `chart_`/`stat_` name prefix since `source` is
named explicitly here, not inferred); every panel `action`'s `->` target
resolves to a real fn, identical to `screen`'s own `action_decl` check.

### What it lowers to

**`ui_gen.rs`**: a new `struct Workspace { name, title, subject_struct:
String, subject_fields: Vec<FieldSpec>, panels: Vec<Panel> }` and `struct
Panel { title: String, source_fn: String, render: PanelRender /* see §2
*/, actions: Vec<Action> }`, built by a new `build_workspaces` pass
alongside `build_screens` — `subject_fields` is `build_field` run over
the subject struct's own fields, literally reusing today's field→control
mapping unchanged. `manifest_json` gains one more top-level array,
`WORKSPACES`, next to the existing `MANIFEST`/`WORKFLOWS`.

**`ui_gen_template.html`**: a new route shape, `#/ws/<snake>/<id>`,
alongside today's `#/<screen>` and `#/wf/<snake>`. The *nav entry* for a
workspace renders the subject struct's own list (literally
`renderListScreen`'s existing table-building code, reused unchanged —
`fetchTablePage`/the paginated `/_nirdosha/table/<name>` route both apply
exactly as they do for an ordinary screen), except each row's click
target is `#/ws/<name>/<row.id>` instead of the default detail expand. A
new `renderWorkspace(ws, id)` fetches `get_<subject>(id)` for the header
(rendered via the same `buildFieldControl`-based singular-form layout
`renderSingularScreen` already has, read-only) plus, in parallel, each
panel's `source` fn called with `{id}` as its one argument (same
`callFn` helper every action already uses) — one `<section class="card">`
per panel, title as a `screen-sub` heading, contents rendered per its
`render` kind (§2; default `"table"` reuses `renderListScreen`'s table-
building sub-routine directly, given the fetched rows in place of a
network call). Panel actions render as ordinary buttons using the exact
same gating/confirm/style code path `buildActionButton` (or its
equivalent) already has for a screen's custom actions — no new client
logic there at all. Motion: the workspace root gets `screen-enter`
exactly like any other screen; each panel card gets the existing `.card`
styling and, where it holds a list, the same `row-enter`/`--stagger-ms`
per-row entrance every other table already has — no new animation
vocabulary, reusing `docs/LANGUAGE.md` §11b's fixed four-keyframe set as-is.

**`serve.rs`**: **nothing.** Every panel's `source` fn and every panel
action are ordinary `.nir` functions already exposed at `POST /api/<fn>`
by the existing dispatcher (`serve.rs::dispatch`), already re-checking
`requires(role/claim)` server-side, already redacting `view`-gated fields
per `redact_gated_fields`. A workspace is a client-side *composition* of
calls that already exist and are already secured — this is the whole
reason it's cheap: no new route, no new server-side authorization
surface, no new trust boundary to design.

### Worked example — Investigation Workspace (Module 3)

```nirdosha
// Reuses Case/Transaction/Alert from the existing ctms.nir foundation
// (git show c6d6e3e:examples/ctms/ctms.nir) plus one new join-shaped fn
// per panel -- each is an ordinary fn, nothing workspace-specific about
// its own body.

struct CaseNote {
    id: i64,
    case_id: i64,
    author: str,
    body: str,
    created_unix: i64,
}

fn list_transaction_for_case_inner(conn: db, case_id: i64) -> Result(json, ErrorCode) requires(public) {
    let r: Result(json, str) = db_query(
        conn,
        "SELECT t.id AS id, t.amount_cents AS amount_cents, t.timestamp_unix AS ts, t.geo_country AS geo FROM transaction t JOIN alert a ON a.transaction_id = t.id JOIN case c ON c.alert_id = a.id WHERE c.id = ?",
        case_id
    )
    return match r {
        Ok(rows) => Ok(rows),
        Err(e) => Err(DbError(e)),
    }
}

fn list_transaction_for_case(case_id: i64) -> Result(json, ErrorCode) requires(role: "investigator") {
    return match db_connect("ctms.db") {
        Ok(conn) => list_transaction_for_case_inner(conn, case_id),
        Err(e) => Err(DbError(e)),
    }
}

// list_related_alerts_for_case, list_case_note, add_case_note: same
// one-param-in / Result(json, ErrorCode)-out shape, omitted for brevity.

workspace CaseInvestigation {
    title: "Investigation Workspace"
    subject: Case

    panel "Transaction Timeline" {
        source: list_transaction_for_case
        render: "timeline"                 // see SS2 -- vertical timeline, not a table
    }
    panel "Related Alerts" {
        source: list_related_alerts_for_case
    }
    panel "Notes" {
        source: list_case_note
        action "Add Note" -> add_case_note {
            style: "filled"
        }
    }
}
```

A Supervisor opening `#/ws/case_investigation/482` sees the case's own
KYC/status fields at the top (from `get_case(482)`, read-only), then
three cards below it: a chronological transaction timeline, a table of
related alerts, and a notes panel with an "Add Note" button — one screen,
composed entirely from functions and structs that already exist as
ordinary CRUD building blocks.

### Open questions

- **Panel refresh.** A panel action that mutates data (e.g. "Add Note")
  should refresh just that panel, not the whole workspace — needs a
  small client-side convention (re-call `source` after a successful
  action call scoped to that panel), not a server change. Not designed
  in depth here.
- **Nested workspaces / panel-of-panels.** Deliberately excluded, same
  "single-level only" discipline `module` and `transact` slots already
  enforce (`docs/GRAMMAR.md`'s note on `module_decl`) — a panel's `render`
  can select a richer visualization (§2) but never another `workspace`.
- **Does a panel need its own pagination?** `dispatch_table_query`'s
  page/sort/search machinery is scoped to a whole struct's table, not an
  arbitrary joined query — a panel with hundreds of rows gets whatever
  its `source` fn itself returns, unpaginated, same honesty limit
  `UI_DSL_TODO.md` already discloses for a hand-written `list_<struct>`
  under `SERVER_TABLE_API`. Left as a real, disclosed limitation, not
  designed around here.

---

## §2. `visual` dashboard/panel item + `render:` key — graph, heatmap, timeline

**Unblocks:** Case Linking/Entity Graph, Wallet Cluster Graph, Graph
Network Explorer, Session/Device Linkage View (graph — 4 screens), Geo
Heatmap (1 screen), plus upgrades the Investigation Workspace's
transaction-timeline panel and any evidence/action-log panel from a flat
table to a real timeline. **~6 screens directly, reused inside §1's
panels for several more.**

### Is this really new?

Yes for the visualization itself (today's `dashboard` has exactly one
chart kind — an inline-SVG bar chart, `docs/LANGUAGE.md` §11's own listed
"deliberate non-goal": "no line/scatter/heatmap/treemap/geo/3D"). But the
*grammar* delta is small and deliberately reuses the existing
`dashboard_item`/`kv_entry` shape rather than inventing a parallel
mini-language per chart type — one new contextual keyword plus one new
`render:` key, not a graph-specific DSL.

### Proposed shape

```ebnf
dashboard_item ::= ("tile" | "chart") string "->" ident
                  | "visual" string "->" ident ("{" kv_entry* "}")?   // NEW
```

`visual`'s own `kv_entry`s: `render: "graph" | "heatmap" | "timeline"`
(closed vocabulary, same "fixed, checked set" treatment `field { format:
... }`'s five values already get — `typeck.rs::check_screen`'s sibling
`check_dashboard` grows one more `TypeErrorKind` variant for an
unrecognized `render` value). No grammar change needed inside `panel`
(§1) at all — `render:` there is just one more ordinary `kv_entry` on an
already-generic `panel_item`.

Each `render` kind fixes its backing fn's expected JSON shape, the same
"the fn returns exactly this shape, usually one `db_query` with the
right column aliases" contract `chart_<name>`'s `{label, value}[]`
already establishes:

| `render` | Expected JSON shape | Typical source |
|---|---|---|
| `"graph"` | `{"nodes": [{"id", "label", "risk"?}], "edges": [{"source", "target", "weight"?}]}` | a `db_query` joining an entity table to itself via a relationship table |
| `"heatmap"` | `[{"lat", "lng", "weight", "label"?}]` | `db_query` grouping transactions/alerts by geo bucket |
| `"timeline"` | `[{"ts", "label", "detail"?}]`, any order (client sorts by `ts`) | `db_query` ordering by timestamp |

### What it lowers to

**`ui_gen.rs`**: `Metric` (`ui_gen.rs:196`) gains a `render: MetricRender`
field (`enum MetricRender { BarChart, Graph, Heatmap, Timeline }`,
default `BarChart` for anything declared via `chart`, so every existing
`.nir` program's `dashboard { chart ... }` blocks are byte-for-byte
unaffected); `build_charts` (`570`) grows a `build_visuals` sibling that
also handles `visual` items. `manifest_json` serializes `render` onto
each `CHARTS` entry unchanged in shape — `ui_gen_template.html` branches
on it at render time rather than needing a second top-level JSON array.

**`ui_gen_template.html`**: `renderDashboard`'s per-chart loop (`1199`)
branches on `chart.render` instead of always calling `renderBarChart`:

- `renderForceGraph(data)` — **v1 is deliberately a static layout, not a
  physics simulation**: nodes placed evenly around a circle (or, when
  every node carries a `risk` field, concentric rings by risk band —
  highest risk innermost, drawing attention to the center), edges as
  straight `<line>`s with `stroke-width` scaled by `weight`, same
  inline-SVG, zero-dependency approach `renderBarChart` (`1132`) already
  uses, same `var(--md-primary)`/`var(--md-on-surface)` theme tokens.
  Explicitly **not** drag/zoom/force-directed physics in v1 — see §7.
- `renderHeatGrid(data)` — bins `{lat, lng, weight}` points into a fixed
  N×M grid (equirectangular bucketing, no basemap), each cell an SVG
  `<rect>` shaded by `var(--md-primary)` at an opacity proportional to
  summed weight in that bucket. Explicitly **not** a real map with tiles
  or borders — see §7.
- `renderTimelineList(data)` — a vertical list, one row per event
  (timestamp left, label/detail right), reusing the existing `row-enter`/
  `--stagger-ms` per-row entrance animation unchanged.

All three are new pure functions alongside `renderBarChart`, same "no
external charting library" stance the file's own header comment already
commits to.

**`serve.rs`**: nothing — same as §1, `visual`'s backing fn is an
ordinary `.nir` function already reachable via `POST /api/<fn>`.

### Worked example — Wallet Cluster Graph (Module 7)

```nirdosha
struct WalletEdge {
    source_wallet: str,
    target_wallet: str,
    hop_count: i64,
}

fn graph_wallet_clusters_inner(conn: db) -> Result(json, ErrorCode) requires(public) {
    // A real implementation shapes two db_query calls (nodes, edges)
    // into the {"nodes":[...], "edges":[...]} envelope client-side is
    // expected to receive -- sketch, not the literal binding shape.
    let r: Result(json, str) = db_query(
        conn,
        "SELECT w.address AS id, w.address AS label, w.risk_score AS risk FROM wallet w WHERE w.cluster_id IS NOT NULL"
    )
    return match r {
        Ok(rows) => Ok(rows),
        Err(e) => Err(DbError(e)),
    }
}

fn graph_wallet_clusters() -> Result(json, ErrorCode) requires(role: "fraud_analyst") {
    return match db_connect("ctms.db") {
        Ok(conn) => graph_wallet_clusters_inner(conn),
        Err(e) => Err(DbError(e)),
    }
}

dashboard {
    tile "High-Risk Wallets" -> stat_high_risk_wallet_count
    visual "Wallet Clusters" -> graph_wallet_clusters {
        render: "graph"
    }
}
```

### Open questions

- **`heatmap`'s honesty limit** is load-bearing enough it's restated in
  §7, not just here: this is a density grid, never a real basemap.
- Should `render` also be legal directly on a `screen`'s own dashboard-
  adjacent metric, or only inside `dashboard`/`panel`? Proposed: only
  `dashboard`/`panel` — a `screen`'s per-row table stays a table, keeping
  `Screen`'s own rendering path (§1's `render: "table"` default) simple.
- Graph node/edge count ceiling for the static circular layout before it
  becomes unreadable (a few dozen nodes, informally) — not enforced
  anywhere in this design; a real implementation likely wants a
  server-side or client-side cap with a "showing top N by risk" note.

---

## §3. `field { render: "countdown" }` — live-SLA and derived-status fields

**Unblocks:** Case Queue, Alert Queue, Compliance Flag Queue, RTFDS
Session/Fraud Alert Queue, Wallet Sanctions Screening Queue, Regulatory
Filing Calendar, plus the "SLA countdown per case"/"cases nearing SLA
breach" widgets on the Investigator/Supervisor Home dashboards — **~9
screens**, all from `SCREENS.md`'s "list/queue (live-status flavored)"
bucket.

### Is this really new?

**No — this is exactly the small extension the coordinator's framing
anticipated.** A case's `sla_deadline_unix: i64` field already exists as
an ordinary struct field, already renders in a table column today — the
only thing missing is telling the client "render this field as a live
countdown, not a raw integer." That's one new value in `field { render:
"..." }`'s vocabulary, following the exact precedent `field { format:
"email" }`'s closed vocabulary already set (`well_known_format_pattern`,
`ui_gen.rs` near `FieldOverride`) — except `render` (unlike `format`)
never becomes a validation `pattern`; it's a display-only client hint,
so it needs its own key, not an overload of `format`'s existing
validation-regex semantics.

### Proposed shape

```nirdosha
screen Case {
    field sla_deadline_unix {
        render: "countdown"
    }
}
```

No grammar change at all: `field_override`'s body is already `"field"
ident "{" kv_entry* "}"`, and `render` is just one more `ident ":"
expr`. `typeck.rs::check_screen`'s field-shape checks (next to
`check_pattern_expr`/`check_format_expr`/`check_min_max_expr`) grow one
more sibling, `check_render_expr`: `render` must be a string literal from
a fixed set (`"countdown"` for now — see open questions for candidate
siblings), and only on an integer-typed field for `"countdown"`
specifically (a unix-seconds deadline).

### What it lowers to

**`ui_gen.rs`**: `FieldSpec` (`80`) gains `render: Option<&'static str>`,
populated by `apply_field_overrides` (`727`) alongside `pattern`/`min`/
`max` — passed straight through to the JSON manifest, no computation at
generation time (the countdown value depends on wall-clock time, which
`ui_gen.rs` obviously can't know at generation time).

**`ui_gen_template.html`**: `buildFieldControl`/the table-cell renderer
checks `f.render === "countdown"` and, instead of `String(value)`, renders
a `<span class="countdown">` whose text is computed client-side as
`value - Math.floor(Date.now() / 1000)` (seconds remaining), formatted as
`"23m left"`/`"2h 14m left"`/`"OVERDUE"` (negative), and re-computed on a
single shared `setInterval(..., 1000)` that walks every `.countdown`
node currently in the DOM and updates its text — one interval for the
whole page, not one per row. Overdue rows additionally get a CSS class
(`countdown-overdue`) driving a color token, no new theme tokens needed
(reuses the existing semantic error/warning color role already in
`ui_gen::Theme`'s ramp-role mapping).

**`serve.rs`**: nothing. This is deliberately **static-field client-side
ticking, exactly the approach the coordinator's own framing preferred**
over a poll or push mechanism — the deadline value itself already comes
down with the row (from `dispatch_table_query`/the ordinary `list_*`
call) exactly as it does today; only the *display* of that one field
changes, entirely in the browser, no new network traffic at all.

### Worked example — Case Queue (Module 3)

```nirdosha
screen Case {
    title: "Case Queue"
    field sla_deadline_unix {
        label: "SLA"
        render: "countdown"
    }
    field status {
        label: "Status"
    }
}
```

Every row in the Case Queue table now shows a live "12m left" /
"OVERDUE" chip in the SLA column, ticking down in the browser with zero
additional requests — directly satisfying `SCREENS.md`'s own "SLA
countdown per case" requirement for the Investigator Home and Case Queue
rows.

### Open questions

- Candidate sibling `render` values worth naming even though not
  designed here: `"badge"` (color a zero-payload-enum field by variant,
  e.g. `AlertSeverity::Critical` in red — cosmetic only, same "default
  polished UI" instinct this project's own theming work already leans
  into) and `"progress"` (a 0–100 numeric field as a bar, e.g. a
  compliance score). Neither is needed by any CTMS screen badly enough
  to design here; naming them now is just to confirm `render` as a key
  is the right shape to extend later, not a one-off hack for countdowns
  specifically.
- A dashboard *tile* (not a table field) showing "N cases nearing SLA
  breach" is just an ordinary `stat_<name>() -> i64` counting function —
  no `render` involvement at all, already fully covered by today's
  dashboard tiles.

---

## §4. `action { show_result: true }` — preview/simulate actions

**Unblocks the "simulate before apply" half of:** Rule Engine
Configuration ("test rule against sample transactions"), Scoring Weights
Configuration ("simulate against historical data"), Policy Management
Engine, RBAC/ABAC Policy Editor ("simulate against a sample identity"),
Integrity/Tamper-Check screen ("run manual scan"), Audit Search & Export
("verify integrity") — **not new screens, a missing piece inside
several already-CRUD-shaped config-as-data screens from `SCREENS.md`.**

### Is this really new?

Barely — it's the smallest extension in this document, one boolean key
on an already-existing construct. Today's declared `action "<label>" ->
<fn> { style, confirm }` (`docs/LANGUAGE.md` §11) calls `fn` and, per
`ui_gen_template.html`'s existing convention, just refreshes the row/list
on success — there's no way today to *show the caller what the fn
returned*. A "Simulate" action's entire value is its return value (e.g.
"23 transactions would be affected by this rule change") — without
somewhere to display that, the action is useless even though the
underlying fn call already works end-to-end today.

### Proposed shape

```nirdosha
action "Simulate" -> simulate_policy_threshold {
    style: "outlined"
    show_result: true
}
```

`show_result` is one more boolean `kv_entry` inside `action_decl`'s
existing body — no grammar change. `typeck.rs::check_screen`'s
`action_decl` check requires the target fn's return type be
`Result(json, E)` when `show_result: true` is present (nothing to show
for a bare `Result(i64, E)` beyond what the row refresh already implies)
— one more shape check next to the existing fn-resolution check.

### What it lowers to

**`ui_gen.rs`**: `Action` (`143`) gains `show_result: bool` (default
`false`), threaded through `build_custom_action` (`469`) from the
declared `ActionDecl`.

**`ui_gen_template.html`**: the button handler that already calls
`callFn(action.fn, ...)` on click, on a successful response where
`action.showResult` is set, opens the existing modal/dialog primitive
(reused, not new) with the JSON response pretty-printed as a `key:
value` list — same `el()` DOM-builder convention every other rendering
function in this file already uses. No new CSS, no new animation.

**`serve.rs`**: nothing — `simulate_policy_threshold` is an ordinary
read-mostly `.nir` function (typically running the *same* query the real
apply path would, without the final `UPDATE`/`INSERT`) already reachable
via `POST /api/<fn>`.

### Worked example — Policy Management Engine (Module 6)

```nirdosha
fn simulate_policy_threshold_inner(conn: db, threshold_cents: i64) -> Result(json, ErrorCode) requires(public) {
    let r: Result(json, str) = db_query(
        conn,
        "SELECT COUNT(*) AS label, threshold_cents AS value FROM transaction WHERE amount_cents > ?",
        threshold_cents
    )
    return match r {
        Ok(rows) => Ok(rows),
        Err(e) => Err(DbError(e)),
    }
}

fn simulate_policy_threshold(threshold_cents: i64) -> Result(json, ErrorCode) requires(role: "compliance_officer") {
    return match db_connect("ctms.db") {
        Ok(conn) => simulate_policy_threshold_inner(conn, threshold_cents),
        Err(e) => Err(DbError(e)),
    }
}

screen CompliancePolicy {
    title: "Policy Management Engine"
    action "Simulate" -> simulate_policy_threshold {
        style: "outlined"
        show_result: true
    }
}
```

A Compliance Officer clicks "Simulate" on a policy row, and the count of
transactions that would breach the proposed threshold appears in a modal
— before the policy is ever activated.

### Open questions

- Should `show_result: true` on a response shaped like `{label,
  value}[]` auto-render as the existing `renderBarChart` instead of a
  raw key/value dump? Cheap to add later, not required for the modal to
  be useful — left as a follow-on, not designed here.

---

## §5. Workflow stage stepper — no grammar change, `ui_gen.rs`-only

**Unblocks:** Case Workflow/Stage Tracker (Module 3) — rendering the
doc's own 4-stage model (Investigation & Enrichment → Compliance
Escalation/Legal Hold → Resolution → Regulatory Filing) as a real
progress stepper instead of a bare state-name label.

### Is this really new?

No — everything the render needs is **already parsed**. A `workflow`
block (`docs/LANGUAGE.md` §14, `docs/WORKFLOW.md`) already declares its `state`s
in order, and `workflow_lower.rs` already synthesizes
`list_<workflow>_pending_for_me`, whose per-row response already
includes `state`/`state_label` (`ui_gen.rs`'s `WorkflowQueue`, `236`).
What's missing is purely that the *manifest* never carries the workflow's
**full ordered state list**, only the current row's own state — so
`ui_gen_template.html` has no way to draw "step 2 of 4" without it. This
is a `ui_gen.rs`-side omission, not a language gap.

### What it lowers to

**`ui_gen.rs`**: `WorkflowQueue` (`236`) gains `all_states:
Vec<String>` — the declared `workflow`'s own `state` list, in
declaration order, read straight off the already-typechecked AST
(`ast::WorkflowDecl`), zero new parsing.

**`ui_gen_template.html`**: `renderWorkflowScreen`/`renderWorkflowQueue`
render a horizontal stepper (`wf.allStates`, current index = position of
the row's own `state` in that list) above each row's detail, replacing
the plain `state_label` text with `●━●━○━○` "step 2 of 4"-style markup —
same MD3 color tokens (`var(--md-primary)` for completed/current,
`var(--md-on-surface-variant)` for upcoming), no new theme tokens.

**`serve.rs`**: nothing — `pending_fn`'s response shape is unchanged.

### Worked example

```nirdosha
workflow CaseLifecycle {
    data {
        case_id: i64
    }
    state Investigation
    state ComplianceEscalation
    state Resolution
    state RegulatoryFiling

    on Escalate -> ComplianceEscalation
    on Resolve -> Resolution
    on FileReport -> RegulatoryFiling
}
```

No change to this block at all — the stepper is entirely a rendering
upgrade on top of what `workflow` already produces today.

---

## §6. Screens needing no new construct — proof, not assertion

Two worked examples, chosen because they look the most "custom" in
`SCREENS.md`'s own language, to make the minimalism claim concrete
rather than asserted.

**Regulatory Reporting Queue** (Module 4, "template + schedule +
transmission status") is ordinary CRUD once template/schedule are
modeled as fields, not new syntax:

```nirdosha
enum ReportTemplate { SarXml, StrJson, CtrPdf }
enum ScheduleFrequency { Daily, Monthly, IncidentDriven }
enum DispatchStatus { Pending, Sent, Acknowledged, Failed }

struct ReportSchedule {
    id: i64,
    template: ReportTemplate,
    frequency: ScheduleFrequency,
    jurisdiction: str,
    next_run_unix: i64,
    status: DispatchStatus,
}

// list_/create_/update_/delete_report_schedule -- plain CRUD.
// generate_report_now(id: i64), retry_dispatch(id: i64) -- ordinary fns.

screen ReportSchedule {
    title: "Regulatory Reporting Queue"
    action "Generate Now" -> generate_report_now {
        style: "filled"
    }
    action "Retry" -> retry_dispatch {
        style: "outlined"
        confirm: "Retry transmission for this report?"
    }
}
```

Both `template` and `frequency` render as searchable dropdowns today
(the existing zero-payload-enum → `select` control, `build_field`,
`309`) — nothing about "template" or "schedule" needed new DSL surface.

**Legal Hold Management** (Module 6, "apply/release, track expiry, an
erasure-request queue") is the same pattern — `LegalHold` as a plain
struct with `applied_by`/`expiry_unix`/`status` fields, `"Apply Hold"`/
`"Release Hold"` as two ordinary custom `action`s with `confirm:` set.
The only piece this screen genuinely needs beyond plain CRUD — showing a
hold's *linked case* alongside its own record — is exactly the §1
`workspace`/`panel` pattern (a `LegalHoldReview` workspace with `subject:
LegalHold` and a `panel "Case" { source: get_case_for_hold }`), not a
fourth new construct.

---

## §7. Explicitly not included

Naming these now so nobody reading this doc mistakes a screen shape from
`SCREENS.md` for something silently promised — same "disclosed gap, not
silently dropped" discipline `docs/MOBILE.md`'s own "Rich profile" section and
`docs/WORKFLOW.md`'s presence-bridge section already practice.

- **Ad-hoc Query Builder (Module 5's Self-Service Query Interface) — not
  designed at all in this doc.** Nirdosha deliberately has zero string
  concatenation/formatting (`AGENTS.md`: "`str` has zero concatenation,
  zero formatting... there is no way to build a string at runtime from
  parts") — a genuine end-user-composed ad-hoc query needs either (a) a
  bounded query-expression sub-language plus a parameterized-SQL builder
  in `serve.rs` (a real feature roughly as large as `dispatch_table_query`
  generalized to arbitrary user-chosen filters/aggregations/joins — a
  meaningful injection-risk surface, not a small extension of anything in
  this doc), or (b) is honestly better served by pre-declared "query
  templates," which are just ordinary parameterized fns behind ordinary
  `screen`s today, no new construct required. Left fully out of scope —
  a future design pass, not a corollary of anything proposed here.
- **No real force-directed graph physics, drag, or zoom** (§2) — v1's
  `renderForceGraph` is a static circular/concentric layout. A real
  force simulation (velocity/repulsion/iteration loop) is a legitimate
  future enhancement to the same `render: "graph"` primitive, not
  designed here.
- **No real geographic basemap** (§2) — `render: "heatmap"` is a binned
  density grid over raw lat/lng, never map tiles, borders, or a
  Leaflet/Mapbox-style dependency (out of reach anyway under this
  project's FOSS/self-hosted, no-external-CDN posture the CTMS doc
  itself insists on: "no cloud-based... no paid SaaS").
- **No file/document upload anywhere in this doc** — Evidence
  Management's panel (§1) can list and link evidence *metadata* (a row
  per document: filename, hash, uploader, tag), never the file bytes
  themselves. This is the exact same gap `docs/MOBILE.md`'s own `D3` names
  and defers: **no file/blob/attachment type exists anywhere in
  Nirdosha today** (confirmed absent, not merely unrendered — there is
  no `Vector`/`Matrix`-style fixed-shape collection that fits either;
  those two are fixed-shape, `f64`-only linear-algebra types, not a
  generic `Vec<T>`/attachment carrier). A camera-capture or file-upload
  field is a language-level type-system decision, out of scope for a
  UI-DSL design doc, and belongs in its own pass before any workspace
  panel can render richer than a metadata table for evidence.
- **No generic repeating/array-of-child-struct as a single struct
  field** — the same missing-collection-type gap above is why a policy's
  list of rule conditions is modeled as a *separate struct plus a
  `workspace` panel* (§1/§6), not as one field on `CompliancePolicy`
  holding a nested list. This is the right relational shape anyway (each
  condition gets its own id, audit trail, and independent CRUD), so this
  isn't a workaround for a gap so much as the gap turning out not to
  matter for this specific need.
- **No real-time push beyond §3's client-side countdown.** Every
  workspace panel, graph, and heatmap in this doc is fetched on
  navigation (or on a manual refresh action), never subscribed to. The
  `presence-gateway` (`docs/ROADMAP.md` Track A5, `[DONE]`) already exists and
  already relays `workflow`'s `notify()` to a live WebSocket for a
  connected browser — nothing in this doc wires any of the new
  constructs above to it. A live-updating Alert Queue or a
  push-refreshed Investigation Workspace panel is a real, separately-
  scoped follow-on (reusing `presence-gateway`, not inventing a second
  push mechanism), not something this design silently assumes.
- **No actual ML model training UI.** Module 2/7's "ML Model Management"
  screen (§1's `workspace` covers its composite layout: model list +
  metrics + drift report + explainability panel) only ever *displays*
  model metadata and *calls* a `trigger_retraining`-shaped fn — the
  training run itself happens wherever `FlinkML`/`scikit-learn` already
  runs per the CTMS doc's own technology stack, entirely outside
  Nirdosha. No training-job orchestration, no hyperparameter UI, no
  experiment tracking designed here.
- **No versioned diff/rollback UI beyond what plain fields give you.**
  Policy Management Engine's "policy versioning and activation timelines"
  (§6) is modeled as ordinary fields on ordinary rows — there is no
  proposed side-by-side version-diff view or one-click rollback-to-any-
  prior-version UI; "rollback" is just another custom `action`, and
  seeing what changed between two versions means opening two rows, not
  a dedicated diff renderer.

---

## Summary

Ordered by leverage (most screens unblocked first):

| # | Construct | Kind of change | Screens unblocked (of 89) |
|---|---|---|---|
| 1 | `workspace` / `panel` | New top-level grammar construct | ~18 |
| 2 | `visual` dashboard/panel item + `render: "graph"\|"heatmap"\|"timeline"` | Small grammar extension (one new keyword + closed-vocabulary kv key) | ~6 directly, reused inside §1 panels |
| 3 | `field { render: "countdown" }` | New field-level kv value, zero grammar change | ~9 |
| 4 | `action { show_result: true }` | New action-level kv (boolean), zero grammar change | ~6 (the simulate/preview half of several config-as-data screens) |
| 5 | Workflow stage stepper | `ui_gen.rs`-only, zero grammar/DSL change | 1 (embedded in several) |
| — | Everything else (plain CRUD, report scheduling, most config-as-data) | **No change needed** | ~49 |

Net language-surface cost for covering essentially all 89 screens: one
new top-level keyword (`workspace`), one new contextual keyword
(`panel`), one new `dashboard_item` keyword (`visual`), and three new
closed-vocabulary `kv_entry` keys (`source`, `render`, `show_result`) —
no changes to `expr`/`primary`/any precedence-climbing production, no
new form-control kinds, no change to the existing four-animation or
one-bar-chart "deliberate non-goal" postures beyond the three new
`render` kinds named explicitly above. `serve.rs` needs **zero** new
routes for any of it — every new construct is a client-side composition
of `POST /api/<fn>` calls that already exist and are already secured.
