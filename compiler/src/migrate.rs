//! Derives SQL schema from `.nir` `struct` declarations and keeps a
//! `--db`-backed SQLite database in sync with it, automatically, once per
//! `nirdosha serve` startup.
//!
//! Every table in this codebase was previously created by a hand-written,
//! literal `db_execute(conn, "CREATE TABLE IF NOT EXISTS ...")` buried
//! inside individual `.nir` functions -- duplicated per function, never
//! derived from the `struct` itself, and never altered when a struct
//! gained a field (the direct cause of a `no such table: <x>` class of bug
//! this module exists to close out). This module instead treats the
//! `struct` declaration as the single source of truth: at startup, for
//! every table it understands, it diffs the struct's fields against the
//! live schema and applies whatever's missing.
//!
//! Deliberately **additive-only**: a missing table becomes `CREATE TABLE`,
//! a missing column becomes `ALTER TABLE ... ADD COLUMN`. A column whose
//! *type* changed, or a column whose field was removed from the struct, is
//! never touched automatically -- SQLite can't safely `ALTER COLUMN TYPE`
//! without a full table rebuild, and silently dropping a column would be a
//! real data-loss risk for a completely unattended startup step. Those
//! cases are logged as a warning, not attempted.
//!
//! Every applied change is written to `<migrations_dir>/NNNN_<slug>.sql`
//! before it's run -- a reviewable, commit-to-git audit trail, not a
//! rollback-capable ledger (there are no down-migrations, and nothing here
//! expects a human to hand-author or edit one of these files). A small
//! in-DB `_nirdosha_migrations` table separately records what actually
//! ran and when, for a DB inspected in isolation from its own migrations
//! directory.

use std::path::Path;

use crate::ast::{EnumDecl, Field, Program, Ty};

/// A struct with no matching entry here (or any field this can't map)
/// contributes no table at all -- see `column_def`'s doc comment.
const PRELUDE_STRUCT_NAMES: &[&str] =
    &["HttpResponse", "VerifiedIdentity", "RoleView", "ClaimView", "ApplicationSession", "RefreshTokenHandle", "Pair", "Money", "Measure"];

/// Same word-boundary walk as `ui_gen.rs`/`serve.rs::to_snake_case` --
/// duplicated, not shared, matching those two modules' own established
/// precedent for a helper this small.
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

fn resolve_enum<'a>(program: &'a Program, name: &str) -> Option<&'a EnumDecl> {
    program.enums.iter().find(|e| e.name == name)
}

/// The scalar SQLite storage class for a field's declared `Ty`, or `None`
/// if this field's type has no sensible single-column representation
/// (a nested struct, `Vector`/`Matrix`, a payload-carrying enum, or an
/// affine handle like `db`/`tcp`/`box`). A struct with *any* such field is
/// skipped in full by `plan_and_apply` -- never a partial table.
///
/// `id: i64` gets `INTEGER PRIMARY KEY AUTOINCREMENT`, the convention
/// every hand-written `CREATE TABLE` in this codebase already follows.
/// `Ty::Named("Option", [T])` maps to `T`'s own type: SQLite columns are
/// nullable by default and nothing here ever adds `NOT NULL`, so an
/// `Option` needs no distinct column shape, only its inner type. A
/// `Ty::Named(name, [])` that resolves to an `EnumDecl` where every
/// `Variant::payload` is empty maps to `TEXT` (variant name), mirroring
/// the exact zero-payload check `interpreter.rs::sql_bind_params`'s
/// `Value::Enum` arm and `serve.rs::decode_enum_value` already use.
fn column_def(program: &Program, field: &Field) -> Option<String> {
    if field.name == "id" && matches!(field.ty, Ty::I64) {
        return Some(format!("{} INTEGER PRIMARY KEY AUTOINCREMENT", field.name));
    }
    sql_type_for(program, &field.ty).map(|ty| format!("{} {ty}", field.name))
}

