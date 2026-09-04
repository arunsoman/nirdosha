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
//! - `POST /api/_demo_login` — demo mode only (no real `--jwks-file`/
//!   `--issuer`/`--audience` configured): mints a real, ephemeral-
//!   signed token for a self-declared `{subject, roles, claims}`, via
//!   `AuthConfig::demo()`'s per-process secret and `mock_issue_token`.
//!   Does not exist at all on a server started with real identity flags.
//! - `GET /auth/login` / `GET /auth/callback` — production mode only
//!   (the `--oidc-*` flags configured, which itself requires the real
//!   identity trio): the real OIDC Authorization Code + PKCE redirect
//!   flow, ending in the same `validate_oidc_token` verification every
//!   other request already goes through.
//! - `GET /api/_whoami` — always present, every mode: confirms a
//!   bearer token is still valid against *this* running process (the
//!   generated UI calls this once at page load to catch a
//!   `localStorage`-cached identity from a previous, since-restarted
//!   demo-mode process, whose ephemeral signing key no longer matches).
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
//! role." A role check specifically (never a claim check) additionally
//! passes through `identity_has_mapped_role`/`RoleMappingCache` —
//! `docs/ROADMAP.md` Track A6 — so a `RoleMapping`-configured project
//! translates the app's own role vocabulary into whatever the connected
//! IdP actually emits; `identity_has_role` itself stays untouched and
//! still means literal-match, since it's also the in-language
//! `check_role` builtin's own implementation, which has no server-only
//! cache to consult.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use base64::Engine;
use rust_decimal::Decimal;
use serde_json::Value as JsonVal;

use crate::ast::{Program, Requirement, Ty};
use crate::interpreter::{self, Interpreter, Value};

pub struct AuthConfig {
    pub jwks_json: String,
    pub issuer: String,
    pub audience: String,
}

impl AuthConfig {
    /// Ephemeral, per-process demo identity: a random HMAC secret
    /// generated once at server startup, never persisted or logged,
    /// wrapped in a synthetic single-key JWKS (`kty:"oct"` — the only
    /// key type `interpreter::mock_issue_token` can sign with). Used
    /// exactly when `nirdosha serve` is given no real `--jwks-file`/
    /// `--issuer`/`--audience` (`main.rs::cmd_serve`'s `(None,None,None)`
    /// arm) — a self-declared demo identity is only ever "real" because
    /// it's actually signed by this and actually re-verified through the
    /// exact same `validate_oidc_token` a genuine IdP's token goes
    /// through (`resolve_identity`, below), never trusted on its face.
    pub fn demo() -> AuthConfig {
        let mut buf = [0u8; 32];
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
            .expect("OS entropy source for the ephemeral demo signing key");
        let secret = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf);
        let jwks_json = serde_json::json!({"keys": [{"kid": "demo", "kty": "oct", "k": secret}]}).to_string();
        AuthConfig { jwks_json, issuer: "nirdosha-demo".to_string(), audience: "nirdosha-demo".to_string() }
    }
}

