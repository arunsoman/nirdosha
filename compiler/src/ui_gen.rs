//! `emit-ui` — derives a self-contained, styled web UI directly from a
//! program's `struct` declarations and its CRUD-shaped function naming
//! convention, with Row 12 identity types (`VerifiedIdentity`/`RoleView`)
//! driving login and role-gated visibility. See `LANGUAGE.md`'s §2 (types)
//! and §"Identity / relying party" for the source vocabulary this reads.
//!
//! ## Design
//!
//! This module does the minimum Rust-side work needed to *derive the
//! shape* of the UI (`Screen`/`FieldSpec`/`Action`, below) from the typed
//! AST, then serializes that shape as a JSON "manifest" embedded in the
//! generated HTML. A small generic JS renderer (baked into the template,
//! not generated per-struct) reads the manifest and builds nav/tables/
//! forms/login at runtime. This keeps the Rust side proportional to the
//! *rules* ("what maps to what"), not to the number of structs/fields any
//! given program happens to declare.
//!
//! ## v1 scope (see the `emit-ui` plan)
//!
//! No server ships here — generated `fetch()` calls hit
//! `${API_BASE}/<fn_name>` (default `/api`, overridable via
//! `window.NIRDOSHA_API_BASE`) against whatever JSON API the user points
//! the file at. Login is an explicit client-side **stub**: it collects a
//! token, stores a mock identity/role list in `localStorage`, and gates
//! nav/actions against it — not real `oidc_validate_token` verification.
//! File upload and realtime are out of scope. A struct-typed field/param
//! expands one level deep into its own fields (the common `create_<S>(x:
//! S)` convention); a reference to a *zero-payload-only* enum (`enum
//! Status { Draft, Active }`, the categorical/ordinal case) renders as a
//! searchable dropdown, options in declaration order; a payload-carrying
//! enum, or anything deeper than the nesting cap, still renders read-only
//! (see `build_field`).
//!
//! Two more conventions besides struct/CRUD, both driven purely by
//! function name prefix + return type (`Metric`, `build_stats`/
//! `build_charts`): a zero-arg `stat_<name>() -> i64|f64` becomes a
//! dashboard tile, and a zero-arg `chart_<name>() -> json` (expected to
//! resolve to `{label, value}[]` — `db_query`'s own row shape when SQL
//! aliases its columns that way) becomes an inline-SVG bar chart. Both
//! land together on a synthetic "Dashboard" nav entry, first in the nav,
//! only when at least one exists.
//!
//! ## Declared `screen`/`dashboard` blocks (Row 12, LANGUAGE.md §11)
//!
//! Everything above is pure inference — no syntax needed. `screen
//! <Struct> { ... }`/`dashboard { ... }` are an **optional, additive**
//! layer on top of it, for the handful of things a naming convention
//! can't express: a friendlier title, a relabeled field, a custom action
//! beyond plain create/update/delete. `find_screen_decl` looks up the
//! declared block (if any) for a given struct; `build_screens` consults
//! it *after* running the same inference as before, overriding only
//! what the block actually mentions (title, per-field `label`, which fn
//! backs a CRUD slot, extra `action`s) — a struct with no matching
//! `ScreenDecl` renders exactly as it did before this DSL existed. See
//! `ast::ScreenDecl`/`ast::DashboardDecl` for the typechecked shape
//! (`typeck.rs::check_screen`/`check_dashboard` validate struct/field/fn
//! references and `view`/`edit` visibility exprs before this module ever
//! sees them) and `compiler/UI_DSL_TODO.md` for what's parsed/
//! typechecked but not yet wired into the generated UI (pagination,
//! search, sort, form insert/update modes). Field-level `view`/`edit`
//! RBAC (`field <name> { view: role(...), edit: role(...) } }`) *is*
//! now enforced, both here (`GatedField`/`field_gates_for_fn`/
//! `field_gates_for_struct`/`update_gates_for_fn`, consumed by the
//! client-side hiding/disabling in `ui_gen_template.html` and, for the
//! actual security boundary, by `serve.rs`'s response redaction and
//! write rejection) — see those functions' own doc comments.

use std::collections::{BTreeSet, HashMap};

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;

use crate::ast::{Effect, Expr, Field, FnDecl, MetricRef, Program, Requirement, ScreenDecl, Ty};
use crate::effects::FnEffects;

/// Prepended to every `Program.structs` by `ast::prelude_structs()` —
/// infrastructure types, never a user's own data model, so screens are
/// never derived from them.
const PRELUDE_STRUCT_NAMES: &[&str] =
    &["HttpResponse", "VerifiedIdentity", "RoleView", "ClaimView", "ApplicationSession", "RefreshTokenHandle", "Pair", "Money", "Measure"];

/// One field of a derived form/table column.
struct FieldSpec {
    name: String,
    /// `"text" | "number" | "checkbox" | "struct" | "readonly"` — see
    /// `build_field`. `"struct"` means `nested` holds that struct's own
    /// fields, one level deep (`build_field`'s `depth` cap).
    control: &'static str,
    required: bool,
    /// Human-readable type label for a `readonly` field's placeholder
    /// (e.g. `"box i64"`, `"Order (struct)"`) — never shown for editable
    /// controls, where the control itself already communicates the type.
    label: String,
    /// `screen <Struct> { field <name> { label: "..." } }` — a
    /// human-friendly display name shown in place of the raw field name
    /// wherever the client renders one (form labels, table headers).
    /// `None` keeps today's inferred behavior (the raw field name).
    /// Deliberately a separate field from `label` above, which already
    /// means something else (a `readonly` field's *type* label).
    display_label: Option<String>,
    nested: Vec<FieldSpec>,
    /// Populated only for `control == "select"` — every zero-payload
    /// variant name of the backing enum, in declaration order (this order
    /// is what gives an *ordinal* field like `RiskRating { Low, Medium,
    /// High }` its meaning; no separate ordinal concept exists, or needs
    /// to). Empty for every other control.
    options: Vec<String>,
    /// `screen <Struct> { field <name> { view: role(...) } }` — role
    /// names the identity needs *any one of* to see this field at all
    /// (any-of, matching `role(...)`'s own typechecked shape,
    /// `typeck.rs::check_visibility_expr`). Empty means ungated. Cosmetic
    /// here (drives client-side hiding only) — `serve.rs` independently
    /// redacts the same field server-side; this is not the security
    /// boundary, same disclosed nature as every other client-side gate
    /// in this module.
    view_roles: Vec<String>,
    /// `view: claim(key, value)` instead of `role(...)` — mutually
    /// exclusive with `view_roles` (typeck only allows one shape per key).
    view_claim: Option<(String, String)>,
    /// Same as `view_roles`/`view_claim`, for `field <name> { edit:
    /// ... } }` — gates whether the field's input is enabled in an edit
    /// form, not whether the field is shown at all (a view-ungated,
    /// edit-gated field still renders, just disabled).
    edit_roles: Vec<String>,
    edit_claim: Option<(String, String)>,
    /// `screen <Struct> { field <name> { pattern: "..." } }` — a regex a
    /// `str` field's value must match, already proven to compile
    /// (`typeck.rs::check_pattern_expr`). `None` means unconstrained.
    /// Client-side-only here (drives the input's HTML5 `pattern`
    /// attribute); `serve.rs`'s `check_field_validations` is the real
    /// enforcement, same split as `view_roles`/`edit_roles` above.
    pattern: Option<String>,
    /// `screen <Struct> { field <name> { min: ... } }` / `{ max: ... }`
    /// — inclusive numeric bounds for a numeric field, already proven
    /// applicable to this field's type at typeck time. `None` means
    /// unbounded on that side.
    min: Option<f64>,
    max: Option<f64>,
    /// `screen <Struct> { field <name> { render: "..." } }` (`ROADMAP.md`
    /// Track E3) — a display-only client hint, already proven by
    /// `typeck.rs::check_field_render_expr` to be one of a fixed set
    /// (`"countdown"` for v1) and, for `"countdown"` specifically, only
    /// on an integer field. Unlike `pattern`/`min`/`max`, this never
    /// becomes a validation rule — `serve.rs` has nothing to enforce
    /// here at all, the value passes straight through unchanged; only
    /// how the client *displays* it changes. `None` means the ordinary
    /// per-control rendering every other field already has.
    render: Option<&'static str>,
}

/// One CRUD-convention function backing a screen, plus what it costs to
/// call: does it need a logged-in identity, and/or a specific role/claim.
struct Action {
    /// `"list" | "create" | "update" | "delete" | "get"`.
    kind: &'static str,
    fn_name: String,
    requires_login: bool,
    required_role: Option<String>,
    required_claim: Option<(String, String)>,
    /// Side-effect badges from `effects::infer_effects`, for display only
    /// (e.g. a "network" chip next to a delete button) — never gates
    /// anything, unlike `required_role`.
    effect_badges: Vec<&'static str>,
    /// The action's own call parameters (e.g. `delete_todo(id: i64)`'s
    /// `id`), rendered as this action's own input form — deliberately
    /// *not* assumed to match the struct's fields (an update fn might
    /// take the whole struct, a delete fn might take just an id). Any
    /// `VerifiedIdentity` param is dropped here: the client supplies it
    /// itself from the stored (stubbed) login, never as a user-entered
    /// field.
    params: Vec<FieldSpec>,
    /// `screen <Struct> { action "<label>" -> <fn> { ... } }` — set only
    /// for a declared custom action (`kind == "custom"`); the button text
    /// (a CRUD action's label is derived client-side from its `kind`
    /// instead, unchanged).
    label: Option<String>,
    /// `action "..." -> fn { style: "filled" | "outlined" }` — button
    /// styling; `None` (a CRUD action, or a custom action that didn't set
    /// it) falls back to the client's own per-kind default.
    style: Option<String>,
    /// `action "..." -> fn { confirm: "Are you sure?" }` — when set, the
    /// client must confirm with the user before calling `fn`, the same
    /// way delete already always does (unconditionally, client-side).
    confirm: Option<String>,
    /// `action "..." -> fn { show_result: true }` (`ROADMAP.md` Track
    /// E4) — already proven by `typeck.rs::check_action_show_result` to
    /// only be `true` on a fn returning `Result(json, _)`. `false` (the
    /// default — every CRUD action, and a custom action that didn't set
    /// it) keeps today's plain row/panel-refresh-on-success behavior
    /// unchanged.
    show_result: bool,
}

/// One dashboard tile or chart — same shape either way (a label, a
/// zero-arg fn to call, and the usual gating), only the *convention*
/// that selects a function (`build_stats`/`build_charts`) and the
/// client-side renderer differ:
///
/// - `stat_<name>() -> i64|f64` (or `Result(i64|f64, str)`) is a single
///   number, rendered as a tile.
/// - `chart_<name>() -> json` (or `Result(json, str)`) is expected to
///   resolve to a JSON array of `{"label": ..., "value": <number>}`
///   objects — exactly `db_query`'s own row shape when the SQL aliases
///   its columns `label`/`value` (e.g. `SELECT service_type AS label,
///   SUM(x) AS value FROM t GROUP BY service_type`), so a chart is
///   usually one `db_query` call, no Nirdosha-side data wrangling
///   needed. Rendered as a simple inline-SVG bar chart, no external
///   charting library (this file's own "self-contained, no external
///   deps" stance) — unless `render` says otherwise (below).
/// - A declared `dashboard { visual "..." -> fn { render: "..." } }`
///   item (`ROADMAP.md` Track E2) has no naming-convention equivalent
///   at all — always explicitly declared, never inferred — and sets
///   `render` to something other than `BarChart`.
///
/// Same role/claim/login gating machinery as `Action` — a metric can be
/// just as sensitive as any other call.
struct Metric {
    label: String,
    fn_name: String,
    requires_login: bool,
    required_role: Option<String>,
    required_claim: Option<(String, String)>,
    render: MetricRender,
}

