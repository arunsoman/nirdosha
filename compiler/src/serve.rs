//! `nirdosha serve` — runs a `.nir` program as a real HTTP service, on
//! top of `tiny_http` (an existing, production-used, synchronous Rust
//! HTTP server library — chosen over an async stack like axum/tokio
//! specifically because it needs no extra runtime and matches the
//! interpreter's own non-async, tree-walking model, the same reasoning
//! `rusqlite`/`redis`'s sync APIs were already chosen for).
//!
//! ## Routes
//! - `GET /` — the program's `emit-ui`-derived UI (`ui_gen::generate`),
//!   so one process serves both UI and API.
//! - `POST /api/<fn_name>` — calls `fn_name` via `Interpreter::call_named`
//!   (already public — no interpreter-dispatch changes needed), decoding
//!   the JSON request body into positional arguments and encoding the
//!   result back, driven entirely by `fn_name`'s declared `Ty`s
//!   (`decode_value`/`encode_value` below), the same type-driven approach
//!   `ui_gen.rs::build_field` already takes on the client side. A
//!   `Ty::Named("Result", [ok, err])` return value encodes to
//!   `{"ok":...}`/`{"err":...}` — exactly what `ui_gen_template.html`'s
//!   `callFn` already expects, so the generated frontend needs no
//!   changes to talk to this.
//!
//! ## Auth and the authz gate this module exists partly *because of*
//!
//! `Authorization: Bearer <token>` is validated via `validate_oidc_token`
//! (real verification, not the mock — `mock_issue_token`-signed tokens
//! validate through this identically to a real IdP's, which is the
//! whole point) against the JWKS/issuer/audience passed to `serve`. The
//! resulting `VerifiedIdentity` is injected wherever a handler declares
//! one as a parameter — never read from the request body, mirroring
//! `ui_gen`'s client-side exclusion of that param from user-entered
//! fields.
//!
//! **`Interpreter::call_named` does not enforce `FnDecl.requires` at
//! runtime** — only `Expr::Acquire` does, and `call_named` never goes
//! through it (confirmed by reading `interpreter.rs`: `call()` checks
//! only that the function exists, arity, and argument types). A
//! `requires`-gated function's own parameter list doesn't even need to
//! include a `VerifiedIdentity` — the gate is structural, checked at
//! `acquire` time in ordinary Nirdosha code, not tied to a call's
//! arguments. So this module does that check itself, independently, for
//! every request against a `requires`-gated function, using the exact
//! same `identity_has_role`/`identity_claim` helpers the `check_role`/
//! `extract_claim` builtins themselves use (`interpreter.rs`) — one
//! source of truth, not a third copy of "does this identity have that
//! role."

use std::sync::Arc;

use serde_json::Value as JsonVal;

use crate::ast::{Program, Requirement, Ty};
use crate::interpreter::{self, Interpreter, Value};

pub struct AuthConfig {
    pub jwks_json: String,
    pub issuer: String,
    pub audience: String,
}

fn cors_headers() -> Vec<tiny_http::Header> {
    vec![
        tiny_http::Header::from_bytes(&b"Access-Control-Allow-Origin"[..], &b"*"[..]).unwrap(),
        tiny_http::Header::from_bytes(&b"Access-Control-Allow-Headers"[..], &b"Content-Type, Authorization"[..]).unwrap(),
        tiny_http::Header::from_bytes(&b"Access-Control-Allow-Methods"[..], &b"GET, POST, OPTIONS"[..]).unwrap(),
    ]
}

fn header(name: &str, value: &str) -> tiny_http::Header {
    tiny_http::Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("header name/value are plain ASCII here")
}

/// Maximum request body size accepted by either `POST` route. `tiny_http`
/// already buffers the whole body; this cap prevents a single huge request
/// from exhausting the server's memory.
const MAX_BODY_BYTES: u64 = 1024 * 1024; // 1 MiB

fn read_limited_body(request: &mut tiny_http::Request) -> Result<String, (u16, String)> {
    let mut buf = Vec::new();
    let reader = request.as_reader();
    let mut remaining = MAX_BODY_BYTES;
    let mut chunk = [0u8; 4096];
    while remaining > 0 {
        let n = chunk.len().min(remaining as usize);
        match reader.read(&mut chunk[..n]) {
            Ok(0) => break,
            Ok(k) => {
                buf.extend_from_slice(&chunk[..k]);
                remaining -= k as u64;
            }
            Err(e) => return Err((413, json_err(&format!("request body too large or unreadable: {e}")))),
        }
    }
    if remaining == 0 && !matches!(reader.read(&mut [0u8; 1]), Ok(0)) {
        return Err((413, json_err("request body exceeded maximum size (1 MiB)")));
    }
    match String::from_utf8(buf) {
        Ok(body) => Ok(body),
        Err(e) => Err((400, json_err(&format!("request body is not valid UTF-8: {e}")))),
    }
}