/// `nirdosha serve --oidc-client-id ... --oidc-redirect-uri ...
/// --oidc-authorize-endpoint ... --oidc-token-endpoint ...
/// [--oidc-client-secret ...]` — production mode's real OIDC
/// Authorization Code (+ PKCE) redirect flow. Only meaningful, and only
/// ever constructed, alongside a real `AuthConfig` (the jwks/issuer/
/// audience trio) — SSO is an additional layer on top of real-token
/// validation, never a replacement for it. `client_secret` is optional:
/// a public, PKCE-only client registration needs none at all (the
/// `code_verifier` already proves possession), so the token-exchange
/// request simply omits it when absent, supporting both public and
/// confidential client registrations without a separate flag.
pub struct OidcSsoConfig {
    pub client_id: String,
    pub client_secret: Option<String>,
    pub redirect_uri: String,
    pub authorize_endpoint: String,
    pub token_endpoint: String,
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

/// Below this, gzip's own framing overhead (~20 bytes) plus the CPU cost
/// of compressing at all routinely outweighs the saving -- most `/api/*`
/// JSON responses (a handful of bytes to a few hundred) are already
/// smaller than that. `GET /`'s `ui_gen`-derived HTML (100s of KB) is the
/// case this actually exists for.
const COMPRESSION_THRESHOLD_BYTES: usize = 512;

fn accepts_gzip(request: &tiny_http::Request) -> bool {
    request
        .headers()
        .iter()
        .any(|h| h.field.as_str().as_str().eq_ignore_ascii_case("Accept-Encoding") && h.value.as_str().to_ascii_lowercase().contains("gzip"))
}

fn gzip_compress(body: &[u8]) -> Vec<u8> {
    use std::io::Write;
    let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(body).expect("writing into an in-memory Vec<u8> cannot fail");
    encoder.finish().expect("finishing an in-memory gzip stream cannot fail")
}

/// Single response-sending path for every route in `run`'s dispatch loop
/// below, replacing what used to be ~17 separate inline `Response::
/// from_string(...).with_header(...)` call sites (one per route) --
/// consolidated so the two "reduce per-request bytes on the wire" levers
/// live in exactly one place instead of needing to be re-added at each
/// site: gzip-compresses the body whenever the client advertises
/// `Accept-Encoding: gzip` and the payload clears
/// `COMPRESSION_THRESHOLD_BYTES` (`Vary: Accept-Encoding` always
/// accompanies it, so a shared cache in front of this never serves the
/// wrong encoding to a client that didn't ask for it), and always applies
/// CORS + records the status in `metrics`. `extra_headers` carries
/// anything route-specific (`Location` on a redirect, `ETag`/
/// `Cache-Control` on `GET /`) that doesn't fit the common
/// status/content-type/body shape.
fn send_response(
    request: tiny_http::Request,
    metrics: &Metrics,
    request_started_at: Instant,
    status: u16,
    content_type: Option<&str>,
    body: String,
    extra_headers: Vec<tiny_http::Header>,
) {
    let use_gzip = body.len() >= COMPRESSION_THRESHOLD_BYTES && accepts_gzip(&request);
    let bytes: Vec<u8> = if use_gzip { gzip_compress(body.as_bytes()) } else { body.into_bytes() };
    let mut resp = tiny_http::Response::from_data(bytes).with_status_code(status);
    if let Some(ct) = content_type {
        resp.add_header(header("Content-Type", ct));
    }
    if use_gzip {
        resp.add_header(header("Content-Encoding", "gzip"));
    }
    resp.add_header(header("Vary", "Accept-Encoding"));
    for h in cors_headers() {
        resp.add_header(h);
    }
    for h in extra_headers {
        resp.add_header(h);
    }
    let _ = request.respond(resp);
    metrics.record(status, request_started_at.elapsed());
}

/// Weak validator for `GET /`'s `ui_gen`-derived HTML -- not a security
/// hash (`DefaultHasher` is not cryptographic), just a cheap change
/// detector so a client that already has the current bytes (the common
/// case: the same generated UI on every page load/refresh until the
/// server is restarted or `--theme` hot-reloads) gets a bodyless `304`
/// instead of re-downloading the whole page.
fn compute_etag(body: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    body.hash(&mut hasher);
    format!("\"{:x}\"", hasher.finish())
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

/// `NIRDOSHA_LOG_FORMAT=json` switches this server's own lifecycle log
/// lines (bind, migration, crash replay, shutdown -- NOT per-request
/// errors, which already carry their own status/body) from the plain
/// `eprintln!` text every one of them used before, to one-line JSON
/// objects a log pipeline (Loki/ELK/CloudWatch) can parse without a
/// regex (`docs/KUBERNETES.md`'s "Observability" row: `--format=json` already
/// existed for the *interpreter's* diagnostics, but nothing covered
/// `serve`'s own lifecycle log). An env var rather than a new `run()`
/// parameter deliberately: every existing test in `tests/*.rs` calls
/// `serve::run(...)` positionally with today's exact argument list, and
/// `main.rs::cmd_serve` is the only real caller that would ever need to
/// pass this — same seam `NIRDOSHA_BIN`/`NIRDOSHA_TEST_ROLE_MAPPING_TTL_MS`
/// already use for exactly this "operator/CLI wants to flip a behavior
/// without changing every test call site" reason. `false` (plain text)
/// is the default -- byte-for-byte the same output this server always
/// produced.
fn log_json_enabled() -> bool {
    std::env::var("NIRDOSHA_LOG_FORMAT").map(|v| v.eq_ignore_ascii_case("json")).unwrap_or(false)
}

/// One lifecycle log line, in whichever format `log_json_enabled()`
/// selects. `level` is a bare word (`"info"`/`"error"`) so a JSON
/// consumer can filter on it without parsing free text.
fn log_lifecycle(level: &str, msg: &str) {
    if log_json_enabled() {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        println!(
            "{}",
            serde_json::json!({"ts": now, "level": level, "component": "nirdosha-serve", "msg": msg})
        );
    } else {
        eprintln!("nirdosha serve: {msg}");
    }
}

/// Process-lifetime request counters behind `GET /metrics`
/// (`docs/KUBERNETES.md`'s "Observability" row: "Net-new -- request
/// counts/latencies from `serve.rs`'s dispatch loop would be the natural
/// first cut"). Plain `AtomicU64`s, not a `Mutex`-guarded struct: this
/// server handles one request at a time on one thread (this module's own
/// doc comment), so there is never real contention to protect against —
/// atomics are just the cheapest way to get interior mutability from a
/// `&Metrics` shared across loop iterations without restructuring the
/// loop into something that owns `&mut self`. Deliberately hand-rolled
/// Prometheus text exposition format rather than pulling in the
/// `prometheus` crate: five counters/gauges is well under the complexity
/// where that dependency earns its cost, and the exposition format
/// itself is a stable, trivial-to-hand-write text protocol.
struct Metrics {
    requests_total: AtomicU64,
    responses_2xx: AtomicU64,
    responses_4xx: AtomicU64,
    responses_5xx: AtomicU64,
    responses_other: AtomicU64,
    latency_ms_sum: AtomicU64,
    started_at: Instant,
}

impl Metrics {
    fn new() -> Self {
        Self {
            requests_total: AtomicU64::new(0),
            responses_2xx: AtomicU64::new(0),
            responses_4xx: AtomicU64::new(0),
            responses_5xx: AtomicU64::new(0),
            responses_other: AtomicU64::new(0),
            latency_ms_sum: AtomicU64::new(0),
            started_at: Instant::now(),
        }
    }

    fn record(&self, status: u16, elapsed: Duration) {
        self.requests_total.fetch_add(1, Ordering::Relaxed);
        self.latency_ms_sum.fetch_add(elapsed.as_millis() as u64, Ordering::Relaxed);
        let bucket = match status {
            200..=299 => &self.responses_2xx,
            400..=499 => &self.responses_4xx,
            500..=599 => &self.responses_5xx,
            _ => &self.responses_other,
        };
        bucket.fetch_add(1, Ordering::Relaxed);
    }

    /// Prometheus text exposition format (v0.0.4) -- `# HELP`/`# TYPE`
    /// then one sample per line, the minimum a real `/metrics` scrape
    /// target needs to be valid input to `promtool check metrics` or a
    /// live Prometheus scrape.
    fn render(&self) -> String {
        let total = self.requests_total.load(Ordering::Relaxed);
        let sum_ms = self.latency_ms_sum.load(Ordering::Relaxed);
        let avg_ms = if total > 0 { sum_ms as f64 / total as f64 } else { 0.0 };
        format!(
            "# HELP nirdosha_requests_total Total HTTP requests handled since process start.\n\
             # TYPE nirdosha_requests_total counter\n\
             nirdosha_requests_total {total}\n\
             # HELP nirdosha_responses_total HTTP responses by status class.\n\
             # TYPE nirdosha_responses_total counter\n\
             nirdosha_responses_total{{class=\"2xx\"}} {}\n\
             nirdosha_responses_total{{class=\"4xx\"}} {}\n\
             nirdosha_responses_total{{class=\"5xx\"}} {}\n\
             nirdosha_responses_total{{class=\"other\"}} {}\n\
             # HELP nirdosha_request_latency_ms_sum Sum of request handling latency in milliseconds.\n\
             # TYPE nirdosha_request_latency_ms_sum counter\n\
             nirdosha_request_latency_ms_sum {sum_ms}\n\
             # HELP nirdosha_request_latency_ms_avg Mean request handling latency in milliseconds.\n\
             # TYPE nirdosha_request_latency_ms_avg gauge\n\
             nirdosha_request_latency_ms_avg {avg_ms:.3}\n\
             # HELP nirdosha_uptime_seconds Seconds since this process started serving.\n\
             # TYPE nirdosha_uptime_seconds gauge\n\
             nirdosha_uptime_seconds {}\n",
            self.responses_2xx.load(Ordering::Relaxed),
            self.responses_4xx.load(Ordering::Relaxed),
            self.responses_5xx.load(Ordering::Relaxed),
            self.responses_other.load(Ordering::Relaxed),
            self.started_at.elapsed().as_secs(),
        )
    }
}

/// `SIGTERM`/`SIGINT` -> a shared flag the request loop below polls
/// between requests (`docs/KUBERNETES.md`'s "Health & lifecycle" row: durability
/// already survives a hard `SIGKILL` today -- `tests/transact_process_kill.rs`
/// -- so this closes the *politeness* gap, not a correctness one: stop
/// accepting new connections and exit promptly instead of waiting out a
/// full `terminationGracePeriodSeconds` SIGKILL on every rolling restart).
/// `signal_hook::flag::register` is async-signal-safe (it only sets an
/// `AtomicBool`, no allocation, no locking) -- the one safe way to react
/// to a signal from Rust without a full async runtime.
fn install_shutdown_signal_handlers() -> Result<Arc<AtomicBool>, String> {
    let flag = Arc::new(AtomicBool::new(false));
    for sig in [signal_hook::consts::SIGTERM, signal_hook::consts::SIGINT] {
        signal_hook::flag::register(sig, Arc::clone(&flag))
            .map_err(|e| format!("failed to install shutdown signal handler: {e}"))?;
    }
    Ok(flag)
}

/// Starts serving `program` on `port` and blocks forever (one request at
/// a time — see the module doc; this is a demo/dev entry point, not a
/// tuned production server). `identity_base`, if given, is baked into
/// the served UI so its login screen talks to a real (mock) identity
/// app instead of the pure client-side stub (`ui_gen::generate`'s doc
/// comment). `otel_port`/`otel_token` are `observability.rs`'s layer 2a
/// — see that module's "Rollout layers 2-4" section: `otel_port` opens a
/// second, loopback-only listener dedicated to APM consumers, separate
/// from `port` above, and every request's own `Interpreter` (`dispatch`,
/// below) carries the same dormant `Tracer`, gated live by whether an
/// APM client is currently connected there. `main.rs::cmd_serve` refuses
/// to call this with `otel_port: Some(_)` and `otel_token: None` — this
/// function itself trusts that invariant rather than re-checking it, the
/// same relationship it already has with `auth`'s all-or-nothing
/// JWKS/issuer/audience validation.
pub fn run(
    program: Arc<Program>,
    host: &str,
    port: u16,
    auth: Option<AuthConfig>,
    identity_base: Option<&str>,
    transact_log_path: impl Into<crate::durability::LogTarget>,
    workflow_log_path: impl Into<crate::durability::LogTarget>,
    presence_token: Option<String>,
    db_path: Option<String>,
    theme: Option<&crate::ui_gen::Theme>,
    theme_path: Option<&str>,
    otel_port: Option<u16>,
    otel_token: Option<String>,
    // Demo mode: true exactly when `auth` was synthesized by
    // `AuthConfig::demo()` (`main.rs::cmd_serve`'s `(None,None,None)`
    // arm) rather than built from real `--jwks-file`/`--issuer`/
    // `--audience`. Gates whether `/api/_demo_login` exists at all and
    // which login-screen branch `ui_gen::generate` renders.
    demo_mode: bool,
    // Production mode's SSO config: `Some` exactly when the `--oidc-*`
    // flags were given (which itself requires the real identity trio,
    // `main.rs::cmd_serve`'s validation) — gates whether `/auth/login`/
    // `/auth/callback` exist at all.
    sso: Option<OidcSsoConfig>,
    // Track B of the plugin-ecosystem plan (rfcs/0003-plugin-abi-v2.md):
    // the stock `nirdosha` CLI has no plugin to pass here (`&[]` at its
    // one call site, `main.rs::cmd_serve`) — this parameter exists so a
    // *custom* binary linking a real plugin crate can call `serve::run`
    // directly with its own `NirdoshaPlugin::builtins()` list, the same
    // way `run_with_plugins` already lets one do for the interpreter
    // path. `serve`'s own request loop is synchronous, single-threaded
    // (no `thread::spawn` anywhere in this file) — a borrowed slice
    // living for this function's whole (non-returning, under normal
    // operation) lifetime needs no `Arc`/`'static` bound to stay valid
    // across every request `dispatch` handles.
    plugins: &[crate::plugin::PluginBuiltin],
) -> Result<(), String> {
    // `impl Into<LogTarget>` on the two params above so every existing
    // `PathBuf`-passing caller (every test in this repo included) keeps
    // compiling unchanged -- resolved to a concrete `LogTarget` once,
    // right here, since everything below needs the concrete type.
    let transact_log_path: crate::durability::LogTarget = transact_log_path.into();
    let workflow_log_path: crate::durability::LogTarget = workflow_log_path.into();
    // Phase 0 (`docs/ROADMAP.md`'s multi-instance fix): refuse to start a
    // second `nirdosha serve` process pointed at the same SQLite
    // durability file -- acquired once, here, for the server's whole
    // lifetime (this function never returns under normal operation:
    // the request loop below runs until the process is killed), entirely
    // separate from `TransactLog::open`/`WorkflowLog::open`'s own
    // per-request opens (`workflow_log.rs`'s own doc comment explains why
    // those stay unlocked). Postgres targets are skipped -- multiple
    // replicas sharing one Postgres database is the *intended*
    // multi-instance story there, not a mistake to refuse
    // (`instance_lock.rs`'s own doc comment covers exactly what this
    // guard does and doesn't protect against). Deduplicated by path so
    // an operator who happens to point `--transact-log`/`--workflow-log`
    // at the same file gets one lock, not two conflicting with each
    // other from this same process.
    let mut locked_paths: Vec<std::path::PathBuf> = Vec::new();
    let mut _instance_locks: Vec<crate::instance_lock::InstanceLock> = Vec::new();
    for target in [&transact_log_path, &workflow_log_path] {
        if let crate::durability::LogTarget::Sqlite(path) = target {
            if !locked_paths.contains(path) {
                _instance_locks.push(crate::instance_lock::InstanceLock::acquire(path)?);
                locked_paths.push(path.clone());
            }
        }
    }
    let registry = crate::ast::TypeRegistry::build(&program);
    let effects = crate::effects::infer_effects(&program, &registry);
    let server_table_api = db_path.is_some();
    let production_mode = sso.is_some();
    let ui_html = crate::ui_gen::generate(&program, &effects, identity_base, server_table_api, demo_mode, production_mode, theme);
    // Live theme reload: `--theme <path>` is otherwise read exactly once,
    // at this startup — a redeployed `theme.json` would need a full
    // server restart to take effect. Same TTL-cache pattern
    // `RoleMappingCache` already established (eager value above, then
    // re-checked/refreshed at most once per `theme_ttl()` window, on
    // demand rather than a background timer): bounded staleness (a new
    // theme takes effect within one TTL window, no restart), the same
    // disclosed tradeoff every other "does this update live" feature in
    // this file already takes. `None` without `--theme` at all — the
    // cache is never consulted, `GET /` serves the captured `ui_html`
    // above forever, byte-for-byte today's behavior.
    let theme_cache: Option<Arc<std::sync::Mutex<ThemeCache>>> =
        theme_path.map(|_| Arc::new(std::sync::Mutex::new(ThemeCache { html: ui_html.clone(), loaded_at: std::time::Instant::now() })));
    let auth = Arc::new(auth);
    let sso = Arc::new(sso);
    let pkce_store: Arc<PkceStore> = Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
    let transact_log_path = Arc::new(transact_log_path);
    let workflow_log_path = Arc::new(workflow_log_path);
    // Layer 2a: built once, dormant, for the server's whole lifetime —
    // every request's `Interpreter` gets a cheap `Arc::clone` of this
    // same `Tracer` (`dispatch`, below), same threading `--otel-console`
    // already established (`lib.rs`'s `_with_tracer` variants). `None`
    // entirely when `--otel-port` wasn't passed: no listener, no
    // `Tracer`, byte-for-byte the same server this always was.
    let tracer = match otel_port {
        Some(p) => {
            let t = crate::observability::Tracer::new_dynamic();
            let token = otel_token.expect("cmd_serve requires --otel-token whenever --otel-port is set");
            crate::observability::spawn_otel_port_listener(Arc::clone(&t), p, token)
                .map_err(|e| format!("failed to bind --otel-port {p}: {e}"))?;
            log_lifecycle("info", &format!("APM port listening on http://127.0.0.1:{p} (dormant until an APM client connects)"));
            Some(t)
        }
        None => None,
    };
    let presence_token = Arc::new(presence_token);
    // `--db <path>`: opens one shared connection, direct rusqlite (not
    // through the interpreter), backing the generic
    // `/_nirdosha/table/<snake>` pagination/sort/filter/search route
    // below -- see that route's own doc comment for why this is real
    // Rust, not `.nir` source, and so isn't bound by `db_query`'s 2..=10
    // arity cap (which only constrains `.nir`-authored calls). `None`
    // (the default) leaves every table exactly as it always rendered:
    // one unpaginated fetch, no new route reachable at all.
    // `docs/KUBERNETES.md`'s "State, data & horizontal scaling" row: unlike
    // `--transact-log`/`--workflow-log` (`durability::LogTarget`) and
    // `db_connect(...)` (`dbconn::DbConn`), this route talks to SQLite
    // directly (`rusqlite::Connection::open`, right below) and has no
    // Postgres branch at all -- a real gap in this codebase, not a
    // protobox config gap (see that doc's own writeup). Rather than let a
    // `postgres://...` value silently fall through to
    // `rusqlite::Connection::open` (which would try to create a garbage
    // local file literally named that URL and fail confusingly, or
    // succeed and silently serve an empty, wrong database), fail fast
    // here with a clear, actionable error -- the documented "explicitly
    // scope multi-replica deployments to not rely on this feature" half
    // of that row's two options, chosen over extending this route to
    // Postgres itself (a materially larger, separately-scoped rewrite:
    // `dispatch_table_query`/`load_role_mapping`/the view-gated
    // pass-through-on-omit step in `dispatch` all build raw SQL and read
    // back `rusqlite::types::Value` directly).
    if let Some(p) = &db_path {
        if p.starts_with("postgres://") || p.starts_with("postgresql://") {
            return Err(format!(
                "--db {p}: the generic /_nirdosha/table/<name> browser and role-mapping cache do not \
                 support Postgres yet (see docs/KUBERNETES.md's \"State, data & horizontal scaling\" row -- \
                 a real gap in this codebase, not a protobox config gap). Point --db at a local SQLite \
                 file instead, and do not rely on this feature for a >1-replica deployment; \
                 --transact-log/--workflow-log and a program's own db_connect(...) calls already \
                 support postgres:// for that."
            ));
        }
    }
    let table_db: Option<Arc<std::sync::Mutex<rusqlite::Connection>>> = match &db_path {
        Some(p) => match rusqlite::Connection::open(p) {
            Ok(conn) => Some(Arc::new(std::sync::Mutex::new(conn))),
            Err(e) => return Err(format!("--db {p}: failed to open: {e}")),
        },
        None => None,
    };
    let table_catalog = Arc::new(build_table_catalog(&program));
    // docs/ROADMAP.md A6's role-mapping cache — see `RoleMappingCache`'s own
    // doc comment. `None` without `--db` (the same all-or-nothing
    // posture every other `--db`-gated feature here already takes):
    // `identity_has_mapped_role` falls back to plain literal-match
    // `identity_has_role` behavior whenever this is `None`, so omitting
    // `--db` leaves role checks byte-for-byte what they always were.
    let role_mapping_cache: Option<Arc<std::sync::Mutex<RoleMappingCache>>> =
        table_db.as_ref().map(|_| Arc::new(std::sync::Mutex::new(RoleMappingCache::empty())));

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
                    log_lifecycle("info", &format!("migrate: applied {f}"));
                }
            }
            Err(e) => return Err(format!("schema migration failed: {e}")),
        }
    }

    // Eager first load of the role-mapping cache, right after migration
    // (so `role_mapping` is guaranteed to exist if `RoleMapping` was
    // declared) and before the server accepts any request — docs/ROADMAP.md
    // A6's own stated design ("load once into a long-lived structure at
    // `serve::run` startup, refreshed on a short TTL"). Without this, a
    // mapping already present in the DB before this process started
    // wouldn't take effect until the first TTL window elapsed, since
    // `RoleMappingCache::empty()`'s `loaded_at` is "just now," which
    // `refresh_role_mapping_if_stale` would otherwise read as "already
    // fresh, nothing to do."
    if let (Some(db), Some(cache)) = (&table_db, &role_mapping_cache) {
        let conn = db.lock().unwrap();
        let mut guard = cache.lock().unwrap();
        guard.by_app_role = load_role_mapping(&conn);
        guard.loaded_at = std::time::Instant::now();
    }

    // Crash replay (`docs/TRANSACT.md`'s Layer 4) once, before the server ever
    // accepts a request -- not per-request (unlike `dispatch`'s own fresh
    // `Interpreter`, one per request for isolation): replaying the same
    // durable log twice per request would be wasted work, and the whole
    // point is to resolve whatever crashed *before* new traffic arrives.
    {
        let replay_interp = Interpreter::new(Arc::clone(&program), Arc::from(""))
            .with_transact_log_path((*transact_log_path).clone())
            .with_workflow_log_path((*workflow_log_path).clone())
            .with_plugins(plugins);
        match replay_interp.replay_pending_transactions() {
            Ok(outcomes) => {
                for o in &outcomes {
                    log_lifecycle("info", &format!("transact replay: {o:?}"));
                }
            }
            Err(e) => return Err(format!("transact durability log error during startup crash replay: {e}")),
        }
        // `docs/WORKFLOW.md`'s own crash-replay pass, same "once, before the
        // server accepts a request" timing as `transact`'s own replay
        // just above.
        match replay_interp.replay_pending_workflow_actions() {
            Ok(outcomes) => {
                for o in &outcomes {
                    log_lifecycle("info", &format!("workflow replay: {o:?}"));
                }
            }
            Err(e) => return Err(format!("workflow log error during startup crash replay: {e}")),
        }
    }

    let server = tiny_http::Server::http((host, port)).map_err(|e| format!("failed to bind {host}:{port}: {e}"))?;
    log_lifecycle("info", &format!("listening on http://{host}:{port}  (GET / for the UI, POST /api/<fn> for the API)"));

    // `docs/KUBERNETES.md`'s "Health & lifecycle" row: stop accepting new
    // connections and exit promptly on SIGTERM/SIGINT instead of forcing
    // the orchestrator to wait out a full `terminationGracePeriodSeconds`
    // and SIGKILL every rolling restart. Checked once at the top of every
    // loop iteration, BEFORE the next `recv_timeout` call -- this server
    // handles exactly one request at a time (module doc above), so "drain
    // in-flight requests" here just means "finish whatever request is
    // already inside this loop body," which happens for free by not
    // interrupting it; the check below only ever skips picking up a *new*
    // one once the flag is set.
    let shutdown = install_shutdown_signal_handlers()?;
    let metrics = Metrics::new();
    // Bounded poll interval so the loop notices `shutdown` promptly even
    // with no traffic arriving -- `recv_timeout` (unlike
    // `incoming_requests()`/`recv()`) returns `Ok(None)` on a plain
    // timeout instead of blocking forever, which is exactly the seam this
    // needs and the only reason the loop below no longer uses
    // `server.incoming_requests()`.
    const SHUTDOWN_POLL_INTERVAL: Duration = Duration::from_millis(200);

    loop {
        if shutdown.load(Ordering::Relaxed) {
            log_lifecycle("info", "shutdown signal received, no request in flight -- exiting");
            break;
        }
        let mut request = match server.recv_timeout(SHUTDOWN_POLL_INTERVAL) {
            Ok(Some(r)) => r,
            Ok(None) => continue, // no request within this poll window -- loop back to the shutdown check
            Err(e) => {
                log_lifecycle("error", &format!("error receiving request: {e}"));
                continue;
            }
        };
        let request_started_at = Instant::now();
        let method = request.method().clone();
        let url = request.url().to_string();

        if method == tiny_http::Method::Get && url == "/healthz" {
            // Liveness: answers the instant the listener is bound, no
            // dependency check -- `docs/KUBERNETES.md`'s own stated contract
            // ("no dependency check needed for liveness specifically").
            // By the time this loop can even run, migration + crash
            // replay (both above, before `Server::http` binds at all)
            // have already succeeded, so there is nothing further to
            // verify here.
            send_response(request, &metrics, request_started_at, 200, Some("application/json"), r#"{"status":"ok"}"#.to_string(), vec![]);
            continue;
        }

        if method == tiny_http::Method::Get && url == "/readyz" {
            // Readiness (and, with a longer `failureThreshold`, a startup
            // probe -- `docs/KUBERNETES.md`'s own note that these can share a
            // route): unlike `/healthz`, this genuinely checks the one
            // thing that can go degraded *after* a successful startup --
            // the `--db`-backed table/role-cache connection, if one was
            // configured. No `--db` at all trivially passes (nothing to
            // check), matching the same all-or-nothing posture every
            // other `--db`-gated feature in this file already takes.
            let (ready, detail) = match &table_db {
                Some(db) => match db.lock() {
                    Ok(conn) => match conn.query_row("SELECT 1", [], |_| Ok(())) {
                        Ok(()) => (true, "db: ok".to_string()),
                        Err(e) => (false, format!("db: {e}")),
                    },
                    Err(_) => (false, "db: mutex poisoned".to_string()),
                },
                None => (true, "db: not configured".to_string()),
            };
            let status = if ready { 200 } else { 503 };
            let body = serde_json::json!({"status": if ready {"ready"} else {"not_ready"}, "detail": detail}).to_string();
            send_response(request, &metrics, request_started_at, status, Some("application/json"), body, vec![]);
            continue;
        }

        if method == tiny_http::Method::Get && url == "/metrics" {
            send_response(request, &metrics, request_started_at, 200, Some("text/plain; version=0.0.4"), metrics.render(), vec![]);
            continue;
        }

        if method == tiny_http::Method::Options {
            send_response(request, &metrics, request_started_at, 204, None, String::new(), vec![]);
            continue;
        }

        if method == tiny_http::Method::Get && url == "/" {
            let html = match (&theme_cache, theme_path) {
                (Some(cache), Some(path)) => {
                    refresh_theme_html_if_stale(cache, path, &program, &effects, identity_base, server_table_api, demo_mode, production_mode);
                    cache.lock().unwrap().html.clone()
                }
                _ => ui_html.clone(),
            };
            // `ETag`/`If-None-Match`: this HTML is static between
            // requests (byte-for-byte the same `ui_html`, or the same
            // `theme_cache` entry until its TTL next refreshes it above),
            // so a client re-requesting it -- a page refresh, or the same
            // tab reopened -- gets a bodyless `304` instead of
            // re-downloading the whole page. `Cache-Control: no-cache`
            // (not `max-age`) so the browser always revalidates rather
            // than risking a stale page after a `--theme` hot-reload or
            // server restart -- correctness first, the 304 round-trip
            // still saves the actual page weight either way.
            let etag = compute_etag(&html);
            let if_none_match = request
                .headers()
                .iter()
                .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("If-None-Match"))
                .map(|h| h.value.as_str().to_string());
            let cache_headers = vec![header("ETag", &etag), header("Cache-Control", "no-cache")];
            if if_none_match.as_deref() == Some(etag.as_str()) {
                send_response(request, &metrics, request_started_at, 304, None, String::new(), cache_headers);
            } else {
                send_response(request, &metrics, request_started_at, 200, Some("text/html; charset=utf-8"), html, cache_headers);
            }
            continue;
        }

        // `GET /api/_whoami` — always present, in every mode (demo,
        // production, or no identity configured at all): lets the SPA
        // confirm a `localStorage`-cached identity is still actually
        // valid *against this specific running server process*, before
        // trusting it to render nav/gated UI. This matters specifically
        // because of how demo mode works: each `nirdosha serve` process
        // mints its own fresh ephemeral signing key (`AuthConfig::
        // demo()`), so a token minted by a *previous* process (an
        // earlier run, before a restart) fails to verify here even
        // though the browser's `localStorage` still remembers it —
        // without this check, the page would otherwise render as
        // "signed in" from stale client-side state alone, with every
        // real gated action then failing with a confusing 401 the user
        // never asked for. Same `resolve_identity` verification every
        // other request already goes through, so this adds no new
        // trust path — it's a read of the existing one.
        if method == tiny_http::Method::Get && url == "/api/_whoami" {
            let auth_header = request
                .headers()
                .iter()
                .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("Authorization"))
                .map(|h| h.value.as_str().to_string());
            let (status, payload) = match resolve_identity(auth_header.as_deref(), auth.as_ref().as_ref(), &workflow_log_path) {
                Ok(Some(identity)) => match &identity {
                    Value::Struct(_, fields) => match (&fields[0], &fields[5]) {
                        (Value::Str(subject), Value::Str(claims_json)) => {
                            (200, format!("{{\"subject\":{},\"claims\":{}}}", serde_json::to_string(subject.as_ref()).unwrap(), claims_json))
                        }
                        _ => (500, json_err("malformed VerifiedIdentity")),
                    },
                    _ => (500, json_err("malformed VerifiedIdentity")),
                },
                Ok(None) => (401, json_err("not signed in")),
                Err(status_body) => status_body,
            };
            send_response(request, &metrics, request_started_at, status, Some("application/json"), payload, vec![]);
            continue;
        }

        if let Some(cfg) = sso.as_ref().as_ref() {
            if method == tiny_http::Method::Get && url == "/auth/login" {
                sweep_expired_pkce_entries(&pkce_store);
                let code_verifier = generate_pkce_code_verifier();
                let code_challenge = pkce_code_challenge(&code_verifier);
                let state = generate_pkce_code_verifier(); // same shape, different purpose
                pkce_store.lock().unwrap().insert(state.clone(), PkceEntry { code_verifier, created_at: Instant::now() });
                let redirect_url = format!(
                    "{}?client_id={}&redirect_uri={}&response_type=code&scope=openid&state={}&code_challenge={}&code_challenge_method=S256",
                    cfg.authorize_endpoint,
                    url_encode_component(&cfg.client_id),
                    url_encode_component(&cfg.redirect_uri),
                    url_encode_component(&state),
                    url_encode_component(&code_challenge),
                );
                send_response(request, &metrics, request_started_at, 302, None, String::new(), vec![header("Location", &redirect_url)]);
                continue;
            }
            if method == tiny_http::Method::Get && url.starts_with("/auth/callback") {
                let query = url.splitn(2, '?').nth(1).unwrap_or("");
                let params = parse_query_string(query);
                let (status, body_or_redirect) = handle_oidc_callback(&params, cfg, &pkce_store, auth.as_ref().as_ref());
                match body_or_redirect {
                    OidcCallbackOutcome::Redirect(location) => {
                        send_response(request, &metrics, request_started_at, status, None, String::new(), vec![header("Location", &location)])
                    }
                    OidcCallbackOutcome::Error(msg) => {
                        send_response(request, &metrics, request_started_at, status, Some("application/json"), json_err(&msg), vec![])
                    }
                };
                continue;
            }
        }

        if method == tiny_http::Method::Post {
            if let Some(table) = url.strip_prefix("/_nirdosha/table/") {
                let body = match read_limited_body(&mut request) {
                    Ok(b) => b,
                    Err(status_body) => {
                        let status = status_body.0;
                        send_response(request, &metrics, request_started_at, status, None, status_body.1, vec![]);
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
                    Some(db) => match resolve_identity(auth_header.as_deref(), auth.as_ref().as_ref(), &workflow_log_path) {
                        Ok(identity) => std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            dispatch_table_query(db, &table_catalog, table, &body, &program, identity.as_ref(), role_mapping_cache.as_deref())
                        }))
                        .unwrap_or_else(|_| (500, json_err("internal server error"))),
                        Err(status_body) => status_body,
                    },
                    None => (404, json_err("no --db was passed to `nirdosha serve`; the generic table-query route is disabled")),
                };
                send_response(request, &metrics, request_started_at, status, Some("application/json"), payload, vec![]);
                continue;
            }
            if demo_mode && url == "/api/_demo_login" {
                let body = match read_limited_body(&mut request) {
                    Ok(b) => b,
                    Err(status_body) => {
                        let status = status_body.0;
                        send_response(request, &metrics, request_started_at, status, None, status_body.1, vec![]);
                        continue;
                    }
                };
                let (status, payload) = handle_demo_login(&body, auth.as_ref().as_ref());
                send_response(request, &metrics, request_started_at, status, Some("application/json"), payload, vec![]);
                continue;
            }
            if url == "/api/_presence_connect" || url == "/api/_presence_disconnect" {
                let online = url == "/api/_presence_connect";
                let body = match read_limited_body(&mut request) {
                    Ok(b) => b,
                    Err(status_body) => {
                        let status = status_body.0;
                        send_response(request, &metrics, request_started_at, status, None, status_body.1, vec![]);
                        continue;
                    }
                };
                let auth_header = request
                    .headers()
                    .iter()
                    .find(|h| h.field.as_str().as_str().eq_ignore_ascii_case("Authorization"))
                    .map(|h| h.value.as_str().to_string());
                let (status, payload) =
                    handle_presence(&body, auth_header.as_deref(), presence_token.as_ref().as_ref(), &workflow_log_path, online);
                send_response(request, &metrics, request_started_at, status, Some("application/json"), payload, vec![]);
                continue;
            }
            if let Some(fn_name) = url.strip_prefix("/api/") {
                let body = match read_limited_body(&mut request) {
                    Ok(b) => b,
                    Err(status_body) => {
                        let status = status_body.0;
                        send_response(request, &metrics, request_started_at, status, None, status_body.1, vec![]);
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
                        &workflow_log_path,
                        table_db.as_deref(),
                        tracer.as_ref(),
                        role_mapping_cache.as_deref(),
                        plugins,
                    )
                })) {
                    Ok(result) => result,
                    Err(_) => (500, json_err(&format!("`{fn_name}` panicked while handling this request"))),
                };
                send_response(request, &metrics, request_started_at, status, Some("application/json"), payload, vec![]);
                continue;
            }
        }

        send_response(request, &metrics, request_started_at, 404, None, r#"{"err":"not found"}"#.to_string(), vec![]);
    }
    log_lifecycle("info", "server stopped accepting requests, exiting cleanly");
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
/// `docs/ROADMAP.md` A6's "identity admin console" — role mapping half. A
/// per-project, admin-editable `RoleMapping { app_role: str, idp_role:
/// str }` table (an ordinary struct, same "communication control"
/// free-CRUD-screen convention `EmailProviderConfig` established)
/// translates the app's canonical role vocabulary into whatever the
/// connected IdP actually puts in a token's `roles` claim — without
/// this, `requires(role: "compliance_officer")` and every `screen`
/// field's `view`/`edit` gate only ever matched if the `.nir` source's
/// string literal was byte-identical to the IdP's own role name, with
/// no translation layer and no error when it silently stopped matching
/// (a renamed IdP group, or two identical apps on two different IdPs
/// with different naming conventions).
///
/// In-memory, refreshed on a short TTL rather than re-queried on every
/// single auth check (this server's request loop is strictly
/// sequential — one process, no concurrent requests — so a plain
/// `Mutex` needs no contention story, just interior mutability for a
/// free function to update from a `&` reference). Bounded staleness (an
/// admin's edit takes up to one TTL window to take effect) is an
/// accepted, disclosed tradeoff — the same category of real-clock/
/// real-world exception `resolve_identity`'s own token-`expires_at`
/// check already is, not a new violation of `.nir`'s own determinism
/// story (this cache lives entirely in `serve.rs`, never touched by
/// interpreted `.nir` code itself).
struct RoleMappingCache {
    /// app_role -> every idp_role that maps to it (a role can have more
    /// than one IdP-side synonym, e.g. migrating naming conventions).
    by_app_role: std::collections::HashMap<String, Vec<String>>,
    loaded_at: std::time::Instant,
}