/// `visual "..." -> fn { render: "..." }`'s closed vocabulary
/// (`typeck.rs::check_visual_render_expr` already proved the string
/// literal is one of these three, or this is `BarChart` — every
/// `stat_`/`chart_`-convention metric and every declared `tile`/`chart`
/// item defaults here, unchanged behavior from before Track E2 existed).
#[derive(Clone, Copy, PartialEq)]
enum MetricRender {
    BarChart,
    Graph,
    Heatmap,
    Timeline,
}

impl MetricRender {
    fn as_str(self) -> &'static str {
        match self {
            MetricRender::BarChart => "bar_chart",
            MetricRender::Graph => "graph",
            MetricRender::Heatmap => "heatmap",
            MetricRender::Timeline => "timeline",
        }
    }
    fn from_kv(entries: &[(String, Expr)]) -> MetricRender {
        match kv_str(entries, "render") {
            Some("graph") => MetricRender::Graph,
            Some("heatmap") => MetricRender::Heatmap,
            Some("timeline") => MetricRender::Timeline,
            // Already proven by typeck to be one of the three above, or
            // absent — an unrecognized string never reaches this trust
            // boundary (same "typeck already proved well-formedness"
            // posture every other `kv_str` consumer in this file has).
            _ => MetricRender::BarChart,
        }
    }
}

/// One derived screen: a user `struct` plus whichever CRUD-convention
/// functions (`list_<s>`/`create_<s>`/`update_<s>`/`delete_<s>`/`get_<s>`)
/// exist for it. A screen with no `list_*` renders as a singular
/// settings-style form instead of a table (`is_singular`).
struct Screen {
    struct_name: String,
    /// Display title (nav label, heading, toast text) — `to_display_label`
    /// of the struct name (`ApiKey` -> `Api Key`) unless a `screen
    /// <Struct> { title: "..." }` block overrides it.
    title: String,
    /// `Some("Display Name")` when the backing struct was declared inside
    /// a `module "Display Name" { ... }` block (`ast::StructDecl::module`)
    /// — `ui_gen_template.html`'s `renderNav` groups nav by this into
    /// collapsible primary-menu sections; `None` renders flat/ungrouped,
    /// exactly as every screen did before `module` existed.
    module: Option<String>,
    fields: Vec<FieldSpec>,
    actions: Vec<Action>,
    is_singular: bool,
}

/// One `panel "<label>" { source: <fn> ... }` inside a `workspace` block
/// — a composed section, backed by a `source` fn already proven
/// (`typeck.rs::check_workspace`) to take one `i64` param and return
/// `Result(json, _)`. `source` is reused as an ordinary `Action` (kind
/// `"source"`) purely for its existing gating-info plumbing
/// (`requires_login`/`required_role`/`required_claim`) — the same reuse
/// `WorkflowQueue::pending_fn` already makes of `Action` for a fn that
/// isn't really a CRUD `list` either.
struct Panel {
    title: String,
    source: Action,
    actions: Vec<Action>,
    render: PanelRender,
}

/// `panel "..." { render: "..." }`'s closed vocabulary (`ROADMAP.md`
/// Track E2) — `Table` (the default: `renderPanel`'s original plain-
/// table rendering, unchanged for a panel that never sets `render`)
/// plus the same three kinds `MetricRender` gives `visual`, reused
/// unchanged client-side (`renderForceGraph`/`renderHeatGrid`/
/// `renderTimelineList` don't care whether they were called from
/// `renderDashboard` or `renderPanel`).
#[derive(Clone, Copy, PartialEq)]
enum PanelRender {
    Table,
    Graph,
    Heatmap,
    Timeline,
}

impl PanelRender {
    fn as_str(self) -> &'static str {
        match self {
            PanelRender::Table => "table",
            PanelRender::Graph => "graph",
            PanelRender::Heatmap => "heatmap",
            PanelRender::Timeline => "timeline",
        }
    }
    /// Same trust posture as `MetricRender::from_kv` — typeck already
    /// proved `render`, if present, is one of the three explicit kinds;
    /// absent (or, defensively, anything else) means the default.
    fn from_kv(entries: &[(String, Expr)]) -> PanelRender {
        match kv_str(entries, "render") {
            Some("graph") => PanelRender::Graph,
            Some("heatmap") => PanelRender::Heatmap,
            Some("timeline") => PanelRender::Timeline,
            _ => PanelRender::Table,
        }
    }
}

/// `workspace <Name> { subject: <Struct> panel "..." { ... } }` — a
/// composite, multi-panel screen scoped to one instance of `subject`
/// (`ROADMAP.md` Track E1, `examples/ctms/UI_CONSTRUCTS.md` §1).
/// `subject_fields` is `build_field_root` run over the subject struct's
/// own fields — literally the same field→control mapping `Screen.fields`
/// already uses, reused unchanged for the read-only header
/// `ui_gen_template.html`'s `renderWorkspace` shows above the panels.
struct Workspace {
    name: String,
    title: String,
    subject_struct: String,
    /// `get_<subject_snake>` if a real fn by that name exists (built via
    /// `build_action`, same convention-fn lookup `build_screens` already
    /// does for `get_<S>`); `None` only if the subject struct has no
    /// `get_<S>` at all, in which case `ui_gen_template.html` skips the
    /// read-only header fetch rather than guessing a fn name that would
    /// just 404 — a disclosed degradation, not a broken feature.
    subject_get: Option<Action>,
    subject_fields: Vec<FieldSpec>,
    panels: Vec<Panel>,
}

/// One derived "Workflows" nav entry (`WORKFLOW.md`'s "state ownership +
/// a generated queue UI" section) — a declared `workflow` plus the two
/// fns `workflow_lower.rs` always synthesizes for it
/// (`list_<workflow>_pending_for_me`/`advance_<workflow>`). Unlike
/// `Screen`, there's no static per-row action set to derive: which
/// buttons a row gets depends on *that row's own current state*, which
/// only `pending_fn`'s own response (`{instance_id, state, state_label,
/// events, data}` per row, `interpreter.rs::workflow_pending_for_me`)
/// knows — so this only carries what genuinely is static: the two fn
/// names and the `data` block's field shape (for column headers/
/// controls, reusing `build_field` exactly as a struct's own fields do).
struct WorkflowQueue {
    name: String,
    title: String,
    /// Built via `build_action(..., "list", ...)` — reused purely for its
    /// existing gating-info plumbing (`requires_login` is always true, a
    /// `VerifiedIdentity`-only param never carries a static role/claim;
    /// see that fn's own doc comment), not because this is a `list`
    /// action in the `Screen`/`Action` sense.
    pending_fn: Action,
    advance_fn_name: String,
    /// `list_<workflow>_submitted_by_me` (`WORKFLOW.md`'s "who submitted
    /// this" section) — the client's "My Requests" tab, same row shape
    /// as `pending_fn`'s but with no action buttons rendered (a
    /// requester watches, they don't decide).
    submitted_by_me_fn: Action,
    /// `get_<workflow>_history` (`WORKFLOW.md`'s "audit trail" section)
    /// — plain fn name only, not a full `Action`: every row in either
    /// tab gets a "History" button calling this with that row's own
    /// `instance_id`, the same per-row-param shape a custom `screen`
    /// action's single-id param already has.
    history_fn_name: String,
    data_fields: Vec<FieldSpec>,
    /// `workflow Name { state A {} state B {} ... }`'s own `state` list,
    /// in declaration order (`ROADMAP.md` Track E5) — read straight off
    /// `ast::WorkflowDecl::states`, already parsed/typechecked, zero new
    /// parsing. What lets the client draw a real "step 2 of 4" stepper
    /// instead of a bare state-name label: a queue row's own `state`
    /// only names *where* it is, never how many stages exist or which
    /// came before/after.
    all_states: Vec<String>,
}

/// `Todo` -> `todo`, `UserProfile` -> `user_profile`, `HTTPClient` ->
/// `http_client`. Only needs to handle the ASCII PascalCase Nirdosha
/// struct names actually use — not a general-purpose Unicode caser.
fn to_snake_case(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push('_');
            }
            out.extend(ch.to_lowercase());
        } else {
            out.push(ch);
        }
    }
    out
}

fn find_fn<'a>(program: &'a Program, name: &str) -> Option<&'a FnDecl> {
    program.fns.iter().find(|f| f.name == name)
}

/// `ApiKey` -> `Api Key`, `FraudCase` -> `Fraud Case`,
/// `DiscrepancyCheckResult` -> `Discrepancy Check Result` — the default
/// nav label/title/heading for a screen, replacing the raw struct name
/// (still overridable via `screen <Struct> { title: "..." }`). Same
/// word-boundary walk as `to_snake_case` (a `_` there is a literal space
/// here), only needs the same ASCII PascalCase struct names.
fn to_display_label(name: &str) -> String {
    let mut out = String::with_capacity(name.len() + 4);
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() {
            if i != 0 {
                out.push(' ');
            }
            out.push(ch);
        } else {
            out.push(ch);
        }
    }
    out
}

/// A short, honest label for a field's declared type — used only on
/// `readonly` fields, where there's no input control to speak for itself.
fn ty_label(ty: &Ty) -> String {
    match ty {
        Ty::Str => "str".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::F64 => "f64".to_string(),
        Ty::Dec128 => "dec128".to_string(),
        Ty::I8 => "i8".to_string(),
        Ty::I16 => "i16".to_string(),
        Ty::I32 => "i32".to_string(),
        Ty::I64 => "i64".to_string(),
        Ty::U8 => "u8".to_string(),
        Ty::U16 => "u16".to_string(),
        Ty::U32 => "u32".to_string(),
        Ty::U64 => "u64".to_string(),
        Ty::Usize => "usize".to_string(),
        Ty::Named(n, args) if args.is_empty() => n.clone(),
        Ty::Named(n, args) => format!("{n}({})", args.iter().map(ty_label).collect::<Vec<_>>().join(", ")),
        Ty::Box(inner) => format!("box {}", ty_label(inner)),
        Ty::Vector(t, n) => format!("Vector({}, {n})", ty_label(t)),
        Ty::Matrix(t, r, c) => format!("Matrix({}, {r}, {c})", ty_label(t)),
        other => format!("{other:?}"),
    }
}

fn resolve_struct<'a>(program: &'a Program, name: &str) -> Option<&'a crate::ast::StructDecl> {
    program.structs.iter().find(|s| s.name == name)
}

fn resolve_enum<'a>(program: &'a Program, name: &str) -> Option<&'a crate::ast::EnumDecl> {
    program.enums.iter().find(|e| e.name == name)
}

/// A field name's snake_case, `_`-split segments include a whole segment
/// literally `"date"` or `"time"` — matches both a trailing suffix
/// (`created_at`... no, that one doesn't match, deliberately: `at` isn't
/// `date`/`time`) and a *leading* segment like `TradeDocument.date_note`
/// (`examples/trade-finance/trade_finance.nir`), which a suffix-only rule
/// would miss. Case-insensitive is unnecessary (Nirdosha field names are
/// always already lowercase snake_case) but harmless.
fn is_date_like_field_name(name: &str) -> bool {
    name.split('_').any(|seg| seg.eq_ignore_ascii_case("date") || seg.eq_ignore_ascii_case("time"))
}

