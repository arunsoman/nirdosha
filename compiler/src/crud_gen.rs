//! `nirdosha gen-crud <plan.json> --db <literal>` — deterministic
//! struct+CRUD `.nir` source from a JSON entity plan. Replaces protobox's
//! `nirdosha_screen_plan.py::_stub_fns`, which emits placeholder bodies
//! (`create_` echoes its input, `list_` returns `[]`, `update_`/`delete_`
//! are no-ops) for every CRUD slot regardless of what a real generated
//! project needs. Every real generated `.nir` file this project has ever
//! produced by hand or by LLM (`b2b.nir`, `examples/trade-finance/
//! trade_finance.nir`) uses the exact same mechanical shape for
//! persistence: `db_connect` a fixed literal, `db_execute`/`db_query` raw
//! SQL whose column list is exactly the struct's field list, `WHERE id = ?`
//! for anything touching one row. None of that needs an LLM to write
//! correctly, and every LLM call spent writing it today is one that could
//! have been spent on the actual novel content (`mvp-nirdosha-lean-
//! pipeline-plan.md` in the protobox repo). Living here, not as a second
//! Python implementation in protobox, means it can never drift from the
//! grammar this compiler actually accepts.
//!
//! v1 scope, same as the screen-plan this consumes: scalar fields only
//! (`i64`/`f64`/`str`/`bool`) — no enum-typed fields (would need a paired
//! `<Enum>_code(v) -> Text` match-converter, real future work, not this
//! pass) and no nested struct/`Option`/`Vector`/`Matrix` fields.

use std::collections::BTreeMap;

use serde::Deserialize;

#[derive(Deserialize, Clone)]
pub struct FieldSpec {
    pub name: String,
    #[serde(rename = "type")]
    pub ty: String,
}

#[derive(Deserialize, Clone)]
pub struct EntityPlan {
    pub struct_name: String,
    #[serde(default)]
    pub fields: Vec<FieldSpec>,
    pub crud_slots: Vec<String>,
    #[serde(default)]
    pub screen_title: Option<String>,
    #[serde(default)]
    pub field_labels: BTreeMap<String, String>,
}

#[derive(Deserialize, Clone)]
pub struct KpiSpec {
    pub name: String,
    pub label: String,
}

#[derive(Deserialize, Clone)]
pub struct ScreenPlan {
    pub entities: Vec<EntityPlan>,
    #[serde(default)]
    pub kpis: Vec<KpiSpec>,
}

/// snake_case column/table/fn-suffix name from a PascalCase struct name —
/// same algorithm as `nirdosha_screen_plan.py::_to_snake`, so table names
/// agree byte-for-byte with whatever protobox already derived when it
/// rendered the struct declaration.
fn to_snake(name: &str) -> String {
    let mut out = String::new();
    for (i, ch) in name.chars().enumerate() {
        if ch.is_uppercase() && i > 0 && !name.chars().nth(i - 1).unwrap().is_uppercase() {
            out.push('_');
        }
        out.extend(ch.to_lowercase());
    }
    out
}

fn json_getter(ty: &str) -> Result<&'static str, String> {
    match ty {
        "i64" => Ok("json_get_i64"),
        "f64" => Ok("json_get_f64"),
        "str" => Ok("json_get_str"),
        "bool" => Ok("json_get_bool"),
        other => Err(format!(
            "gen-crud v1 only supports scalar field types i64/f64/str/bool, got {other:?} \
             (enum/struct/Option/Vector/Matrix fields need a hand-written or v2 converter)"
        )),
    }
}

fn validate_fields(entity: &EntityPlan) -> Result<(), String> {
    for f in &entity.fields {
        json_getter(&f.ty)?;
    }
    Ok(())
}