/// `RoleMappingCache::empty()`'s empty map is the correct "no mapping
/// configured" state, not a sentinel needing special-casing:
/// `identity_has_mapped_role` falls back to `identity_has_role`'s
/// literal-match behavior whenever the map has no entry for the app
/// role being checked, so a program that never declares `RoleMapping`
/// (or one whose table is just empty) behaves byte-for-byte like it
/// always did.
impl RoleMappingCache {
    fn empty() -> Self {
        Self { by_app_role: std::collections::HashMap::new(), loaded_at: std::time::Instant::now() }
    }
}

/// 30s in production. `tests/role_mapping.rs` (an integration test,
/// compiled as its own crate against a normally-built `nirdosha` rlib —
/// `#[cfg(test)]` items in this crate aren't visible to it at all, so
/// that's not an available seam) overrides this via
/// `NIRDOSHA_TEST_ROLE_MAPPING_TTL_MS` to prove the actual TTL boundary
/// with a real, just much shorter, wait instead of eating a 30-second
/// tax on every test run or faking the clock. Same opt-in-override
/// pattern `NIRDOSHA_TEST_POSTGRES_URL` already established for this
/// codebase's other environment-dependent-by-nature integration tests.
fn role_mapping_ttl() -> std::time::Duration {
    if let Some(ms) = std::env::var("NIRDOSHA_TEST_ROLE_MAPPING_TTL_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
        return std::time::Duration::from_millis(ms);
    }
    std::time::Duration::from_secs(30)
}

