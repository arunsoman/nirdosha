//! Minimal Rust client for [rqlite](https://rqlite.io)'s HTTP API
//! (`/db/execute`, `/db/query`) — `docs/ROADMAP.md`'s Phase 2 of the
//! `workflow`/`transact` multi-instance fix, the "real prior art" note
//! on that entry: rqlite already replicates SQLite correctly via real
//! Raft consensus (Ongaro & Ousterhout, *"In Search of an Understandable
//! Consensus Algorithm,"* 2014) — this module is deliberately just an
//! HTTP client speaking rqlite's wire protocol, not a reimplementation
//! of consensus itself. Hand-rolled raw HTTP over `TcpStream`/
//! `native_tls` rather than pulling in `reqwest`/`ureq` — the same
//! choice `interpreter.rs`'s own `http_request`/`https_request`
//! (backing `.nir`'s `http_get`/`https_get` builtins) already made, for
//! the same "no extra runtime dependency" reason; not reused directly
//! from there because this client needs two things those don't: reading
//! response *headers* (to follow a `Location` redirect to the current
//! Raft leader) and an `Authorization: Basic` header for rqlite's own
//! optional auth.
//!
//! ## Why every existing SQLite query string just works here too
//!
//! rqlite *is* SQLite under the hood — every table it serves is a real
//! SQLite database kept in sync via a replicated write-ahead log. That
//! means every SQL string and `?` placeholder already written for
//! `Backend::Sqlite` in `workflow_log.rs`/`transact_log.rs` is valid
//! here verbatim — unlike the Postgres backend (`durability.rs`), which
//! needed a parallel dialect (`$1` placeholders, `BIGSERIAL`/`BOOLEAN`
//! column types). Nothing about the SQL changes for this backend; only
//! how each call physically reaches the database does.

use serde_json::Value as Json;
use std::io::{Read, Write};
use std::net::TcpStream;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine;

/// One bind parameter, already reduced to the handful of JSON-
/// representable shapes rqlite's `/db/execute`/`/db/query` bodies accept
/// — the same small scalar set `dbconn.rs::Param` already carries for
/// the Postgres/SQLite `db_connect` backends, kept as its own type here
/// rather than reused because this one also needs `Null` (an `Option`
/// that's `None`), which `Param` has no case for.
#[derive(Clone, Debug)]
pub enum RqliteParam {
    Text(String),
    Int(i64),
    Bool(bool),
    Null,
}

impl RqliteParam {
    fn to_json(&self) -> Json {
        match self {
            RqliteParam::Text(s) => Json::String(s.clone()),
            RqliteParam::Int(n) => Json::Number((*n).into()),
            RqliteParam::Bool(b) => Json::Bool(*b),
            RqliteParam::Null => Json::Null,
        }
    }
}

/// Converts a Rust value at a call site into a [`RqliteParam`] — the
/// same role `rusqlite::ToSql`/`postgres::types::ToSql` play for the
/// other two backends, kept minimal (only the handful of shapes
/// `workflow_log.rs`/`transact_log.rs` ever actually pass) rather than
/// pulling in a full serialization trait for one narrow use.
pub trait ToRqliteParam {
    fn to_rqlite_param(&self) -> RqliteParam;
}

impl ToRqliteParam for i64 {
    fn to_rqlite_param(&self) -> RqliteParam {
        RqliteParam::Int(*self)
    }
}
impl ToRqliteParam for bool {
    fn to_rqlite_param(&self) -> RqliteParam {
        RqliteParam::Bool(*self)
    }
}
impl ToRqliteParam for str {
    fn to_rqlite_param(&self) -> RqliteParam {
        RqliteParam::Text(self.to_string())
    }
}
impl ToRqliteParam for String {
    fn to_rqlite_param(&self) -> RqliteParam {
        RqliteParam::Text(self.clone())
    }
}
impl<T: ToRqliteParam> ToRqliteParam for Option<T> {
    fn to_rqlite_param(&self) -> RqliteParam {
        match self {
            Some(v) => v.to_rqlite_param(),
            None => RqliteParam::Null,
        }
    }
}
impl<T: ToRqliteParam + ?Sized> ToRqliteParam for &T {
    fn to_rqlite_param(&self) -> RqliteParam {
        (**self).to_rqlite_param()
    }
}

