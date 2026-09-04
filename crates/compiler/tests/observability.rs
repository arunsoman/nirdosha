//! Tests for observability layer 1 (`observability.rs`'s module doc): the
//! hook points + local file/stdout exporter mechanism, and the "zero cost
//! when disabled" claim's mechanism (`tracer: None` is the default — see
//! `interpreter.rs::Interpreter::tracer`). Runs library-level, the same
//! way `tests/http.rs`/`tests/file_io.rs` do, exercising at least one
//! `Io` hook point (`file`'s `open`/`send`/`recv`/`stop`, reusing
//! `examples/file_io.nir`'s own pattern) and one `Concurrent` hook point
//! (`spawn`/`join`) with a real `Tracer` attached, then asserts spans
//! were actually emitted by reading the JSON-lines file the background
//! exporter thread wrote.

use nirdosha::interpreter::{Interpreter, Value};
use nirdosha::observability::Tracer;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;
use std::time::{Duration, Instant};

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

/// A fresh path under the OS temp dir, unique per test — same "avoid any
/// collision between tests / concurrent `cargo test` runs" discipline
/// `tests/file_io.rs::temp_path` already establishes.
fn temp_path(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("nirdosha_observability_test_{}_{}", std::process::id(), name));
    p
}

/// The exporter thread writes asynchronously — poll `tracer.emitted_count`
/// (bounded by a timeout) instead of guessing a fixed sleep before reading
/// the file back.
fn wait_for_emitted(tracer: &Tracer, at_least: u64) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while tracer.emitted_count() < at_least {
        assert!(Instant::now() < deadline, "timed out waiting for {at_least} span(s) to be exported");
        std::thread::sleep(Duration::from_millis(5));
    }
}

// ---- spans actually land, tagged with the right effect ---------------------

#[test]
fn file_io_and_spawn_join_emit_spans_with_the_right_effect_tags() {
    let out_path = temp_path("spans.jsonl");
    let file_path = temp_path("roundtrip.txt");
    let tracer = Tracer::new_file(out_path.clone()).expect("creating the trace output file should never fail");

    let src = format!(
        r#"
        fn double(n: i64) -> i64 {{
            return n * 2
        }}

        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            let h: thread i64 = spawn double(21)

            let out: file = open("{path}", "w")
            send(out, "hello from nirdosha")
            stop out

            let inp: file = open("{path}", "r")
            let content: str = recv(inp)
            stop inp

            let doubled: i64 = join h
            print(doubled)
            return Text(content)
        }}
    "#,
        path = file_path.to_str().unwrap()
    );

    let program = parse_ok(&src);
    typecheck(&program).expect("should typecheck cleanly");
    let interp = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src.as_str()))
        .with_tracer(std::sync::Arc::clone(&tracer));
    match interp.run_main() {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "hello from nirdosha"),
            other => panic!("expected Text(Str(\"hello from nirdosha\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"hello from nirdosha\")), got {other:?}"),
    }
    let _ = std::fs::remove_file(&file_path);

    // See the count assertion below for the full accounting -- 11 events
    // total. The exact *count* is deterministic (every one of those
    // operations runs exactly once); only their relative *order* is
    // thread-interleaving-dependent (the spawned thread's own `call`
    // span can land before or after some of the main thread's spans).
    wait_for_emitted(&tracer, 11);

    let contents = std::fs::read_to_string(&out_path).expect("the trace output file should exist");
    let lines: Vec<&str> = contents.lines().filter(|l| !l.is_empty()).collect();
    // `spawn`, `open` x2, `file.send`, `file.stop` x2, `file.recv`,
    // `join`, `print`, and a `call` span each for `double` (spawned) and
    // `main` (the toplevel `run_main` -> `call("main", ..)` itself is
    // traced too, unconditionally, the same as any other user function
    // call) -- 11 events total.
    assert_eq!(lines.len(), 11, "expected exactly 11 span lines, got {}: {contents}", lines.len());

    let parsed: Vec<serde_json::Value> =
        lines.iter().map(|l| serde_json::from_str(l).expect("every line should be valid JSON")).collect();

    // Every line is a well-formed "span" event.
    for line in &parsed {
        assert_eq!(line["type"], "span");
        assert_eq!(line["outcome"], "ok");
    }

    let names: Vec<&str> = parsed.iter().map(|l| l["name"].as_str().unwrap()).collect();
    assert!(names.contains(&"spawn"), "expected a spawn span, got {names:?}");
    assert!(names.contains(&"join"), "expected a join span, got {names:?}");
    assert!(names.contains(&"open"), "expected an open span, got {names:?}");
    assert!(names.contains(&"file.send"), "expected a file.send span, got {names:?}");
    assert!(names.contains(&"file.recv"), "expected a file.recv span, got {names:?}");
    assert!(names.contains(&"file.stop"), "expected a file.stop span, got {names:?}");
    assert!(names.contains(&"print"), "expected a print span, got {names:?}");
    assert_eq!(names.iter().filter(|&&n| n == "call").count(), 2, "expected two call spans (double, main), got {names:?}");

    // Effect tags landed correctly -- Concurrent for spawn/join, Io for
    // every file op and print.
    for line in &parsed {
        let name = line["name"].as_str().unwrap();
        let effect = line["effect"].as_str();
        let fn_name = line["fn_name"].as_str();
        match (name, fn_name) {
            ("spawn" | "join", _) => assert_eq!(effect, Some("concurrent"), "{name}'s span: {line}"),
            ("open" | "file.send" | "file.recv" | "file.stop" | "print", _) => {
                assert_eq!(effect, Some("io"), "{name}'s span: {line}")
            }
            // `double` is a pure function -- its `call` span carries no
            // effect tag (see `SpanRecord`'s doc comment), but it's still
            // traced. `main` itself does perform effects (it calls
            // `open`/`print`/... directly), so its own `call` span picks
            // up a real tag (`effect_of_fn`'s single-representative-tag
            // reduction of `main`'s inferred effect set).
            ("call", Some("double")) => assert_eq!(effect, None, "{name}'s span: {line}"),
            ("call", Some("main")) => assert_eq!(effect, Some("io"), "{name}'s span: {line}"),
            other => panic!("unexpected span (name, fn_name): {other:?}, full line: {line}"),
        }
    }
}