/// Reads every `(app_role, idp_role)` row of `role_mapping`, if the
/// table exists at all — a program that never declared a `RoleMapping`
/// struct has no such table, and `migrate.rs` only ever creates tables
/// for structs the program actually declares, so a missing-table query
/// error here is the normal "not opted into this feature" case, not a
/// fault; tolerated the same way `check_edit_gates`/`send_via_channel`
/// already tolerate "no such row/table yet" elsewhere in this codebase.
fn load_role_mapping(conn: &rusqlite::Connection) -> std::collections::HashMap<String, Vec<String>> {
    let mut out: std::collections::HashMap<String, Vec<String>> = std::collections::HashMap::new();
    if let Ok(mut stmt) = conn.prepare("SELECT app_role, idp_role FROM role_mapping") {
        if let Ok(rows) = stmt.query_map([], |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))) {
            for (app_role, idp_role) in rows.flatten() {
                out.entry(app_role).or_default().push(idp_role);
            }
        }
    }
    out
}

/// Reloads `cache` from `db` only if the TTL has elapsed — called once
/// per request (cheap: just an `Instant::elapsed` check under the lock
/// in the common case), so an admin's edit through the `RoleMapping`
/// CRUD screen is picked up within one TTL window without a DB round
/// trip on every single request.
fn refresh_role_mapping_if_stale(cache: &std::sync::Mutex<RoleMappingCache>, db: &std::sync::Mutex<rusqlite::Connection>) {
    let mut guard = cache.lock().unwrap();
    if guard.loaded_at.elapsed() < role_mapping_ttl() {
        return;
    }
    let conn = db.lock().unwrap();
    guard.by_app_role = load_role_mapping(&conn);
    guard.loaded_at = std::time::Instant::now();
}