/// Maps one `name: ty` (a struct field, or a CRUD action's own param) to
/// a form control. `Option(T)` unwraps to `T`'s control with
/// `required = false`. A bare reference to another struct in this same
/// program (e.g. `create_todo(t: Todo)`'s `t`) expands into that
/// struct's own fields (`control = "struct"`, `nested` holds them) — the
/// common `create_<S>(x: S)` convention would otherwise render as a
/// single unfillable blob. `visiting` (struct names on the current
/// expansion path) is real cycle protection, not a flat depth cap
/// (2026-08-27 — a flat `depth < 2` cap used to reject a legitimate
/// `Order -> LineItem -> Product -> Category` schema exactly as
/// unconditionally as an actually-cyclic one; `struct A { b: B } struct
/// B { a: A }` typechecks today with no cycle check at declaration time,
/// confirmed empirically via `emit-ast`, so this is the one place that
/// still has to guard against it). A name reappearing on `visiting`
/// falls back to `readonly` — same "not expressible, don't guess"
/// policy the enum/affine-handle/`Result`/`Fn` cases below already have,
/// just reached by a real cycle instead of an arbitrary depth number.
/// `build_field` for every caller building one independent top-level
/// field (a struct's own field, a CRUD action's param) rather than
/// recursing itself -- same "fresh guard per independent tree" reasoning
/// `serve.rs::decode_value_root` documents.
fn build_field_root(program: &Program, name: &str, ty: &Ty) -> FieldSpec {
    build_field(program, name, ty, &mut Vec::new())
}

fn build_field(program: &Program, name: &str, ty: &Ty, visiting: &mut Vec<String>) -> FieldSpec {
    let base = |control, required| FieldSpec {
        name: name.to_string(),
        control,
        required,
        label: ty_label(ty),
        display_label: None,
        nested: vec![],
        options: vec![],
        view_roles: vec![],
        view_claim: None,
        edit_roles: vec![],
        edit_claim: None,
        pattern: None,
        min: None,
        max: None,
        render: None,
    };
    match ty {
        // `date`/`time`-named str fields get a calendar picker instead of
        // a plain text box (a naming-convention heuristic, not a new
        // language type — Nirdosha's lack of a date/time primitive is a
        // deliberate no-wall-clock determinism stance, LANGUAGE.md §9,
        // left untouched here). The client shows a decorative lock badge
        // next to it ("human-supplied, not an auto clock-stamp") — the
        // field itself stays a plain, fully editable `str`.
        Ty::Str if is_date_like_field_name(name) => base("date", true),
        Ty::Str => base("text", true),
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize | Ty::F64 => {
            base("number", true)
        }
        // Text, not `number` — a `dec128` field round-trips through
        // JSON/DB as its canonical decimal *string* (`serve.rs::
        // decode_value`/`encode_value`'s `Ty::Dec128` arms,
        // `interpreter.rs::sql_bind_params`), and an `<input type=number>`
        // is IEEE-754 under the hood in every browser, exactly the
        // silent-drift failure `dec128` exists to prevent (`LANGUAGE.md`
        // §5's "Decimal arithmetic"). `pattern` gives it basic decimal-
        // shape validation client-side without a numeric spinner nudging
        // toward float behavior.
        Ty::Dec128 => {
            let mut f = base("text", true);
            f.pattern = Some(r"^-?\d+(\.\d+)?$".to_string());
            f
        }
        Ty::Bool => base("checkbox", false),
        Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
            let mut inner = build_field(program, name, &args[0], visiting);
            inner.required = false;
            inner
        }
        // A zero-payload-only enum ("categorical"/"ordinal" -- e.g. `enum
        // RiskRating { Low, Medium, High }`) is the one enum shape that
        // actually round-trips through `db_execute`/`db_query`
        // (`interpreter.rs::sql_bind_params`) and a JSON request body
        // (`serve.rs::decode_value`/`decode_enum_value`) -- see both
        // functions' doc comments. It renders as a searchable dropdown,
        // options in declaration order (which is also that field's
        // ordinal order, with no separate concept needed). Any
        // payload-carrying variant means the enum can't sensibly occupy
        // one SQL column, so it keeps the pre-existing `readonly`
        // fallback below, unchanged.
        Ty::Named(n, args) if args.is_empty() => {
            if let Some(e) = resolve_enum(program, n) {
                if e.variants.iter().all(|v| v.payload.is_empty()) {
                    return FieldSpec {
                        name: name.to_string(),
                        control: "select",
                        required: true,
                        label: n.clone(),
                        display_label: None,
                        nested: vec![],
                        options: e.variants.iter().map(|v| v.name.clone()).collect(),
                        view_roles: vec![],
                        view_claim: None,
                        edit_roles: vec![],
                        edit_claim: None,
                        pattern: None,
                        min: None,
                        max: None,
                        render: None,
                    };
                }
                return base("readonly", false);
            }
            // The "enum favoring" `str` ban's free-text carrier
            // (`struct Text { value: str }`, used wherever genuine free
            // text like a justification/note/reference needs to cross a
            // function boundary that can no longer take/return bare
            // `str`) renders exactly like a plain `Ty::Str` field would
            // have — a single text box — instead of falling through to
            // the generic one-level nested-struct case just below. Without
            // this, every migrated free-text field would show as an
            // expandable single-field group instead of an ordinary input.
            if n == "Text" {
                if let Some(s) = resolve_struct(program, n) {
                    if let [Field { name: field_name, ty: Ty::Str }] = s.fields.as_slice() {
                        if field_name == "value" {
                            return base("text", true);
                        }
                    }
                }
            }
            if !visiting.iter().any(|v| v == n)
                && let Some(s) = resolve_struct(program, n)
            {
                visiting.push(n.clone());
                let nested = s.fields.iter().map(|f| build_field(program, &f.name, &f.ty, visiting)).collect();
                visiting.pop();
                return FieldSpec {
                    name: name.to_string(),
                    control: "struct",
                    required: true,
                    label: n.clone(),
                    display_label: None,
                    nested,
                    options: vec![],
                    view_roles: vec![],
                    view_claim: None,
                    edit_roles: vec![],
                    edit_claim: None,
                    pattern: None,
                    min: None,
                    max: None,
                    render: None,
                };
            }
            base("readonly", false)
        }
        _ => base("readonly", false),
    }
}

fn fn_requires_login(f: &FnDecl) -> bool {
    f.params.iter().any(|p| matches!(&p.ty, Ty::Named(n, args) if n == "VerifiedIdentity" && args.is_empty()))
}

fn fn_role_gate(f: &FnDecl) -> (Option<String>, Option<(String, String)>) {
    match &f.requires {
        Some(Requirement::Role(role)) => (Some(role.clone()), None),
        Some(Requirement::Claim(key, value)) => (None, Some((key.clone(), value.clone()))),
        None => (None, None),
    }
}

fn effect_badges(effects: &HashMap<String, FnEffects>, fn_name: &str) -> Vec<&'static str> {
    let Some(fe) = effects.get(fn_name) else { return vec![] };
    let mut badges = vec![];
    let tags: &BTreeSet<Effect> = &fe.inferred;
    if tags.contains(&Effect::Network) {
        badges.push("network");
    }
    if tags.contains(&Effect::Io) {
        badges.push("io");
    }
    if tags.contains(&Effect::Concurrent) {
        badges.push("concurrent");
    }
    badges
}

fn build_action(program: &Program, effects: &HashMap<String, FnEffects>, kind: &'static str, fn_name: &str) -> Option<Action> {
    let f = find_fn(program, fn_name)?;
    let (required_role, required_claim) = fn_role_gate(f);
    let params = f
        .params
        .iter()
        .filter(|p| !matches!(&p.ty, Ty::Named(n, args) if n == "VerifiedIdentity" && args.is_empty()))
        .map(|p| build_field_root(program, &p.name, &p.ty))
        .collect();
    Some(Action {
        kind,
        fn_name: fn_name.to_string(),
        requires_login: fn_requires_login(f) || required_role.is_some() || required_claim.is_some(),
        required_role,
        required_claim,
        effect_badges: effect_badges(effects, fn_name),
        params,
        label: None,
        style: None,
        confirm: None,
        show_result: false,
    })
}

/// `screen <Struct> { action "<label>" -> <fn> { style: ..., confirm: ... } }`
/// — a custom action beyond the inferred CRUD set. Reuses `build_action`
/// for the fn-existence/gating/params/badges plumbing (already validated
/// by typeck, so `find_fn` is trusted to succeed here) and layers the
/// declared label/style/confirm on top.
fn build_custom_action(
    program: &Program,
    effects: &HashMap<String, FnEffects>,
    decl: &crate::ast::ActionDecl,
) -> Option<Action> {
    let mut action = build_action(program, effects, "custom", &decl.target_fn)?;
    action.label = Some(decl.label.clone());
    action.style = kv_str(&decl.entries, "style").map(str::to_string);
    action.confirm = kv_str(&decl.entries, "confirm").map(str::to_string);
    action.show_result = kv_bool(&decl.entries, "show_result");
    Some(action)
}

/// Looks up a string-literal-valued entry by key in a `screen`/`field`/
/// `action`'s `Vec<(String, Expr)>` — `None` if the key is absent *or*
/// its value isn't a plain string literal (typeck doesn't constrain most
/// keys' shapes yet; a non-string value here is silently ignored rather
/// than treated as a hard error, consistent with this phase's
/// existence/shape-only validation scope).
fn kv_str<'a>(entries: &'a [(String, Expr)], key: &str) -> Option<&'a str> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        Expr::Str(s, _) => Some(s.as_str()),
        _ => None,
    })
}

/// `kv_str`'s numeric sibling, for `field <name> { min: ... }`/`{ max:
/// ... }` — `typeck.rs::check_min_max_expr` already proved the value is
/// an `Expr::Int`/`Expr::Float`, so this trusts that the same way every
/// other `screen`-block consumer here trusts typeck's shape checks.
fn kv_num(entries: &[(String, Expr)], key: &str) -> Option<f64> {
    entries.iter().find(|(k, _)| k == key).and_then(|(_, v)| match v {
        Expr::Int(n, _) => Some(*n as f64),
        Expr::Float(n, _) => Some(*n),
        _ => None,
    })
}

/// `kv_str`'s boolean sibling, for `action <name> { show_result: ... }`
/// (`ROADMAP.md` Track E4) — `typeck.rs::check_action_show_result`
/// already proved the value is an `Expr::Bool` when present; `false`
/// (never `None`) covers both "absent" and "declared false" alike, since
/// neither needs a rendering difference from today's default.
fn kv_bool(entries: &[(String, Expr)], key: &str) -> bool {
    entries.iter().find(|(k, _)| k == key).is_some_and(|(_, v)| matches!(v, Expr::Bool(true, _)))
}

/// `kv_str`'s sibling for `field <name> { view: role(...) }`/`{ edit:
/// role(...) }`/`{ ... : claim(k, v) }` — extracts the role list (any-of,
/// possibly more than one) or the single claim pair. `typeck.rs::
/// check_visibility_expr` already proved shape (a `role(...)` with only
/// string args, or a `claim(k, v)` with exactly two string args) before
/// `ui_gen` ever sees this, so this trusts well-formedness the same way
/// `build_screens`'s other `screen`-block consumers already do; an
/// absent key or a value that doesn't match either shape is simply
/// ungated (`(vec![], None)`), not an error at this phase.
fn kv_gate(entries: &[(String, Expr)], key: &str) -> (Vec<String>, Option<(String, String)>) {
    let Some((_, v)) = entries.iter().find(|(k, _)| k == key) else { return (vec![], None) };
    match v {
        Expr::Call(name, args, _) if name == "role" => {
            (args.iter().filter_map(|a| if let Expr::Str(s, _) = a { Some(s.clone()) } else { None }).collect(), None)
        }
        Expr::Call(name, args, _) if name == "claim" && args.len() == 2 => match (&args[0], &args[1]) {
            (Expr::Str(k, _), Expr::Str(val, _)) => (vec![], Some((k.clone(), val.clone()))),
            _ => (vec![], None),
        },
        _ => (vec![], None),
    }
}

