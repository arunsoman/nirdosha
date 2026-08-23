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

use crate::ast::{Effect, Expr, Field, FnDecl, Program, Requirement, ScreenDecl, Ty};
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
        options: vec![],
        view_roles: vec![],
        view_claim: None,
        edit_roles: vec![],
        edit_claim: None,
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
        Ty::Bool => base("checkbox", false),
        Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
            let mut inner = build_field(program, name, &args[0], depth);
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
            if depth < 2 {
                if let Some(s) = resolve_struct(program, n) {
                    return FieldSpec {
                        name: name.to_string(),
                        control: "struct",
                        required: true,
                        label: n.clone(),
                        display_label: None,
                        nested: s.fields.iter().map(|f| build_field(program, &f.name, &f.ty, depth + 1)).collect(),
                        options: vec![],
                        view_roles: vec![],
                        view_claim: None,
                        edit_roles: vec![],
                        edit_claim: None,
                    };
                }
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

/// Applies a `screen <Struct> { field <name> { ... } }` block's per-field
/// overrides (`label`, `view`, `edit`) to a `FieldSpec` tree — either
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
        let mut fields: Vec<FieldSpec> = s.fields.iter().map(|f| build_field(program, &f.name, &f.ty, 0)).collect();

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

fn field_json(f: &FieldSpec) -> serde_json::Value {
    serde_json::json!({
        "name": f.name, "control": f.control, "required": f.required, "label": f.label,
        "displayLabel": f.display_label,
        "nested": f.nested.iter().map(field_json).collect::<Vec<_>>(),
        "options": f.options,
        "requiredViewRoles": f.view_roles, "requiredViewClaim": f.view_claim,
        "requiredEditRoles": f.edit_roles, "requiredEditClaim": f.edit_claim,
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
pub fn generate(
    program: &Program,
    effects: &HashMap<String, FnEffects>,
    identity_base: Option<&str>,
    server_table_api: bool,
    theme: Option<&Theme>,
) -> String {
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
        .replace("__NIRDOSHA_SERVER_TABLE_API__", if server_table_api { "true" } else { "false" })
        .replace("__NIRDOSHA_THEME_OVERRIDE__", &theme_override_css(theme))
}

/// Optional per-project theme, layered on top of the baked-in Material
/// Design 3 token set (`ui_gen_template.html`'s own `:root`/dark `:root`
/// blocks) rather than replacing it — every field is `Option`, and an
/// absent field simply leaves that token at its MD3 default. Sourced from
/// protobox's `DesignSpec`/DESIGN.md (`be-v2/src/plugins/languages/
/// nirdosha.py`'s theme mapper writes this as JSON next to the project's
/// `.nir` entrypoint) — deliberately a narrow token set (primary/
/// on-primary/primary-container/on-primary-container color roles, corner
/// radii, one font stack), not a full re-theme: nirdosha's generated UI
/// has exactly one layout (`ui_gen_template.html`'s nav-rail + top-app-bar
/// shell, driven entirely by the manifest, same as before this existed),
/// so there is no per-component styling surface to expose beyond these
/// tokens without inventing one.
#[derive(Debug, Default, Clone, serde::Deserialize)]
pub struct Theme {
    pub primary_light: Option<String>,
    pub on_primary_light: Option<String>,
    pub primary_container_light: Option<String>,
    pub on_primary_container_light: Option<String>,
    pub primary_dark: Option<String>,
    pub on_primary_dark: Option<String>,
    pub primary_container_dark: Option<String>,
    pub on_primary_container_dark: Option<String>,
    pub radius_sm: Option<String>,
    pub radius_md: Option<String>,
    pub radius_lg: Option<String>,
    pub font_sans: Option<String>,
}

/// A CSS value is only ever a bare hex color, a CSS length (`12px`,
/// `0.5rem`, ...), or a font-family list here (`ui_gen.rs`'s only three
/// `Theme` value shapes) — never markup. Reject anything containing `<`,
/// `>`, `{`, or `}` outright rather than trying to validate each shape
/// precisely: those four characters are the only ones that could break
/// out of "one CSS custom-property declaration value" into a new rule or
/// (via `<`/`>`) out of the `<style>` element entirely, and a theme file
/// with a legitimate value never needs any of them.
fn theme_value_is_safe(v: &str) -> bool {
    !v.is_empty() && !v.contains(['<', '>', '{', '}'])
}

fn theme_override_css(theme: Option<&Theme>) -> String {
    let Some(t) = theme else { return String::new() };
    let push = |out: &mut Vec<String>, indent: &str, prop: &str, value: &Option<String>| {
        if let Some(v) = value {
            if theme_value_is_safe(v) {
                out.push(format!("{indent}--{prop}: {v};"));
            }
        }
    };

    let mut light = Vec::new();
    push(&mut light, "    ", "md-primary", &t.primary_light);
    push(&mut light, "    ", "md-on-primary", &t.on_primary_light);
    push(&mut light, "    ", "md-primary-container", &t.primary_container_light);
    push(&mut light, "    ", "md-on-primary-container", &t.on_primary_container_light);
    push(&mut light, "    ", "md-radius-sm", &t.radius_sm);
    push(&mut light, "    ", "md-radius-md", &t.radius_md);
    push(&mut light, "    ", "md-radius-lg", &t.radius_lg);
    push(&mut light, "    ", "md-font", &t.font_sans);

    let mut dark = Vec::new();
    push(&mut dark, "      ", "md-primary", &t.primary_dark);
    push(&mut dark, "      ", "md-on-primary", &t.on_primary_dark);
    push(&mut dark, "      ", "md-primary-container", &t.primary_container_dark);
    push(&mut dark, "      ", "md-on-primary-container", &t.on_primary_container_dark);

    let mut out = String::new();
    if !light.is_empty() {
        out.push_str("  :root {\n");
        out.push_str(&light.join("\n"));
        out.push_str("\n  }\n");
    }
    if !dark.is_empty() {
        out.push_str("  @media (prefers-color-scheme: dark) {\n    :root {\n");
        out.push_str(&dark.join("\n"));
        out.push_str("\n    }\n  }\n");
    }
    out
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
}