/// Starts serving `program` on `port` and blocks forever (one request at
/// a time — see the module doc; this is a demo/dev entry point, not a
/// tuned production server). `identity_base`, if given, is baked into
/// the served UI so its login screen talks to a real (mock) identity
/// app instead of the pure client-side stub (`ui_gen::generate`'s doc
/// comment).
pub fn run(
    program: Arc<Program>,
    host: &str,
    port: u16,
    auth: Option<AuthConfig>,
    identity_base: Option<&str>,
    transact_log_path: std::path::PathBuf,
    db_path: Option<String>,
    theme: Option<&crate::ui_gen::Theme>,
) -> Result<(), String> {
    let registry = crate::ast::TypeRegistry::build(&program);
    let effects = crate::effects::infer_effects(&program, &registry);
    let ui_html = crate::ui_gen::generate(&program, &effects, identity_base, db_path.is_some(), theme);
    let auth = Arc::new(auth);
    let transact_log_path = Arc::new(transact_log_path);
    // `--db <path>`: opens one shared connection, direct rusqlite (not
    // through the interpreter), backing the generic
    // `/_nirdosha/table/<snake>` pagination/sort/filter/search route
    // below -- see that route's own doc comment for why this is real
    // Rust, not `.nir` source, and so isn't bound by `db_query`'s 2..=10
    // arity cap (which only constrains `.nir`-authored calls). `None`
    // (the default) leaves every table exactly as it always rendered:
    // one unpaginated fetch, no new route reachable at all.
    let table_db: Option<Arc<std::sync::Mutex<rusqlite::Connection>>> = match &db_path {
        Some(p) => match rusqlite::Connection::open(p) {
            Ok(conn) => Some(Arc::new(std::sync::Mutex::new(conn))),
            Err(e) => return Err(format!("--db {p}: failed to open: {e}")),
        },
        None => None,
    };
    let table_catalog = Arc::new(build_table_catalog(&program));

    // Schema migration (`migrate.rs`) once, before crash replay -- replay
    // may write to tables a migration is about to create. Only runs when
    // `--db` is given, matching every other `table_db`-gated behavior
    // above: omit `--db` and this app is byte-for-byte what it always was.
    if let (Some(conn), Some(p)) = (&table_db, &db_path) {
        let migrations_dir = std::path::Path::new(p).parent().unwrap_or_else(|| std::path::Path::new(".")).join("migrations");
        let applied_at = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0).to_string();
        let conn = conn.lock().unwrap();
        match crate::migrate::plan_and_apply(&program, &conn, &migrations_dir, &applied_at) {
            Ok(applied) => {
                for f in &applied {
                    eprintln!("migrate: applied {f}");
                }
            }
            Err(e) => return Err(format!("schema migration failed: {e}")),
        }
    }

    // Crash replay (`TRANSACT.md`'s Layer 4) once, before the server ever
    // accepts a request -- not per-request (unlike `dispatch`'s own fresh
    // `Interpreter`, one per request for isolation): replaying the same
    // durable log twice per request would be wasted work, and the whole
    // point is to resolve whatever crashed *before* new traffic arrives.
    {
        let replay_interp =
            Interpreter::new(Arc::clone(&program), Arc::from("")).with_transact_log_path((*transact_log_path).clone());
        match replay_interp.replay_pending_transactions() {
            Ok(outcomes) => {
                for o in &outcomes {
                    eprintln!("transact replay: {o:?}");
                }
            }
            Err(e) => return Err(format!("transact durability log error during startup crash replay: {e}")),
        }
    }

    let server = tiny_http::Server::http((host, port)).map_err(|e| format!("failed to bind {host}:{port}: {e}"))?;
    eprintln!("nirdosha serve: listening on http://{host}:{port}  (GET / for the UI, POST /api/<fn> for the API)");

    for mut request in server.incoming_requests() {
        let method = request.method().clone();
        let url = request.url().to_string();

        if method == tiny_http::Method::Options {
            let mut resp = tiny_http::Response::from_string("").with_status_code(204);
            for h in cors_headers() {
                resp.add_header(h);
            }
            let _ = request.respond(resp);
            continue;
        }

        if method == tiny_http::Method::Get && url == "/" {
            let mut resp = tiny_http::Response::from_string(ui_html.clone())
                .with_header(header("Content-Type", "text/html; charset=utf-8"));
            for h in cors_headers() {
                resp.add_header(h);
            }
            let _ = request.respond(resp);
            continue;
        }

        if method == tiny_http::Method::Post {
            if let Some(table) = url.strip_prefix("/_nirdosha/table/") {
                let body = match read_limited_body(&mut request) {
                    Ok(b) => b,
                    Err(status_body) => {
                        let mut resp = tiny_http::Response::from_string(status_body.1).with_status_code(status_body.0);
                        for h in cors_headers() {
                            resp.add_header(h);
                        }
                        let _ = request.respond(resp);
                        continue;
                    }
                };
                // Same bearer-token resolution `dispatch` uses -- this
                // route bypasses `dispatch` entirely (talks straight to
                // SQLite, not through the interpreter), so without this
                // it had no identity awareness at all, and any
                // `view`-gated field would leak straight through it.
                let auth_header = request
                    .headers()
                    .iter()
                    .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("Authorization"))
                    .map(|h| h.value.as_str().to_string());
                let (status, payload) = match &table_db {
                    Some(db) => match resolve_identity(auth_header.as_deref(), auth.as_ref().as_ref()) {
                        Ok(identity) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            dispatch_table_query(db, &table_catalog, table, &body, &program, identity.as_ref())
                        }))
                        .unwrap_or_else(|_| (500, json_err("internal server error"))),
                        Err(status_body) => status_body,
                    },
                    None => (404, json_err("no --db was passed to `nirdosha serve`; the generic table-query route is disabled")),
                };
                let mut resp = tiny_http::Response::from_string(payload)
                    .with_status_code(status)
                    .with_header(header("Content-Type", "application/json"));
                for h in cors_headers() {
                    resp.add_header(h);
                }
                let _ = request.respond(resp);
                continue;
            }
            if let Some(fn_name) = url.strip_prefix("/api/") {
                let body = match read_limited_body(&mut request) {
                    Ok(b) => b,
                    Err(status_body) => {
                        let mut resp = tiny_http::Response::from_string(status_body.1).with_status_code(status_body.0);
                        for h in cors_headers() {
                            resp.add_header(h);
                        }
                        let _ = request.respond(resp);
                        continue;
                    }
                };
                let auth_header = request
                    .headers()
                    .iter()
                    .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("Authorization"))
                    .map(|h| h.value.as_str().to_string());

                // Defense in depth against any panic in `dispatch`'s own
                // interpretation path we haven't found yet (this loop is
                // strictly sequential -- one process serving every
                // caller, per this module's doc comment -- so an
                // uncaught panic here would otherwise unwind straight
                // out of `main` and take the whole server down for every
                // other concurrent user over one bad request). This does
                // *not* cover a real stack-overflow abort -- Rust's
                // stack-overflow handler calls `process::abort()`
                // unconditionally, which `catch_unwind` cannot intercept
                // on any thread -- that class is instead prevented from
                // happening at all by `interpreter::MAX_CALL_DEPTH`
                // (`call_named_on_big_stack`, which `dispatch` already
                // goes through).
                let (status, payload) = match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    dispatch(
                        &program,
                        fn_name,
                        &body,
                        auth_header.as_deref(),
                        auth.as_ref().as_ref(),
                        &transact_log_path,
                        table_db.as_deref(),
                    )
                })) {
                    Ok(result) => result,
                    Err(_) => (500, json_err(&format!("`{fn_name}` panicked while handling this request"))),
                };
                let mut resp = tiny_http::Response::from_string(payload)
                    .with_status_code(status)
                    .with_header(header("Content-Type", "application/json"));
                for h in cors_headers() {
                    resp.add_header(h);
                }
                let _ = request.respond(resp);
                continue;
            }
        }

        let mut resp = tiny_http::Response::from_string(r#"{"err":"not found"}"#).with_status_code(404);
        for h in cors_headers() {
            resp.add_header(h);
        }
        let _ = request.respond(resp);
    }
    Ok(())
}

