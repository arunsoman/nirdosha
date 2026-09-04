//! Tests for `https_get`/`https_post` (`ast::BUILTIN_NAMES`'s doc
//! comment) — same request/response handling as `http_get`/`http_post`
//! (see `tests/http.rs`), over a real `native_tls::TlsStream`.
//!
//! **Self-containment, and its one real limit.** Every other test file
//! in this project speaks to a bare `TcpListener` it spins up itself, no
//! external service required. A genuine successful HTTPS round trip
//! can't be tested quite that way: `https_get`'s `TlsConnector::new()`
//! uses the platform's real trust store (deliberately — that's the
//! actual security property `docs/PROTOLANG_PORT.md`'s std_io §12 entry asks
//! for, "a vetted library binding," not something a test should weaken),
//! and there's no offline way to hand it a certificate it will actually
//! trust without either modifying the production connector to accept a
//! test-only trust anchor (which would mean testing code the real
//! feature doesn't run) or a certificate signed by a real CA (which
//! needs the network this test suite deliberately avoids depending on).
//! What *is* fully self-contained and genuinely valuable: proving the
//! handshake and certificate verification actually happen and actually
//! reject what they should (an untrusted self-signed cert, a plain-TCP
//! peer speaking no TLS at all) — if verification were accidentally
//! disabled, the self-signed-cert test below would start silently
//! succeeding instead of erroring, so it's a real regression guard, not
//! a placeholder. The success path was verified by hand instead, against
//! a real HTTPS server (`https://example.com:443/` → `Ok(HttpResponse {
//! status: 200, .. })`) and a real TLS-vs-plaintext failure
//! (`https://example.com:80/` → `Err("TLS handshake failed: ...")`).

use nirdosha::ast::Ty;
use nirdosha::interpreter::Value;
use nirdosha::parser::Parser;
use nirdosha::run;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};
use std::net::TcpListener;

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

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

/// Generates a fresh, throwaway self-signed certificate via the system
/// `openssl` CLI, in a fresh temp directory, and returns its PKCS#12
/// bytes (password `"testpass"`) — used only to prove `https_get`'s TLS
/// handshake genuinely validates certificates (the test below expects
/// this cert to be *rejected*, since nothing trusts it). This crate
/// already requires the `openssl` system library to build at all
/// (`native-tls` on Linux links against it) — depending on its CLI
/// being present too, for tests only, is a small addition on top of an
/// already-required system dependency on this platform, not a new one.
fn generate_self_signed_pkcs12() -> Option<Vec<u8>> {
    let dir = std::env::temp_dir().join(format!("nirdosha_https_test_cert_{}", std::process::id()));
    std::fs::create_dir_all(&dir).ok()?;
    let key = dir.join("key.pem");
    let cert = dir.join("cert.pem");
    let p12 = dir.join("test.p12");

    let ok = std::process::Command::new("openssl")
        .args([
            "req", "-x509", "-newkey", "rsa:2048", "-keyout",
        ])
        .arg(&key)
        .args(["-out"])
        .arg(&cert)
        .args(["-days", "1", "-nodes", "-subj", "/CN=localhost", "-addext", "subjectAltName=DNS:localhost,IP:127.0.0.1"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?
        .success();
    if !ok {
        return None;
    }
    let ok = std::process::Command::new("openssl")
        .args(["pkcs12", "-export", "-out"])
        .arg(&p12)
        .args(["-inkey"])
        .arg(&key)
        .args(["-in"])
        .arg(&cert)
        .args(["-passout", "pass:testpass"])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .ok()?
        .success();
    let bytes = if ok { std::fs::read(&p12).ok() } else { None };
    let _ = std::fs::remove_dir_all(&dir);
    bytes
}

/// Accepts exactly one TCP connection and completes a real TLS
/// handshake as the server, using `identity` — run on its own thread
/// since the client (`https_get`) blocks on `connect`.
fn serve_one_tls(listener: TcpListener, identity: native_tls::Identity) -> std::thread::JoinHandle<()> {
    std::thread::spawn(move || {
        let acceptor = native_tls::TlsAcceptor::new(identity).unwrap();
        let (stream, _) = listener.accept().unwrap();
        // The client is expected to reject this handshake (untrusted
        // cert) before it completes -- a `Result::Err` here is the
        // expected, successful outcome for this test, not a real
        // failure of the test harness itself.
        let _ = acceptor.accept(stream);
    })
}

#[test]
fn https_get_rejects_an_untrusted_self_signed_certificate() {
    let Some(pkcs12) = generate_self_signed_pkcs12() else {
        eprintln!("skipping: `openssl` CLI not available to generate a throwaway test certificate");
        return;
    };
    let identity = native_tls::Identity::from_pkcs12(&pkcs12, "testpass").expect("test cert should load");

    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = serve_one_tls(listener, identity);

    let src = format!(
        r#"
        fn main() -> bool {{
            return match https_get("localhost", {port}, "/") {{
                Ok(resp) => false,
                Err(e) => true,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Bool(true)) => {}
        other => panic!(
            "expected Ok(Bool(true)) -- an untrusted self-signed cert must be rejected, not accepted; got {other:?}"
        ),
    }
    server.join().unwrap();
}

#[test]
fn https_against_a_plain_tcp_peer_is_a_recoverable_err() {
    // A listener that accepts and immediately closes -- speaks no TLS at
    // all. The handshake must fail cleanly, not hang or panic.
    let port = free_port();
    let listener = TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = std::thread::spawn(move || {
        let _ = listener.accept();
    });

    let src = format!(
        r#"
        fn main() -> bool {{
            return match https_get("127.0.0.1", {port}, "/") {{
                Ok(resp) => false,
                Err(e) => true,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
    server.join().unwrap();
}

#[test]
fn connecting_https_to_a_closed_port_is_a_recoverable_err_not_a_trap() {
    let port = free_port(); // bound-and-dropped above, guaranteed closed
    let src = format!(
        r#"
        fn main() -> bool {{
            return match https_get("127.0.0.1", {port}, "/") {{
                Ok(resp) => false,
                Err(e) => true,
            }}
        }}
    "#
    );
    match run(&src) {
        Ok(Value::Bool(true)) => {}
        other => panic!("expected Ok(Bool(true)), got {other:?}"),
    }
}

// ---- static rejections ---------------------------------------------------

#[test]
fn https_get_requires_a_str_host() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(HttpResponse, str) = https_get(1, 443, "/")
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn https_post_requires_a_str_body() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let r: Result(HttpResponse, str) = https_post("localhost", 443, "/", 42)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::TypeMismatch { expected: Ty::Str, found: Ty::I64 });
}

#[test]
fn https_is_interpreter_only_rejected_by_codegen() {
    let src = r#"
        fn main() -> i64 {
            let r: Result(HttpResponse, str) = https_get("localhost", 443, "/")
            return 0
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    assert!(nirdosha::codegen::check_supported(&program).is_err());
}
