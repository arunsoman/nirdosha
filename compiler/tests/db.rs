//! Tests for `db_connect`/`db_query`/`db_execute` (`Ty::Db`'s doc
//! comment) — layer 1: SQLite only, via `rusqlite` ("bundled": statically
//! linked, no system `libsqlite3`). Every test here uses `:memory:` — a
//! real SQLite in-memory database, not a mock — so this is fully
//! self-contained, the same discipline every other affine-handle feature
//! here already follows, with no server process or Docker needed.

use nirdosha::ast::Ty;
use nirdosha::interpreter::Value;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::run;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

fn first_type_error(src: &str) -> TypeErrorKind {
    let program = parse_ok(src);
    match typecheck(&program) {
        Ok(()) => panic!("expected a type error, but the program type-checked cleanly"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

// ---- basic connect/execute/query, against a real SQLite database ----------

#[test]
fn connect_create_insert_and_query_round_trip() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn main() -> Text {
            return match db_connect(":memory:") {
                Ok(conn) => match db_execute(conn, "CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT)") {
                    Ok(n) => match db_execute(conn, "INSERT INTO users (name) VALUES ('ada')") {
                        Ok(m) => match db_query(conn, "SELECT name FROM users") {
                            Ok(rows) => match json_array_get(rows, 0) {
                                Ok(row) => match json_get_str(row, "name") {
                                    Ok(name) => Text(name),
                                    Err(e) => Text(e),
                                },
                                Err(e) => Text(e),
                            },
                            Err(e) => Text(e),
                        },
                        Err(e) => Text(e),
                    },
                    Err(e) => Text(e),
                },
                Err(e) => Text(e),
            }
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "ada"),
            other => panic!("expected Text(Str(\"ada\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"ada\")), got {other:?}"),
    }
}

#[test]
fn a_connection_can_run_many_queries_not_just_one() {
    // The exact ergonomic bug this design has to get right: `db_query`/
    // `db_execute` are ordinary builtin calls (not a dedicated `Expr`
    // node the way `tcp`/`file`'s `send`/`recv` are), so without a real
    // fix in `ownership.rs`, an affine `conn` would be usable exactly
    // once *even within the same function*. (Passing the same `conn` on
    // to more than one *other* function is a separate, real, and much
    // more general limitation shared by every affine handle in this
    // language today, not specific to `db` — there's no `&db`-aware
    // borrowing story yet, so ownership of the whole connection would
    // have to be threaded back and forth explicitly; not attempted here,
    // and not what this test is about.)
    let src = r#"
        fn run_all(conn: db) -> i64 {
            let a: i64 = match db_execute(conn, "CREATE TABLE t (n INTEGER)") { Ok(x) => x, Err(e) => -1 }
            let b: i64 = match db_execute(conn, "INSERT INTO t (n) VALUES (1)") { Ok(x) => x, Err(e) => -1 }
            let c: i64 = match db_execute(conn, "INSERT INTO t (n) VALUES (2)") { Ok(x) => x, Err(e) => -1 }
            let d: i64 = match db_execute(conn, "INSERT INTO t (n) VALUES (3)") { Ok(x) => x, Err(e) => -1 }
            return match db_query(conn, "SELECT COUNT(*) as c FROM t") {
                Ok(rows) => match json_array_get(rows, 0) {
                    Ok(row) => match json_get_i64(row, "c") {
                        Ok(n) => n,
                        Err(e) => -1,
                    },
                    Err(e) => -1,
                },
                Err(e) => -1,
            }
        }
        fn main() -> i64 {
            return match db_connect(":memory:") {
                Ok(conn) => run_all(conn),
                Err(e) => -1,
            }
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("a connection must be usable for more than one query");
    match run(src) {
        Ok(Value::Int(3)) => {}
        other => panic!("expected Ok(Int(3)), got {other:?}"),
    }
}

#[test]
fn db_execute_returns_the_affected_row_count() {
    let src = r#"
        fn main() -> i64 {
            return match db_connect(":memory:") {
                Ok(conn) => match db_execute(conn, "CREATE TABLE t (n INTEGER)") {
                    Ok(created) => match db_execute(conn, "INSERT INTO t (n) VALUES (1), (2), (3)") {
                        Ok(inserted) => inserted,
                        Err(e) => -1,
                    },
                    Err(e) => -1,
                },
                Err(e) => -1,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Int(3)) => {}
        other => panic!("expected Ok(Int(3)), got {other:?}"),
    }
}

#[test]
fn multiple_rows_and_columns_come_back_as_a_json_array_of_objects() {
    let src = r#"
        fn summarize(rows: json) -> i64 {
            let len: i64 = match json_array_len(rows) { Ok(n) => n, Err(e) => -1 }
            let first_age: i64 = match json_array_get(rows, 0) {
                Ok(row) => match json_get_i64(row, "age") { Ok(n) => n, Err(e) => -1 },
                Err(e) => -1,
            }
            return len * 1000 + first_age
        }
        fn run_all(conn: db) -> i64 {
            let a: i64 = match db_execute(conn, "CREATE TABLE t (name TEXT, age INTEGER)") { Ok(n) => n, Err(e) => -1 }
            let b: i64 = match db_execute(conn, "INSERT INTO t VALUES ('ada', 36)") { Ok(n) => n, Err(e) => -1 }
            let c: i64 = match db_execute(conn, "INSERT INTO t VALUES ('grace', 45)") { Ok(n) => n, Err(e) => -1 }
            return match db_query(conn, "SELECT name, age FROM t ORDER BY age") {
                Ok(rows) => summarize(rows),
                Err(e) => -1,
            }
        }
        fn main() -> i64 {
            return match db_connect(":memory:") {
                Ok(conn) => run_all(conn),
                Err(e) => -1,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Int(2036)) => {} // 2 rows, first (by age) is 36
        other => panic!("expected Ok(Int(2036)), got {other:?}"),
    }
}

#[test]
fn null_values_are_not_confused_with_a_real_integer() {
    let src = r#"
        fn run_all(conn: db) -> bool {
            let a: i64 = match db_execute(conn, "CREATE TABLE t (n INTEGER)") { Ok(x) => x, Err(e) => -1 }
            let b: i64 = match db_execute(conn, "INSERT INTO t (n) VALUES (NULL)") { Ok(x) => x, Err(e) => -1 }
            return match db_query(conn, "SELECT n FROM t") {
                Ok(rows) => match json_array_get(rows, 0) {
                    Ok(row) => match json_get_i64(row, "n") {
                        Ok(x) => false,
                        Err(e) => true, // NULL isn't an integer -- a recoverable Err
                    },
                    Err(e) => false,
                },
                Err(e) => false,
            }
        }
        fn main() -> bool {
            return match db_connect(":memory:") {
                Ok(conn) => run_all(conn),
                Err(e) => false,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

// ---- recoverable failures ------------------------------------------------

#[test]
fn a_syntax_error_in_the_sql_is_a_recoverable_err_not_a_trap() {
    let src = r#"
        fn main() -> bool {
            return match db_connect(":memory:") {
                Ok(conn) => match db_execute(conn, "NOT VALID SQL AT ALL") {
                    Ok(n) => false,
                    Err(e) => true,
                },
                Err(e) => false,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn querying_a_nonexistent_table_is_a_recoverable_err() {
    let src = r#"
        fn main() -> bool {
            return match db_connect(":memory:") {
                Ok(conn) => match db_query(conn, "SELECT * FROM nope") {
                    Ok(rows) => false,
                    Err(e) => true,
                },
                Err(e) => false,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn a_unique_constraint_violation_is_a_recoverable_err() {
    let src = r#"
        fn run_all(conn: db) -> bool {
            let a: i64 = match db_execute(conn, "CREATE TABLE t (n INTEGER UNIQUE)") { Ok(x) => x, Err(e) => -1 }
            let b: i64 = match db_execute(conn, "INSERT INTO t (n) VALUES (1)") { Ok(x) => x, Err(e) => -1 }
            return match db_execute(conn, "INSERT INTO t (n) VALUES (1)") {
                Ok(x) => false,
                Err(e) => true,
            }
        }
        fn main() -> bool {
            return match db_connect(":memory:") {
                Ok(conn) => run_all(conn),
                Err(e) => false,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

#[test]
fn connecting_to_an_unwritable_path_is_a_recoverable_err() {
    let src = r#"
        fn main() -> bool {
            return match db_connect("/nonexistent-directory/definitely/not/here.db") {
                Ok(conn) => false,
                Err(e) => true,
            }
        }
    "#;
    match run(src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

// ---- ownership --------------------------------------------------------

#[test]
fn stopping_a_connection_twice_is_a_static_use_after_move() {
    let program = parse_ok(
        r#"
        fn double_stop(conn: db) -> i64 {
            stop conn
            stop conn
            return 0
        }
        fn main() -> i64 {
            return 0
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    assert!(
        check_ownership(&program).is_err(),
        "using a db connection after `stop`ping it must be a static ownership error"
    );
}

#[test]
fn using_a_connection_after_stop_is_a_static_use_after_move() {
    let program = parse_ok(
        r#"
        fn bad(conn: db) -> i64 {
            stop conn
            return match db_execute(conn, "SELECT 1") {
                Ok(n) => n,
                Err(e) => -1,
            }
        }
        fn main() -> i64 {
            return 0
        }
    "#,
    );
    typecheck(&program).expect("should typecheck cleanly");
    assert!(
        check_ownership(&program).is_err(),
        "querying a db connection after `stop`ping it must be a static ownership error"
    );
}

// ---- static rejections ---------------------------------------------------

#[test]
fn db_connect_requires_a_str_path() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(db, str) = db_connect(1)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn db_query_requires_a_db_first_argument() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(json, str) = db_query(1, "SELECT 1")
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Db, found: Ty::I64 });
}

#[test]
fn db_execute_requires_a_str_sql_argument() {
    let kind = first_type_error(
        r#"
        fn run(conn: db) -> i64 {
            return match db_execute(conn, 42) {
                Ok(n) => n,
                Err(e) => -1,
            }
        }
        fn main() -> i64 { return 0 }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn db_is_interpreter_only_rejected_by_codegen() {
    let src = r#"
        fn main() -> i64 {
            let r: Result(db, str) = db_connect(":memory:")
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(nirdosha::codegen::check_supported(&program).is_err());
}

// ---- worked example -------------------------------------------------------

#[test]
fn example_db_runs_to_completion() {
    let program = parse_ok(include_str!("../examples/db.nir"));
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should pass ownership checking");
    assert_eq!(run(include_str!("../examples/db.nir")), Ok(Value::Unit));
}

// ---- parameterized queries: the only way to embed runtime data into SQL,
// since Nirdosha `str` has no concatenation (LANGUAGE.md §2) -----------

#[test]
fn parameterized_execute_and_query_round_trip_runtime_values() {
    let src = r#"
        struct Text {
            value: str,
        }
        fn run_all(conn: db) -> Text {
            let created: i64 = match db_execute(conn, "CREATE TABLE items (name TEXT, price INTEGER)") {
                Ok(n) => n,
                Err(e) => -1,
            }
            let name: str = "widget"
            let price: i64 = 42
            let inserted: i64 = match db_execute(conn, "INSERT INTO items (name, price) VALUES (?, ?)", name, price) {
                Ok(n) => n,
                Err(e) => -1,
            }
            let found: Text = match db_query(conn, "SELECT name FROM items WHERE price = ?", price) {
                Ok(rows) => match json_array_get(rows, 0) {
                    Ok(row) => match json_get_str(row, "name") {
                        Ok(n) => Text(n),
                        Err(e) => Text(e),
                    },
                    Err(e) => Text(e),
                },
                Err(e) => Text(e),
            }
            stop conn
            return found
        }
        fn main() -> Text {
            return match db_connect(":memory:") {
                Ok(conn) => run_all(conn),
                Err(e) => Text(e),
            }
        }
    "#;
    match run(src) {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "widget"),
            other => panic!("expected Text(Str(\"widget\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"widget\")), got {other:?}"),
    }
}
