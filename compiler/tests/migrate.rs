//! Integration tests for `migrate::plan_and_apply` (`src/migrate.rs`) —
//! derives `CREATE TABLE`/`ALTER TABLE ADD COLUMN` from `struct` field
//! declarations and applies them to a real on-disk SQLite database, once
//! per call (mirroring the once-per-`serve`-startup call site in
//! `serve.rs::run`). Pure-function style, no `tiny_http` server needed —
//! same discipline `tests/db.rs` already uses for `db_connect`/`db_query`/
//! `db_execute`, just against a real file on disk (not `:memory:`) since
//! the whole point here is state that persists *across* separate calls,
//! the same way it persists across separate `nirdosha serve` restarts.

use nirdosha::ast::Program;
use nirdosha::migrate::plan_and_apply;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

fn build_program(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
    program
}

fn temp_paths(name: &str) -> (std::path::PathBuf, std::path::PathBuf) {
    let dir = std::env::temp_dir().join(format!("nirdosha-migrate-test-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    (dir.join("app.db"), dir.join("migrations"))
}

const V1: &str = r#"
    struct Widget {
        id: i64,
        name: str,
    }
    fn main() {}
"#;

const V2: &str = r#"
    struct Widget {
        id: i64,
        name: str,
        note: str,
    }
    fn main() {}
"#;

#[test]
fn first_run_creates_table_and_one_migration_file() {
    let (db_path, migrations_dir) = temp_paths("create");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let program = build_program(V1);

    let applied = plan_and_apply(&program, &conn, &migrations_dir, "t0").expect("migration should succeed");
    assert_eq!(applied, vec!["0001_create_widget.sql".to_string()]);

    let files: Vec<_> = std::fs::read_dir(&migrations_dir).unwrap().map(|e| e.unwrap().file_name().into_string().unwrap()).collect();
    assert_eq!(files, vec!["0001_create_widget.sql".to_string()]);

    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(widget)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(cols, vec!["id".to_string(), "name".to_string()]);
}

#[test]
fn second_run_with_no_struct_change_applies_nothing() {
    let (db_path, migrations_dir) = temp_paths("noop");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let program = build_program(V1);

    plan_and_apply(&program, &conn, &migrations_dir, "t0").unwrap();
    let applied_again = plan_and_apply(&program, &conn, &migrations_dir, "t1").expect("migration should succeed");
    assert!(applied_again.is_empty(), "no struct change should mean no new migration");

    let files: Vec<_> = std::fs::read_dir(&migrations_dir).unwrap().collect();
    assert_eq!(files.len(), 1, "still only the first file");
}

#[test]
fn added_field_produces_an_alter_table_migration() {
    let (db_path, migrations_dir) = temp_paths("alter");
    let conn = rusqlite::Connection::open(&db_path).unwrap();

    plan_and_apply(&build_program(V1), &conn, &migrations_dir, "t0").unwrap();
    let applied = plan_and_apply(&build_program(V2), &conn, &migrations_dir, "t1").expect("migration should succeed");
    assert_eq!(applied, vec!["0002_alter_widget_add_note.sql".to_string()]);

    let cols: Vec<String> = conn
        .prepare("PRAGMA table_info(widget)")
        .unwrap()
        .query_map([], |row| row.get::<_, String>(1))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(cols, vec!["id".to_string(), "name".to_string(), "note".to_string()]);

    // A third, unchanged run stays a no-op even after an ALTER happened.
    let applied_again = plan_and_apply(&build_program(V2), &conn, &migrations_dir, "t2").unwrap();
    assert!(applied_again.is_empty());
    let files: Vec<_> = std::fs::read_dir(&migrations_dir).unwrap().collect();
    assert_eq!(files.len(), 2);
}

#[test]
fn struct_with_unsupported_field_type_is_skipped_not_errored() {
    let (db_path, migrations_dir) = temp_paths("skip");
    let conn = rusqlite::Connection::open(&db_path).unwrap();
    let src = r#"
        struct Sensor {
            id: i64,
            reading: Vector(f64, 3),
        }
        fn main() {}
    "#;

    let applied = plan_and_apply(&build_program(src), &conn, &migrations_dir, "t0").expect("should not error, just skip");
    assert!(applied.is_empty());
    assert!(!migrations_dir.exists(), "nothing should be written for an all-skipped run");

    let table_exists: bool =
        conn.query_row("SELECT 1 FROM sqlite_master WHERE type='table' AND name='sensor'", [], |_| Ok(())).is_ok();
    assert!(!table_exists);
}