/// Same word-boundary walk as `ui_gen.rs::to_snake_case` — duplicated,
/// not shared, matching this pair of modules' existing precedent
/// (`resolve_struct` is already duplicated the same way, for the same
/// reason: each file's own small local copy of a two-line helper is
/// lower-risk than a cross-module export for something this size).
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

/// The `/_nirdosha/table/<snake>` route's allowlist, computed once at
/// startup (mirrors `ui_gen::generate`'s own "generate once at boot"
/// model — a `.nir` reload without a server restart isn't a thing this
/// server does at all today, so there's no drift concern here beyond
/// what already exists). Maps a struct's `to_snake_case` name to its own
/// field names — assumed to equal the real SQLite table/column names,
/// which is this whole codebase's own established, universal
/// convention, not something the type system enforces. Every
/// `sort_field`/`filters` key the route below receives is checked
/// against this *before* ever being interpolated into SQL text —
/// SQLite can't bind an identifier as a `?` parameter, only a value, so
/// allowlisting first is the only correct way to build a dynamic
/// `ORDER BY`/`WHERE`, not a shortcut.
fn build_table_catalog(program: &Program) -> std::collections::HashMap<String, Vec<String>> {
    const PRELUDE_STRUCT_NAMES: &[&str] =
        &["HttpResponse", "VerifiedIdentity", "RoleView", "ClaimView", "ApplicationSession", "RefreshTokenHandle", "Pair"];
    program
        .structs
        .iter()
        .filter(|s| !PRELUDE_STRUCT_NAMES.contains(&s.name.as_str()))
        .map(|s| (to_snake_case(&s.name), s.fields.iter().map(|f| f.name.clone()).collect()))
        .collect()
}

fn json_to_sql_value(v: &JsonVal) -> rusqlite::types::Value {
    match v {
        JsonVal::String(s) => rusqlite::types::Value::Text(s.clone()),
        JsonVal::Number(n) => match n.as_i64() {
            Some(i) => rusqlite::types::Value::Integer(i),
            None => rusqlite::types::Value::Real(n.as_f64().unwrap_or(0.0)),
        },
        JsonVal::Bool(b) => rusqlite::types::Value::Integer(if *b { 1 } else { 0 }),
        _ => rusqlite::types::Value::Null,
    }
}

/// `json_to_sql_value`'s inverse -- used only to splice a currently
/// stored column value back into a request body as a stand-in for a
/// view-gated field the caller couldn't supply themselves (see
/// `dispatch`'s view-gated pass-through-on-omit step).
fn sql_value_to_json(v: &rusqlite::types::Value) -> JsonVal {
    match v {
        rusqlite::types::Value::Null => JsonVal::Null,
        rusqlite::types::Value::Integer(i) => JsonVal::Number((*i).into()),
        rusqlite::types::Value::Real(f) => serde_json::Number::from_f64(*f).map(JsonVal::Number).unwrap_or(JsonVal::Null),
        rusqlite::types::Value::Text(s) => JsonVal::String(s.clone()),
        rusqlite::types::Value::Blob(_) => JsonVal::Null,
    }
}