/// The actual translated role check every `requires(role: ...)`/`view`/
/// `edit` enforcement point below now goes through, in place of a bare
/// `interpreter::identity_has_role` call: `app_role` matches if the
/// identity's raw token roles contain `app_role` *literally* (today's
/// pre-mapping behavior, always checked first so "no mapping
/// configured" is exactly as fast and exactly as correct as before this
/// feature existed) OR contain any `idp_role` the cache maps to
/// `app_role`. `cache: None` (no `--db`, the same all-or-nothing
/// posture every other `--db`-gated feature in this file already
/// takes) skips the second half entirely — literal match only, byte-
/// for-byte the original behavior.
fn identity_has_mapped_role(identity: &Value, app_role: &str, cache: Option<&std::sync::Mutex<RoleMappingCache>>) -> bool {
    if interpreter::identity_has_role(identity, app_role).unwrap_or(false) {
        return true;
    }
    let Some(cache) = cache else { return false };
    let guard = cache.lock().unwrap();
    let Some(synonyms) = guard.by_app_role.get(app_role) else { return false };
    synonyms.iter().any(|idp_role| interpreter::identity_has_role(identity, idp_role).unwrap_or(false))
}

/// Live theme reload's cached, ready-to-serve page — see `run`'s own
/// `theme_cache` doc comment for the "why."
struct ThemeCache {
    html: String,
    loaded_at: std::time::Instant,
}

/// 30s in production; overridable via `NIRDOSHA_TEST_THEME_TTL_MS` for
/// `tests/theme_reload.rs`, same reasoning and same env-var-seam pattern
/// as `role_mapping_ttl` above (a `#[cfg(test)]` item in this crate
/// isn't visible to an integration-test crate at all).
fn theme_ttl() -> std::time::Duration {
    if let Some(ms) = std::env::var("NIRDOSHA_TEST_THEME_TTL_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
        return std::time::Duration::from_millis(ms);
    }
    std::time::Duration::from_secs(30)
}

/// Production mode's PKCE `state` → `code_verifier` correlation,
/// generated and held entirely server-side (`GET /auth/login`,
/// consumed once by `GET /auth/callback`) — the callback is a plain
/// top-level `GET` from the IdP's own redirect, not an XHR the SPA
/// controls, so the verifier can't be round-tripped through the
/// browser the way a same-origin fetch could; keeping it server-side
/// for its whole lifetime is also strictly more secure than a
/// cookie-based round-trip (never touches the browser at all).
struct PkceEntry {
    code_verifier: String,
    created_at: std::time::Instant,
}

/// Keyed by `state`. One-shot: `/auth/callback` removes an entry the
/// moment it's looked up (`HashMap::remove`), so replaying the same
/// `state` after first use always fails, same as it should for a
/// value that's supposed to prove "this is the one browser session
/// that started this exact login."
type PkceStore = std::sync::Mutex<std::collections::HashMap<String, PkceEntry>>;

/// 5 minutes — generous for an interactive login redirect round-trip,
/// short enough that an abandoned login doesn't linger meaningfully.
/// Overridable via `NIRDOSHA_TEST_PKCE_TTL_MS`, same env-var-seam
/// pattern as `theme_ttl` above — a `#[cfg(test)]` item in this crate
/// isn't visible to an integration-test crate at all, so expiry has to
/// be testable from outside without waiting 5 real minutes.
fn pkce_ttl() -> std::time::Duration {
    if let Some(ms) = std::env::var("NIRDOSHA_TEST_PKCE_TTL_MS").ok().and_then(|s| s.parse::<u64>().ok()) {
        return std::time::Duration::from_millis(ms);
    }
    std::time::Duration::from_secs(300)
}

/// Opportunistic cleanup, called from `GET /auth/login` (every new
/// login attempt is a natural, cheap point to sweep) — no background
/// timer, same "cheap, on-demand" posture `ThemeCache`/
/// `RoleMappingCache` already take for their own staleness checks.
fn sweep_expired_pkce_entries(store: &PkceStore) {
    let mut guard = store.lock().unwrap();
    guard.retain(|_, entry| entry.created_at.elapsed() < pkce_ttl());
}

/// A fresh, high-entropy, URL-safe random string — used both for PKCE's
/// `code_verifier` (RFC 7636 wants 43-128 chars from the unreserved URL
/// set; 32 random bytes base64url-encoded is 43 chars, comfortably
/// inside that) and, reused unchanged, for the `state` value (same
/// "random, unguessable, URL-safe" requirement, different purpose — no
/// need for a second generator). Same `/dev/urandom` pattern as
/// `AuthConfig::demo()`'s own ephemeral secret.
fn generate_pkce_code_verifier() -> String {
    let mut buf = [0u8; 32];
    std::fs::File::open("/dev/urandom")
        .and_then(|mut f| std::io::Read::read_exact(&mut f, &mut buf))
        .expect("OS entropy source for a PKCE code_verifier/state value");
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(buf)
}