/// Builds a `Vec<RqliteParam>` from a call site's arguments, mirroring
/// `rusqlite::params!`'s own ergonomics so a `Backend::Rqlite` arm reads
/// like its `Backend::Sqlite` sibling right above it: `rq_params![id,
/// state, now]` instead of a hand-built `vec![...]`.
#[macro_export]
macro_rules! rq_params {
    ($($v:expr),* $(,)?) => {
        vec![$(($crate::rqlite::ToRqliteParam::to_rqlite_param(&$v))),*]
    };
}

/// One statement's result from `/db/execute` — `last_insert_id` is
/// `None` for a statement with nothing to insert (an `UPDATE`, e.g.);
/// `create_instance`/`begin_pending_action` are the two callers that
/// actually need it.
#[derive(Debug, Default)]
pub struct RqliteExecResult {
    pub last_insert_id: Option<i64>,
    pub rows_affected: i64,
}

fn io_err(e: std::io::Error) -> String {
    format!("rqlite I/O error: {e}")
}

/// A durable handle to one rqlite cluster — cheap to clone (just the
/// connection coordinates), and deliberately *not* holding a live
/// socket: every request opens a fresh `TcpStream`, the same "no
/// persistent-connection reuse" shape `interpreter.rs::http_request`
/// already has (`Connection: close` on every request) — rqlite requests
/// here are infrequent enough (one per durability-log write, not a hot
/// per-byte loop) that connection-per-request is the honest tradeoff,
/// not a missed optimization.
#[derive(Clone)]
pub struct RqliteClient {
    tls: bool,
    host: String,
    port: u16,
    auth: Option<(String, String)>,
}

impl RqliteClient {
    /// `conn_str` like `rqlite://host:port` or
    /// `rqlites://user:pass@host:port` (`rqlites://` selects TLS, the
    /// same doubled-scheme convention `postgres://`/`postgresql://`
    /// already isn't quite, but `dbconn.rs`'s own `sslmode=`-based
    /// opt-in TLS rule inspired keeping this one simple: the scheme
    /// itself, not a query parameter, decides). Fails fast with a real
    /// request against `/status` — matching `pool.rs::get_or_create`'s
    /// own documented contract that a bad connection target surfaces
    /// immediately at connect time, not on the first real query later.
    pub fn connect(conn_str: &str) -> Result<Self, String> {
        let (tls, rest) = if let Some(r) = conn_str.strip_prefix("rqlites://") {
            (true, r)
        } else if let Some(r) = conn_str.strip_prefix("rqlite://") {
            (false, r)
        } else {
            return Err(format!("not an rqlite:// or rqlites:// connection string: {conn_str}"));
        };
        let (auth, hostport) = match rest.split_once('@') {
            Some((userinfo, hostport)) => {
                let (user, pass) = userinfo.split_once(':').unwrap_or((userinfo, ""));
                (Some((user.to_string(), pass.to_string())), hostport)
            }
            None => (None, rest),
        };
        let (host, port_str) = hostport
            .split_once(':')
            .ok_or_else(|| format!("rqlite connection string is missing :port -- {conn_str}"))?;
        let port: u16 =
            port_str.parse().map_err(|e| format!("rqlite connection string has an invalid port {port_str:?}: {e}"))?;
        let client = RqliteClient { tls, host: host.to_string(), port, auth };
        let (status, body) = client.request("GET", "/status", None)?;
        if status >= 400 {
            return Err(format!("rqlite {}:{}/status returned HTTP {status}: {body}", client.host, client.port));
        }
        Ok(client)
    }

    /// Runs one statement via `/db/execute`, returning its
    /// `last_insert_id`/`rows_affected`.
    pub fn execute(&self, sql: &str, params: &[RqliteParam]) -> Result<RqliteExecResult, String> {
        let results = self.execute_many(&[(sql, params)])?;
        results.into_iter().next().ok_or_else(|| "rqlite: /db/execute returned no results for one statement".to_string())
    }

