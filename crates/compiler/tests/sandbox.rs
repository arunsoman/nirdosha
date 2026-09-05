//! Tests for `sandbox`/`stop` (docs/SANDBOXING.md's "layer 1": an affine
//! handle around a real, separate OS process, no isolation backend and no
//! typed IPC yet). Unlike `thread T`, the "spawned computation" here is a
//! genuinely different OS process -- a fresh `nirdosha --sandbox-worker`
//! invocation (see `main.rs`) -- not a thread inside this one, so several
//! tests here verify real process-level behavior (PIDs, `kill -0`), not
//! just in-process state.

use nirdosha::ast::Ty;
use nirdosha::interpreter::{ErrorKind, Interpreter, Value};
use nirdosha::ownership::{check_ownership, OwnershipErrorKind};
use nirdosha::parser::Parser;
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

fn first_ownership_error(src: &str) -> OwnershipErrorKind {
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly before ownership-checking it");
    match check_ownership(&program) {
        Ok(()) => panic!("expected an ownership error, but none was found"),
        Err(errors) => errors.into_iter().next().unwrap().kind,
    }
}

/// `kill -0 <pid>` — sends no signal, just checks whether the process
/// still exists (and this user has permission to signal it). Shelling out
/// rather than pulling in a `libc`/`nix` dependency for one liveness
/// check.
#[cfg(unix)]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// Windows has no `kill -0` equivalent as a shell command; `tasklist`
/// filtered to the target PID is the same "shell out, don't pull in a
/// dependency for one liveness check" approach as the Unix side above —
/// it prints the process's row (containing its own PID as a decimal
/// string) if it still exists, or an "INFO: No tasks..." message if not.
/// Found missing on real Windows CI: without this arm the Unix-only
/// version above always failed to spawn `kill` at all, so this check
/// silently read as "never running," even immediately after a real
/// spawn.
#[cfg(windows)]
fn process_exists(pid: u32) -> bool {
    std::process::Command::new("tasklist")
        .args(["/FI", &format!("PID eq {pid}"), "/NH"])
        .output()
        .map(|out| String::from_utf8_lossy(&out.stdout).contains(&pid.to_string()))
        .unwrap_or(false)
}

// ---- the example, run end to end ------------------------------------

#[test]
fn example_sandbox_runs_to_completion() {
    // Not the plain `run(src)` entry point -- a real, independent-
    // verification-caught gap (see docs/PHASE0.md's "Thirteenth update"): this
    // test used to call `run` directly, with no `with_sandbox_exe`
    // override, so under `cargo test` it silently raced the exact same
    // wrong-binary bug every other runtime test in this file was already
    // fixed against -- it only ever "passed" because its assertion never
    // checked what the sandboxed child actually ran, so a fast-failing
    // wrong-binary child looked the same as a correctly-killed real one.
    let src = include_str!("fixtures/sandbox.nir");
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Ok(v) => assert_eq!(v, Value::Unit),
        Err(e) => panic!("expected Ok(Unit), got Err({e})"),
    }
}

// ---- spawn + stop, deterministically ----------------------------------

#[test]
fn stopping_a_still_running_sandbox_kills_it_and_returns_negative_one() {
    // `background_work` runs forever, so `stop` is guaranteed to catch it
    // still running (not racing against how fast the child happens to
    // finish) -- deterministic, not timing-dependent, *given* the child
    // is really the `nirdosha` binary running `background_work` at all.
    // Must use `interp()`/`with_sandbox_exe`, not the plain `run(src)` --
    // an earlier version of this test used `run` directly and was flaky
    // under `cargo test`'s parallel execution: with the wrong binary
    // re-exec'd (the test harness, which immediately errors out on an
    // unrecognized flag instead of looping forever), `stop`'s `try_wait`
    // can sometimes observe it as *already exited* rather than still
    // running, returning that binary's own incidental exit code instead
    // of -1. See docs/PHASE0.md's "Thirteenth update" for the same bug caught
    // in a different test first.
    let src = r#"
        fn background_work() -> unit {
            while true {
            }
        }
        fn main() -> i64 {
            let s: sandbox = sandbox background_work()
            let code: i64 = stop s
            return code
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Ok(v) => assert_eq!(v, Value::Int(-1)),
        Err(e) => panic!("expected Ok(Int(-1)), got Err({e})"),
    }
}

/// Every runtime test below points sandboxed children at the *real*,
/// separately-built `nirdosha` binary (`CARGO_BIN_EXE_nirdosha`, which
/// Cargo builds before running integration tests and exposes at this
/// compile-time-known path) instead of `std::env::current_exe()`'s
/// default. Found by testing, not assumed: `current_exe()` inside a
/// `cargo test` process resolves to the *test harness* binary, which
/// doesn't understand `--sandbox-worker` at all -- a sandboxed child
/// would silently run the wrong program, and a test that never checks
/// what actually ran wouldn't catch it. `Interpreter::with_sandbox_exe`
/// exists specifically for this.
fn interp(program: nirdosha::ast::Program, src: &str) -> Interpreter {
    Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src))
        .with_sandbox_exe(std::path::PathBuf::from(env!("CARGO_BIN_EXE_nirdosha")))
}

