//! `Ty::Db` backend dispatch (`ast.rs`'s `Ty::Db` doc comment): layer 1
//! shipped SQLite-only and named Postgres as a specific future layer --
//! "a new `db_connect`-recognized connection *string* scheme, not a new
//! `Ty`". This module is that layer: `db_connect`/`db_query`/
//! `db_execute`'s Nirdosha-facing surface (results always `Ty::Json`)
//! stays identical; only what's underneath a given `Value::Db` handle
//! changes, chosen once at `connect` time from the connection string's
//! scheme and fixed for the handle's whole lifetime.
//!
//! `interpreter.rs` still owns `Value::Db`, `db_row_to_json` (SQLite-
//! shaped, also reused by `serve.rs`'s direct-to-SQLite table routes,
//! which stay SQLite-only -- `ROADMAP.md` scopes Postgres to the
//! language-level `db` handle, not yet to `nirdosha serve --db`/
//! `migrate.rs`'s auto-migration, a materially larger, separately-scoped
//! piece of work with its own SQL-dialect and schema-introspection
//! differences) and `sql_bind_params` (still the single place that
//! decides which `Value`s are legal bind values); this module only owns
//! the two real drivers and the query/execute dispatch between them.

use crate::interpreter::db_row_to_json;
use postgres::types::ToSql;
use serde_json::Value as JsonDoc;
use std::fmt;

/// A `db_query`/`db_execute` bind value, already validated by
/// `interpreter.rs::sql_bind_params` -- backend-neutral so that function
/// (and every builtin call site) never needs to know which driver is on
/// the other end of a given `Ty::Db` handle. Deliberately the same four
/// scalar cases `sql_bind_params` already recognized pre-Postgres.
#[derive(Debug, Clone)]
pub enum Param {
    Int(i64),
    Float(f64),
    Text(String),
    Bool(bool),
}

/// The live connection behind one `Value::Db` handle. Exactly one of
/// these, picked once by `connect` and never switched mid-lifetime --
/// `db_connect` doesn't reconnect.
pub enum DbConn {
    Sqlite(rusqlite::Connection),
    Postgres(postgres::Client),
}

/// Neither `postgres::Client` nor (transitively) `Value::Db` needs
/// `Debug` for any real reason other than `Value`'s own `#[derive(Debug)]`
/// -- same "handle, not data" treatment `interpreter.rs::MqConn` already
/// gives `redis::Connection`, which also has no `Debug` impl.
impl fmt::Debug for DbConn {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            DbConn::Sqlite(_) => write!(f, "DbConn::Sqlite(..)"),
            DbConn::Postgres(_) => write!(f, "DbConn::Postgres(..)"),
        }
    }
}

/// `db_connect`'s real implementation. `postgres://`/`postgresql://` --
/// the standard libpq URI scheme -- selects Postgres; every other
/// string, a bare file path or `:memory:` included, is unchanged layer-1
/// SQLite behavior, so no existing `.nir` program's behavior moves.
pub fn connect(conn_str: &str) -> Result<DbConn, String> {
    if conn_str.starts_with("postgres://") || conn_str.starts_with("postgresql://") {
        connect_postgres(conn_str)
    } else {
        rusqlite::Connection::open(conn_str).map(DbConn::Sqlite).map_err(|e| e.to_string())
    }
}

/// TLS is opt-in, read straight out of the connection string's own
/// `sslmode` parameter (`require`/`verify-ca`/`verify-full`) rather than
/// negotiated silently -- and reuses the `native-tls` dependency already
/// pulled in for `https_get`/`https_post` (`interpreter.rs`) instead of
/// adding a second TLS stack. No `sslmode`, or `disable`/`prefer`/
/// `allow`, connects in plaintext -- the common local/dev case.
fn connect_postgres(conn_str: &str) -> Result<DbConn, String> {
    let wants_tls = ["sslmode=require", "sslmode=verify-ca", "sslmode=verify-full"]
        .iter()
        .any(|flag| conn_str.contains(flag));
    if wants_tls {
        let tls = native_tls::TlsConnector::new().map_err(|e| format!("failed to initialize TLS: {e}"))?;
        let connector = postgres_native_tls::MakeTlsConnector::new(tls);
        postgres::Client::connect(conn_str, connector).map(DbConn::Postgres).map_err(|e| e.to_string())
    } else {
        postgres::Client::connect(conn_str, postgres::NoTls).map(DbConn::Postgres).map_err(|e| e.to_string())
    }
}