    /// Runs every `(sql, params)` pair as one atomic `/db/execute`
    /// call (`?transaction`) — `open_rqlite`'s own DDL batch is the one
    /// caller that needs more than one statement at a time; every other
    /// caller in `workflow_log.rs`/`transact_log.rs` passes exactly one.
    pub fn execute_many(&self, statements: &[(&str, &[RqliteParam])]) -> Result<Vec<RqliteExecResult>, String> {
        let body = Json::Array(
            statements
                .iter()
                .map(|(sql, params)| {
                    let mut arr = vec![Json::String((*sql).to_string())];
                    arr.extend(params.iter().map(RqliteParam::to_json));
                    Json::Array(arr)
                })
                .collect(),
        )
        .to_string();
        let (status, resp_body) = self.request("POST", "/db/execute?transaction", Some(&body))?;
        if status >= 400 {
            return Err(format!("rqlite /db/execute returned HTTP {status}: {resp_body}"));
        }
        let parsed: Json = serde_json::from_str(&resp_body).map_err(|e| format!("rqlite: malformed /db/execute response: {e}"))?;
        let results = parsed
            .get("results")
            .and_then(Json::as_array)
            .ok_or_else(|| format!("rqlite: /db/execute response has no \"results\" array: {resp_body}"))?;
        results
            .iter()
            .map(|r| {
                if let Some(err) = r.get("error").and_then(Json::as_str) {
                    return Err(format!("rqlite statement error: {err}"));
                }
                Ok(RqliteExecResult {
                    last_insert_id: r.get("last_insert_id").and_then(Json::as_i64),
                    rows_affected: r.get("rows_affected").and_then(Json::as_i64).unwrap_or(0),
                })
            })
            .collect()
    }

    /// Runs one `SELECT` via `/db/query?level=strong` (linearizable:
    /// reads the leader's own up-to-date state — `workflow_log.rs`/
    /// `transact_log.rs`'s durability contract needs a read to see every
    /// write already acknowledged, the same reason those modules use
    /// `PRAGMA synchronous = FULL` on the plain-SQLite backend rather
    /// than accepting a faster, weaker guarantee). Returns each row as a
    /// `Vec<Json>` in `SELECT`-column order — callers index into it
    /// positionally, the same convention `row.get(0)`/`row.get(1)`
    /// already uses for the other two backends.
    pub fn query(&self, sql: &str, params: &[RqliteParam]) -> Result<Vec<Vec<Json>>, String> {
        let body = Json::Array(vec![{
            let mut arr = vec![Json::String(sql.to_string())];
            arr.extend(params.iter().map(RqliteParam::to_json));
            Json::Array(arr)
        }])
        .to_string();
        let (status, resp_body) = self.request("POST", "/db/query?level=strong", Some(&body))?;
        if status >= 400 {
            return Err(format!("rqlite /db/query returned HTTP {status}: {resp_body}"));
        }
        let parsed: Json = serde_json::from_str(&resp_body).map_err(|e| format!("rqlite: malformed /db/query response: {e}"))?;
        let result = parsed
            .get("results")
            .and_then(Json::as_array)
            .and_then(|a| a.first())
            .ok_or_else(|| format!("rqlite: /db/query response has no results[0]: {resp_body}"))?;
        if let Some(err) = result.get("error").and_then(Json::as_str) {
            return Err(format!("rqlite query error: {err}"));
        }
        let values = match result.get("values") {
            Some(Json::Array(rows)) => rows,
            _ => return Ok(Vec::new()), // no "values" key at all means zero rows
        };
        Ok(values
            .iter()
            .map(|row| row.as_array().cloned().unwrap_or_default())
            .collect())
    }

    /// One row, or `None` if the query matched nothing — the
    /// `.optional()`/`query_opt` equivalent the other two backends
    /// already have.
    pub fn query_opt(&self, sql: &str, params: &[RqliteParam]) -> Result<Option<Vec<Json>>, String> {
        Ok(self.query(sql, params)?.into_iter().next())
    }

