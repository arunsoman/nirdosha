//! Tests for `db_connect`/`db_query`/`db_execute` against a real Postgres
//! server (`Ty::Db`'s doc comment; `crates/compiler/src/dbconn.rs`'s module doc)
//! -- layer 2, on top of layer 1's SQLite-only `tests/db.rs`. Every test
//! here is `#[ignore]`d by default: unlike SQLite's embedded, in-process
//! `:memory:` database, there's no way to stand up a real Postgres server
//! from inside a plain `cargo test` run without a system dependency this
//! project's test discipline otherwise avoids everywhere else
//! (`docs/PROTOLANG_PORT.md`'s "Locked design 5: DB" names this exact gap
//! before it existed). Run these explicitly, against a real server,
//! with:
//!
//! ```text
//! NIRDOSHA_TEST_POSTGRES_URL=postgres://user@host:5432/dbname \
//!     cargo test --test postgres -- --ignored
//! ```
//!
//! Defaults to `postgres://postgres@127.0.0.1:5432/postgres` (a common
//! local dev default) if the env var isn't set. Every test creates its
//! own uniquely-named table and drops it at the end, so tests can share
//! one database without colliding.

use nirdosha::interpreter::Value;
use nirdosha::run;

fn test_db_url() -> String {
    std::env::var("NIRDOSHA_TEST_POSTGRES_URL")
        .unwrap_or_else(|_| "postgres://postgres@127.0.0.1:5432/postgres".to_string())
}

/// Every test gets its own table name (`SplitMix64`-free — this is test
/// setup, not `.nir` source, so an ordinary `AtomicU64` counter is fine)
/// so parallel `cargo test` threads sharing one live server never
/// collide on `CREATE TABLE`.
fn unique_table(label: &str) -> String {
    static COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
    let n = COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    format!("nirdosha_pg_test_{label}_{n}")
}

#[test]
#[ignore]
fn connect_create_insert_and_query_round_trip() {
    let table = unique_table("basic");
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn run_all(conn: db) -> Text {{
            let created: i64 = match db_execute(conn, "CREATE TABLE {table} (id BIGINT PRIMARY KEY, name TEXT)") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            let inserted: i64 = match db_execute(conn, "INSERT INTO {table} (id, name) VALUES (?, ?)", 1, "ada") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            let found: Text = match db_query(conn, "SELECT name FROM {table} WHERE id = ?", 1) {{
                Ok(rows) => match json_array_get(rows, 0) {{
                    Ok(row) => match json_get_str(row, "name") {{
                        Ok(n) => Text(n),
                        Err(e) => Text(e),
                    }},
                    Err(e) => Text(e),
                }},
                Err(e) => Text(e),
            }}
            let dropped: i64 = match db_execute(conn, "DROP TABLE {table}") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            stop conn
            return found
        }}
        fn main() -> Text {{
            return match db_connect("{url}") {{
                Ok(conn) => run_all(conn),
                Err(e) => Text(e),
            }}
        }}
    "#,
        url = test_db_url(),
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "ada"),
            other => panic!("expected Text(Str(\"ada\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"ada\")), got {other:?}"),
    }
}

