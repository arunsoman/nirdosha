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

/// Starts serving `program` on `port` and blocks forever (one request at
/// a time — see the module doc; this is a demo/dev entry point, not a
/// tuned production server). `identity_base`, if given, is baked into
/// the served UI so its login screen talks to a real (mock) identity
/// app instead of the pure client-side stub (`ui_gen::generate`'s doc
/// comment).
pub fn run(
    program: Arc<Program>,
    port: u16,
    auth: Option<AuthConfig>,
    identity_base: Option<&str>,
    transact_log_path: std::path::PathBuf,
) -> Result<(), String> {
    let registry = crate::ast::TypeRegistry::build(&program);
    let effects = crate::effects::infer_effects(&program, &registry);
    let ui_html = crate::ui_gen::generate(&program, &effects, identity_base);
    let auth = Arc::new(auth);
    let transact_log_path = Arc::new(transact_log_path);

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

    let server = tiny_http::Server::http(("0.0.0.0", port)).map_err(|e| format!("failed to bind :{port}: {e}"))?;
    eprintln!("nirdosha serve: listening on http://0.0.0.0:{port}  (GET / for the UI, POST /api/<fn> for the API)");

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
            if let Some(fn_name) = url.strip_prefix("/api/") {
                let mut body = String::new();
                let _ = request.as_reader().read_to_string(&mut body);
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
                    dispatch(&program, fn_name, &body, auth_header.as_deref(), auth.as_ref().as_ref(), &transact_log_path)
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
) -> (u16, String) {
    let Some(f) = program.fns.iter().find(|f| f.name == fn_name) else {
        return (404, json_err(&format!("no such function `{fn_name}`")));
    };

    // Bearer token -> VerifiedIdentity, if present and valid. A present-
    // but-invalid token is a hard 401 (the caller was trying to
    // authenticate and failed), not silently treated as anonymous.
    let identity: Option<Value> = match (auth_header, auth) {
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
                        Ok(expires_at) if now > expires_at => {
                            return (401, json_err("invalid token: token has expired"));
                        }
                        _ => {}
                    }
                    Some(v)
                }
                Err(e) => return (401, json_err(&format!("invalid token: {e}"))),
            },
            None => return (401, json_err("Authorization header must be `Bearer <token>`")),
        },
        (Some(_), None) => return (500, json_err("this server has no --jwks-file/--issuer/--audience configured to validate a bearer token against")),
        (None, _) => None,
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

    let body_json: JsonVal = if body.trim().is_empty() { JsonVal::Object(Default::default()) } else {
        match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return (400, json_err(&format!("malformed JSON body: {e}"))),
        }
    };
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

    let program_arc = Arc::clone(program);
    let interp = Interpreter::new(program_arc, Arc::from("")).with_transact_log_path(transact_log_path.to_path_buf());
    match interp.call_named_on_big_stack(fn_name, &args) {
        Ok(result) => match encode_value(&result, program) {
            Ok(json) => (200, json.to_string()),
            Err(e) => (500, json_err(&format!("`{fn_name}` returned a value this server can't encode as JSON: {e}"))),
        },
        Err(e) => (500, json_err(&e.to_string())),
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

/// Decodes one JSON value into a `Value`, driven by `ty` — the server-
/// side mirror of `ui_gen.rs::build_field`'s client-side control
/// derivation: scalars decode directly, `Option(T)` unwraps (`null` ->
/// `None`), a bare reference to another struct in this program expands
/// one level deep (depth-capped at 2, same cycle-safety reasoning
/// `build_field` documents), and anything else (affine handles, `Fn`,
/// enum references, deeper nesting) is a clear 400, never a silent
/// misdecode.
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
        Ty::Named(n, args) if args.is_empty() && depth < 2 => match resolve_struct(program, n) {
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
        },
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