    fn request(&self, method: &str, path: &str, body: Option<&str>) -> Result<(u16, String), String> {
        self.request_at(self.host.clone(), self.port, method, path, body, 0)
    }

    /// Follows an rqlite `Location` redirect (a follower node pointing
    /// at the current Raft leader) up to a small hop cap — a genuine
    /// redirect loop would otherwise hang the caller forever instead of
    /// failing with a clear error.
    fn request_at(&self, host: String, port: u16, method: &str, path: &str, body: Option<&str>, hop: u8) -> Result<(u16, String), String> {
        if hop > 5 {
            return Err("rqlite: too many redirects (possible leader-election loop)".to_string());
        }
        let mut req = format!("{method} {path} HTTP/1.1\r\nHost: {host}:{port}\r\nConnection: close\r\nUser-Agent: nirdosha\r\n");
        if let Some((user, pass)) = &self.auth {
            let token = BASE64_STANDARD.encode(format!("{user}:{pass}"));
            req.push_str(&format!("Authorization: Basic {token}\r\n"));
        }
        let body_bytes = body.unwrap_or("").as_bytes();
        if body.is_some() {
            req.push_str(&format!("Content-Type: application/json\r\nContent-Length: {}\r\n", body_bytes.len()));
        }
        req.push_str("\r\n");
        let mut req_bytes = req.into_bytes();
        req_bytes.extend_from_slice(body_bytes);

        let raw = if self.tls {
            let tcp = TcpStream::connect((host.as_str(), port)).map_err(io_err)?;
            let connector = native_tls::TlsConnector::new().map_err(|e| format!("rqlite TLS init failed: {e}"))?;
            let mut stream = connector.connect(&host, tcp).map_err(|e| format!("rqlite TLS handshake failed: {e}"))?;
            read_all_after_write(&mut stream, &req_bytes)?
        } else {
            let mut stream = TcpStream::connect((host.as_str(), port)).map_err(io_err)?;
            read_all_after_write(&mut stream, &req_bytes)?
        };

        let (status, headers, resp_body) = parse_raw_response(&raw)?;
        if matches!(status, 301 | 302 | 307 | 308) {
            let location = headers
                .iter()
                .find(|(k, _)| k.eq_ignore_ascii_case("location"))
                .map(|(_, v)| v.clone())
                .ok_or_else(|| format!("rqlite: HTTP {status} redirect with no Location header"))?;
            let (new_host, new_port) = parse_host_port_from_url(&location)?;
            return self.request_at(new_host, new_port, method, path, body, hop + 1);
        }
        Ok((status, resp_body))
    }
}

fn read_all_after_write<S: Read + Write>(stream: &mut S, request: &[u8]) -> Result<Vec<u8>, String> {
    stream.write_all(request).map_err(io_err)?;
    const MAX_RESPONSE_BYTES: usize = 10 * 1024 * 1024;
    let mut raw = Vec::new();
    let mut buf = [0u8; 8192];
    loop {
        match stream.read(&mut buf) {
            Ok(0) => break,
            Ok(n) => {
                if raw.len() + n > MAX_RESPONSE_BYTES {
                    return Err("rqlite: response exceeded maximum size (10 MiB)".to_string());
                }
                raw.extend_from_slice(&buf[..n]);
            }
            Err(e) => return Err(io_err(e)),
        }
    }
    Ok(raw)
}