#[test]
#[ignore]
fn scalar_types_round_trip_through_pg_row_to_json() {
    let table = unique_table("scalars");
    let src = format!(
        r#"
        struct Row {{
            score: f64,
            active: bool,
            note: str,
        }}
        fn run_all(conn: db) -> Row {{
            let created: i64 = match db_execute(conn, "CREATE TABLE {table} (id BIGINT PRIMARY KEY, score DOUBLE PRECISION, active BOOLEAN, note TEXT)") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            let inserted: i64 = match db_execute(conn, "INSERT INTO {table} (id, score, active, note) VALUES (?, ?, ?, ?)", 1, 3.5, true, "hello") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            let out: Row = match db_query(conn, "SELECT score, active, note FROM {table} WHERE id = ?", 1) {{
                Ok(rows) => match json_array_get(rows, 0) {{
                    Ok(r) => match json_get_f64(r, "score") {{
                        Ok(f) => match json_get_bool(r, "active") {{
                            Ok(b) => match json_get_str(r, "note") {{
                                Ok(s) => Row(f, b, s),
                                Err(e) => Row(-1.0, false, e),
                            }},
                            Err(e) => Row(-1.0, false, e),
                        }},
                        Err(e) => Row(-1.0, false, e),
                    }},
                    Err(e) => Row(-1.0, false, e),
                }},
                Err(e) => Row(-1.0, false, e),
            }}
            let dropped: i64 = match db_execute(conn, "DROP TABLE {table}") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            stop conn
            return out
        }}
        fn main() -> Row {{
            return match db_connect("{url}") {{
                Ok(conn) => run_all(conn),
                Err(e) => Row(-1.0, false, e),
            }}
        }}
    "#,
        url = test_db_url(),
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Row" => {
            assert_eq!(fields[0], Value::Float(3.5));
            assert_eq!(fields[1], Value::Bool(true));
            match &fields[2] {
                Value::Str(s) => assert_eq!(&**s, "hello"),
                other => panic!("expected Str(\"hello\"), got {other:?}"),
            }
        }
        other => panic!("expected Ok(Row(3.5, true, \"hello\")), got {other:?}"),
    }
}

/// A literal `?` inside a `'...'` SQL string constant must survive
/// `dbconn::rewrite_placeholders` unchanged -- it isn't a bind
/// placeholder, and rewriting it to `$1` would both corrupt the stored
/// text and throw off every subsequent placeholder's numbering.
#[test]
#[ignore]
fn a_literal_question_mark_inside_a_string_literal_is_not_rewritten() {
    let table = unique_table("qmark");
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn run_all(conn: db) -> Text {{
            let created: i64 = match db_execute(conn, "CREATE TABLE {table} (id BIGINT PRIMARY KEY, note TEXT)") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            let inserted: i64 = match db_execute(conn, "INSERT INTO {table} (id, note) VALUES (?, 'are you sure?')", 1) {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            let found: Text = match db_query(conn, "SELECT note FROM {table} WHERE id = ?", 1) {{
                Ok(rows) => match json_array_get(rows, 0) {{
                    Ok(row) => match json_get_str(row, "note") {{
                        Ok(n) => Text(n),
                        Err(e) => Text(e),
                    }},
                    Err(e) => Text(e),
                }},
                Err(e) => Text(e),
            }}
            let dropped: i64 = match db_execute(conn, "DROP TABLE {table}") {{
                Ok(n) => n,
                Err(e) => -1,
            }}
            stop conn
            return found
        }}
        fn main() -> Text {{
            return match db_connect("{url}") {{
                Ok(conn) => run_all(conn),
                Err(e) => Text(e),
            }}
        }}
    "#,
        url = test_db_url(),
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "are you sure?"),
            other => panic!("expected Text(Str(\"are you sure?\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"are you sure?\")), got {other:?}"),
    }
}

#[test]
#[ignore]
fn a_sql_syntax_error_is_a_recoverable_err_not_a_trap() {
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn run_all(conn: db) -> Text {{
            let out: Text = match db_execute(conn, "NOT REAL SQL") {{
                Ok(n) => Text("no error"),
                Err(e) => Text(e),
            }}
            stop conn
            return out
        }}
        fn main() -> Text {{
            return match db_connect("{url}") {{
                Ok(conn) => run_all(conn),
                Err(e) => Text(e),
            }}
        }}
    "#,
        url = test_db_url(),
    );
    match run(&src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_ne!(&**s, "no error"),
            other => panic!("expected a non-empty error message, got {other:?}"),
        },
        other => panic!("expected Ok(Text(<error message>)), got {other:?}"),
    }
}