/// RFC 7636 §4.2: `code_challenge = BASE64URL-ENCODE(SHA256(ASCII(code_verifier)))`.
fn pkce_code_challenge(code_verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(code_verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// Percent-encodes one query-string component (RFC 3986's unreserved
/// set — `A-Za-z0-9-_.~` — passes through unchanged, everything else
/// becomes `%XX`). No dependency needed for this — the alphabet is
/// small and fixed.
fn url_encode_component(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => out.push(b as char),
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// The inverse of `url_encode_component`, applied to one query-string
/// component — also decodes `+` as a space, the traditional
/// `application/x-www-form-urlencoded` convention query strings also
/// follow. A malformed `%` escape (not followed by two hex digits) is
/// passed through literally rather than erroring — this is parsing an
/// IdP's own redirect back to us, not something to hard-fail a whole
/// login attempt over one stray `%`.
fn url_decode_component(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'%' if i + 2 < bytes.len() => match u8::from_str_radix(std::str::from_utf8(&bytes[i + 1..i + 3]).unwrap_or(""), 16) {
                Ok(byte) => {
                    out.push(byte);
                    i += 3;
                }
                Err(_) => {
                    out.push(bytes[i]);
                    i += 1;
                }
            },
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b => {
                out.push(b);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// `?code=...&state=...` (or any other query string) → a plain
/// key/value map, percent-decoded. No existing query-string parser in
/// this file — every other route's params come from a JSON body, not a
/// URL — this is new territory specifically for the IdP's redirect back.
fn parse_query_string(qs: &str) -> std::collections::HashMap<String, String> {
    qs.split('&')
        .filter(|p| !p.is_empty())
        .filter_map(|pair| {
            let mut it = pair.splitn(2, '=');
            let k = it.next()?;
            let v = it.next().unwrap_or("");
            Some((url_decode_component(k), url_decode_component(v)))
        })
        .collect()
}

/// `GET /auth/callback`'s two possible outcomes: redirect the browser
/// back into the SPA with a token (success), or hand back a plain JSON
/// error (the PKCE `state` was missing/expired/already used, or the
/// token exchange/verification itself failed) — mirrors the "plain data
/// in, `(status, body)` out" factoring every other route handler here
/// already uses, just with two different response shapes depending on
/// which happened.
enum OidcCallbackOutcome {
    Redirect(String),
    Error(String),
}

/// The production-mode token exchange: consume the one-shot PKCE entry,
/// POST the authorization code to the IdP's token endpoint, verify the
/// resulting `id_token` through the exact same `validate_oidc_token`
/// every other request already goes through (no second trust path), and
/// hand it back to the SPA via a `#` fragment redirect (never sent to
/// the server on the browser's own follow-up navigation, unlike a
/// `?query` parameter — still visible in local browser history, a
/// disclosed, minor residual exposure noted in the implementation plan;
/// avoiding that entirely would need cookie/`postMessage`-based
/// delivery, a bigger change than this feature's first landing).
fn handle_oidc_callback(
    params: &std::collections::HashMap<String, String>,
    cfg: &OidcSsoConfig,
    pkce_store: &PkceStore,
    auth: Option<&AuthConfig>,
) -> (u16, OidcCallbackOutcome) {
    let Some(code) = params.get("code") else {
        return (400, OidcCallbackOutcome::Error("missing `code` query parameter".to_string()));
    };
    let Some(state) = params.get("state") else {
        return (400, OidcCallbackOutcome::Error("missing `state` query parameter".to_string()));
    };
    let code_verifier = {
        let mut guard = pkce_store.lock().unwrap();
        match guard.remove(state) {
            Some(entry) if entry.created_at.elapsed() < pkce_ttl() => entry.code_verifier,
            Some(_) => return (400, OidcCallbackOutcome::Error("login attempt expired — please sign in again".to_string())),
            None => return (400, OidcCallbackOutcome::Error("unknown or already-used `state` — please sign in again".to_string())),
        }
    };
    let Some(cfg_auth) = auth else {
        return (500, OidcCallbackOutcome::Error("this server has no --jwks-file/--issuer/--audience configured".to_string()));
    };

    let mut form = format!(
        "grant_type=authorization_code&code={}&redirect_uri={}&client_id={}&code_verifier={}",
        url_encode_component(code),
        url_encode_component(&cfg.redirect_uri),
        url_encode_component(&cfg.client_id),
        url_encode_component(&code_verifier),
    );
    if let Some(secret) = &cfg.client_secret {
        form.push_str(&format!("&client_secret={}", url_encode_component(secret)));
    }

    let Some((token_host, token_port, token_path, token_https)) = parse_endpoint_url(&cfg.token_endpoint) else {
        return (500, OidcCallbackOutcome::Error(format!("--oidc-token-endpoint is not a valid http(s):// URL: {}", cfg.token_endpoint)));
    };
    // `https://` in any real deployment (this carries the client secret
    // and authorization code) -- but not hard-enforced here, the same
    // way `--identity-base`/`--oidc-authorize-endpoint` aren't either:
    // an internal service behind a TLS-terminating proxy, or (this
    // repo's own test suite) a loopback-only mock IdP double, are both
    // normal `http://` shapes this shouldn't refuse to talk to. That
    // judgment call belongs to whoever supplies `--oidc-token-endpoint`.
    let exchange = if token_https {
        interpreter::https_post_with_content_type(&token_host, token_port, &token_path, "application/x-www-form-urlencoded", &form)
    } else {
        interpreter::http_post_with_content_type(&token_host, token_port, &token_path, "application/x-www-form-urlencoded", &form)
    };
    let (status, body) = match exchange {
        Ok(r) => r,
        Err(e) => return (502, OidcCallbackOutcome::Error(format!("token exchange request failed: {e}"))),
    };
    if !(200..300).contains(&status) {
        return (502, OidcCallbackOutcome::Error(format!("token endpoint returned {status}: {body}")));
    }
    let parsed: JsonVal = match serde_json::from_str(&body) {
        Ok(v) => v,
        Err(e) => return (502, OidcCallbackOutcome::Error(format!("token endpoint returned malformed JSON: {e}"))),
    };
    let Some(id_token) = parsed.get("id_token").and_then(JsonVal::as_str) else {
        return (502, OidcCallbackOutcome::Error("token endpoint response has no `id_token`".to_string()));
    };
    if let Err(e) = interpreter::validate_oidc_token(id_token, &cfg_auth.issuer, &cfg_auth.audience, &cfg_auth.jwks_json) {
        return (401, OidcCallbackOutcome::Error(format!("invalid id_token from identity provider: {e}")));
    }
    (302, OidcCallbackOutcome::Redirect(format!("/#/auth/callback?token={}", url_encode_component(id_token))))
}

/// `--oidc-authorize-endpoint`/`--oidc-token-endpoint` are given as full
/// `http(s)://host[:port]/path` URLs (unlike `--identity-base`'s bare
/// base URL, which `ui_gen_template.html` only ever appends a fixed
/// `/api/login` suffix to) — this splits one apart into the pieces
/// `interpreter::https_post_with_content_type` needs. No `url` crate
/// dependency for this: the shape accepted is deliberately narrow
/// (scheme, host, optional port, path), not general URL parsing.
fn parse_endpoint_url(url: &str) -> Option<(String, i64, String, bool)> {
    let (https, rest) = if let Some(r) = url.strip_prefix("https://") {
        (true, r)
    } else if let Some(r) = url.strip_prefix("http://") {
        (false, r)
    } else {
        return None;
    };
    let (authority, path) = match rest.find('/') {
        Some(i) => (&rest[..i], &rest[i..]),
        None => (rest, "/"),
    };
    let (host, port) = match authority.rsplit_once(':') {
        Some((h, p)) => (h.to_string(), p.parse::<i64>().ok()?),
        None => (authority.to_string(), if https { 443 } else { 80 }),
    };
    if host.is_empty() {
        return None;
    }
    Some((host, port, path.to_string(), https))
}

/// Reloads `theme_path` from disk and regenerates `cache`'s HTML only if
/// the TTL has elapsed — called on every `GET /` (cheap: just an
/// `Instant::elapsed` check under the lock in the common case). A
/// missing file, an I/O error, or a `theme.json` that doesn't parse
/// (e.g. an editor's half-written save caught mid-write) is tolerated —
/// logged to stderr, cache left exactly as it was — rather than ever
/// serving a broken page or crashing the server over one bad edit; the
/// next TTL window tries again.
fn refresh_theme_html_if_stale(
    cache: &std::sync::Mutex<ThemeCache>,
    theme_path: &str,
    program: &Program,
    effects: &std::collections::HashMap<String, crate::effects::FnEffects>,
    identity_base: Option<&str>,
    server_table_api: bool,
    demo_mode: bool,
    production_mode: bool,
) {
    let mut guard = cache.lock().unwrap();
    if guard.loaded_at.elapsed() < theme_ttl() {
        return;
    }
    guard.loaded_at = std::time::Instant::now();
    let text = match std::fs::read_to_string(theme_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nirdosha serve: live theme reload: error reading {theme_path}: {e} (keeping previous theme)");
            return;
        }
    };
    let theme: crate::ui_gen::Theme = match serde_json::from_str(&text) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("nirdosha serve: live theme reload: error parsing {theme_path}: {e} (keeping previous theme)");
            return;
        }
    };
    guard.html = crate::ui_gen::generate(program, effects, identity_base, server_table_api, demo_mode, production_mode, Some(&theme));
}

fn build_table_catalog(program: &Program) -> std::collections::HashMap<String, Vec<String>> {
    const PRELUDE_STRUCT_NAMES: &[&str] =
        &["HttpResponse", "VerifiedIdentity", "RoleView", "ClaimView", "ApplicationSession", "RefreshTokenHandle", "Pair", "Money", "Measure"];
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
    role_mapping: Option<&std::sync::Mutex<RoleMappingCache>>,
) -> (u16, String) {
    if let Some(cache) = role_mapping {
        refresh_role_mapping_if_stale(cache, db);
    }
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
        redact_gated_fields(row, identity, &view_gates, role_mapping);
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
fn resolve_identity(
    auth_header: Option<&str>,
    auth: Option<&AuthConfig>,
    workflow_log_path: &crate::durability::LogTarget,
) -> Result<Option<Value>, (u16, String)> {
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
                        _ => {
                            upsert_identity_directory(&v, now, workflow_log_path);
                            Ok(Some(v))
                        }
                    }
                }
                Err(e) => Err((401, json_err(&format!("invalid token: {e}")))),
            },
            None => Err((401, json_err("Authorization header must be `Bearer <token>`"))),
        },
        // Unreachable via `nirdosha serve` today: `main.rs::cmd_serve`
        // always passes `Some(AuthConfig::demo())` when no real identity
        // flags are given (demo mode), so `auth` is never `None` here
        // for any CLI-started server -- a bearer-carrying request used
        // to 500 unconditionally in that case, even for an ungated
        // function, before demo mode existed. Left in place as harmless
        // defensive code for any future non-CLI caller of `serve::run`
        // that does pass `auth: None`.
        (Some(_), None) => {
            Err((500, json_err("this server has no --jwks-file/--issuer/--audience configured to validate a bearer token against")))
        }
        (None, _) => Ok(None),
    }
}

/// `docs/WORKFLOW.md`'s `identity_directory` — the piece that makes
/// `Recipient::ByRole` resolvable, upserted on every successful auth
/// (this is the only writer). Best-effort: a directory-write failure
/// (disk full, a locked file) must never turn a successful auth into a
/// failed request — `notify`/`send_email`'s own `ByRole` lookup, not this
/// call, is where that kind of failure should surface, if it ever
/// matters to a particular request at all.
fn upsert_identity_directory(identity: &Value, now: i64, workflow_log_path: &crate::durability::LogTarget) {
    let Value::Struct(_, fields) = identity else { return };
    let (Value::Str(subject), Value::Str(claims_json)) = (&fields[0], &fields[5]) else { return };
    match crate::workflow_log::WorkflowLog::open(workflow_log_path) {
        Ok(wlog) => {
            if let Err(e) = wlog.upsert_identity(subject, claims_json, now) {
                eprintln!("identity_directory upsert failed for `{subject}`: {e}");
            }
        }
        Err(e) => eprintln!("identity_directory: failed to open workflow log: {e}"),
    }
}

/// `POST /api/_demo_login` — demo mode only (`nirdosha serve` with no
/// real `--jwks-file`/`--issuer`/`--audience`, `main.rs::cmd_serve`'s
/// `(None,None,None)` arm). Body: `{"subject": "...", "roles": [...],
/// "claims": {...}}` (all optional — default subject `"demo"`, empty
/// roles/claims). Mints a real token, signed by this process's own
/// `AuthConfig::demo()` ephemeral secret, for exactly the self-declared
/// identity — the frontend then attaches it as an ordinary `Authorization:
/// Bearer <token>` on every later call, and it's re-verified through the
/// exact same `resolve_identity`/`validate_oidc_token` path a real IdP's
/// token goes through. This is what makes demo mode *functional*
/// (a self-picked role can actually satisfy `requires(role: ...)`) rather
/// than the old client-only stub that never sent anything to the server
/// at all. `auth` is `None` here only if `demo_mode` was somehow true
/// without `AuthConfig::demo()` having been constructed — not reachable
/// via `main.rs`, but handled cleanly rather than assumed away.
fn handle_demo_login(body: &str, auth: Option<&AuthConfig>) -> (u16, String) {
    let Some(cfg) = auth else { return (500, json_err("demo login is not available on this server")) };
    let req: JsonVal = if body.trim().is_empty() { JsonVal::Object(Default::default()) } else {
        match serde_json::from_str(body) {
            Ok(v) => v,
            Err(e) => return (400, json_err(&format!("invalid JSON body: {e}"))),
        }
    };
    let subject = req.get("subject").and_then(JsonVal::as_str).unwrap_or("demo").to_string();
    let roles: Vec<String> =
        req.get("roles").and_then(JsonVal::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_string)).collect()).unwrap_or_default();
    let claim_pairs: Vec<(String, String)> = req
        .get("claims")
        .and_then(JsonVal::as_object)
        .map(|m| m.iter().filter_map(|(k, v)| v.as_str().map(|s| (k.clone(), s.to_string()))).collect())
        .unwrap_or_default();

    // `identity_has_role`/`identity_claim` (interpreter.rs) read a
    // top-level `"roles"` array and flat top-level claim keys out of
    // `VerifiedIdentity.claims_json` -- build exactly that shape, since
    // `mock_issue_token` merges this object straight into the token's
    // payload unchanged.
    let mut claims_obj = serde_json::Map::new();
    claims_obj.insert("roles".to_string(), serde_json::json!(roles));
    for (k, v) in &claim_pairs {
        claims_obj.insert(k.clone(), serde_json::json!(v));
    }
    let claims_json = serde_json::to_string(&JsonVal::Object(claims_obj)).expect("built from plain strings, always serializes");

    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    match interpreter::mock_issue_token(&subject, &cfg.issuer, &cfg.audience, now, 3600, &claims_json, &cfg.jwks_json) {
        Ok(token) => (200, serde_json::json!({"token": token}).to_string()),
        Err(e) => (500, json_err(&format!("failed to mint demo token: {e}"))),
    }
}