/// Splits a raw HTTP response into `(status, headers, body)` — unlike
/// `interpreter.rs::parse_http_response`, this keeps the headers (needed
/// to read `Location` on a redirect) rather than discarding them.
fn parse_raw_response(raw: &[u8]) -> Result<(u16, Vec<(String, String)>, String), String> {
    let sep = raw.windows(4).position(|w| w == b"\r\n\r\n").ok_or("rqlite: malformed HTTP response (no header/body separator)")?;
    let header_block = std::str::from_utf8(&raw[..sep]).map_err(|_| "rqlite: HTTP response headers are not valid UTF-8")?;
    let body = String::from_utf8_lossy(&raw[sep + 4..]).into_owned();
    let mut lines = header_block.split("\r\n");
    let status_line = lines.next().ok_or("rqlite: empty HTTP status line")?;
    let status: u16 = status_line
        .split_whitespace()
        .nth(1)
        .and_then(|s| s.parse().ok())
        .ok_or_else(|| format!("rqlite: malformed HTTP status line: {status_line:?}"))?;
    let headers = lines
        .filter_map(|line| line.split_once(':').map(|(k, v)| (k.trim().to_string(), v.trim().to_string())))
        .collect();
    Ok((status, headers, body))
}

/// Extracts `(host, port)` from a `Location` header value, which rqlite
/// always sends as an absolute URL (`http://leader-host:4001/db/execute`)
/// — never needs to preserve the redirect target's own scheme/path since
/// `request_at` re-sends the *original* request's method/path/body
/// against the new host, just following where the leader actually is.
fn parse_host_port_from_url(url: &str) -> Result<(String, u16), String> {
    let without_scheme = url.split_once("://").map(|(_, rest)| rest).unwrap_or(url);
    let authority = without_scheme.split(['/', '?']).next().unwrap_or(without_scheme);
    let (host, port_str) = authority.split_once(':').ok_or_else(|| format!("rqlite redirect Location has no :port -- {url}"))?;
    let port: u16 = port_str.parse().map_err(|e| format!("rqlite redirect Location has an invalid port {port_str:?}: {e}"))?;
    Ok((host.to_string(), port))
}

/// Splits a `;`-separated batch of `CREATE TABLE`/DDL statements (the
/// exact strings `Backend::Sqlite`'s own `conn.execute_batch` already
/// uses) into individual statements for rqlite's `/db/execute` JSON
/// array, which — unlike SQLite's own `execute_batch` — takes one
/// statement per array element, not one semicolon-joined string. Safe
/// for this module's own DDL specifically (no string literal in any of
/// it contains a `;`); not a general-purpose SQL splitter.
pub fn split_ddl_statements(batch: &str) -> Vec<String> {
    batch.split(';').map(str::trim).filter(|s| !s.is_empty()).map(|s| s.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_ddl_statements_drops_empty_pieces_and_trims_whitespace() {
        let batch = "CREATE TABLE a (x INT);\n  CREATE TABLE b (y INT);  \n";
        let stmts = split_ddl_statements(batch);
        assert_eq!(stmts, vec!["CREATE TABLE a (x INT)".to_string(), "CREATE TABLE b (y INT)".to_string()]);
    }

    #[test]
    fn parse_host_port_from_url_handles_a_typical_rqlite_redirect_location() {
        let (host, port) = parse_host_port_from_url("http://10.0.0.5:4001/db/execute?transaction").unwrap();
        assert_eq!(host, "10.0.0.5");
        assert_eq!(port, 4001);
    }

    #[test]
    fn parse_raw_response_extracts_status_headers_and_body() {
        let raw = b"HTTP/1.1 301 Moved Permanently\r\nLocation: http://leader:4001/db/execute\r\nContent-Length: 0\r\n\r\n";
        let (status, headers, body) = parse_raw_response(raw).unwrap();
        assert_eq!(status, 301);
        assert!(body.is_empty());
        assert!(headers.iter().any(|(k, v)| k.eq_ignore_ascii_case("location") && v == "http://leader:4001/db/execute"));
    }

    #[test]
    fn rq_params_macro_converts_mixed_argument_shapes() {
        let name = "alice";
        let now: i64 = 1000;
        let maybe: Option<&str> = None;
        let flag = true;
        let params = rq_params![name, now, maybe, flag];
        assert!(matches!(params[0], RqliteParam::Text(ref s) if s == "alice"));
        assert!(matches!(params[1], RqliteParam::Int(1000)));
        assert!(matches!(params[2], RqliteParam::Null));
        assert!(matches!(params[3], RqliteParam::Bool(true)));
    }
}