/// `db_query`'s real implementation, dispatched per backend. Always
/// returns a JSON array of row objects -- the one thing that never
/// varies by backend (`Ty::Db`'s doc comment).
pub fn query(conn: &mut DbConn, sql: &str, params: &[Param]) -> Result<JsonDoc, String> {
    match conn {
        DbConn::Sqlite(c) => sqlite_query(c, sql, params),
        DbConn::Postgres(c) => pg_query(c, sql, params),
    }
}

/// `db_execute`'s real implementation, dispatched per backend. Returns
/// the affected-row count either way.
pub fn execute(conn: &mut DbConn, sql: &str, params: &[Param]) -> Result<i64, String> {
    match conn {
        DbConn::Sqlite(c) => sqlite_execute(c, sql, params),
        DbConn::Postgres(c) => pg_execute(c, sql, params),
    }
}

fn sqlite_bind(params: &[Param]) -> Vec<rusqlite::types::Value> {
    params
        .iter()
        .map(|p| match p {
            Param::Int(n) => rusqlite::types::Value::Integer(*n),
            Param::Float(f) => rusqlite::types::Value::Real(*f),
            Param::Text(s) => rusqlite::types::Value::Text(s.clone()),
            // SQLite has no native boolean storage class -- 0/1 integer
            // is its own established convention (`sql_bind_params`'s doc
            // comment), unchanged from layer 1.
            Param::Bool(b) => rusqlite::types::Value::Integer(if *b { 1 } else { 0 }),
        })
        .collect()
}

fn sqlite_query(conn: &rusqlite::Connection, sql: &str, params: &[Param]) -> Result<JsonDoc, String> {
    let bound = sqlite_bind(params);
    let result: rusqlite::Result<JsonDoc> = (|| {
        let mut stmt = conn.prepare(sql)?;
        let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = stmt.query_map(rusqlite::params_from_iter(&bound), |row| db_row_to_json(row, &column_names))?;
        let rows: rusqlite::Result<Vec<JsonDoc>> = rows.collect();
        Ok(JsonDoc::Array(rows?))
    })();
    result.map_err(|e| e.to_string())
}

fn sqlite_execute(conn: &rusqlite::Connection, sql: &str, params: &[Param]) -> Result<i64, String> {
    let bound = sqlite_bind(params);
    conn.execute(sql, rusqlite::params_from_iter(&bound)).map(|n| n as i64).map_err(|e| e.to_string())
}

/// One boxed bind value per `Param`, using `postgres`'s own primitive
/// `ToSql` impls directly (`i64`/`f64`/`String`/`bool`) rather than a
/// hand-written wrapper impl -- there's nothing a wrapper would add here.
fn pg_bind(params: &[Param]) -> Vec<Box<dyn ToSql + Sync>> {
    params
        .iter()
        .map(|p| -> Box<dyn ToSql + Sync> {
            match p {
                Param::Int(n) => Box::new(*n),
                Param::Float(f) => Box::new(*f),
                Param::Text(s) => Box::new(s.clone()),
                // Unlike SQLite, Postgres has a real `boolean` type --
                // bound natively here, not as a 0/1 integer.
                Param::Bool(b) => Box::new(*b),
            }
        })
        .collect()
}

fn pg_refs(bound: &[Box<dyn ToSql + Sync>]) -> Vec<&(dyn ToSql + Sync)> {
    bound.iter().map(|b| b.as_ref()).collect()
}

fn pg_query(client: &mut postgres::Client, sql: &str, params: &[Param]) -> Result<JsonDoc, String> {
    let sql = rewrite_placeholders(sql);
    let bound = pg_bind(params);
    let refs = pg_refs(&bound);
    let rows = client.query(&sql, &refs).map_err(|e| e.to_string())?;
    let mut arr = Vec::with_capacity(rows.len());
    for row in &rows {
        arr.push(pg_row_to_json(row)?);
    }
    Ok(JsonDoc::Array(arr))
}