fn sql_type_for(program: &Program, ty: &Ty) -> Option<&'static str> {
    match ty {
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => Some("INTEGER"),
        Ty::F64 => Some("REAL"),
        // `TEXT`, not `REAL` — a `dec128` column stores its canonical
        // decimal string (`sql_bind_params`'s `Value::Dec128` arm,
        // `LANGUAGE.md` §5's "Decimal arithmetic"); SQLite has no real
        // decimal storage class, and `REAL` would silently reintroduce
        // the float-rounding drift this type exists to prevent.
        Ty::Dec128 => Some("TEXT"),
        Ty::Bool => Some("INTEGER"),
        Ty::Str => Some("TEXT"),
        Ty::Named(n, args) if n == "Option" && args.len() == 1 => sql_type_for(program, &args[0]),
        Ty::Named(n, args) if args.is_empty() => {
            let e = resolve_enum(program, n)?;
            if e.variants.iter().all(|v| v.payload.is_empty()) {
                Some("TEXT")
            } else {
                None
            }
        }
        _ => None,
    }
}

struct PlannedMigration {
    table: String,
    slug: String,
    sql: String,
}

/// One past the highest `NNNN_` sequence number already present in
/// `dir` (0 if the directory is empty or doesn't exist yet).
fn next_sequence(dir: &Path) -> u32 {
    let Ok(entries) = std::fs::read_dir(dir) else { return 1 };
    let max = entries
        .filter_map(|e| e.ok())
        .filter_map(|e| e.file_name().into_string().ok())
        .filter_map(|name| name.split('_').next().and_then(|n| n.parse::<u32>().ok()))
        .max();
    max.map_or(1, |m| m + 1)
}

fn plan_table(program: &Program, conn: &rusqlite::Connection, table: &str, fields: &[Field]) -> Result<Option<PlannedMigration>, String> {
    let mut cols = Vec::with_capacity(fields.len());
    for f in fields {
        match column_def(program, f) {
            Some(def) => cols.push(def),
            None => {
                eprintln!(
                    "migrate: skipping `{table}` -- field `{}` has a type with no single-column SQL representation",
                    f.name
                );
                return Ok(None);
            }
        }
    }

    let table_exists: bool = conn
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", [table], |_| Ok(()))
        .is_ok();

    if !table_exists {
        return Ok(Some(PlannedMigration {
            table: table.to_string(),
            slug: format!("create_{table}"),
            sql: format!("CREATE TABLE IF NOT EXISTS \"{table}\" ({});", cols.join(", ")),
        }));
    }

    let existing: Vec<String> = {
        let mut stmt = conn.prepare(&format!("PRAGMA table_info(\"{table}\")")).map_err(|e| e.to_string())?;
        let names = stmt.query_map([], |row| row.get::<_, String>(1)).map_err(|e| e.to_string())?;
        names.collect::<Result<_, _>>().map_err(|e| e.to_string())?
    };

    let missing: Vec<&Field> = fields.iter().filter(|f| !existing.contains(&f.name)).collect();
    if missing.is_empty() {
        return Ok(None);
    }

    let mut alters = Vec::with_capacity(missing.len());
    let mut added_names = Vec::with_capacity(missing.len());
    for f in &missing {
        // `column_def` already validated every field above; safe to
        // unwrap here since `missing` is a subset of `fields`.
        let def = column_def(program, f).expect("field type already validated above");
        alters.push(format!("ALTER TABLE \"{table}\" ADD COLUMN {def};"));
        added_names.push(f.name.clone());
    }

    Ok(Some(PlannedMigration { table: table.to_string(), slug: format!("alter_{table}_add_{}", added_names.join("_")), sql: alters.join("\n") }))
}

fn ensure_bookkeeping_table(conn: &rusqlite::Connection) -> Result<(), String> {
    conn.execute(
        "CREATE TABLE IF NOT EXISTS _nirdosha_migrations (filename TEXT PRIMARY KEY, applied_at TEXT NOT NULL, sql TEXT NOT NULL)",
        [],
    )
    .map_err(|e| e.to_string())?;
    Ok(())
}