/// `POST /api/_presence_connect` / `_disconnect` — `docs/WORKFLOW.md`'s
/// presence bridge: a trusted external WS gateway (not an end user)
/// reports "this subject just connected/disconnected its live browser
/// session," authenticated with `--presence-token` (a service credential,
/// compared constant-time — same discipline `trade_finance.nir:676`'s
/// magic-link token compare already established) rather than a normal
/// `Authorization: Bearer <identity-token>`. No `--presence-token`
/// configured means these routes 404 outright — `notify()` still works,
/// it just always takes the offline path (`docs/TRANSACT.md`'s own "a program
/// that never opts in pays nothing" framing, reused here).
fn handle_presence(
    body: &str,
    auth_header: Option<&str>,
    presence_token: Option<&String>,
    workflow_log_path: &crate::durability::LogTarget,
    online: bool,
) -> (u16, String) {
    let Some(expected) = presence_token else {
        return (404, json_err("presence routes are disabled: no --presence-token was passed to `nirdosha serve`"));
    };
    let presented = auth_header.and_then(|h| h.strip_prefix("Bearer ").or_else(|| h.strip_prefix("bearer ")));
    let Some(presented) = presented else {
        return (401, json_err("Authorization header must be `Bearer <presence-token>`"));
    };
    if !interpreter::constant_time_eq(presented, expected) {
        return (401, json_err("invalid presence token"));
    }
    let req: JsonVal = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => return (400, json_err(&format!("invalid JSON body: {e}"))),
    };
    let Some(subject) = req.get("subject").and_then(JsonVal::as_str) else {
        return (400, json_err("missing `subject` field"));
    };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs() as i64).unwrap_or(0);
    match crate::workflow_log::WorkflowLog::open(workflow_log_path) {
        Ok(wlog) => match wlog.set_presence(subject, online, now) {
            Ok(()) => (200, r#"{"ok":true}"#.to_string()),
            Err(e) => (500, json_err(&format!("failed to update presence: {e}"))),
        },
        Err(e) => (500, json_err(&format!("failed to open workflow log: {e}"))),
    }
}