/// The generic, interpreter-bypassing pagination/sort/filter/search read
/// path behind `/_nirdosha/table/<snake>` — real hand-written Rust
/// talking to rusqlite directly (not through `db_query`/`db_execute`,
/// and not bound by their 2..=10-argument arity cap, which only ever
/// constrained `.nir`-authored calls, never new Rust). Body shape:
/// `{page, page_size, sort_field, sort_dir, search, filters}`, every
/// field optional. `page` is 1-based; `page_size` clamps to [1, 200].
/// `search` LIKE-matches across every known column (`CAST(.. AS TEXT)`,
/// safe regardless of a column's actual SQLite storage type); `filters`
/// is per-column exact match. Real limitation, disclosed not hidden:
/// this always runs a plain `SELECT * FROM <table>` — a hand-written
/// `list_<struct>` doing a join or custom computed column is invisible
/// to this route entirely; the client falls back to that function's own
/// unpaginated call when `serverTableApi` is false, or for any screen it
/// judges unsuitable (see `ui_gen_template.html`).
fn dispatch_table_query(
    db: &std::sync::Mutex<rusqlite::Connection>,
    catalog: &std::collections::HashMap<String, Vec<String>>,
    table: &str,
    body: &str,
    program: &Program,
    identity: Option<&Value>,
) -> (u16, String) {
    let Some(columns) = catalog.get(table) else {
        return (404, json_err(&format!("no such table `{table}`")));
    };
    // Field-level `view` RBAC -- this route talks straight to SQLite, so
    // there's no `fn_name` to resolve gates from the way `dispatch` does;
    // resolve by struct name instead (the struct whose `to_snake_case`
    // name equals this table), via the same declared `screen` block.
    let view_gates: Vec<crate::ui_gen::GatedField> = program
        .structs
        .iter()
        .find(|s| to_snake_case(&s.name) == table)
        .map(|s| crate::ui_gen::field_gates_for_struct(program, &s.name))
        .unwrap_or_default();
    let req: JsonVal = if body.trim().is_empty() {
        serde_json::json!({})
    } else {
        match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return (400, json_err(&format!("invalid JSON body: {e}"))),
        }
    };

    let page = req.get("page").and_then(JsonVal::as_i64).unwrap_or(1).max(1);
    let page_size = req.get("page_size").and_then(JsonVal::as_i64).unwrap_or(25).clamp(1, 200);
    let offset = (page - 1) * page_size;

    let sort_field = match req.get("sort_field").and_then(JsonVal::as_str) {
        Some(f) if columns.iter().any(|c| c == f) => f.to_string(),
        Some(f) => return (400, json_err(&format!("`{f}` is not a valid sort field for `{table}`"))),
        None => columns.first().cloned().unwrap_or_default(),
    };
    let sort_dir = match req.get("sort_dir").and_then(JsonVal::as_str).map(str::to_ascii_lowercase).as_deref() {
        Some("desc") => "DESC",
        _ => "ASC",
    };

    let search = req.get("search").and_then(JsonVal::as_str).filter(|s| !s.is_empty());
    let filters: Vec<(String, JsonVal)> = match req.get("filters").and_then(JsonVal::as_object) {
        Some(obj) => obj.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
        None => vec![],
    };
    for (k, _) in &filters {
        if !columns.iter().any(|c| c == k) {
            return (400, json_err(&format!("`{k}` is not a valid filter field for `{table}`")));
        }
    }

    let select_cols = columns.iter().map(|c| format!("\"{c}\"")).collect::<Vec<_>>().join(", ");
    let mut where_clauses: Vec<String> = vec![];
    let mut bind_values: Vec<rusqlite::types::Value> = vec![];
    if let Some(q) = search {
        let ors = columns.iter().map(|c| format!("CAST(\"{c}\" AS TEXT) LIKE ?")).collect::<Vec<_>>().join(" OR ");
        where_clauses.push(format!("({ors})"));
        let pattern = format!("%{q}%");
        for _ in columns {
            bind_values.push(rusqlite::types::Value::Text(pattern.clone()));
        }
    }
    for (k, v) in &filters {
        where_clauses.push(format!("\"{k}\" = ?"));
        bind_values.push(json_to_sql_value(v));
    }
    let where_sql = if where_clauses.is_empty() { String::new() } else { format!("WHERE {}", where_clauses.join(" AND ")) };

    let conn = db.lock().unwrap();

    // The physical SQLite table only ever gets created lazily, by
    // whichever `.nir` function's own `CREATE TABLE IF NOT EXISTS` first
    // actually runs (e.g. `list_<struct>` or `create_<struct>`) -- this
    // route talks to SQLite directly, bypassing the interpreter
    // entirely, so it has no way to trigger that. A struct whose table
    // hasn't been created yet genuinely has zero rows (there's nothing
    // to have created it *for*), so that's the correct response here --
    // not a 500, and not a reason to duplicate every function's own
    // schema as a second, independently-maintained migration script.
    let table_exists: bool = conn
        .query_row("SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = ?", [table], |_| Ok(()))
        .is_ok();
    if !table_exists {
        drop(conn);
        let body = serde_json::json!({ "rows": [], "total": 0, "page": page, "page_size": page_size }).to_string();
        return (200, body);
    }

    let count_sql = format!("SELECT COUNT(*) FROM \"{table}\" {where_sql}");
    let total: i64 = match conn.query_row(&count_sql, rusqlite::params_from_iter(&bind_values), |row| row.get(0)) {
        Ok(n) => n,
        Err(e) => return (500, json_err(&format!("count query failed: {e}"))),
    };

    let select_sql = format!("SELECT {select_cols} FROM \"{table}\" {where_sql} ORDER BY \"{sort_field}\" {sort_dir} LIMIT ? OFFSET ?");
    let mut all_params = bind_values;
    all_params.push(rusqlite::types::Value::Integer(page_size));
    all_params.push(rusqlite::types::Value::Integer(offset));

    let mut stmt = match conn.prepare(&select_sql) {
        Ok(s) => s,
        Err(e) => return (500, json_err(&format!("query failed: {e}"))),
    };
    let column_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
    let rows: Result<Vec<JsonVal>, rusqlite::Error> = (|| {
        let mapped = stmt.query_map(rusqlite::params_from_iter(&all_params), |row| interpreter::db_row_to_json(row, &column_names))?;
        mapped.collect()
    })();
    let mut rows = match rows {
        Ok(r) => r,
        Err(e) => return (500, json_err(&format!("query failed: {e}"))),
    };
    for row in &mut rows {
        redact_gated_fields(row, identity, &view_gates);
    }

    let body = serde_json::json!({ "rows": rows, "total": total, "page": page, "page_size": page_size }).to_string();
    (200, body)
}