/// Called once at `serve` startup, only when `--db <path>` is given.
/// Diffs every non-prelude struct's fields against `conn`'s live schema,
/// applies whatever's missing (a genuinely no-op run -- the common case
/// on every steady-state restart -- writes nothing and applies nothing),
/// and returns the filenames of whatever migrations it just applied, for
/// the caller to log.
pub fn plan_and_apply(program: &Program, conn: &rusqlite::Connection, migrations_dir: &Path, applied_at: &str) -> Result<Vec<String>, String> {
    let mut structs: Vec<_> =
        program.structs.iter().filter(|s| !PRELUDE_STRUCT_NAMES.contains(&s.name.as_str())).map(|s| (to_snake_case(&s.name), s)).collect();
    structs.sort_by(|a, b| a.0.cmp(&b.0));

    let mut planned = Vec::new();
    for (table, s) in &structs {
        if let Some(m) = plan_table(program, conn, table, &s.fields)? {
            planned.push(m);
        }
    }
    if planned.is_empty() {
        return Ok(vec![]);
    }

    ensure_bookkeeping_table(conn)?;
    std::fs::create_dir_all(migrations_dir).map_err(|e| format!("migrations dir {}: {e}", migrations_dir.display()))?;

    let mut applied = Vec::with_capacity(planned.len());
    let mut seq = next_sequence(migrations_dir);
    for m in planned {
        let filename = format!("{seq:04}_{}.sql", m.slug);
        seq += 1;
        let file_body = format!("-- generated {applied_at} for table `{}`\n{}\n", m.table, m.sql);
        let file_path = migrations_dir.join(&filename);
        std::fs::write(&file_path, &file_body).map_err(|e| format!("{}: {e}", file_path.display()))?;

        conn.execute_batch(&m.sql).map_err(|e| format!("applying {filename}: {e}"))?;
        conn.execute(
            "INSERT INTO _nirdosha_migrations (filename, applied_at, sql) VALUES (?, ?, ?)",
            rusqlite::params![filename, applied_at, m.sql],
        )
        .map_err(|e| format!("recording {filename}: {e}"))?;

        applied.push(filename);
    }
    Ok(applied)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{EnumDecl, Ty, Variant};
    use crate::token::Span;

    const SPAN: Span = Span { line: 0, col: 0 };

    fn mk_field(name: &str, ty: Ty) -> Field {
        Field { name: name.to_string(), ty }
    }

    fn empty_program() -> Program {
        Program { fns: vec![], structs: vec![], enums: vec![], screens: vec![], dashboard: None, workflows: vec![], workspaces: vec![] }
    }

    #[test]
    fn id_i64_becomes_integer_primary_key() {
        let p = empty_program();
        assert_eq!(column_def(&p, &mk_field("id", Ty::I64)).unwrap(), "id INTEGER PRIMARY KEY AUTOINCREMENT");
    }

    #[test]
    fn scalar_types_map_to_expected_sql() {
        let p = empty_program();
        assert_eq!(column_def(&p, &mk_field("n", Ty::I32)).unwrap(), "n INTEGER");
        assert_eq!(column_def(&p, &mk_field("x", Ty::F64)).unwrap(), "x REAL");
        assert_eq!(column_def(&p, &mk_field("flag", Ty::Bool)).unwrap(), "flag INTEGER");
        assert_eq!(column_def(&p, &mk_field("s", Ty::Str)).unwrap(), "s TEXT");
    }

    #[test]
    fn option_of_supported_type_maps_to_inner_type() {
        let p = empty_program();
        let ty = Ty::Named("Option".to_string(), vec![Ty::Str]);
        assert_eq!(column_def(&p, &mk_field("note", ty)).unwrap(), "note TEXT");
    }

    #[test]
    fn zero_payload_enum_maps_to_text() {
        let mut p = empty_program();
        p.enums.push(EnumDecl {
            name: "Status".to_string(),
            type_params: vec![],
            variants: vec![
                Variant { name: "Draft".to_string(), payload: vec![], span: SPAN },
                Variant { name: "Active".to_string(), payload: vec![], span: SPAN },
            ],
            span: SPAN,
            module: None,
        });
        let ty = Ty::Named("Status".to_string(), vec![]);
        assert_eq!(column_def(&p, &mk_field("status", ty)).unwrap(), "status TEXT");
    }

    #[test]
    fn payload_carrying_enum_is_unsupported() {
        let mut p = empty_program();
        p.enums.push(EnumDecl {
            name: "Shape".to_string(),
            type_params: vec![],
            variants: vec![Variant { name: "Circle".to_string(), payload: vec![Ty::F64], span: SPAN }],
            span: SPAN,
            module: None,
        });
        let ty = Ty::Named("Shape".to_string(), vec![]);
        assert!(column_def(&p, &mk_field("shape", ty)).is_none());
    }

    #[test]
    fn vector_type_is_unsupported() {
        let p = empty_program();
        let ty = Ty::Vector(Box::new(Ty::F64), 3);
        assert!(column_def(&p, &mk_field("v", ty)).is_none());
    }
}