fn pg_execute(client: &mut postgres::Client, sql: &str, params: &[Param]) -> Result<i64, String> {
    let sql = rewrite_placeholders(sql);
    let bound = pg_bind(params);
    let refs = pg_refs(&bound);
    client.execute(&sql, &refs).map(|n| n as i64).map_err(|e| e.to_string())
}

/// SQLite's `?` positional-placeholder convention -- layer 1's only
/// style, and the one every existing `.nir` program's SQL was written
/// against -- doesn't exist on Postgres's wire protocol, which requires
/// numbered `$1, $2, ...` placeholders instead. Rewriting here, rather
/// than asking a program to write backend-specific SQL depending on
/// which connection string it happened to be given, is what keeps
/// `db_query`/`db_execute`'s call site identical regardless of backend
/// (`Ty::Db`'s doc comment). Skips `?` characters inside `'...'` string
/// literals (`''`-escaped, the standard SQL convention) so a literal `?`
/// in string data is never mistaken for a placeholder; a bare toggle on
/// every `'` is correct even for the escaped-quote case, since the two
/// halves of a `''` escape are adjacent with no `?` between them to
/// mis-toggle around.
fn rewrite_placeholders(sql: &str) -> String {
    let mut out = String::with_capacity(sql.len());
    let mut count = 0u32;
    let mut in_string = false;
    for c in sql.chars() {
        match c {
            '\'' => {
                in_string = !in_string;
                out.push(c);
            }
            '?' if !in_string => {
                count += 1;
                out.push('$');
                out.push_str(&count.to_string());
            }
            _ => out.push(c),
        }
    }
    out
}

/// One Postgres row -> the same row-shaped JSON object `db_row_to_json`
/// (SQLite) already produces, so `db_query`'s result shape never depends
/// on the backend. Postgres is strongly typed at the wire level (unlike
/// SQLite's dynamic typing), so this dispatches by the column's real
/// Postgres type name; a type with no mapping here is a named, honest
/// runtime error -- the same "clear error, not a silent misread" stance
/// `db_row_to_json`'s own `Blob`-as-hex handling already takes for
/// SQLite's one unrepresentable case.
fn pg_row_to_json(row: &postgres::Row) -> Result<JsonDoc, String> {
    let mut map = serde_json::Map::with_capacity(row.len());
    for (i, col) in row.columns().iter().enumerate() {
        let value = match col.type_().name() {
            "bool" => opt_json(row.try_get::<_, Option<bool>>(i), JsonDoc::Bool)?,
            "int2" => opt_json(row.try_get::<_, Option<i16>>(i), |n| JsonDoc::from(n as i64))?,
            "int4" => opt_json(row.try_get::<_, Option<i32>>(i), |n| JsonDoc::from(n as i64))?,
            "int8" => opt_json(row.try_get::<_, Option<i64>>(i), JsonDoc::from)?,
            "float4" => opt_json(row.try_get::<_, Option<f32>>(i), |f| json_float(f as f64))?,
            "float8" => opt_json(row.try_get::<_, Option<f64>>(i), json_float)?,
            "text" | "varchar" | "bpchar" | "name" | "citext" => {
                opt_json(row.try_get::<_, Option<String>>(i), JsonDoc::String)?
            }
            other => return Err(format!("unsupported postgres column type: {other}")),
        };
        map.insert(col.name().to_string(), value);
    }
    Ok(JsonDoc::Object(map))
}

fn opt_json<T>(got: Result<Option<T>, postgres::Error>, f: impl FnOnce(T) -> JsonDoc) -> Result<JsonDoc, String> {
    got.map(|opt| opt.map(f).unwrap_or(JsonDoc::Null)).map_err(|e| e.to_string())
}

fn json_float(f: f64) -> JsonDoc {
    serde_json::Number::from_f64(f).map(JsonDoc::Number).unwrap_or(JsonDoc::Null)
}