/// Bearer token -> `VerifiedIdentity`, shared by `dispatch`'s `/api/<fn>`
/// route and the `/_nirdosha/table/<name>` route's field-RBAC redaction
/// (previously only `dispatch` extracted this at all — that route had no
/// identity awareness whatsoever, a real gap for any view-gated field).
/// `Ok(None)` means no `Authorization` header was sent (anonymous, fine
/// for an ungated call); `Err` is a ready-to-return `(status, body)` for
/// every case that should stop the request outright (missing config,
/// malformed header, invalid or expired token).
fn resolve_identity(auth_header: Option<&str>, auth: Option<&AuthConfig>) -> Result<Option<Value>, (u16, String)> {
    match (auth_header, auth) {
        (Some(h), Some(cfg)) => match h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")) {
            Some(token) => match interpreter::validate_oidc_token(token, &cfg.issuer, &cfg.audience, &cfg.jwks_json) {
                Ok(v) => {
                    // A red-team finding: `validate_oidc_token` deliberately
                    // never checks `exp` against a real clock (see its own
                    // doc comment — that would break the determinism
                    // contract every `.nir`-level identity builtin keeps),
                    // so nothing rejected an expired bearer token here
                    // before — a leaked/logged/stolen token stayed valid
                    // forever. This *is* the right place for a real-clock
                    // check: `serve.rs` is Rust infrastructure at the
                    // actual network boundary, not `.nir` execution.
                    let now = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .map(|d| d.as_secs() as i64)
                        .unwrap_or(i64::MAX);
                    match interpreter::identity_expires_at(&v) {
                        Ok(expires_at) if now > expires_at => Err((401, json_err("invalid token: token has expired"))),
                        _ => Ok(Some(v)),
                    }
                }
                Err(e) => Err((401, json_err(&format!("invalid token: {e}")))),
            },
            None => Err((401, json_err("Authorization header must be `Bearer <token>`"))),
        },
        (Some(_), None) => {
            Err((500, json_err("this server has no --jwks-file/--issuer/--audience configured to validate a bearer token against")))
        }
        (None, _) => Ok(None),
    }
}

/// Whether `identity` satisfies one field's `view`/`edit` gate — any one
/// of `roles` (array-membership, `identity_has_role`, same helper
/// `requires(role: ...)` enforcement already uses) or the `claim`
/// key/value, if either was declared; ungated (`roles` empty and `claim`
/// `None`) always satisfies. No identity at all only satisfies an
/// ungated field.
fn identity_satisfies_gate(identity: Option<&Value>, roles: &[String], claim: &Option<(String, String)>) -> bool {
    if roles.is_empty() && claim.is_none() {
        return true;
    }
    let Some(id) = identity else { return false };
    if roles.iter().any(|r| interpreter::identity_has_role(id, r).unwrap_or(false)) {
        return true;
    }
    if let Some((k, v)) = claim {
        if interpreter::identity_claim(id, k).map(|cv| cv == *v).unwrap_or(false) {
            return true;
        }
    }
    false
}

/// Redacts every `view`-gated field `identity` isn't authorized for,
/// in place, from a response JSON value of unknown shape (a plain
/// object, an array of them, or a `{"ok": ...}`/`{"err": ...}`
/// `Result` envelope — every shape `list_`/`get_`/`create_`/`update_`
/// actually return in this codebase). Deliberately shallow: redacts
/// only an object's own top-level keys, no nested-struct recursion,
/// since no struct in this codebase currently nests another — extend if
/// that ever changes. `gates` is unfiltered (may include edit-only
/// entries with no `view` gate at all); those are simply never matched
/// below since their `view_roles`/`view_claim` stay empty/`None`.
fn redact_gated_fields(json: &mut JsonVal, identity: Option<&Value>, gates: &[crate::ui_gen::GatedField]) {
    if gates.is_empty() {
        return;
    }
    match json {
        JsonVal::Array(items) => {
            for item in items {
                redact_gated_fields(item, identity, gates);
            }
        }
        JsonVal::Object(m) => {
            if let Some(inner) = m.get_mut("ok") {
                redact_gated_fields(inner, identity, gates);
                return;
            }
            if m.contains_key("err") {
                return;
            }
            for g in gates {
                if g.view_roles.is_empty() && g.view_claim.is_none() {
                    continue;
                }
                if identity_satisfies_gate(identity, &g.view_roles, &g.view_claim) {
                    continue;
                }
                if let Some(v) = m.get_mut(&g.field_name) {
                    *v = JsonVal::Null;
                }
            }
        }
        _ => {}
    }
}

