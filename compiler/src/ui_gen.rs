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
//! S)` convention); enum references and deeper nesting render read-only
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
//! search, sort, field-level RBAC enforcement, form insert/update
//! modes).

use std::collections::{BTreeSet, HashMap};

use crate::ast::{Effect, Expr, FnDecl, Program, Requirement, ScreenDecl, Ty};
use crate::effects::FnEffects;

/// Prepended to every `Program.structs` by `ast::prelude_structs()` —
/// infrastructure types, never a user's own data model, so screens are
/// never derived from them.
const PRELUDE_STRUCT_NAMES: &[&str] =
    &["HttpResponse", "VerifiedIdentity", "RoleView", "ClaimView", "ApplicationSession", "RefreshTokenHandle", "Pair"];

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
///   deps" stance).
///
/// Same role/claim/login gating machinery as `Action` — a metric can be
/// just as sensitive as any other call.
struct Metric {
    label: String,
    fn_name: String,
    requires_login: bool,
    required_role: Option<String>,
    required_claim: Option<(String, String)>,
}

/// One derived screen: a user `struct` plus whichever CRUD-convention
/// functions (`list_<s>`/`create_<s>`/`update_<s>`/`delete_<s>`/`get_<s>`)
/// exist for it. A screen with no `list_*` renders as a singular
/// settings-style form instead of a table (`is_singular`).
struct Screen {
    struct_name: String,
    /// Display title (nav label, heading, toast text) — the struct name
    /// unless a `screen <Struct> { title: "..." }` block overrides it.
    title: String,
    fields: Vec<FieldSpec>,
    actions: Vec<Action>,
    is_singular: bool,
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

/// A short, honest label for a field's declared type — used only on
/// `readonly` fields, where there's no input control to speak for itself.
fn ty_label(ty: &Ty) -> String {
    match ty {
        Ty::Str => "str".to_string(),
        Ty::Bool => "bool".to_string(),
        Ty::F64 => "f64".to_string(),
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

/// Maps one `name: ty` (a struct field, or a CRUD action's own param) to
/// a form control. `Option(T)` unwraps to `T`'s control with
/// `required = false`. A bare reference to another struct in this same
/// program (e.g. `create_todo(t: Todo)`'s `t`) expands one level deep
/// into that struct's own fields (`control = "struct"`, `nested` holds
/// them) — the common `create_<S>(x: S)` convention would otherwise
/// render as a single unfillable blob. `depth` caps that expansion at
/// two levels, which also makes it cycle-safe against a self-referential
/// struct without needing a separate visited-set. Anything still left —
/// enum references, affine handles (`box`/`thread`/`chan`/`sandbox`/
/// `tcp`/`file`), `Result`, `Fn`, deeper-than-cap nesting — renders
/// `readonly` instead of guessing; a stated v1 limit, not a silent
/// misrender.
fn build_field(program: &Program, name: &str, ty: &Ty, depth: u8) -> FieldSpec {
    let base = |control, required| FieldSpec {
        name: name.to_string(),
        control,
        required,
        label: ty_label(ty),
        display_label: None,
        nested: vec![],
    };
    match ty {
        Ty::Str => base("text", true),
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize | Ty::F64 => {
            base("number", true)
        }
        Ty::Bool => base("checkbox", false),
        Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
            let mut inner = build_field(program, name, &args[0], depth);
            inner.required = false;
            inner
        }
        Ty::Named(n, args) if args.is_empty() && depth < 2 => match resolve_struct(program, n) {
            Some(s) => FieldSpec {
                name: name.to_string(),
                control: "struct",
                required: true,
                label: n.clone(),
                display_label: None,
                nested: s.fields.iter().map(|f| build_field(program, &f.name, &f.ty, depth + 1)).collect(),
            },
            None => base("readonly", false),
        },
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
        .map(|p| build_field(program, &p.name, &p.ty, 0))
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

/// Shared by `build_stats`/`build_charts`: every zero-arg fn whose name
/// starts with `prefix` and whose return type passes `return_ok`
/// becomes one `Metric`, labeled from the rest of its name.
fn build_metrics(program: &Program, prefix: &str, return_ok: impl Fn(&Ty) -> bool) -> Vec<Metric> {
    program
        .fns
        .iter()
        .filter(|f| f.name.starts_with(prefix) && f.params.is_empty() && return_ok(&f.ret))
        .map(|f| {
            let (required_role, required_claim) = fn_role_gate(f);
            Metric {
                label: to_title_case(f.name.strip_prefix(prefix).unwrap_or(&f.name)),
                fn_name: f.name.clone(),
                requires_login: fn_requires_login(f) || required_role.is_some() || required_claim.is_some(),
                required_role,
                required_claim,
            }
        })
        .collect()
}

fn build_stats(program: &Program) -> Vec<Metric> {
    build_metrics(program, "stat_", is_stat_return_ty)
}

fn build_charts(program: &Program) -> Vec<Metric> {
    build_metrics(program, "chart_", is_chart_return_ty)
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
        let mut fields: Vec<FieldSpec> = s.fields.iter().map(|f| build_field(program, &f.name, &f.ty, 0)).collect();

        // `screen <Struct> { field <name> { label: "..." } }` — display-
        // label overrides, applied after inference so a screen block can
        // relabel just one field and leave everything else untouched.
        if let Some(d) = decl {
            for fo in &d.fields {
                if let Some(spec) = fields.iter_mut().find(|f| f.name == fo.field_name) {
                    if let Some(label) = kv_str(&fo.entries, "label") {
                        spec.display_label = Some(label.to_string());
                    }
                }
            }
        }

        let title = decl.and_then(|d| kv_str(&d.entries, "title")).map(str::to_string).unwrap_or_else(|| s.name.clone());
        screens.push(Screen { struct_name: s.name.clone(), title, fields, actions, is_singular });
    }
    screens
}

fn field_json(f: &FieldSpec) -> serde_json::Value {
    serde_json::json!({
        "name": f.name, "control": f.control, "required": f.required, "label": f.label,
        "displayLabel": f.display_label,
        "nested": f.nested.iter().map(field_json).collect::<Vec<_>>(),
    })
}

fn metrics_json(metrics: &[Metric]) -> String {
    let value = serde_json::json!(metrics
        .iter()
        .map(|m| serde_json::json!({
            "label": m.label, "fn": m.fn_name, "requiresLogin": m.requires_login,
            "requiredRole": m.required_role, "requiredClaim": m.required_claim,
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
                "snake": to_snake_case(&sc.struct_name),
                "singular": sc.is_singular,
                "fields": sc.fields.iter().map(field_json).collect::<Vec<_>>(),
                "actions": sc.actions.iter().map(|a| serde_json::json!({
                    "kind": a.kind, "fn": a.fn_name, "requiresLogin": a.requires_login,
                    "requiredRole": a.required_role, "requiredClaim": a.required_claim,
                    "badges": a.effect_badges,
                    "params": a.params.iter().map(field_json).collect::<Vec<_>>(),
                    "label": a.label, "style": a.style, "confirm": a.confirm,
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
pub fn generate(program: &Program, effects: &HashMap<String, FnEffects>, identity_base: Option<&str>) -> String {
    let screens = build_screens(program, effects);
    let manifest = manifest_json(&screens);
    let stats = metrics_json(&build_stats(program));
    let charts = metrics_json(&build_charts(program));
    let identity_base_js = match identity_base {
        Some(url) => serde_json::to_string(url).expect("a URL string always serializes"),
        None => "null".to_string(),
    };
    TEMPLATE
        .replace("__NIRDOSHA_MANIFEST__", &manifest)
        .replace("__NIRDOSHA_STATS__", &stats)
        .replace("__NIRDOSHA_CHARTS__", &charts)
        .replace("__NIRDOSHA_IDENTITY_BASE__", &identity_base_js)
}

/// The one baked-in design: a Material Design 3 token set (color roles,
/// type scale, shape, elevation), light+dark via `prefers-color-scheme`,
/// system-font stack by default with Roboto as an opt-in `<link>` (kept
/// commented so the file has zero network dependency out of the box).
/// Chrome is a nav rail + top app bar; content is a generic renderer
/// driven entirely by `__NIRDOSHA_MANIFEST__` — no per-struct markup is
/// generated, the same handful of render functions handle every screen.
const TEMPLATE: &str = include_str!("ui_gen_template.html");