/// The declared `screen <Struct> { ... }` block for one struct, if any —
/// `ui_gen`'s bridge from Row 12's typechecked DSL into the inference
/// pipeline below. A struct with no matching `ScreenDecl` takes every
/// default from inference, unchanged.
fn find_screen_decl<'a>(program: &'a Program, struct_name: &str) -> Option<&'a ScreenDecl> {
    program.screens.iter().find(|sd| sd.struct_name == struct_name)
}

/// `open_cases` -> `Open Cases`. Only needs to handle the ASCII
/// snake_case names Nirdosha fn identifiers actually use.
fn to_title_case(snake: &str) -> String {
    snake
        .split('_')
        .map(|word| {
            let mut chars = word.chars();
            match chars.next() {
                Some(first) => first.to_uppercase().collect::<String>() + chars.as_str(),
                None => String::new(),
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn is_numeric_scalar(ty: &Ty) -> bool {
    matches!(ty, Ty::I64 | Ty::F64)
}

/// A tile's return type must be a plain number or a `Result` of one —
/// anything else (including a numeric `Option`) isn't a stat, it's just
/// an ordinary function that happens to start with `stat_`.
fn is_stat_return_ty(ty: &Ty) -> bool {
    match ty {
        Ty::I64 | Ty::F64 => true,
        Ty::Named(n, args) if n == "Result" && args.len() == 2 => is_numeric_scalar(&args[0]),
        _ => false,
    }
}

/// A chart's return type must be `json` (a `{label, value}[]` array, by
/// convention — not statically checkable, same trust boundary `db_query`
/// itself already has) or a `Result` of one.
fn is_chart_return_ty(ty: &Ty) -> bool {
    match ty {
        Ty::Json => true,
        Ty::Named(n, args) if n == "Result" && args.len() == 2 => matches!(args[0], Ty::Json),
        _ => false,
    }
}

/// One `Metric` from a zero-arg fn — shared by every path that builds
/// one (naming-convention inference below, and `apply_declared_metrics`/
/// `build_visuals` for the declared-block path), so gating derivation
/// (`fn_role_gate`/`fn_requires_login`) stays in exactly one place.
fn build_metric_from_fn(f: &FnDecl, label: String, render: MetricRender) -> Metric {
    let (required_role, required_claim) = fn_role_gate(f);
    Metric {
        label,
        fn_name: f.name.clone(),
        requires_login: fn_requires_login(f) || required_role.is_some() || required_claim.is_some(),
        required_role,
        required_claim,
        render,
    }
}

/// Shared by `build_stats`/`build_charts`: every zero-arg fn whose name
/// starts with `prefix` and whose return type passes `return_ok`
/// becomes one `Metric`, labeled from the rest of its name.
fn build_metrics(program: &Program, prefix: &str, return_ok: impl Fn(&Ty) -> bool) -> Vec<Metric> {
    program
        .fns
        .iter()
        .filter(|f| f.name.starts_with(prefix) && f.params.is_empty() && return_ok(&f.ret))
        .map(|f| build_metric_from_fn(f, to_title_case(f.name.strip_prefix(prefix).unwrap_or(&f.name)), MetricRender::BarChart))
        .collect()
}

/// A declared `dashboard { tile/chart "<label>" -> <fn> }` entry
/// (`typeck.rs::check_dashboard` already proved `<fn>` resolves) applied
/// on top of naming-convention inference, which already ran (`metrics`
/// is `build_metrics`'s own output) — the "additive, for the handful of
/// things a naming convention can't express" layer this whole
/// declarative DSL exists for (this file's own module doc comment),
/// finally actually wired in: a fn naming-convention inference *also*
/// picked up gets its label overridden by the declared one; a fn it
/// didn't (any name, not just `stat_`/`chart_`-prefixed) gets added as a
/// new entry.
fn apply_declared_metrics(program: &Program, declared: &[MetricRef], metrics: &mut Vec<Metric>) {
    for d in declared {
        if let Some(existing) = metrics.iter_mut().find(|m| m.fn_name == d.target_fn) {
            existing.label = d.label.clone();
        } else if let Some(f) = find_fn(program, &d.target_fn) {
            metrics.push(build_metric_from_fn(f, d.label.clone(), MetricRender::BarChart));
        }
    }
}

fn build_stats(program: &Program) -> Vec<Metric> {
    let mut metrics = build_metrics(program, "stat_", is_stat_return_ty);
    if let Some(dash) = &program.dashboard {
        apply_declared_metrics(program, &dash.tiles, &mut metrics);
    }
    metrics
}

fn build_charts(program: &Program) -> Vec<Metric> {
    let mut metrics = build_metrics(program, "chart_", is_chart_return_ty);
    let Some(dash) = &program.dashboard else { return metrics };
    apply_declared_metrics(program, &dash.charts, &mut metrics);
    // `visual` (Track E2) has no naming-convention counterpart at all --
    // always a new entry, never a label-only override of one already
    // found above.
    for v in &dash.visuals {
        if let Some(f) = find_fn(program, &v.target_fn) {
            metrics.push(build_metric_from_fn(f, v.label.clone(), MetricRender::from_kv(&v.entries)));
        }
    }
    metrics
}

/// One `screen <Struct> { field <name> { view/edit: ... } }` field's
/// resolved gate — the shared shape `field_gates_for_fn` returns to
/// `serve.rs`, so the server enforces exactly what the client was told
/// to hide/disable, not a second, independently-derived notion of it.
pub struct GatedField {
    pub field_name: String,
    pub view_roles: Vec<String>,
    pub view_claim: Option<(String, String)>,
    pub edit_roles: Vec<String>,
    pub edit_claim: Option<(String, String)>,
}

fn gates_from_screen_decl(decl: &ScreenDecl) -> Vec<GatedField> {
    decl.fields
        .iter()
        .filter_map(|fo| {
            let (view_roles, view_claim) = kv_gate(&fo.entries, "view");
            let (edit_roles, edit_claim) = kv_gate(&fo.entries, "edit");
            if view_roles.is_empty() && view_claim.is_none() && edit_roles.is_empty() && edit_claim.is_none() {
                return None;
            }
            Some(GatedField { field_name: fo.field_name.clone(), view_roles, view_claim, edit_roles, edit_claim })
        })
        .collect()
}

/// A struct's declared `screen` block's field-level `view`/`edit` gates,
/// by struct name directly — empty if the struct has no `screen` block,
/// or the block declares no field gates. Used by `serve.rs`'s generic
/// `/_nirdosha/table/<name>` route, which already knows the table's
/// (== `to_snake_case`d struct's) name and has no `fn_name` to resolve
/// from at all (see `field_gates_for_fn` for the fn-name-keyed sibling
/// `dispatch`'s `/api/<fn>` route uses instead).
pub fn field_gates_for_struct(program: &Program, struct_name: &str) -> Vec<GatedField> {
    find_screen_decl(program, struct_name).map(gates_from_screen_decl).unwrap_or_default()
}

/// Given a fn name that might be one of a `screen <Struct> { ... }`
/// block's CRUD slots (`list`/`get`/`create`/`update`, default
/// `<kind>_<snake_case_struct_name>` or a declared override — the exact
/// resolution `build_screens`'s own `crud_fn_name` closure uses,
/// deliberately reimplemented here rather than shared, since threading a
/// closure or refactoring that private helper into something callable
/// from outside this module is a bigger, riskier change than repeating
/// ~10 lines of struct/fn-name matching) — returns every field that
/// struct's `screen` block gates with `view`/`edit`, or an empty `Vec`
/// if `fn_name` doesn't back any screen, or the screen declares no field
/// gates at all. **The only piece of `ui_gen`'s screen-resolution logic
/// exposed outside this module** — `serve.rs` has no other way to know
/// "which struct (if any) does this fn's screen belong to, and what does
/// it gate," and needs to ask that question independently of whether the
/// fn's actual return shape is a typed struct or a raw `json` blob built
/// by hand (`db_query`'s common shape in hand-written `.nir` apps) —
/// this resolves purely from the declared `screen` block, never from the
/// fn's own body.
pub fn field_gates_for_fn(program: &Program, fn_name: &str) -> Vec<GatedField> {
    for s in &program.structs {
        if PRELUDE_STRUCT_NAMES.contains(&s.name.as_str()) {
            continue;
        }
        let Some(decl) = find_screen_decl(program, &s.name) else { continue };
        let snake = to_snake_case(&s.name);
        let crud_fn_name = |kind: &str, default: String| -> String {
            decl.entries
                .iter()
                .find(|(k, _)| k == kind)
                .and_then(|(_, v)| if let Expr::Ident(n, _) = v { Some(n.clone()) } else { None })
                .unwrap_or(default)
        };
        let backs_this_fn = [
            crud_fn_name("list", format!("list_{snake}")),
            crud_fn_name("get", format!("get_{snake}")),
            crud_fn_name("create", format!("create_{snake}")),
            crud_fn_name("update", format!("update_{snake}")),
        ]
        .iter()
        .any(|n| n == fn_name);
        if !backs_this_fn {
            continue;
        }
        return gates_from_screen_decl(decl);
    }
    vec![]
}

/// Like `field_gates_for_fn`, but matches ONLY a struct's `update` CRUD
/// slot specifically (not `list`/`get`/`create`), and returns just the
/// `edit`-gated fields (a field with only a `view` gate is irrelevant to
/// a write check). `serve.rs`'s write-enforcement path only ever rejects
/// an *edit* to an existing row, never a *create* — `create_<S>`/
/// `update_<S>` both take the whole struct positionally, so "edit" most
/// honestly maps to *changing something already stored*, not to what a
/// brand-new row is created with — so it needs to know specifically
/// "does this fn update struct S," not merely that some struct's screen
/// mentions it. Returns `None` if `fn_name` isn't a struct's `update`
/// slot, or that struct declares no `edit` gates at all.
pub fn update_gates_for_fn(program: &Program, fn_name: &str) -> Option<(String, Vec<GatedField>)> {
    for s in &program.structs {
        if PRELUDE_STRUCT_NAMES.contains(&s.name.as_str()) {
            continue;
        }
        let Some(decl) = find_screen_decl(program, &s.name) else { continue };
        let snake = to_snake_case(&s.name);
        let update_fn = decl
            .entries
            .iter()
            .find(|(k, _)| k == "update")
            .and_then(|(_, v)| if let Expr::Ident(n, _) = v { Some(n.clone()) } else { None })
            .unwrap_or_else(|| format!("update_{snake}"));
        if update_fn != fn_name {
            continue;
        }
        let gates: Vec<GatedField> =
            gates_from_screen_decl(decl).into_iter().filter(|g| !g.edit_roles.is_empty() || g.edit_claim.is_some()).collect();
        if gates.is_empty() {
            return None;
        }
        return Some((s.name.clone(), gates));
    }
    None
}

/// One `screen <Struct> { field <name> { pattern/min/max: ... } }`
/// field's resolved format constraint — the shared shape
/// `field_validations_for_fn` returns to `serve.rs`, mirroring
/// `GatedField`'s role in the RBAC path: the server enforces exactly
/// what the client was told to constrain, not a second, independently-
/// derived notion of it.
pub struct ValidatedField {
    pub field_name: String,
    pub pattern: Option<String>,
    pub min: Option<f64>,
    pub max: Option<f64>,
}

/// `pattern: "<regex>"` directly, or `format: "email"|"phone"|"date"|
/// "url"|"uuid"` resolved through `ast::well_known_format_pattern` —
/// `typeck.rs::check_screen` already rejected declaring both on the same
/// field, so this trusts at most one is actually present, same posture
/// every other `kv_*` helper here already takes toward typeck's proofs.
fn resolve_pattern(entries: &[(String, Expr)]) -> Option<String> {
    kv_str(entries, "pattern")
        .map(str::to_string)
        .or_else(|| kv_str(entries, "format").and_then(crate::ast::well_known_format_pattern).map(str::to_string))
}

fn validations_from_screen_decl(decl: &ScreenDecl) -> Vec<ValidatedField> {
    decl.fields
        .iter()
        .filter_map(|fo| {
            let pattern = resolve_pattern(&fo.entries);
            let min = kv_num(&fo.entries, "min");
            let max = kv_num(&fo.entries, "max");
            if pattern.is_none() && min.is_none() && max.is_none() {
                return None;
            }
            Some(ValidatedField { field_name: fo.field_name.clone(), pattern, min, max })
        })
        .collect()
}

/// Like `update_gates_for_fn`, but for format validation
/// (`pattern`/`min`/`max`) rather than RBAC, and matches EITHER a
/// struct's `create` OR `update` slot — unlike an edit gate (which only
/// makes sense once a row already exists to compare against), a format
/// constraint applies just as much to a brand-new row as to a changed
/// one, so both write slots need the same check. Returns `None` if
/// `fn_name` isn't a struct's `create`/`update` slot, or that struct's
/// screen declares no `pattern`/`min`/`max` at all.
pub fn field_validations_for_fn(program: &Program, fn_name: &str) -> Option<(String, Vec<ValidatedField>)> {
    for s in &program.structs {
        if PRELUDE_STRUCT_NAMES.contains(&s.name.as_str()) {
            continue;
        }
        let Some(decl) = find_screen_decl(program, &s.name) else { continue };
        let snake = to_snake_case(&s.name);
        let crud_fn_name = |kind: &str, default: String| -> String {
            decl.entries
                .iter()
                .find(|(k, _)| k == kind)
                .and_then(|(_, v)| if let Expr::Ident(n, _) = v { Some(n.clone()) } else { None })
                .unwrap_or(default)
        };
        let is_create = crud_fn_name("create", format!("create_{snake}")) == fn_name;
        let is_update = crud_fn_name("update", format!("update_{snake}")) == fn_name;
        if !is_create && !is_update {
            continue;
        }
        let validations = validations_from_screen_decl(decl);
        if validations.is_empty() {
            return None;
        }
        return Some((s.name.clone(), validations));
    }
    None
}

/// Applies a `screen <Struct> { field <name> { ... } }` block's per-field
/// overrides (`label`, `view`, `edit`, `pattern`, `min`, `max`) to a
/// `FieldSpec` tree — either
/// `owner_struct`'s own top-level fields directly (`Screen.fields`), or,
/// one level down, a struct-typed action param's `nested` fields (an
/// action's `c: Counterparty` param is itself a `FieldSpec` with
/// `control == "struct"`, `label == "Counterparty"`, and *its* `nested`
/// holding Counterparty's actual fields — recognized by that `label`
/// match before descending, so a param belonging to some *other* struct
/// entirely — a custom action's own unrelated params — is left alone).
fn apply_field_overrides(decl: Option<&ScreenDecl>, fields: &mut [FieldSpec], owner_struct: &str) {
    let Some(d) = decl else { return };
    for spec in fields.iter_mut() {
        if spec.control == "struct" {
            if spec.label == owner_struct {
                apply_field_overrides(Some(d), &mut spec.nested, owner_struct);
            }
            continue;
        }
        let Some(fo) = d.fields.iter().find(|fo| fo.field_name == spec.name) else { continue };
        if let Some(label) = kv_str(&fo.entries, "label") {
            spec.display_label = Some(label.to_string());
        }
        let (view_roles, view_claim) = kv_gate(&fo.entries, "view");
        spec.view_roles = view_roles;
        spec.view_claim = view_claim;
        let (edit_roles, edit_claim) = kv_gate(&fo.entries, "edit");
        spec.edit_roles = edit_roles;
        spec.edit_claim = edit_claim;
        spec.pattern = resolve_pattern(&fo.entries);
        spec.min = kv_num(&fo.entries, "min");
        spec.max = kv_num(&fo.entries, "max");
        // Already proven by `typeck.rs::check_field_render_expr` to be
        // one of a fixed set — `"countdown"` is the only one with
        // meaning yet (Track E3); matched explicitly rather than
        // passing any string through, so a future closed-vocabulary
        // addition here is a deliberate one-line change, not implicit.
        spec.render = match kv_str(&fo.entries, "render") {
            Some("countdown") => Some("countdown"),
            _ => None,
        };
    }
}

fn build_screens(program: &Program, effects: &HashMap<String, FnEffects>) -> Vec<Screen> {
    let mut screens = vec![];
    for s in &program.structs {
        if PRELUDE_STRUCT_NAMES.contains(&s.name.as_str()) {
            continue;
        }
        let decl = find_screen_decl(program, &s.name);
        let snake = to_snake_case(&s.name);

        // `screen <Struct> { list: other_fn }` overrides which fn backs
        // a given CRUD slot; a slot the block doesn't mention keeps the
        // `<kind>_<snake>` inferred name. Every target here was already
        // confirmed to resolve to a real fn by typeck's `check_screen`.
        let crud_fn_name = |kind: &str, default: String| -> String {
            decl.and_then(|d| d.entries.iter().find(|(k, _)| k == kind))
                .and_then(|(_, v)| if let Expr::Ident(n, _) = v { Some(n.clone()) } else { None })
                .unwrap_or(default)
        };
        let mut actions: Vec<Action> = [
            ("list", crud_fn_name("list", format!("list_{snake}"))),
            ("create", crud_fn_name("create", format!("create_{snake}"))),
            ("update", crud_fn_name("update", format!("update_{snake}"))),
            ("delete", crud_fn_name("delete", format!("delete_{snake}"))),
            ("get", crud_fn_name("get", format!("get_{snake}"))),
        ]
        .into_iter()
        .filter_map(|(kind, name)| build_action(program, effects, kind, &name))
        .collect();

        // Custom actions declared on the screen, appended after the
        // inferred CRUD set (rendered as extra per-row buttons — see
        // `ui_gen_template.html`'s `renderListScreen`).
        if let Some(d) = decl {
            for a in &d.actions {
                if let Some(action) = build_custom_action(program, effects, a) {
                    actions.push(action);
                }
            }
        }

        if actions.is_empty() {
            continue; // no convention fn at all -- not a screen, just a data type
        }
        let is_singular = !actions.iter().any(|a| a.kind == "list") && actions.iter().any(|a| a.kind == "get" || a.kind == "update");
        let mut fields: Vec<FieldSpec> = s.fields.iter().map(|f| build_field_root(program, &f.name, &f.ty)).collect();

        // `screen <Struct> { field <name> { label: "...", view: role(...),
        // edit: role(...) } }` — display-label and RBAC-gate overrides,
        // applied after inference so a screen block can relabel/gate just
        // one field and leave everything else untouched. Applied to
        // `fields` (the list/detail view's own field list) AND to every
        // action's `params` (an action's struct-typed param, e.g.
        // `create_<S>(x: S)`/`update_<S>(x: S)`, expands into its own
        // `nested` fields via `build_field` — a completely separate
        // `FieldSpec` tree from `fields` above, since forms and list/
        // detail views are rendered from different manifest paths — so
        // without this second pass, a form would never see the override
        // at all, only the list/detail view would).
        apply_field_overrides(decl, &mut fields, &s.name);
        for action in &mut actions {
            apply_field_overrides(decl, &mut action.params, &s.name);
        }

        let title = decl.and_then(|d| kv_str(&d.entries, "title")).map(str::to_string).unwrap_or_else(|| to_display_label(&s.name));
        screens.push(Screen { struct_name: s.name.clone(), title, module: s.module.clone(), fields, actions, is_singular });
    }
    screens
}

/// One `WorkflowQueue` per declared `workflow` — `WORKFLOW.md`'s "state
/// ownership + a generated queue UI" section. Both synthesized fns
/// (`list_<workflow>_pending_for_me`/`advance_<workflow>`) always exist
/// once `workflow_lower.rs` has run (this pass only ever sees an already-
/// lowered `Program`), so — unlike `build_screens`' "no convention fn at
/// all -> not a screen" skip — every declared `workflow` becomes a queue
/// entry unconditionally.
fn build_workflow_queues(program: &Program, effects: &HashMap<String, FnEffects>) -> Vec<WorkflowQueue> {
    let mut queues = vec![];
    for w in &program.workflows {
        let snake = to_snake_case(&w.name);
        let pending_fn_name = format!("list_{snake}_pending_for_me");
        let submitted_by_me_fn_name = format!("list_{snake}_submitted_by_me");
        let advance_fn_name = format!("advance_{snake}");
        let history_fn_name = format!("get_{snake}_history");
        // `workflow_lower.rs` always emits all four; absence means a
        // stale/hand-edited AST, not a real case — skip the queue
        // entirely rather than render a partially-wired one.
        let (Some(pending_fn), Some(submitted_by_me_fn)) = (
            build_action(program, effects, "list", &pending_fn_name),
            build_action(program, effects, "list", &submitted_by_me_fn_name),
        ) else {
            continue;
        };
        let data_fields: Vec<FieldSpec> = w.data.iter().map(|f| build_field_root(program, &f.name, &f.ty)).collect();
        let all_states: Vec<String> = w.states.iter().map(|s| s.name.clone()).collect();
        queues.push(WorkflowQueue {
            name: w.name.clone(),
            title: to_display_label(&w.name),
            pending_fn,
            advance_fn_name,
            submitted_by_me_fn,
            history_fn_name,
            data_fields,
            all_states,
        });
    }
    queues
}

fn workflows_json(queues: &[WorkflowQueue]) -> String {
    let value = serde_json::json!(queues
        .iter()
        .map(|q| serde_json::json!({
            "name": q.name,
            "snake": to_snake_case(&q.name),
            "title": q.title,
            "pendingFn": q.pending_fn.fn_name,
            "pendingRequiresLogin": q.pending_fn.requires_login,
            "advanceFn": q.advance_fn_name,
            "submittedByMeFn": q.submitted_by_me_fn.fn_name,
            "historyFn": q.history_fn_name,
            "dataFields": q.data_fields.iter().map(field_json).collect::<Vec<_>>(),
            "allStates": q.all_states,
        }))
        .collect::<Vec<_>>());
    serde_json::to_string(&value).expect("workflow manifest is built from plain strings/bools, always serializes")
}

/// One `Workspace` per declared `workspace` block — `typeck.rs::
/// check_workspace` already proved `subject` names a real struct with an
/// `id: i64` field and every panel's `source` has the right one-`i64`-
/// param/`Result(json, _)` shape, so this trusts that the same way
/// `build_screens`/`build_workflow_queues` trust their own typeck passes.
/// A malformed workspace (already reported as an error) is simply
/// skipped here rather than built partially — typecheck already failed
/// the whole program in that case, so `ui_gen` never actually runs on it
/// (`main.rs::typecheck_and_own` gates codegen on a clean typeck pass).
fn build_workspaces(program: &Program, effects: &HashMap<String, FnEffects>) -> Vec<Workspace> {
    let mut workspaces = vec![];
    for wd in &program.workspaces {
        let Some((_, Expr::Ident(subject_struct, _))) = wd.entries.iter().find(|(k, _)| k == "subject") else {
            continue;
        };
        let snake = to_snake_case(subject_struct);
        let subject_fields: Vec<FieldSpec> = resolve_struct(program, subject_struct)
            .map(|s| s.fields.iter().map(|f| build_field_root(program, &f.name, &f.ty)).collect())
            .unwrap_or_default();

        let mut panels = vec![];
        for pd in &wd.panels {
            let Some((_, Expr::Ident(source_fn, _))) = pd.entries.iter().find(|(k, _)| k == "source") else {
                continue;
            };
            let Some(source) = build_action(program, effects, "source", source_fn) else { continue };
            let actions: Vec<Action> = pd.actions.iter().filter_map(|a| build_custom_action(program, effects, a)).collect();
            let render = PanelRender::from_kv(&pd.entries);
            panels.push(Panel { title: pd.title.clone(), source, actions, render });
        }

        let title = kv_str(&wd.entries, "title").map(str::to_string).unwrap_or_else(|| to_display_label(&wd.name));
        let subject_get = build_action(program, effects, "get", &format!("get_{snake}"));
        workspaces.push(Workspace {
            name: wd.name.clone(),
            title,
            subject_struct: subject_struct.clone(),
            subject_get,
            subject_fields,
            panels,
        });
    }
    workspaces
}

fn workspaces_json(workspaces: &[Workspace]) -> String {
    let value = serde_json::json!(workspaces
        .iter()
        .map(|w| serde_json::json!({
            "name": w.name,
            "snake": to_snake_case(&w.name),
            "title": w.title,
            "subject": w.subject_struct,
            "subjectSnake": to_snake_case(&w.subject_struct),
            "subjectGetFn": w.subject_get.as_ref().map(|a| a.fn_name.as_str()),
            "subjectGetParam": w.subject_get.as_ref().and_then(|a| a.params.first()).map(|p| p.name.as_str()).unwrap_or("id"),
            "subjectFields": w.subject_fields.iter().map(field_json).collect::<Vec<_>>(),
            "panels": w.panels.iter().map(|p| serde_json::json!({
                "title": p.title,
                "render": p.render.as_str(),
                "sourceFn": p.source.fn_name,
                // Guaranteed exactly one param by `typeck.rs::
                // check_workspace`'s shape check before `ui_gen` ever
                // runs (`main.rs::typecheck_and_own` gates codegen on a
                // clean typeck pass) -- `unwrap_or("id")` is defensive
                // only, never actually exercised.
                "sourceParam": p.source.params.first().map(|f| f.name.as_str()).unwrap_or("id"),
                "sourceRequiresLogin": p.source.requires_login,
                "sourceRequiredRole": p.source.required_role,
                "sourceRequiredClaim": p.source.required_claim,
                "actions": p.actions.iter().map(|a| serde_json::json!({
                    "fn": a.fn_name, "requiresLogin": a.requires_login,
                    "requiredRole": a.required_role, "requiredClaim": a.required_claim,
                    "badges": a.effect_badges,
                    "params": a.params.iter().map(field_json).collect::<Vec<_>>(),
                    "label": a.label, "style": a.style, "confirm": a.confirm, "showResult": a.show_result,
                })).collect::<Vec<_>>(),
            })).collect::<Vec<_>>(),
        }))
        .collect::<Vec<_>>());
    serde_json::to_string(&value).expect("workspace manifest is built from plain strings/bools, always serializes")
}

fn field_json(f: &FieldSpec) -> serde_json::Value {
    serde_json::json!({
        "name": f.name, "control": f.control, "required": f.required, "label": f.label,
        "displayLabel": f.display_label,
        "nested": f.nested.iter().map(field_json).collect::<Vec<_>>(),
        "options": f.options,
        "requiredViewRoles": f.view_roles, "requiredViewClaim": f.view_claim,
        "requiredEditRoles": f.edit_roles, "requiredEditClaim": f.edit_claim,
        "pattern": f.pattern, "min": f.min, "max": f.max, "render": f.render,
    })
}

/// The demo-mode login screen's "what can I try" catalog — every role/
/// claim string `typeck::collect_role_claim_strings` found anywhere in
/// the program, so a visitor picks from what's actually gated instead
/// of guessing. Shape mirrors `field_json`'s "one flat JSON object,
/// plain strings" convention: `{"roles": [...], "claims": [{"key":...,
/// "value":...}, ...]}`.
fn identity_catalog_json(roles: &[String], claims: &[(String, String)]) -> String {
    let value = serde_json::json!({
        "roles": roles,
        "claims": claims.iter().map(|(k, v)| serde_json::json!({"key": k, "value": v})).collect::<Vec<_>>(),
    });
    serde_json::to_string(&value).expect("role/claim catalog is built from plain strings, always serializes")
}

/// A small, always-visible app-bar badge naming which identity mode
/// this server is running in — the user-visible half of "the system
/// understands if it's running in demo or production mode" (the other
/// half is `IDENTITY_CATALOG`'s bootstrap check re-validating any
/// `localStorage`-cached identity against `GET /api/_whoami` before
/// trusting it, since a demo server's ephemeral signing key is
/// different on every restart). Plain server-rendered HTML, not a JS
/// decision — it's known at `generate()` time and should never flash
/// unstyled/absent before JS runs. Empty string for neither mode
/// (`nirdosha emit-ui`'s static-file output, or a real-identity server
/// with no `--oidc-*` SSO configured) — today's byte-identical app-bar.
fn mode_badge_html(demo_mode: bool, production_mode: bool) -> String {
    if demo_mode {
        r#"<span class="mode-badge mode-badge-demo" title="No --jwks-file/--issuer/--audience configured -- self-service sign-in mints a real but ephemeral, per-process token, never a stand-in for production identity.">Demo Mode</span>"#.to_string()
    } else if production_mode {
        r#"<span class="mode-badge mode-badge-production" title="Real identity provider configured -- sign-in redirects to your organization's own hosted login.">Production</span>"#.to_string()
    } else {
        String::new()
    }
}

fn metrics_json(metrics: &[Metric]) -> String {
    let value = serde_json::json!(metrics
        .iter()
        .map(|m| serde_json::json!({
            "label": m.label, "fn": m.fn_name, "requiresLogin": m.requires_login,
            "requiredRole": m.required_role, "requiredClaim": m.required_claim,
            // Always present (also on `STATS`/plain `chart_`-convention
            // entries, always `"bar_chart"` there) rather than a second
            // JSON shape just for `visual` items -- the client ignores
            // it entirely for a tile, and a plain chart's own default
            // renders byte-for-byte the same as before this key existed.
            "render": m.render.as_str(),
        }))
        .collect::<Vec<_>>());
    serde_json::to_string(&value).expect("stats/charts manifest is built from plain strings/bools, always serializes")
}

fn manifest_json(screens: &[Screen]) -> String {
    let value = serde_json::json!(screens
        .iter()
        .map(|sc| {
            serde_json::json!({
                "name": sc.struct_name,
                "title": sc.title,
                "module": sc.module,
                "snake": to_snake_case(&sc.struct_name),
                // The generic `/_nirdosha/table/<table>` pagination route
                // (`serve.rs`) assumes the DB table name is exactly the
                // struct's own snake_case — this app's own established,
                // universal convention, not enforced by the type system.
                // `columns` is the allowlist that route validates
                // `sort_field`/`filters` keys against before they're ever
                // interpolated into SQL text.
                "table": to_snake_case(&sc.struct_name),
                "columns": sc.fields.iter().map(|f| f.name.clone()).collect::<Vec<_>>(),
                "singular": sc.is_singular,
                "fields": sc.fields.iter().map(field_json).collect::<Vec<_>>(),
                "actions": sc.actions.iter().map(|a| serde_json::json!({
                    "kind": a.kind, "fn": a.fn_name, "requiresLogin": a.requires_login,
                    "requiredRole": a.required_role, "requiredClaim": a.required_claim,
                    "badges": a.effect_badges,
                    "params": a.params.iter().map(field_json).collect::<Vec<_>>(),
                    "label": a.label, "style": a.style, "confirm": a.confirm, "showResult": a.show_result,
                })).collect::<Vec<_>>(),
            })
        })
        .collect::<Vec<_>>());
    serde_json::to_string(&value).expect("manifest is built from plain strings/bools, always serializes")
}

/// Entry point for `nirdosha emit-ui`/`nirdosha serve`. `program` must
/// already be typechecked + ownership-checked (see
/// `main.rs::typecheck_and_own`) — this pass trusts struct/fn shapes are
/// well-formed and only re-derives the UI-relevant subset of what typeck
/// already confirmed.
///
/// `identity_base`, when set, points the generated login screen at a
/// real `nirdosha serve`d identity app's `/api/login` (e.g.
/// `examples/identity_mock.nir`) instead of the pure client-side stub:
/// it POSTs there, stores the *real* signed token it gets back, and
/// attaches it as `Authorization: Bearer <token>` on every `callFn`
/// request — the only way `requires(role: ...)`-gated actions can
/// actually pass `serve.rs`'s server-side authz check (see that
/// module's doc comment for why that check exists at all). `None` keeps
/// the original pure-stub behavior (`emit-ui`'s own tests use this —
/// no server assumed).
///
/// `server_table_api` reflects whether the caller (`nirdosha serve
/// --db <path>`) exposes the generic `/_nirdosha/table/<snake>`
/// pagination/sort/filter/search route (`serve.rs`). `false` (the
/// default — `nirdosha emit-ui`'s static-file mode always passes this,
/// since there's no running server to route to) means every table
/// renders exactly as it always has: one unpaginated `callFn(listFn,
/// {})` fetch, no pagination/sort/search controls shown at all — a
/// deliberate, disclosed degradation, not a broken feature, for any
/// screen whose author-written `list_<struct>` does custom joins/logic
/// the generic endpoint can't see.
///
/// `demo_mode`/`production_mode` pick the generated login screen's
/// third and fourth branches (`IDENTITY_BASE`'s own POST-to-a-mock-
/// identity-server flow stays first, unconditionally, and is untouched
/// by either): `demo_mode` shows the self-service role/claim picker
/// backed by `serve.rs`'s `/api/_demo_login` (only meaningful, and only
/// ever true, when `nirdosha serve` has no real `--jwks-file`/
/// `--issuer`/`--audience` configured); `production_mode` shows a
/// "Sign in with SSO" redirect backed by `serve.rs`'s `/auth/login`
/// (only true when the `--oidc-*` flags are configured, which itself
/// requires the real identity trio). Both `false` — `nirdosha emit-ui`'s
/// static-file mode, no server behind either route — falls back to the
/// original free-text stub, unchanged.
pub fn generate(
    program: &Program,
    effects: &HashMap<String, FnEffects>,
    identity_base: Option<&str>,
    server_table_api: bool,
    demo_mode: bool,
    production_mode: bool,
    theme: Option<&Theme>,
) -> String {
    let screens = build_screens(program, effects);
    let manifest = manifest_json(&screens);
    let stats = metrics_json(&build_stats(program));
    let charts = metrics_json(&build_charts(program));
    let workflows = workflows_json(&build_workflow_queues(program, effects));
    let workspaces = workspaces_json(&build_workspaces(program, effects));
    let identity_base_js = match identity_base {
        Some(url) => serde_json::to_string(url).expect("a URL string always serializes"),
        None => "null".to_string(),
    };
    let (all_roles, all_claims) = crate::typeck::collect_role_claim_strings(program);
    let identity_catalog = identity_catalog_json(&all_roles, &all_claims);
    TEMPLATE
        .replace("__NIRDOSHA_MANIFEST__", &manifest)
        .replace("__NIRDOSHA_STATS__", &stats)
        .replace("__NIRDOSHA_CHARTS__", &charts)
        .replace("__NIRDOSHA_WORKFLOWS__", &workflows)
        .replace("__NIRDOSHA_WORKSPACES__", &workspaces)
        .replace("__NIRDOSHA_IDENTITY_BASE__", &identity_base_js)
        .replace("__NIRDOSHA_IDENTITY_CATALOG__", &identity_catalog)
        .replace("__NIRDOSHA_DEMO_MODE__", if demo_mode { "true" } else { "false" })
        .replace("__NIRDOSHA_PRODUCTION_MODE__", if production_mode { "true" } else { "false" })
        .replace("__NIRDOSHA_MODE_BADGE__", &mode_badge_html(demo_mode, production_mode))
        .replace("__NIRDOSHA_SERVER_TABLE_API__", if server_table_api { "true" } else { "false" })
        .replace("__NIRDOSHA_THEME_OVERRIDE__", &theme_override_css(theme))
        .replace("__NIRDOSHA_HTML_CLASS__", &theme_html_class(theme))
        .replace("__NIRDOSHA_THEME_SCRIPT__", &theme_bootstrap_script(theme))
        .replace("__NIRDOSHA_FAVICON__", &favicon_data_uri())
        .replace("__NIRDOSHA_LOGO__", &logo_data_uri())
}

/// The same brand mark as `favicon_data_uri`, at a larger 96x96 size for
/// the app-bar itself (rendered at ~32px — 96px source stays crisp on a
/// 3x-density display, the favicon's 128px source would too but that one
/// stays dedicated to the `<link rel=icon>` role it already has). Same
/// "baked in at compile time, zero per-project opt-in" posture.
fn logo_data_uri() -> String {
    const LOGO_PNG: &[u8] = include_bytes!("nirdosha-app-bar-icon.png");
    format!("data:image/png;base64,{}", BASE64_STANDARD.encode(LOGO_PNG))
}

/// The Nirdosha brand mark (`assets/brand/`'s standalone icon, cropped
/// tighter and downsized to a 128x128/16-color PNG for a small embed
/// payload — ~2KB source, ~2.6KB base64) as an inline `data:` URI, baked
/// into every `emit-ui`/`serve` page via `__NIRDOSHA_FAVICON__` with zero
/// network dependency — same "zero network dependency by default"
/// posture `ui_gen_template.html`'s own header comment already states for
/// fonts. `include_bytes!` pulls the PNG in at COMPILE time, so every
/// `.nir` file gets this icon automatically the moment a built `nirdosha`
/// binary runs `emit-ui`/`serve` on it — no per-project asset, no
/// override flag, nothing the project author has to opt into.
fn favicon_data_uri() -> String {
    const FAVICON_PNG: &[u8] = include_bytes!("nirdosha-favicon.png");
    format!("data:image/png;base64,{}", BASE64_STANDARD.encode(FAVICON_PNG))
}

/// Optional per-project theme, layered on top of the baked-in Material
/// Design 3 token set (`ui_gen_template.html`'s own `:root`/dark `:root`
/// blocks) rather than replacing it — every top-level field is `Option`,
/// and an absent section simply leaves those tokens at their MD3
/// defaults. This struct is a 1:1 mirror of protobox's
/// `resolve_design_tokens(spec)` (`be-v2/src/features/design_studio/
/// generate_palettes.py`) — a project's `theme.json` IS that function's
/// direct JSON output (`nirdosha.py`'s theme mapper writes it verbatim,
/// no hand-picked subset, no second field-name vocabulary to keep in
/// sync by hand) — unknown fields (e.g. its `spec_hash`) are ignored by
/// ordinary `serde` deserialization, not an error. Each present
/// sub-object's own fields are NOT further `Option`-wrapped: `resolve_
/// design_tokens()` always emits a section whole or not at all, never
/// partially, so this struct only needs one level of "is this section
/// present" optionality.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct Theme {
    /// 11-step brand ramp, keys `"50"`..`"950"` (`RAMP_STEPS`), values
    /// `#rrggbb`.
    pub brand: Option<std::collections::HashMap<String, String>>,
    /// Same 11-step shape, the neutral scale (page background, borders,
    /// text hierarchy).
    pub neutral: Option<std::collections::HashMap<String, String>>,
    pub fonts: Option<ThemeFonts>,
    pub radius: Option<ThemeRadius>,
    pub shadow_card: Option<String>,
    pub density: Option<ThemeDensity>,
    pub motion: Option<ThemeMotion>,
    /// `"none" | "media" | "class" | "always"` — `DesignSpec.dark_mode`'s
    /// own vocabulary, passed through unchanged (`theme_override_css`
    /// dispatches on the string directly rather than parsing it into a
    /// Rust enum, since the only consumer is string-matching into a CSS
    /// shape, not language-level logic).
    pub dark_mode: Option<String>,
    pub layout: Option<ThemeLayout>,
    pub type_scale: Option<ThemeTypeScale>,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeFonts {
    pub sans: String,
    pub display: String,
    pub mono: Option<String>,
}

/// CSS length strings (`"0.375rem"`, `"9999px"`, ...) — `DesignSpec.
/// radius_tokens()`'s own value shape, not px numbers.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeRadius {
    pub control: String,
    pub card: String,
}

/// All four fields are px numbers (`generate_palettes.py::_DENSITY_PX`).
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeDensity {
    pub card_padding: f64,
    pub gap: f64,
    pub control_height: f64,
    pub font_size: f64,
}

/// `DesignSpec`'s motion vocabulary, fully resolved
/// (`generate_palettes.py::resolve_design_tokens`'s `"motion"` key) —
/// `animations` is a subset of the fixed 4-name vocabulary
/// `ui_gen_template.html`'s `@keyframes` block hardcodes (`fade-in`/
/// `slide-up`/`scale-in`/`pop`), never an arbitrary string.
#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeMotion {
    pub level: String,
    pub interaction_ms: f64,
    pub entrance_ms: f64,
    pub easing: String,
    pub animations: Vec<String>,
    pub hover_lift_px: f64,
    pub hover_scale: f64,
    pub press_scale: f64,
    pub stagger_ms: f64,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeLayout {
    /// `"auto" | "sidebar" | "topbar" | "sidebar-topbar" | "minimal"` —
    /// `"auto"`/absent keeps today's fixed nav-rail + top-app-bar shell,
    /// byte-for-byte, for every `.nir` app that predates this field.
    pub app_shell: String,
    /// `"auto" | "boxed" | "fluid"`.
    pub content_width: String,
}

#[derive(Debug, Clone, serde::Deserialize)]
pub struct ThemeTypeScale {
    pub level: String,
    pub title_px: f64,
    pub hero_px: f64,
}

/// A CSS value is only ever a bare hex color, a CSS length (`12px`,
/// `0.5rem`, ...), a font-family list, a bare number, or a `cubic-
/// bezier(...)`/keyword easing function here — never markup. Reject
/// anything containing `<`, `>`, `{`, or `}` outright rather than trying
/// to validate each shape precisely: those four characters are the only
/// ones that could break out of "one CSS custom-property declaration
/// value" into a new rule or (via `<`/`>`) out of the `<style>` element
/// entirely, and a theme file with a legitimate value never needs any of
/// them.
fn theme_value_is_safe(v: &str) -> bool {
    !v.is_empty() && !v.contains(['<', '>', '{', '}'])
}

/// `RAMP_STEPS` (`design_spec.py`) in order — the fixed 11-step key set
/// every `brand`/`neutral` ramp carries. Iterated in this order (not
/// `HashMap`'s arbitrary order) so `--brand-50`.`--brand-950` emit
/// deterministically; a step missing from a hand-authored (non-
/// `resolve_design_tokens()`-sourced) theme file is silently skipped,
/// same tolerant posture every other optional theme field already takes.
const RAMP_STEPS: [&str; 11] = ["50", "100", "200", "300", "400", "500", "600", "700", "800", "900", "950"];

/// Semantic MD3 role -> which ramp step backs it, light and dark. Not
/// sourced from an unread protobox internal file — `resolve_design_
/// tokens()` hands over the raw 11-step ramps only, not pre-resolved
/// semantic roles, so this mapping is this integration's own choice,
/// using ordinary, widely-used Tailwind-ramp semantic conventions (mid
/// ramp for a light-mode primary/on light text, inverted for dark) —
/// visually correct against the project's real brand/neutral colors
/// either way, since the ramp values themselves come straight from
/// `DesignSpec`, only the *step choice* per role is this file's own.
struct RampRoleStep {
    step: &'static str,
    dark_step: &'static str,
}
const PRIMARY: RampRoleStep = RampRoleStep { step: "600", dark_step: "300" };
const ON_PRIMARY: RampRoleStep = RampRoleStep { step: "50", dark_step: "900" };
const PRIMARY_CONTAINER: RampRoleStep = RampRoleStep { step: "100", dark_step: "800" };
const ON_PRIMARY_CONTAINER: RampRoleStep = RampRoleStep { step: "900", dark_step: "100" };
const SURFACE: RampRoleStep = RampRoleStep { step: "50", dark_step: "900" };
const ON_SURFACE: RampRoleStep = RampRoleStep { step: "900", dark_step: "50" };
const SURFACE_VARIANT: RampRoleStep = RampRoleStep { step: "100", dark_step: "800" };
const ON_SURFACE_VARIANT: RampRoleStep = RampRoleStep { step: "600", dark_step: "400" };
const OUTLINE: RampRoleStep = RampRoleStep { step: "300", dark_step: "600" };
const BACKGROUND: RampRoleStep = RampRoleStep { step: "50", dark_step: "950" };

fn theme_override_css(theme: Option<&Theme>) -> String {
    let Some(t) = theme else { return String::new() };
    let push = |out: &mut Vec<String>, indent: &str, prop: &str, value: &str| {
        if theme_value_is_safe(value) {
            out.push(format!("{indent}--{prop}: {value};"));
        }
    };
    let push_opt = |out: &mut Vec<String>, indent: &str, prop: &str, value: &Option<String>| {
        if let Some(v) = value {
            push(out, indent, prop, v);
        }
    };
    let push_num = |out: &mut Vec<String>, indent: &str, prop: &str, value: f64, suffix: &str| {
        out.push(format!("{indent}--{prop}: {value}{suffix};"));
    };
    let ramp_color = |ramp: &std::collections::HashMap<String, String>, role: &RampRoleStep, dark: bool| -> Option<String> {
        ramp.get(if dark { role.dark_step } else { role.step }).cloned()
    };

    let mut base = Vec::new();
    let mut dark = Vec::new();

    if let Some(brand) = &t.brand {
        for step in RAMP_STEPS {
            if let Some(v) = brand.get(step) {
                push(&mut base, "    ", &format!("brand-{step}"), v);
            }
        }
        push_opt(&mut base, "    ", "md-primary", &ramp_color(brand, &PRIMARY, false));
        push_opt(&mut base, "    ", "md-on-primary", &ramp_color(brand, &ON_PRIMARY, false));
        push_opt(&mut base, "    ", "md-primary-container", &ramp_color(brand, &PRIMARY_CONTAINER, false));
        push_opt(&mut base, "    ", "md-on-primary-container", &ramp_color(brand, &ON_PRIMARY_CONTAINER, false));
        push_opt(&mut dark, "      ", "md-primary", &ramp_color(brand, &PRIMARY, true));
        push_opt(&mut dark, "      ", "md-on-primary", &ramp_color(brand, &ON_PRIMARY, true));
        push_opt(&mut dark, "      ", "md-primary-container", &ramp_color(brand, &PRIMARY_CONTAINER, true));
        push_opt(&mut dark, "      ", "md-on-primary-container", &ramp_color(brand, &ON_PRIMARY_CONTAINER, true));
    }
    if let Some(neutral) = &t.neutral {
        for step in RAMP_STEPS {
            if let Some(v) = neutral.get(step) {
                push(&mut base, "    ", &format!("neutral-{step}"), v);
            }
        }
        push_opt(&mut base, "    ", "md-surface", &ramp_color(neutral, &SURFACE, false));
        push_opt(&mut base, "    ", "md-on-surface", &ramp_color(neutral, &ON_SURFACE, false));
        push_opt(&mut base, "    ", "md-surface-variant", &ramp_color(neutral, &SURFACE_VARIANT, false));
        push_opt(&mut base, "    ", "md-on-surface-variant", &ramp_color(neutral, &ON_SURFACE_VARIANT, false));
        push_opt(&mut base, "    ", "md-outline", &ramp_color(neutral, &OUTLINE, false));
        push_opt(&mut base, "    ", "md-background", &ramp_color(neutral, &BACKGROUND, false));
        push_opt(&mut dark, "      ", "md-surface", &ramp_color(neutral, &SURFACE, true));
        push_opt(&mut dark, "      ", "md-on-surface", &ramp_color(neutral, &ON_SURFACE, true));
        push_opt(&mut dark, "      ", "md-surface-variant", &ramp_color(neutral, &SURFACE_VARIANT, true));
        push_opt(&mut dark, "      ", "md-on-surface-variant", &ramp_color(neutral, &ON_SURFACE_VARIANT, true));
        push_opt(&mut dark, "      ", "md-outline", &ramp_color(neutral, &OUTLINE, true));
        push_opt(&mut dark, "      ", "md-background", &ramp_color(neutral, &BACKGROUND, true));
    }
    if let Some(fonts) = &t.fonts {
        push(&mut base, "    ", "font-sans", &fonts.sans);
        push(&mut base, "    ", "md-font", &fonts.sans);
        push(&mut base, "    ", "font-display", &fonts.display);
        if let Some(mono) = &fonts.mono {
            push(&mut base, "    ", "font-mono", mono);
        }
    }
    if let Some(radius) = &t.radius {
        push(&mut base, "    ", "radius-control", &radius.control);
        push(&mut base, "    ", "radius-card", &radius.card);
        push(&mut base, "    ", "md-radius-sm", &radius.control);
        push(&mut base, "    ", "md-radius-md", &radius.control);
        push(&mut base, "    ", "md-radius-lg", &radius.card);
    }
    push_opt(&mut base, "    ", "shadow-card", &t.shadow_card);
    if let Some(density) = &t.density {
        push_num(&mut base, "    ", "density-card-padding", density.card_padding, "px");
        push_num(&mut base, "    ", "density-gap", density.gap, "px");
        push_num(&mut base, "    ", "density-control-height", density.control_height, "px");
        push_num(&mut base, "    ", "density-font-size", density.font_size, "px");
    }
    if let Some(motion) = &t.motion {
        push_num(&mut base, "    ", "motion-interaction-ms", motion.interaction_ms, "ms");
        push_num(&mut base, "    ", "motion-entrance-ms", motion.entrance_ms, "ms");
        push(&mut base, "    ", "motion-easing", &motion.easing);
        push_num(&mut base, "    ", "hover-lift-px", motion.hover_lift_px, "px");
        push_num(&mut base, "    ", "hover-scale", motion.hover_scale, "");
        push_num(&mut base, "    ", "press-scale", motion.press_scale, "");
        push_num(&mut base, "    ", "stagger-ms", motion.stagger_ms, "");
    }
    if let Some(type_scale) = &t.type_scale {
        push_num(&mut base, "    ", "title-px", type_scale.title_px, "px");
        push_num(&mut base, "    ", "hero-px", type_scale.hero_px, "px");
    }

    let mut out = String::new();
    if !base.is_empty() {
        out.push_str("  :root {\n");
        out.push_str(&base.join("\n"));
        out.push_str("\n  }\n");
    }
    // `dark_mode` strategy dispatch — "media" (default when a brand/
    // neutral ramp is present but no strategy is named, matching every
    // pre-existing theme's behavior) wraps the same overrides in
    // `prefers-color-scheme`; "class" wraps them in `:root.dark` instead
    // (the template's own tiny toggle script, added alongside this,
    // flips that class); "always" writes the dark values directly into
    // the base `:root` block above instead of a separate one (no light
    // variant at all); "none" emits no dark block regardless of ramp
    // presence.
    if !dark.is_empty() {
        match t.dark_mode.as_deref() {
            Some("none") => {}
            Some("class") => {
                out.push_str("  :root.dark {\n");
                out.push_str(&dark.join("\n"));
                out.push_str("\n  }\n");
            }
            Some("always") => {
                out.push_str("  :root {\n");
                out.push_str(&dark.join("\n").replace("      ", "    "));
                out.push_str("\n  }\n");
            }
            _ => {
                out.push_str("  @media (prefers-color-scheme: dark) {\n    :root {\n");
                out.push_str(&dark.join("\n"));
                out.push_str("\n    }\n  }\n");
            }
        }
    }
    out
}

/// `Theme.layout`'s `app_shell`/`content_width` -- computed once, here,
/// into a static `<html class="...">` list (`__NIRDOSHA_HTML_CLASS__`)
/// rather than shipped to the client as JSON: both are whole-page layout
/// decisions known entirely at generation time, so there is nothing for
/// client JS to decide at runtime. `"auto"`/absent (or an unrecognized
/// value — tolerated, not an error, same posture as an unset field)
/// contributes no class at all, keeping today's one fixed shell
/// (`ui_gen_template.html`'s nav-rail + top-app-bar) the default for
/// every `.nir` app, unaffected by this field's mere existence.
fn theme_html_class(theme: Option<&Theme>) -> String {
    let Some(t) = theme else { return String::new() };
    let Some(layout) = &t.layout else { return String::new() };
    let mut classes = Vec::new();
    match layout.app_shell.as_str() {
        "topbar" => classes.push("shell-topbar"),
        "minimal" => classes.push("shell-minimal"),
        _ => {} // "auto" | "sidebar" | "sidebar-topbar" | anything else: today's shell, unchanged
    }
    match layout.content_width.as_str() {
        "boxed" => classes.push("content-boxed"),
        "fluid" => classes.push("content-fluid"),
        _ => {}
    }
    classes.join(" ")
}

/// `Theme.dark_mode == "class"` needs one small inline script
/// (`__NIRDOSHA_THEME_SCRIPT__`, spliced into `<head>` before `<style>`
/// so it runs before first paint — no flash of the wrong mode): with no
/// manual light/dark toggle control in this template, "class" strategy
/// still needs *something* to decide the class's initial state, so it
/// mirrors system preference the same way "media" would, just via a
/// class instead of a media query — a real, distinct CSS mechanism
/// (proven by `tests/emit_ui.rs`'s `dark_mode_class_*` tests), not
/// cosmetically identical output to "media" reached a different way.
/// Every other `dark_mode` value (including absent) emits nothing.
fn theme_bootstrap_script(theme: Option<&Theme>) -> String {
    let Some(t) = theme else { return String::new() };
    if t.dark_mode.as_deref() != Some("class") {
        return String::new();
    }
    "<script>(function(){try{if(window.matchMedia('(prefers-color-scheme: dark)').matches){document.documentElement.classList.add('dark');}}catch(e){}})();</script>".to_string()
}

/// The one baked-in design: a Material Design 3 token set (color roles,
/// type scale, shape, elevation), light+dark via `prefers-color-scheme`,
/// system-font stack by default with Roboto as an opt-in `<link>` (kept
/// commented so the file has zero network dependency out of the box).
/// Chrome is a nav rail + top app bar; content is a generic renderer
/// driven entirely by `__NIRDOSHA_MANIFEST__` — no per-struct markup is
/// generated, the same handful of render functions handle every screen.
const TEMPLATE: &str = include_str!("ui_gen_template.html");

#[cfg(test)]
mod tests {
    use super::*;
    use crate::token::Span;

    const SPAN: Span = Span { line: 0, col: 0 };

    fn call(name: &str, args: Vec<Expr>) -> Expr {
        Expr::Call(name.to_string(), args, SPAN)
    }

    fn str_expr(s: &str) -> Expr {
        Expr::Str(s.to_string(), SPAN)
    }

    #[test]
    fn kv_gate_extracts_role_list() {
        let entries = vec![("view".to_string(), call("role", vec![str_expr("a"), str_expr("b")]))];
        let (roles, claim) = kv_gate(&entries, "view");
        assert_eq!(roles, vec!["a".to_string(), "b".to_string()]);
        assert!(claim.is_none());
    }

    #[test]
    fn kv_gate_extracts_claim_pair() {
        let entries = vec![("edit".to_string(), call("claim", vec![str_expr("dept"), str_expr("sales")]))];
        let (roles, claim) = kv_gate(&entries, "edit");
        assert!(roles.is_empty());
        assert_eq!(claim, Some(("dept".to_string(), "sales".to_string())));
    }

    #[test]
    fn kv_gate_absent_key_is_ungated() {
        let entries = vec![("label".to_string(), str_expr("Whatever"))];
        let (roles, claim) = kv_gate(&entries, "view");
        assert!(roles.is_empty() && claim.is_none());
    }

    fn int_expr(n: i64) -> Expr {
        Expr::Int(n, SPAN)
    }

    fn float_expr(n: f64) -> Expr {
        Expr::Float(n, SPAN)
    }

    #[test]
    fn kv_num_extracts_int_literal() {
        let entries = vec![("min".to_string(), int_expr(5))];
        assert_eq!(kv_num(&entries, "min"), Some(5.0));
    }

    #[test]
    fn kv_num_extracts_float_literal() {
        let entries = vec![("max".to_string(), float_expr(9.5))];
        assert_eq!(kv_num(&entries, "max"), Some(9.5));
    }

    #[test]
    fn kv_num_absent_key_is_none() {
        let entries = vec![("label".to_string(), str_expr("Whatever"))];
        assert_eq!(kv_num(&entries, "min"), None);
    }

    #[test]
    fn resolve_pattern_prefers_explicit_pattern_over_format() {
        let entries = vec![("pattern".to_string(), str_expr("^abc$")), ("format".to_string(), str_expr("email"))];
        assert_eq!(resolve_pattern(&entries), Some("^abc$".to_string()));
    }

    #[test]
    fn resolve_pattern_expands_known_format() {
        let entries = vec![("format".to_string(), str_expr("email"))];
        assert_eq!(resolve_pattern(&entries), Some(crate::ast::well_known_format_pattern("email").unwrap().to_string()));
    }

    #[test]
    fn resolve_pattern_absent_is_none() {
        let entries = vec![("label".to_string(), str_expr("Whatever"))];
        assert_eq!(resolve_pattern(&entries), None);
    }

    #[test]
    fn validations_from_screen_decl_collects_pattern_min_max() {
        let decl = ScreenDecl {
            struct_name: "Widget".to_string(),
            entries: vec![],
            fields: vec![
                crate::ast::FieldOverride {
                    field_name: "name".to_string(),
                    entries: vec![("pattern".to_string(), str_expr("^[A-Z]"))],
                    span: SPAN,
                },
                crate::ast::FieldOverride {
                    field_name: "quantity".to_string(),
                    entries: vec![("min".to_string(), int_expr(0)), ("max".to_string(), int_expr(100))],
                    span: SPAN,
                },
                crate::ast::FieldOverride { field_name: "untouched".to_string(), entries: vec![], span: SPAN },
            ],
            actions: vec![],
            span: SPAN,
        };
        let validations = validations_from_screen_decl(&decl);
        assert_eq!(validations.len(), 2, "the untouched field should contribute nothing");
        let name_v = validations.iter().find(|v| v.field_name == "name").unwrap();
        assert_eq!(name_v.pattern, Some("^[A-Z]".to_string()));
        let qty_v = validations.iter().find(|v| v.field_name == "quantity").unwrap();
        assert_eq!(qty_v.min, Some(0.0));
        assert_eq!(qty_v.max, Some(100.0));
    }
}