/// The real per-request work, factored out of the `tiny_http`-specific
/// loop above so it's plain data in, `(status, json body)` out — no
/// `tiny_http` types below this line, which is what makes `tests/
/// serve.rs` able to exercise it without spinning up a real socket for
/// every case (only the true end-to-end tests need a real port).
fn dispatch(
    program: &Arc<Program>,
    fn_name: &str,
    body: &str,
    auth_header: Option<&str>,
    auth: Option<&AuthConfig>,
    transact_log_path: &std::path::Path,
    table_db: Option<&std::sync::Mutex<rusqlite::Connection>>,
) -> (u16, String) {
    let Some(f) = program.fns.iter().find(|f| f.name == fn_name) else {
        return (404, json_err(&format!("no such function `{fn_name}`")));
    };

    // Bearer token -> VerifiedIdentity, if present and valid.
    let identity: Option<Value> = match resolve_identity(auth_header, auth) {
        Ok(id) => id,
        Err(status_body) => return status_body,
    };

    // `requires(role/claim: ...)` is enforced here, independently of
    // `f`'s own parameter list — see this module's doc comment for why
    // `call_named` alone can't be trusted to do this.
    if let Some(req) = &f.requires {
        match &identity {
            None => return (401, json_err(&format!("sign in required: {}", describe_requirement(req)))),
            Some(id) => {
                let ok = match req {
                    Requirement::Role(role) => interpreter::identity_has_role(id, role).unwrap_or(false),
                    Requirement::Claim(key, value) => interpreter::identity_claim(id, key).map(|v| v == *value).unwrap_or(false),
                };
                if !ok {
                    return (403, json_err(&format!("insufficient privilege: {}", describe_requirement(req))));
                }
            }
        }
    }

    let mut body_json: JsonVal = if body.trim().is_empty() { JsonVal::Object(Default::default()) } else {
        match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return (400, json_err(&format!("malformed JSON body: {e}"))),
        }
    };

    // View-gated field pass-through-on-omit: a caller who can't VIEW a
    // required field also never saw its current value, so it can't
    // meaningfully supply a new one. `create_<S>`/`update_<S>` take the
    // whole struct positionally, so without this, submitting any change
    // to some *other*, unrelated field on the same struct would fail
    // outright for that caller (an omitted/`null` value for a required,
    // non-`Option` field is a decode error, not silently ignored) --
    // exactly what happens if the client dutifully echoes back the
    // `null` `serve.rs`'s own view-gate redaction already put there.
    // Treat that specific shape (this field, omitted or `null`, caller
    // not authorized to view it) as "leave it as it already is":
    // substitute the currently stored value before decoding. Scoped to
    // `update_` the same way edit-gate enforcement is (needs a struct-
    // typed param and `--db`; `create_` has no "currently stored" row
    // to fall back to at all).
    if let Some(db) = table_db {
        let view_gates: Vec<crate::ui_gen::GatedField> = crate::ui_gen::field_gates_for_fn(program, fn_name)
            .into_iter()
            .filter(|g| !g.view_roles.is_empty() || g.view_claim.is_some())
            .collect();
        if !view_gates.is_empty() {
            if let Some(struct_p) = f.params.iter().find(|p| matches!(&p.ty, Ty::Named(n, a) if a.is_empty() && resolve_struct(program, n).is_some())) {
                if let Ty::Named(struct_name, _) = &struct_p.ty {
                    let table = to_snake_case(struct_name);
                    if let Some(obj) =
                        body_json.as_object_mut().and_then(|o| o.get_mut(&struct_p.name)).and_then(JsonVal::as_object_mut)
                    {
                        if let Some(id) = obj.get("id").and_then(JsonVal::as_i64) {
                            let conn = db.lock().unwrap();
                            for g in &view_gates {
                                if identity_satisfies_gate(identity.as_ref(), &g.view_roles, &g.view_claim) {
                                    continue;
                                }
                                let needs_fill = obj.get(&g.field_name).map(JsonVal::is_null).unwrap_or(true);
                                if !needs_fill {
                                    continue;
                                }
                                let stored: Result<rusqlite::types::Value, _> = conn.query_row(
                                    &format!("SELECT \"{}\" FROM \"{table}\" WHERE id = ?", g.field_name),
                                    [id],
                                    |row| row.get(0),
                                );
                                if let Ok(stored) = stored {
                                    obj.insert(g.field_name.clone(), sql_value_to_json(&stored));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    let empty_map = serde_json::Map::new();
    let body_obj = body_json.as_object().unwrap_or(&empty_map);

    let mut args = Vec::with_capacity(f.params.len());
    for p in &f.params {
        if is_verified_identity(&p.ty) {
            match &identity {
                Some(id) => args.push(id.clone()),
                None => return (401, json_err(&format!("sign in required: `{fn_name}` takes a VerifiedIdentity"))),
            }
            continue;
        }
        let Some(v) = body_obj.get(&p.name) else {
            return (400, json_err(&format!("missing field `{}` in request body", p.name)));
        };
        match decode_value(v, &p.ty, program, 0) {
            Ok(val) => args.push(val),
            Err(e) => return (400, json_err(&format!("field `{}`: {e}", p.name))),
        }
    }

    // Field-level `edit` RBAC (`screen <Struct> { field <name> { edit:
    // role(...) } }`) — enforced only for a struct's `update` slot (not
    // `create`, see `ui_gen::update_gates_for_fn`'s own doc comment for
    // why), and only when `--db` was passed (the "before" row needs a
    // real read). `create_<S>`/`update_<S>` both take the whole struct
    // positionally, so this compares each edit-gated field's incoming
    // value against what's actually stored, not merely "is the field
    // present" — every submission necessarily includes every field,
    // changed or not.
    if let (Some(db), Some((struct_name, gates))) = (table_db, crate::ui_gen::update_gates_for_fn(program, fn_name)) {
        if let Some(status_body) = check_edit_gates(db, program, f, &args, &struct_name, &gates, identity.as_ref()) {
            return status_body;
        }
    }

    let program_arc = Arc::clone(program);
    let interp = Interpreter::new(program_arc, Arc::from("")).with_transact_log_path(transact_log_path.to_path_buf());
    match interp.call_named_on_big_stack(fn_name, &args) {
        Ok(result) => match encode_value(&result, program) {
            Ok(mut json) => {
                let view_gates = crate::ui_gen::field_gates_for_fn(program, fn_name);
                redact_gated_fields(&mut json, identity.as_ref(), &view_gates);
                (200, json.to_string())
            }
            Err(e) => (500, json_err(&format!("`{fn_name}` returned a value this server can't encode as JSON: {e}"))),
        },
        Err(e) => (500, json_err(&e.to_string())),
    }
}

/// The actual `edit`-gate check `dispatch` runs before calling an
/// `update_<S>` fn: finds the struct-typed arg among `args` (the fn's
/// own declared param types, `f.params`, say which one), reads its `id`
/// field, `SELECT`s the *currently stored* value of every edit-gated
/// column via `table_db` (same table/column-name convention `migrate.rs`
/// already relies on), and compares. Returns `Some((403, ...))` for the
/// first gated field whose incoming value genuinely differs from what's
/// stored and whose caller isn't authorized for that field's `edit`
/// gate; `None` means every gated field either matches what's already
/// stored, or the caller is authorized — the call may proceed. A field
/// this can't compare (an unsupported column type) or a row that
/// doesn't exist yet is treated as nothing to protect, not an error —
/// `update_<S>`'s own SQL will handle a missing row exactly as it
/// always did.
fn check_edit_gates(
    db: &std::sync::Mutex<rusqlite::Connection>,
    program: &Program,
    f: &crate::ast::FnDecl,
    args: &[Value],
    struct_name: &str,
    gates: &[crate::ui_gen::GatedField],
    identity: Option<&Value>,
) -> Option<(u16, String)> {
    let struct_arg = f
        .params
        .iter()
        .zip(args)
        .find_map(|(p, v)| if matches!(&p.ty, Ty::Named(n, a) if n == struct_name && a.is_empty()) { Some(v) } else { None })?;
    let Value::Struct(_, fields) = struct_arg else { return None };
    let decl = resolve_struct(program, struct_name)?;
    let id_idx = decl.fields.iter().position(|fd| fd.name == "id")?;
    let id = match &fields[id_idx] {
        Value::Int(id) => *id,
        _ => return None,
    };

    let table = to_snake_case(struct_name);
    let conn = db.lock().unwrap();
    for g in gates {
        if g.edit_roles.is_empty() && g.edit_claim.is_none() {
            continue;
        }
        let Some(field_idx) = decl.fields.iter().position(|fd| fd.name == g.field_name) else { continue };
        let stored: rusqlite::types::Value =
            match conn.query_row(&format!("SELECT \"{}\" FROM \"{table}\" WHERE id = ?", g.field_name), [id], |row| row.get(0)) {
                Ok(v) => v,
                Err(_) => continue, // no such row yet -- nothing stored to protect
            };
        let Some(unchanged) = value_matches_stored(&fields[field_idx], &stored) else { continue };
        if unchanged {
            continue;
        }
        if identity_satisfies_gate(identity, &g.edit_roles, &g.edit_claim) {
            continue;
        }
        return Some((403, json_err(&format!("insufficient privilege: cannot change `{}.{}`", struct_name, g.field_name))));
    }
    None
}

/// Compares a decoded `Value` (the incoming, not-yet-applied field
/// value) against the currently stored SQLite column value. `None` when
/// the pairing isn't one this can compare (an unsupported column type,
/// or a genuine type mismatch that shouldn't happen post-typecheck) —
/// callers treat that as "can't verify, nothing to protect," the same
/// tolerant posture `migrate.rs::column_def` takes for a field type with
/// no single-column SQL shape.
fn value_matches_stored(new: &Value, stored: &rusqlite::types::Value) -> Option<bool> {
    use rusqlite::types::Value as SqlVal;
    match (new, stored) {
        (Value::Int(a), SqlVal::Integer(b)) => Some(a == b),
        (Value::Float(a), SqlVal::Real(b)) => Some(a == b),
        (Value::Str(a), SqlVal::Text(b)) => Some(a.as_ref() == b.as_str()),
        (Value::Bool(a), SqlVal::Integer(b)) => Some(*a == (*b != 0)),
        (Value::Enum(_, variant, payload), SqlVal::Text(b)) if payload.is_empty() => Some(variant.as_ref() == b.as_str()),
        _ => None,
    }
}

fn describe_requirement(req: &Requirement) -> String {
    match req {
        Requirement::Role(role) => format!("requires role `{role}`"),
        Requirement::Claim(key, value) => format!("requires claim `{key}` = `{value}`"),
    }
}

fn is_verified_identity(ty: &Ty) -> bool {
    matches!(ty, Ty::Named(n, args) if n == "VerifiedIdentity" && args.is_empty())
}

fn json_err(msg: &str) -> String {
    serde_json::json!({ "err": msg }).to_string()
}

fn resolve_struct<'a>(program: &'a Program, name: &str) -> Option<&'a crate::ast::StructDecl> {
    program.structs.iter().find(|s| s.name == name)
}

fn resolve_enum<'a>(program: &'a Program, name: &str) -> Option<&'a crate::ast::EnumDecl> {
    program.enums.iter().find(|e| e.name == name)
}

/// A zero-payload ("unit") enum variant decodes from a plain JSON string
/// naming the variant -- `"Active"`, not `{"variant":"Active","payload":
/// []}` -- matching `ui_gen.rs::build_field`'s `"select"` control, which
/// only exists for enums where every variant is unit (categorical/ordinal
/// fields; see its own doc comment). A payload-carrying variant is a
/// clear 400, not a silent misdecode -- the same discipline `decode_value`
/// already applies to every other unsupported shape.
fn decode_enum_value(json: &JsonVal, enum_name: &str, e: &crate::ast::EnumDecl) -> Result<Value, String> {
    let variant_name = json.as_str().ok_or_else(|| format!("expected a string naming a `{enum_name}` variant"))?;
    let variant = e
        .variants
        .iter()
        .find(|v| v.name == variant_name)
        .ok_or_else(|| format!("`{variant_name}` is not a valid variant of `{enum_name}`"))?;
    if !variant.payload.is_empty() {
        return Err(format!(
            "`{enum_name}::{variant_name}` carries a payload -- only zero-payload variants can be decoded from a plain string"
        ));
    }
    Ok(Value::Enum(Arc::from(enum_name), Arc::from(variant.name.as_str()), Arc::from(vec![])))
}

/// Decodes one JSON value into a `Value`, driven by `ty` — the server-
/// side mirror of `ui_gen.rs::build_field`'s client-side control
/// derivation: scalars decode directly, `Option(T)` unwraps (`null` ->
/// `None`), a zero-payload enum reference decodes from its variant name
/// (`decode_enum_value` above), a bare reference to another struct in
/// this program expands one level deep (depth-capped at 2, same
/// cycle-safety reasoning `build_field` documents), and anything else
/// (affine handles, `Fn`, a payload-carrying enum, deeper nesting) is a
/// clear 400, never a silent misdecode.
fn decode_value(json: &JsonVal, ty: &Ty, program: &Program, depth: u8) -> Result<Value, String> {
    match ty {
        Ty::Str => json.as_str().map(|s| Value::Str(Arc::from(s))).ok_or_else(|| "expected a string".to_string()),
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => {
            json.as_i64().map(Value::Int).ok_or_else(|| "expected an integer".to_string())
        }
        Ty::F64 => json.as_f64().map(Value::Float).ok_or_else(|| "expected a number".to_string()),
        Ty::Bool => json.as_bool().map(Value::Bool).ok_or_else(|| "expected a boolean".to_string()),
        Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
            if json.is_null() {
                Ok(Value::Enum(Arc::from("Option"), Arc::from("None"), Arc::from(vec![])))
            } else {
                let inner = decode_value(json, &args[0], program, depth)?;
                Ok(Value::Enum(Arc::from("Option"), Arc::from("Some"), Arc::from(vec![inner])))
            }
        }
        Ty::Named(n, args) if args.is_empty() => {
            if let Some(e) = resolve_enum(program, n) {
                return decode_enum_value(json, n, e);
            }
            if depth >= 2 {
                return Err(format!("cannot decode a `{n}` from a JSON request body"));
            }
            match resolve_struct(program, n) {
                Some(s) => {
                    let obj = json.as_object().ok_or_else(|| format!("expected an object for `{n}`"))?;
                    let mut fields = Vec::with_capacity(s.fields.len());
                    for field in &s.fields {
                        let fv = obj.get(&field.name).ok_or_else(|| format!("missing field `{}` for `{n}`", field.name))?;
                        fields.push(decode_value(fv, &field.ty, program, depth + 1)?);
                    }
                    Ok(Value::Struct(Arc::from(n.as_str()), Arc::from(fields)))
                }
                None => Err(format!("cannot decode a `{n}` from a JSON request body")),
            }
        }
        other => Err(format!("cannot decode a `{}` from a JSON request body", other.name())),
    }
}

/// The inverse of `decode_value` — a `Ty::Named("Result", [_, _])`
/// `Value::Enum` encodes to `{"ok":...}`/`{"err":...}`, matching
/// `ui_gen_template.html`'s `callFn` exactly, so a `Result`-returning
/// handler needs no client-side special-casing. `Value::Json` (Nirdosha's
/// own opaque JSON type, e.g. a raw `db_query` result) passes through
/// unchanged — it's already a `serde_json::Value`. Affine handles
/// (`box`/`thread`/`chan`/`sandbox`/`tcp`/`file`/`db`/`mq`), `Fn`, are
/// refused with a clear error rather than a garbage encoding.
fn encode_value(v: &Value, program: &Program) -> Result<JsonVal, String> {
    match v {
        Value::Int(n) => Ok(JsonVal::from(*n)),
        Value::Float(f) => Ok(if f.is_finite() { JsonVal::from(*f) } else { JsonVal::Null }),
        Value::Bool(b) => Ok(JsonVal::Bool(*b)),
        Value::Unit => Ok(JsonVal::Null),
        Value::Str(s) => Ok(JsonVal::String(s.to_string())),
        Value::Json(doc) => Ok((**doc).clone()),
        Value::Vector(elems) => elems.iter().map(|e| encode_value(e, program)).collect::<Result<Vec<_>, _>>().map(JsonVal::Array),
        Value::Matrix(elems, rows, cols) => (0..*rows)
            .map(|r| elems[r * cols..(r + 1) * cols].iter().map(|e| encode_value(e, program)).collect::<Result<Vec<_>, _>>().map(JsonVal::Array))
            .collect::<Result<Vec<_>, _>>()
            .map(JsonVal::Array),
        Value::Enum(enum_name, variant, payload) => {
            let key = match enum_name.as_ref() {
                "Result" if variant.as_ref() == "Ok" => Some("ok"),
                "Result" if variant.as_ref() == "Err" => Some("err"),
                _ => None,
            };
            if let Some(key) = key {
                let inner = payload.first().map(|p| encode_value(p, program)).transpose()?.unwrap_or(JsonVal::Null);
                let mut m = serde_json::Map::new();
                m.insert(key.to_string(), inner);
                return Ok(JsonVal::Object(m));
            }
            if enum_name.as_ref() == "Option" {
                return match variant.as_ref() {
                    "Some" => encode_value(&payload[0], program),
                    _ => Ok(JsonVal::Null),
                };
            }
            let payload_json = payload.iter().map(|p| encode_value(p, program)).collect::<Result<Vec<_>, _>>()?;
            Ok(serde_json::json!({ "variant": variant.as_ref(), "payload": payload_json }))
        }
        Value::Struct(name, fields) => match resolve_struct(program, name) {
            Some(s) => {
                let mut m = serde_json::Map::new();
                for (field, val) in s.fields.iter().zip(fields.iter()) {
                    m.insert(field.name.clone(), encode_value(val, program)?);
                }
                Ok(JsonVal::Object(m))
            }
            None => Err(format!("unknown struct `{name}`")),
        },
        Value::Boxed(_) | Value::Ref(_) | Value::Thread(_) | Value::Channel(_) | Value::Sandbox(_) | Value::Tcp(_)
        | Value::TcpListener(_) | Value::File(_) | Value::Db(_) | Value::Mq(_) | Value::Fn(_) => {
            Err("this value type can't be sent over HTTP (an affine handle or function value)".to_string())
        }
    }
}