/// The struct declaration — `id: i64` is always first and implicit, exactly
/// matching `nirdosha_screen_plan.py::_render_entity`'s convention (callers
/// never declare their own `id` field).
fn render_struct(entity: &EntityPlan) -> String {
    let mut lines = vec!["    id: i64,".to_string()];
    for f in &entity.fields {
        lines.push(format!("    {}: {},", f.name, f.ty));
    }
    format!("struct {} {{\n{}\n}}", entity.struct_name, lines.join("\n"))
}

fn render_list(entity: &EntityPlan, snake: &str, db: &str) -> String {
    let mut cols = vec!["id".to_string()];
    cols.extend(entity.fields.iter().map(|f| f.name.clone()));
    let sql = format!("SELECT {} FROM {} ORDER BY id", cols.join(", "), snake);
    format!(
        "fn list_{snake}() -> Result(json, Text) {{\n\
        \x20   return match db_connect(\"{db}\") {{\n\
        \x20       Ok(conn) => match db_query(conn, \"{sql}\") {{\n\
        \x20           Ok(rows) => Ok(rows),\n\
        \x20           Err(e) => Err(Text(e)),\n\
        \x20       }},\n\
        \x20       Err(e) => Err(Text(e)),\n\
        \x20   }}\n}}"
    )
}

/// One level of the `get_`/decode-chain `match`, built with each level's
/// OWN absolute indentation (`depth`) computed up front — rather than
/// nesting a finished inner string inside an outer template and trying to
/// re-indent it after the fact (fragile: a fixed-width `indent()` pass
/// can't know how far right the previous line's `"Ok(x) => "` prefix
/// already pushed it). Each recursive call only ever emits lines at its
/// own `depth`/`depth + 1`, so the whole chain comes out correctly nested
/// regardless of how deep it goes.
fn render_decode_chain(steps: &[(String, String)], idx: usize, ctor: &str, depth: usize) -> String {
    let pad = "    ".repeat(depth);
    let inner_pad = "    ".repeat(depth + 1);
    if idx == steps.len() {
        return ctor.to_string();
    }
    let (getter, var) = &steps[idx];
    let inner = render_decode_chain(steps, idx + 1, ctor, depth + 1);
    format!("match {getter} {{\n{inner_pad}Ok({var}) => {inner},\n{inner_pad}Err(e) => Err(Text(e)),\n{pad}}}")
}

/// `get_<snake>(id) -> Result(Struct, Text)` — decodes one row via a
/// nested `match` chain, one `json_get_<type>` per column (`id` first,
/// then declared fields in order), then constructs the struct
/// positionally. Mechanical, but genuinely nested — see module doc for why
/// this is still cheaper than an LLM call: the shape never varies, only the
/// field count/types do.
fn render_get(entity: &EntityPlan, snake: &str, db: &str) -> String {
    let mut cols = vec!["id".to_string()];
    cols.extend(entity.fields.iter().map(|f| f.name.clone()));
    let sql = format!("SELECT {} FROM {} WHERE id = ?", cols.join(", "), snake);

    // (getter call, bound var name) for id + every declared field, in
    // column order.
    let mut steps: Vec<(String, String)> = vec![("json_get_i64(row, \"id\")".to_string(), "id_v".to_string())];
    for f in &entity.fields {
        let getter = json_getter(&f.ty).expect("validated by validate_fields before this is called");
        steps.push((format!("{getter}(row, \"{}\")", f.name), format!("{}_v", f.name)));
    }
    let ctor_args: Vec<String> = steps.iter().map(|(_, v)| v.clone()).collect();
    let ctor = format!("Ok({}({}))", entity.struct_name, ctor_args.join(", "));
    // Decode chain starts at depth 4: fn body(1) -> db_connect match
    // arm(2) -> db_query match arm(3) -> json_array_get match arm(4).
    let row_body = render_decode_chain(&steps, 0, &ctor, 4);

    format!(
        "fn get_{snake}(id: i64) -> Result({struct_name}, Text) {{\n\
        \x20   return match db_connect(\"{db}\") {{\n\
        \x20       Ok(conn) => match db_query(conn, \"{sql}\", id) {{\n\
        \x20           Ok(rows) => match json_array_get(rows, 0) {{\n\
        \x20               Ok(row) => {row_body},\n\
        \x20               Err(e) => Err(Text(e)),\n\
        \x20           }},\n\
        \x20           Err(e) => Err(Text(e)),\n\
        \x20       }},\n\
        \x20       Err(e) => Err(Text(e)),\n\
        \x20   }}\n}}",
        struct_name = entity.struct_name,
    )
}