#[test]
fn a_sandboxed_process_can_take_scalar_arguments_and_actually_run() {
    // The parent burns enough real wall-clock time after spawning
    // (`burn`, not a sleep -- no timing primitive exists in the language)
    // that the sandboxed `worker` -- trivially fast once it's genuinely
    // the real `nirdosha` binary -- reliably finishes on its own before
    // `stop` runs, so this asserts the graceful exit code (0), not the
    // looser "0 or -1" a race would otherwise force.
    let src = r#"
        fn worker(n: i64, flag: bool) -> unit {
            print(n)
            print(flag)
            return
        }
        fn burn(n: i64) -> i64 {
            let x: i64 = 0
            while x < n {
                x = x + 1
            }
            return x
        }
        fn main() -> i64 {
            let s: sandbox = sandbox worker(42, true)
            let wasted: i64 = burn(1000000)
            return stop s
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    match interp(program, src).run_main() {
        Ok(v) => assert_eq!(v, Value::Int(0)),
        Err(e) => panic!("expected Ok(Int(0)), got Err({e})"),
    }
}

// ---- deterministic teardown, even without an explicit `stop` ----------

#[test]
fn dropping_a_sandbox_handle_without_stopping_it_still_kills_the_process() {
    // The actual point of docs/SANDBOXING.md's layer 1: a `sandbox` handle
    // that goes out of scope without ever being `stop`ped still kills its
    // process -- no zombie left behind just because a program forgot to
    // clean up. `main` hands the handle back unconsumed (legal today --
    // ownership.rs doesn't yet enforce "every affine value must be used
    // before its scope ends", a known, separate gap) so this test can
    // inspect the live process, then drop it on the Rust side and prove
    // the process is actually gone.
    let src = r#"
        fn background_work() -> unit {
            while true {
            }
        }
        fn main() -> sandbox {
            return sandbox background_work()
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    let result = interp(program, src).run_main().expect("spawning should succeed");

    let pid = match &result {
        Value::Sandbox(slot) => slot.lock().unwrap().as_ref().unwrap().pid(),
        other => panic!("expected a Value::Sandbox, got {other:?}"),
    };
    assert!(process_exists(pid), "the sandboxed process should be running before drop");

    drop(result); // last Arc reference gone -> SandboxChild::drop -> kill + wait

    assert!(
        !process_exists(pid),
        "dropping the handle without `stop` should still have killed the process"
    );
}

// ---- dynamic backstop (defense in depth, not the real gate) -----------

#[test]
fn stopping_an_already_stopped_sandbox_is_rejected_at_runtime_if_ownership_is_bypassed() {
    // `ownership.rs` already proves this statically (see the ownership
    // test below); this drives the interpreter directly, skipping
    // `check_ownership`, to confirm the runtime backstop
    // (`ErrorKind::AlreadySandboxStopped`) fires on its own.
    let src = r#"
        fn worker() -> unit { return }
        fn main() -> i64 {
            let s: sandbox = sandbox worker()
            let a: i64 = stop s
            let b: i64 = stop s
            return a + b
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    let result = interp(program, src).run_main();
    match result {
        Err(e) => assert_eq!(e.kind, ErrorKind::AlreadySandboxStopped),
        Ok(v) => panic!("expected AlreadySandboxStopped, got Ok({v:?})"),
    }
}

#[test]
fn a_sandbox_exe_that_does_not_exist_produces_sandbox_spawn_failed() {
    // Deterministic, not the "environment-fragile" case docs/PHASE0.md
    // originally described (an unwritable temp dir) -- `with_sandbox_exe`
    // already gives every test a way to point the child re-exec at an
    // arbitrary path, so pointing it at one that flatly doesn't exist
    // triggers the OS-level spawn failure every time, no special setup
    // needed. A gap an independent verification pass caught: this path
    // was wired but had no test at all before.
    let src = r#"
        fn worker() -> unit { return }
        fn main() -> i64 {
            let s: sandbox = sandbox worker()
            return stop s
        }
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    let result = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src))
        .with_sandbox_exe(std::path::PathBuf::from("/does/not/exist/nirdosha"))
        .run_main();
    match result {
        Err(e) => assert!(
            matches!(e.kind, ErrorKind::SandboxSpawnFailed { .. }),
            "expected SandboxSpawnFailed, got {:?}",
            e.kind
        ),
        Ok(v) => panic!("expected SandboxSpawnFailed, got Ok({v:?})"),
    }
}

// ---- static rejections -------------------------------------------------

#[test]
fn stopping_the_same_sandbox_twice_is_a_static_use_after_move() {
    let kind = first_ownership_error(
        r#"
        fn worker() -> unit { return }
        fn main() -> i64 {
            let s: sandbox = sandbox worker()
            let a: i64 = stop s
            let b: i64 = stop s
            return a + b
        }
    "#,
    );
    assert_eq!(kind, OwnershipErrorKind::UseAfterMove { name: "s".to_string() });
}

#[test]
fn spawning_a_builtin_as_a_sandbox_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let s: sandbox = sandbox print(1)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::CannotSpawnBuiltin { name: "print".to_string() });
}

#[test]
fn a_sandboxed_function_must_return_unit() {
    let kind = first_type_error(
        r#"
        fn worker() -> i64 { return 1 }
        fn main() -> i64 {
            let s: sandbox = sandbox worker()
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::SandboxFnMustReturnUnit { name: "worker".to_string() });
}

#[test]
fn a_sandboxed_function_cannot_take_a_box_parameter() {
    let kind = first_type_error(
        r#"
        fn worker(b: box i64) -> unit { return }
        fn main() -> i64 {
            let x: box i64 = box 5
            let s: sandbox = sandbox worker(x)
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::SandboxArgMustBeScalar { found: Ty::Box(Box::new(Ty::I64)) });
}

#[test]
fn stopping_a_non_sandbox_value_is_rejected_statically() {
    let kind = first_type_error(
        r#"
        fn main() -> i64 {
            let x: i64 = 5
            stop x
            return 0
        }
    "#,
    );
    assert_eq!(kind, TypeErrorKind::ExpectedSandboxType { found: Ty::I64 });
}