/// Whether `identity` satisfies one field's `view`/`edit` gate — any one
/// of `roles` (array-membership, `identity_has_role`, same helper
/// `requires(role: ...)` enforcement already uses) or the `claim`
/// key/value, if either was declared; ungated (`roles` empty and `claim`
/// `None`) always satisfies. No identity at all only satisfies an
/// ungated field.
fn identity_satisfies_gate(
    identity: Option<&Value>,
    roles: &[String],
    claim: &Option<(String, String)>,
    role_mapping: Option<&std::sync::Mutex<RoleMappingCache>>,
) -> bool {
    if roles.is_empty() && claim.is_none() {
        return true;
    }
    let Some(id) = identity else { return false };
    if roles.iter().any(|r| identity_has_mapped_role(id, r, role_mapping)) {
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
fn redact_gated_fields(
    json: &mut JsonVal,
    identity: Option<&Value>,
    gates: &[crate::ui_gen::GatedField],
    role_mapping: Option<&std::sync::Mutex<RoleMappingCache>>,
) {
    if gates.is_empty() {
        return;
    }
    match json {
        JsonVal::Array(items) => {
            for item in items {
                redact_gated_fields(item, identity, gates, role_mapping);
            }
        }
        JsonVal::Object(m) => {
            if let Some(inner) = m.get_mut("ok") {
                redact_gated_fields(inner, identity, gates, role_mapping);
                return;
            }
            if m.contains_key("err") {
                return;
            }
            for g in gates {
                if g.view_roles.is_empty() && g.view_claim.is_none() {
                    continue;
                }
                if identity_satisfies_gate(identity, &g.view_roles, &g.view_claim, role_mapping) {
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
    transact_log_path: &crate::durability::LogTarget,
    workflow_log_path: &crate::durability::LogTarget,
    table_db: Option<&std::sync::Mutex<rusqlite::Connection>>,
    tracer: Option<&Arc<crate::observability::Tracer>>,
    role_mapping: Option<&std::sync::Mutex<RoleMappingCache>>,
    plugins: &[crate::plugin::PluginBuiltin],
) -> (u16, String) {
    let Some(f) = program.fns.iter().find(|f| f.name == fn_name) else {
        return (404, json_err(&format!("no such function `{fn_name}`")));
    };
    if let (Some(db), Some(cache)) = (table_db, role_mapping) {
        refresh_role_mapping_if_stale(cache, db);
    }

    // Bearer token -> VerifiedIdentity, if present and valid.
    let identity: Option<Value> = match resolve_identity(auth_header, auth, workflow_log_path) {
        Ok(id) => id,
        Err(status_body) => return status_body,
    };

    // `requires(role/claim: ...)` is enforced here, independently of
    // `f`'s own parameter list — see this module's doc comment for why
    // `call_named` alone can't be trusted to do this. `Requirement::Role`
    // goes through `identity_has_mapped_role`, not a bare
    // `identity_has_role`, so a `RoleMapping`-configured project
    // translates the IdP's own role name transparently here too, not
    // just on `screen` field gates.
    if let Some(req) = &f.requires {
        match &identity {
            None => return (401, json_err(&format!("sign in required: {}", describe_requirement(req)))),
            Some(id) => {
                let ok = match req {
                    Requirement::Role(role) => identity_has_mapped_role(id, role, role_mapping),
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
                                if identity_satisfies_gate(identity.as_ref(), &g.view_roles, &g.view_claim, role_mapping) {
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
        if is_optional_verified_identity(&p.ty) {
            args.push(match &identity {
                Some(id) => Value::Enum(Arc::from("Option"), Arc::from("Some"), Arc::from(vec![id.clone()])),
                None => Value::Enum(Arc::from("Option"), Arc::from("None"), Arc::from(vec![])),
            });
            continue;
        }
        let Some(v) = body_obj.get(&p.name) else {
            return (400, json_err(&format!("missing field `{}` in request body", p.name)));
        };
        match decode_value_root(v, &p.ty, program) {
            Ok(val) => args.push(val),
            Err(e) => return (400, json_err(&format!("field `{}`: {e}", p.name))),
        }
    }

    // Field-level format validation (`screen <Struct> { field <name> {
    // pattern/min/max: ... } }`) — unlike edit RBAC below, this needs no
    // `--db` (nothing to compare against, just the incoming value) and
    // applies to BOTH `create_<S>` and `update_<S>` (a brand-new row's
    // fields need to satisfy the same format constraints an edited row's
    // do). `typeck.rs::check_pattern_expr`/`check_min_max_expr` already
    // proved each `pattern` compiles as a regex and every constrained
    // field is a type it can actually apply to, so this only ever
    // rejects on the *value*, never on a malformed declaration.
    if let Some((struct_name, validations)) = crate::ui_gen::field_validations_for_fn(program, fn_name) {
        if let Some(status_body) = check_field_validations(program, f, &args, &struct_name, &validations) {
            return status_body;
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
        if let Some(status_body) = check_edit_gates(db, program, f, &args, &struct_name, &gates, identity.as_ref(), role_mapping) {
            return status_body;
        }
    }

    let program_arc = Arc::clone(program);
    let mut interp = Interpreter::new(program_arc, Arc::from(""))
        .with_transact_log_path(transact_log_path.clone())
        .with_workflow_log_path(workflow_log_path.clone())
        .with_plugins(plugins);
    if let Some(t) = tracer {
        interp = interp.with_tracer(Arc::clone(t));
    }
    match interp.call_named_on_big_stack(fn_name, &args) {
        Ok(result) => match encode_value(&result, program) {
            Ok(mut json) => {
                let view_gates = crate::ui_gen::field_gates_for_fn(program, fn_name);
                redact_gated_fields(&mut json, identity.as_ref(), &view_gates, role_mapping);
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
    role_mapping: Option<&std::sync::Mutex<RoleMappingCache>>,
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
        if identity_satisfies_gate(identity, &g.edit_roles, &g.edit_claim, role_mapping) {
            continue;
        }
        return Some((403, json_err(&format!("insufficient privilege: cannot change `{}.{}`", struct_name, g.field_name))));
    }
    None
}

/// The actual format-validation check `dispatch` runs before calling a
/// `create_<S>` or `update_<S>` fn: finds the struct-typed arg among
/// `args`, and for every `ValidatedField`, checks the incoming decoded
/// value against its `pattern`/`min`/`max`. Unlike `check_edit_gates`,
/// needs no `--db` connection at all — a format constraint is checked
/// purely against the submitted value, never against what's currently
/// stored. Returns `Some((400, ...))` for the first field that fails
/// its constraint; `None` means every constrained field passes (or
/// wasn't present as a real struct field/value pairing this can check
/// at all, treated as "nothing to enforce," the same tolerant posture
/// `check_edit_gates`/`value_matches_stored` already take for a
/// pairing they can't compare).
fn check_field_validations(
    program: &Program,
    f: &crate::ast::FnDecl,
    args: &[Value],
    struct_name: &str,
    validations: &[crate::ui_gen::ValidatedField],
) -> Option<(u16, String)> {
    let struct_arg = f
        .params
        .iter()
        .zip(args)
        .find_map(|(p, v)| if matches!(&p.ty, Ty::Named(n, a) if n == struct_name && a.is_empty()) { Some(v) } else { None })?;
    let Value::Struct(_, fields) = struct_arg else { return None };
    let decl = resolve_struct(program, struct_name)?;
    for v in validations {
        let Some(field_idx) = decl.fields.iter().position(|fd| fd.name == v.field_name) else { continue };
        let value = &fields[field_idx];
        if let Some(pattern) = &v.pattern {
            if let Value::Str(s) = value {
                // Already proven to compile by `typeck.rs::check_pattern_expr`
                // — a construction failure here would mean that guarantee
                // broke, not that this request is malformed, so it's
                // treated as "nothing to enforce" rather than a 500.
                if let Ok(re) = regex::Regex::new(pattern) {
                    if !re.is_match(s) {
                        return Some((
                            400,
                            json_err(&format!("field `{}.{}` does not match the required pattern", struct_name, v.field_name)),
                        ));
                    }
                }
            }
        }
        let numeric = match value {
            Value::Int(n) => Some(*n as f64),
            Value::Float(n) => Some(*n),
            _ => None,
        };
        if let (Some(n), Some(min)) = (numeric, v.min) {
            if n < min {
                return Some((400, json_err(&format!("field `{}.{}` must be >= {min}", struct_name, v.field_name))));
            }
        }
        if let (Some(n), Some(max)) = (numeric, v.max) {
            if n > max {
                return Some((400, json_err(&format!("field `{}.{}` must be <= {max}", struct_name, v.field_name))));
            }
        }
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

/// `Option(VerifiedIdentity)` — `docs/WORKFLOW.md`'s "who submitted this"
/// section: a fn param that wants to know the caller's identity *when
/// there is one*, without demanding a token the way a bare
/// `VerifiedIdentity` param does. `dispatch` injects `Some(id)` when a
/// valid bearer token was presented, `None` when absent or invalid —
/// never a 401 for this param alone (a *different* declared
/// `requires(...)`/bare `VerifiedIdentity` param on the same fn can
/// still demand one, unaffected by this).
fn is_optional_verified_identity(ty: &Ty) -> bool {
    matches!(ty, Ty::Named(n, args) if n == "Option" && args.len() == 1 && is_verified_identity(&args[0]))
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

/// Real recursion protection isn't the same question as "how deep is
/// this domain model" — `Order -> LineItem -> Product -> Category` is
/// four real, non-repeating levels a legitimate schema needs
/// (2026-08-27: the old flat `depth >= 2` cap rejected exactly this,
/// unconditionally, regardless of whether anything was actually
/// cyclic). What genuinely can't be allowed to recurse forever is a
/// **repeat** — the same struct name reappearing on the very path
/// that's expanding it (`struct A { b: B } struct B { a: A }` typechecks
/// today, `ast.rs`/`typeck.rs` have no cycle check on struct
/// declarations themselves, confirmed empirically: `emit-ast` accepts
/// that pair with no error). `DecodeGuard::enter` is that real check —
/// a name already on `visiting` means a cycle, a clear 400 instead of a
/// stack overflow. `MAX_DEPTH` is a second, independent, much more
/// generous backstop: JSON *input* depth is caller-controlled
/// regardless of whether the `.nir` program's own struct graph is
/// cyclic (an attacker can nest plain JSON objects to any depth without
/// needing a cyclic type at all), so recursion still needs a hard
/// ceiling even on a genuinely acyclic schema — just one sized for
/// "deeper than any real domain model", not "deeper than the *shallowest*
/// real one".
const MAX_DECODE_DEPTH: u32 = 64;

pub(crate) struct DecodeGuard<'a> {
    pub(crate) visiting: &'a mut Vec<String>,
    pub(crate) depth: u32,
}

impl<'a> DecodeGuard<'a> {
    /// `Err` on a real cycle (`name` already mid-expansion on this path)
    /// or on hitting `MAX_DECODE_DEPTH` — `Ok(())` pushes `name` onto
    /// `visiting`; the caller must pop it (`leave`) once done expanding,
    /// so sibling fields of the same struct type (not an ancestor) are
    /// never falsely flagged.
    fn enter(&mut self, name: &str) -> Result<(), String> {
        if self.depth >= MAX_DECODE_DEPTH {
            return Err(format!(
                "JSON request body nests {MAX_DECODE_DEPTH} levels deep at `{name}` -- refusing to decode further"
            ));
        }
        if self.visiting.iter().any(|v| v == name) {
            return Err(format!(
                "`{name}` is already being decoded higher up this same path -- cyclic/self-referential struct nesting isn't supported"
            ));
        }
        self.visiting.push(name.to_string());
        self.depth += 1;
        Ok(())
    }

    fn leave(&mut self) {
        self.visiting.pop();
        self.depth -= 1;
    }
}

/// Decodes one JSON value into a `Value`, driven by `ty` — the server-
/// side mirror of `ui_gen.rs::build_field`'s client-side control
/// derivation: scalars decode directly, `Option(T)` unwraps (`null` ->
/// `None`), a zero-payload enum reference decodes from its variant name
/// (`decode_enum_value` above), a bare reference to another struct in
/// this program expands (`DecodeGuard`'s doc comment above — real cycle
/// detection, not a flat depth cap), and anything else (affine handles,
/// `Fn`, a payload-carrying enum) is a clear 400, never a silent
/// misdecode.
/// `decode_value` for every caller that's decoding one independent
/// top-level value (a request parameter, a workflow `data` blob) rather
/// than recursing itself -- builds the one-shot `DecodeGuard` so call
/// sites don't each repeat that boilerplate.
pub(crate) fn decode_value_root(json: &JsonVal, ty: &Ty, program: &Program) -> Result<Value, String> {
    let mut visiting = Vec::new();
    let mut guard = DecodeGuard { visiting: &mut visiting, depth: 0 };
    decode_value(json, ty, program, &mut guard)
}

pub(crate) fn decode_value(json: &JsonVal, ty: &Ty, program: &Program, guard: &mut DecodeGuard) -> Result<Value, String> {
    match ty {
        Ty::Str => json.as_str().map(|s| Value::Str(Arc::from(s))).ok_or_else(|| "expected a string".to_string()),
        Ty::I8 | Ty::I16 | Ty::I32 | Ty::I64 | Ty::U8 | Ty::U16 | Ty::U32 | Ty::U64 | Ty::Usize => {
            json.as_i64().map(Value::Int).ok_or_else(|| "expected an integer".to_string())
        }
        Ty::F64 => json.as_f64().map(Value::Float).ok_or_else(|| "expected a number".to_string()),
        // A JSON **string**, not a JSON number — `docs/LANGUAGE.md` §5's
        // "Decimal arithmetic" section: a JSON number is IEEE-754 double
        // under nearly every consumer's parser, exactly the silent-
        // drift failure `dec128` exists to prevent. Mirrors
        // `encode_value`'s `Value::Dec128 => JsonVal::String(...)` arm.
        Ty::Dec128 => json
            .as_str()
            .ok_or_else(|| "expected a decimal string".to_string())
            .and_then(|s| Decimal::from_str(s).map(Value::Dec128).map_err(|e| format!("malformed dec128: {e}"))),
        Ty::Bool => json.as_bool().map(Value::Bool).ok_or_else(|| "expected a boolean".to_string()),
        // Nirdosha's own opaque JSON type -- passes through unchanged,
        // the mirror of `encode_value`'s own `Value::Json(doc) =>
        // Ok((**doc).clone())` arm. Found missing while wiring
        // `docs/WORKFLOW.md`'s "state ownership" section: every desugared
        // `advance_<workflow>`/`*_via_link` fn takes a trailing `payload:
        // json` param (reserved for a future increment, currently unused
        // by `on_entry`/`on_exit` bindings -- see that doc's "deliberate
        // non-goals" section), and with no arm here, calling any of them
        // through a real `nirdosha serve` request always 400'd on that
        // field before this fix, regardless of what the caller sent --
        // a pre-existing gap, not something specific to the ownership
        // feature, just never previously exercised end-to-end over HTTP.
        Ty::Json => Ok(Value::Json(Arc::new(json.clone()))),
        Ty::Named(n, args) if n == "Option" && args.len() == 1 => {
            if json.is_null() {
                Ok(Value::Enum(Arc::from("Option"), Arc::from("None"), Arc::from(vec![])))
            } else {
                let inner = decode_value(json, &args[0], program, guard)?;
                Ok(Value::Enum(Arc::from("Option"), Arc::from("Some"), Arc::from(vec![inner])))
            }
        }
        Ty::Named(n, args) if args.is_empty() => {
            if let Some(e) = resolve_enum(program, n) {
                return decode_enum_value(json, n, e);
            }
            match resolve_struct(program, n) {
                Some(s) => {
                    guard.enter(n)?;
                    let obj = json.as_object().ok_or_else(|| format!("expected an object for `{n}`"))?;
                    let mut fields = Vec::with_capacity(s.fields.len());
                    for field in &s.fields {
                        let fv = obj.get(&field.name).ok_or_else(|| format!("missing field `{}` for `{n}`", field.name))?;
                        fields.push(decode_value(fv, &field.ty, program, guard)?);
                    }
                    guard.leave();
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
pub(crate) fn encode_value(v: &Value, program: &Program) -> Result<JsonVal, String> {
    match v {
        Value::Int(n) => Ok(JsonVal::from(*n)),
        Value::Float(f) => Ok(if f.is_finite() { JsonVal::from(*f) } else { JsonVal::Null }),
        // A JSON string, not a JSON number — see `decode_value`'s
        // `Ty::Dec128` arm for why.
        Value::Dec128(d) => Ok(JsonVal::String(d.to_string())),
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
            // A zero-payload variant encodes as its bare name -- the
            // wire format `decode_value`/`decode_enum_value` already
            // *requires* on the way in (a JSON string naming the
            // variant), and the same one `sql_bind_params`/`migrate.rs`
            // already use for the DB round-trip. Found while returning a
            // `Money`/`Measure` value (whose `currency`/`unit_code`
            // field is exactly this shape) directly from a `.nir` fn:
            // this arm's own `_ => None` fallback below used to encode
            // *every* enum, zero-payload or not, as `{"variant":...,
            // "payload":[]}}` -- a real, previously-untested asymmetry
            // with what decode accepts back, not a deliberate format. A
            // payload-carrying variant keeps the tagged shape below;
            // there's no flat representation for it to fall back to.
            if payload.is_empty() {
                return Ok(JsonVal::String(variant.to_string()));
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