fn render_create(entity: &EntityPlan, snake: &str, db: &str) -> String {
    // A zero-field entity (only `id`) has no columns to list — SQLite and
    // Postgres both reject `INSERT INTO t () VALUES ()`, but both accept
    // the standard `DEFAULT VALUES` form for exactly this case.
    let sql = if entity.fields.is_empty() {
        format!("INSERT INTO {snake} DEFAULT VALUES")
    } else {
        let cols: Vec<String> = entity.fields.iter().map(|f| f.name.clone()).collect();
        let placeholders: Vec<&str> = entity.fields.iter().map(|_| "?").collect();
        format!("INSERT INTO {} ({}) VALUES ({})", snake, cols.join(", "), placeholders.join(", "))
    };
    let args: Vec<String> = entity.fields.iter().map(|f| format!("x.{}", f.name)).collect();
    let args_suffix = if args.is_empty() { String::new() } else { format!(", {}", args.join(", ")) };
    format!(
        "fn create_{snake}(x: {struct_name}) -> Result(i64, Text) {{\n\
        \x20   return match db_connect(\"{db}\") {{\n\
        \x20       Ok(conn) => match db_execute(conn, \"{sql}\"{args_suffix}) {{\n\
        \x20           Ok(n) => Ok(n),\n\
        \x20           Err(e) => Err(Text(e)),\n\
        \x20       }},\n\
        \x20       Err(e) => Err(Text(e)),\n\
        \x20   }}\n}}",
        struct_name = entity.struct_name,
    )
}

fn render_update(entity: &EntityPlan, snake: &str, db: &str) -> Result<String, String> {
    // A zero-field entity has nothing to SET besides `id` itself, which
    // isn't a meaningful update — `UPDATE t SET  WHERE id = ?` is invalid
    // SQL, and "update the id to itself" isn't a real operation worth
    // silently emitting. Reject at generation time rather than emit code
    // that compiles but fails at runtime.
    if entity.fields.is_empty() {
        return Err(format!(
            "entity {:?} has no fields besides id — \"update\" has nothing to set; \
             drop \"update\" from its crud_slots",
            entity.struct_name
        ));
    }
    let sets: Vec<String> = entity.fields.iter().map(|f| format!("{} = ?", f.name)).collect();
    let sql = format!("UPDATE {} SET {} WHERE id = ?", snake, sets.join(", "));
    let mut args: Vec<String> = entity.fields.iter().map(|f| format!("x.{}", f.name)).collect();
    args.push("x.id".to_string());
    Ok(format!(
        "fn update_{snake}(x: {struct_name}) -> Result(i64, Text) {{\n\
        \x20   return match db_connect(\"{db}\") {{\n\
        \x20       Ok(conn) => match db_execute(conn, \"{sql}\", {args}) {{\n\
        \x20           Ok(n) => Ok(n),\n\
        \x20           Err(e) => Err(Text(e)),\n\
        \x20       }},\n\
        \x20       Err(e) => Err(Text(e)),\n\
        \x20   }}\n}}",
        struct_name = entity.struct_name,
        args = args.join(", "),
    ))
}

