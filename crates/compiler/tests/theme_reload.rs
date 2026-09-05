//! Integration tests for live `--theme` reload (`serve.rs`'s
//! `ThemeCache`) — a redeployed `theme.json` on disk takes effect
//! within one TTL window, without restarting `nirdosha serve`. Same
//! shape as `tests/role_mapping.rs`: real `tiny_http` server, TTL
//! overridable via `NIRDOSHA_TEST_THEME_TTL_MS` (`serve.rs::theme_ttl`'s
//! own documented testing seam) so the boundary is proven with a real,
//! short wait rather than a 30-second tax per test run or a faked clock.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

use nirdosha::ast::Program;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

const TEST_TTL_MS: u64 = 150;

fn free_port() -> u16 {
    TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

fn build_program(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
    program
}

fn write_theme(path: &std::path::Path, primary_600: &str) {
    let json = format!(r#"{{"brand": {{"600": "{primary_600}"}}}}"#);
    std::fs::write(path, json).expect("write theme.json");
}

fn start_server(theme_path: &std::path::Path) -> u16 {
    // SAFETY: every test in this file sets the same value -- a
    // concurrent write from another test thread is a same-value race,
    // not a correctness issue.
    unsafe { std::env::set_var("NIRDOSHA_TEST_THEME_TTL_MS", TEST_TTL_MS.to_string()) };
    let port = free_port();
    let program = Arc::new(build_program("fn main() {}"));
    let theme_json = std::fs::read_to_string(theme_path).expect("read initial theme.json");
    let theme: nirdosha::ui_gen::Theme = serde_json::from_str(&theme_json).expect("parse initial theme.json");
    let transact_log = std::env::temp_dir().join(format!("nirdosha-theme-reload-transact-{port}.db"));
    let workflow_log = std::env::temp_dir().join(format!("nirdosha-theme-reload-workflow-{port}.db"));
    let theme_path_owned = theme_path.to_string_lossy().to_string();
    std::thread::spawn(move || {
        nirdosha::serve::run(
            program, "127.0.0.1", port, None, None, transact_log, workflow_log, None, None, Some(&theme),
            Some(&theme_path_owned), None, None, false, None,
            &[],
        )
        .expect("serve::run should not fail to bind");
    });
    for _ in 0..100 {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return port;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    panic!("server on port {port} never came up");
}

fn get_index(port: u16) -> String {
    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("connect");
    stream.write_all(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n").expect("write request");
    let mut resp = String::new();
    stream.read_to_string(&mut resp).expect("read response");
    resp
}

fn tempfile(name: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("nirdosha-theme-reload-{name}-{}.json", std::process::id()))
}

#[test]
fn theme_change_is_not_reflected_within_the_ttl_window() {
    let theme_path = tempfile("within-ttl");
    write_theme(&theme_path, "#111111");
    let port = start_server(&theme_path);

    assert!(get_index(port).contains("--md-primary: #111111;"));
    write_theme(&theme_path, "#222222");
    // No sleep -- immediately after the on-disk change, still within the
    // TTL window from the server's own eager startup load.
    let body = get_index(port);
    assert!(body.contains("--md-primary: #111111;"), "should still show the old value within the TTL window");
    assert!(!body.contains("--md-primary: #222222;"));
}

#[test]
fn theme_change_is_reflected_after_the_ttl_elapses() {
    let theme_path = tempfile("after-ttl");
    write_theme(&theme_path, "#111111");
    let port = start_server(&theme_path);

    assert!(get_index(port).contains("--md-primary: #111111;"));
    write_theme(&theme_path, "#222222");
    std::thread::sleep(Duration::from_millis(TEST_TTL_MS + 100));
    let body = get_index(port);
    assert!(body.contains("--md-primary: #222222;"), "the new theme should be live after the TTL refreshes");
    assert!(!body.contains("--md-primary: #111111;"), "the old value should be fully replaced, not just appended");
}

#[test]
fn malformed_theme_on_reload_keeps_serving_the_last_good_page() {
    let theme_path = tempfile("malformed");
    write_theme(&theme_path, "#111111");
    let port = start_server(&theme_path);

    assert!(get_index(port).contains("--md-primary: #111111;"));
    std::fs::write(&theme_path, "{ not valid json").expect("write malformed theme.json");
    std::thread::sleep(Duration::from_millis(TEST_TTL_MS + 100));
    let body = get_index(port);
    assert!(body.contains("HTTP/1.1 200"), "the server must not crash or error on a malformed reload");
    assert!(body.contains("--md-primary: #111111;"), "the last-good theme should still be served, not blanked out");
}