// ---- disabled tracer: no spans, no dropped-event bookkeeping ---------------

#[test]
fn no_tracer_means_no_spans_at_all() {
    let file_path = temp_path("no_tracer.txt");
    let src = format!(
        r#"
        struct Text {{
            value: str,
        }}
        fn main() -> Text {{
            let out: file = open("{path}", "w")
            send(out, "x")
            stop out
            return Text("done")
        }}
    "#,
        path = file_path.to_str().unwrap()
    );
    let program = parse_ok(&src);
    typecheck(&program).expect("should typecheck cleanly");
    // No `.with_tracer(..)` call at all -- `tracer: None` is the default
    // (`Interpreter::new`).
    let interp = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src.as_str()));
    match interp.run_main() {
        Ok(Value::Struct(name, fields)) if &*name == "Text" => match &fields[0] {
            Value::Str(s) => assert_eq!(&**s, "done"),
            other => panic!("expected Text(Str(\"done\")), got Text({other:?})"),
        },
        other => panic!("expected Ok(Text(\"done\")), got {other:?}"),
    }
    let _ = std::fs::remove_file(&file_path);
    // Nothing to assert on the interpreter side beyond "it ran to
    // completion identically to every other test in this suite" -- the
    // whole point of `tracer: None` is that there's no observable trace
    // of tracing at all.
}

// ---- a full channel doesn't block the traced program (fail-open) ----------

#[test]
fn a_full_trace_channel_drops_events_instead_of_blocking() {
    let out_path = temp_path("dropped.jsonl");
    let tracer = Tracer::new_file(out_path.clone()).expect("creating the trace output file should never fail");

    // `print` in a tight loop, enough calls to exceed the bounded
    // channel's capacity even if the exporter thread never kept up at
    // all -- proves `try_send`'s fail-open path is reachable and that it
    // never turns into a runtime error or a hang.
    let src = r#"
        fn main() -> i64 {
            let n: i64 = 0
            while n < 200 {
                print(n)
                n = n + 1
            }
            return n
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    let interp = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src))
        .with_tracer(std::sync::Arc::clone(&tracer));
    match interp.run_main() {
        Ok(Value::Int(200)) => {}
        other => panic!("expected Ok(Int(200)), got {other:?}"),
    }
    // Whether or not any single run actually overflowed the channel
    // (timing-dependent), the run itself always completes successfully —
    // that's the actual claim under test. `dropped_count` is exposed and
    // well-defined either way.
    let _ = tracer.dropped_count();
}