fn render_delete(snake: &str, db: &str) -> String {
    let sql = format!("DELETE FROM {snake} WHERE id = ?");
    format!(
        "fn delete_{snake}(id: i64) -> Result(i64, Text) {{\n\
        \x20   return match db_connect(\"{db}\") {{\n\
        \x20       Ok(conn) => match db_execute(conn, \"{sql}\", id) {{\n\
        \x20           Ok(n) => Ok(n),\n\
        \x20           Err(e) => Err(Text(e)),\n\
        \x20       }},\n\
        \x20       Err(e) => Err(Text(e)),\n\
        \x20   }}\n}}"
    )
}

fn render_screen(entity: &EntityPlan) -> String {
    let mut lines = Vec::new();
    if let Some(title) = &entity.screen_title {
        lines.push(format!("    title: \"{title}\""));
    }
    for (field, label) in &entity.field_labels {
        lines.push(format!("    field {field} {{ label: \"{label}\" }}"));
    }
    if lines.is_empty() {
        return String::new();
    }
    format!("screen {} {{\n{}\n}}", entity.struct_name, lines.join("\n"))
}

/// One entity's full source: struct + real CRUD bodies for its declared
/// slots + optional `screen{}` customization. `db` is the project's single
/// `db_connect(...)` literal — the caller's job to keep byte-identical
/// across the whole project (`PROTOBOX_INTEGRATION.md` §3), not this
/// function's — it just splices whatever it's given.
pub fn render_entity(entity: &EntityPlan, db: &str) -> Result<String, String> {
    validate_fields(entity)?;
    let snake = to_snake(&entity.struct_name);
    let mut parts = vec![render_struct(entity)];
    for slot in &entity.crud_slots {
        let fn_src = match slot.as_str() {
            "list" => render_list(entity, &snake, db),
            "get" => render_get(entity, &snake, db),
            "create" => render_create(entity, &snake, db),
            "update" => render_update(entity, &snake, db)?,
            "delete" => render_delete(&snake, db),
            other => return Err(format!("unknown crud_slot {other:?} (expected list/get/create/update/delete)")),
        };
        parts.push(fn_src);
    }
    let screen = render_screen(entity);
    if !screen.is_empty() {
        parts.push(screen);
    }
    Ok(parts.join("\n\n"))
}

/// The whole plan -> real `.nir` source text. KPI tiles stay 0-value stubs
/// deliberately — a real aggregation query is genuinely domain-specific
/// (which rows count, over what time window), unlike CRUD persistence, so
/// it's still LLM/human territory; only their `stat_<name>`/`dashboard{}`
/// scaffolding is mechanical enough to render here.
pub fn render_plan(plan: &ScreenPlan, db: &str, header_comment: &str) -> Result<String, String> {
    let mut sections = Vec::new();
    if !header_comment.is_empty() {
        sections.push(header_comment.lines().map(|l| format!("// {l}")).collect::<Vec<_>>().join("\n"));
    }
    // struct Text { value: str } is the standing str-crosses-a-boundary
    // wrapper (LANGUAGE.md SS6b) every error path above returns through —
    // declared once here, never per entity, and only when at least one
    // entity actually has a CRUD slot to need it (an unused declaration
    // is harmless either way, but this matches init.rs's own
    // only-declare-what's-needed convention).
    if plan.entities.iter().any(|e| !e.crud_slots.is_empty()) {
        sections.push("struct Text {\n    value: str,\n}".to_string());
    }
    for entity in &plan.entities {
        sections.push(render_entity(entity, db)?);
    }
    if !plan.kpis.is_empty() {
        let kpi_fns: Vec<String> =
            plan.kpis.iter().map(|k| format!("fn stat_{}() -> i64 {{\n    return 0\n}}", k.name)).collect();
        sections.push(kpi_fns.join("\n\n"));
        let tiles: Vec<String> =
            plan.kpis.iter().map(|k| format!("    tile \"{}\" -> stat_{}", k.label, k.name)).collect();
        sections.push(format!("dashboard {{\n{}\n}}", tiles.join("\n")));
    }
    Ok(sections.join("\n\n") + "\n")
}
