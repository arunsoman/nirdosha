//! Tests for `codegen.rs` — goal.md row 5's first real content: this
//! backend actually produces native binaries, and these tests actually
//! run them, not just check that IR text was emitted. Every bug fixed
//! while building this module (see codegen.rs's doc comments) was found
//! by exactly this kind of check — inspecting real output, not reasoning
//! about the code in the abstract — so the tests here follow the same
//! discipline: run the compiled binary, compare its real stdout/exit
//! code against the interpreter's, don't just assert the pipeline
//! "succeeded."

use std::process::Command;

use nirdosha::ast::Program;
use nirdosha::codegen;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::smt::analyze;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

fn parse_checked(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("should typecheck cleanly");
    check_ownership(&program).expect("should ownership-check cleanly");
    program
}

/// Compiles `src` to a real native binary in a fresh temp path at the
/// given optimization level, runs it, and returns its stdout and exit
/// code. Panics (loudly, with clang's own error) if compilation itself
/// fails — a test that expects compilation to fail should call
/// `codegen::build` directly instead.
fn compile_and_run_opt(src: &str, opt: codegen::OptLevel) -> (String, i32) {
    let program = parse_checked(src);
    let report = analyze(&program);
    let mut out_path = std::env::temp_dir();
    out_path.push(format!("nirdosha_test_{}_{}", std::process::id(), unique_suffix()));
    codegen::build(&program, &report, &out_path, opt).expect("codegen::build should succeed for this program");
    let output = Command::new(&out_path).output().expect("compiled binary should run");
    let _ = std::fs::remove_file(&out_path);
    (String::from_utf8_lossy(&output.stdout).to_string(), output.status.code().unwrap_or(-1))
}

/// The default most tests use — `-O2`, matching `nirdosha build`'s own
/// default (module doc: goal.md row 5 is about hardware speed) and, not
/// incidentally, the stronger correctness check: an aggressive optimizer
/// is exactly what would expose a subtly wrong `unreachable` marker that
/// `-O0` happens not to disturb.
fn compile_and_run(src: &str) -> (String, i32) {
    compile_and_run_opt(src, codegen::OptLevel::O2)
}

fn unique_suffix() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    COUNTER.fetch_add(1, Ordering::Relaxed)
}

#[test]
fn hello_compiles_and_matches_interpreter() {
    let src = include_str!("../examples/hello.nir");
    let (stdout, code) = compile_and_run(src);
    assert_eq!(stdout, "8\n");
    assert_eq!(code, 0);
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn factorial_compiles_and_matches_interpreter() {
    let src = include_str!("../examples/factorial.nir");
    let (stdout, code) = compile_and_run(src);
    assert_eq!(stdout, "3628800\n");
    assert_eq!(code, 0);
}

#[test]
fn loop_compiles_and_matches_interpreter() {
    let src = include_str!("../examples/loop.nir");
    let (stdout, code) = compile_and_run(src);
    assert_eq!(stdout, "20\n19\n18\n17\n16\n11\n");
    assert_eq!(code, 0);
}

// ---- box/&/* are honestly rejected, not silently mis-compiled ----------

#[test]
fn ownership_example_is_rejected_by_codegen() {
    let program = parse_checked(include_str!("../examples/ownership.nir"));
    let report = analyze(&program);
    let result = codegen::emit_llvm_ir(&program, &report);
    assert!(result.is_err(), "codegen doesn't support `box` yet -- must reject, not mis-compile");
}

#[test]
fn borrow_example_is_rejected_by_codegen() {
    let program = parse_checked(include_str!("../examples/borrow.nir"));
    let report = analyze(&program);
    let result = codegen::emit_llvm_ir(&program, &report);
    assert!(result.is_err(), "codegen doesn't support `&` yet -- must reject, not mis-compile");
}

#[test]
fn threads_example_is_rejected_by_codegen() {
    let program = parse_checked(include_str!("../examples/threads.nir"));
    let report = analyze(&program);
    let result = codegen::emit_llvm_ir(&program, &report);
    assert!(
        result.is_err(),
        "codegen doesn't support `spawn`/`join` yet -- must reject, not mis-compile"
    );
}

#[test]
fn channels_example_is_rejected_by_codegen() {
    let program = parse_checked(include_str!("../examples/channels.nir"));
    let report = analyze(&program);
    let result = codegen::emit_llvm_ir(&program, &report);
    assert!(
        result.is_err(),
        "codegen doesn't support `chan`/`send`/`recv` yet -- must reject, not mis-compile"
    );
}

// ---- the bug this module actually shipped with, pinned as a regression -

#[test]
fn narrow_type_overflow_actually_traps_at_runtime() {
    // The real bug found by testing (see codegen.rs's `guard_in_range`
    // doc comment): computing arithmetic directly at a narrow LLVM
    // width (`add i8`) wraps silently on overflow, the same as any
    // two's-complement machine addition, which meant the range check
    // was comparing an already-wrapped value against the very bounds
    // it's supposed to catch escaping -- it could never fire. 100 + 100
    // overflows i8 (max 127); a correct backend has to trap, not
    // silently produce -56 and exit 0.
    let src = r#"
        fn main() -> i64 {
            let a: i8 = 100
            let b: i8 = 100
            let c: i8 = a + b
            return 0
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "100 + 100 overflows i8 -- the compiled binary must not exit 0");
}

#[test]
fn division_by_zero_traps_at_runtime() {
    let src = r#"
        fn main() -> i64 {
            let z: i64 = 0
            let x: i64 = 10 / z
            return 0
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "division by zero must not exit 0");
}

// ---- Tier 1 vs Tier 2 is real in the generated IR, not just documented -

#[test]
fn proven_safe_arithmetic_has_no_trap_block_in_the_ir() {
    // Straight-line, safely-bounded arithmetic that smt.rs proves --
    // the generated IR should contain no `range_trap` block for it at
    // all (Tier 1: silent, no cost), not just "the check happens to
    // never fire at runtime".
    // Note: `let x: i32 = 100 + 50` (combining two bare literals
    // directly) does NOT typecheck as written -- typeck.rs's literal
    // flexibility only applies to a *bare* literal expression, and
    // `unify_operands`'s both-literals case resolves the combined result
    // to a concrete `i64`, which then needs an exact match against a
    // narrower target. Declaring the operands at the target type first
    // (as here) is the form that's actually accepted.
    //
    // `main` deliberately has no declared return type (so no `return`
    // statement, no return-value guard) -- `Stmt::Return`'s own guard is
    // always Tier 2 today regardless of what it returns (codegen.rs's
    // doc comment: neither refine.rs nor smt.rs currently records a
    // proof for a `return` site), so a `return`-shaped test would show a
    // second, unrelated trap block and muddy exactly what this test is
    // checking: Tier-1 elision for the `let x` statement specifically.
    let src = r#"
        fn main() {
            let a: i32 = 100
            let b: i32 = 50
            let x: i32 = a + b
            print(x)
        }
    "#;
    let program = parse_checked(src);
    let report = analyze(&program);
    let ir = codegen::emit_llvm_ir(&program, &report).expect("should compile");
    assert!(
        !ir.contains("range_trap"),
        "100 + 50 fits i32 and smt.rs proves it -- no Tier-2 trap block should be emitted:\n{ir}"
    );
}

#[test]
fn unproven_arithmetic_does_have_a_trap_block_in_the_ir() {
    // The `i8 + i8` case from the runtime test above, checked at the IR
    // level too: unproven arithmetic must have a real guard-and-trap
    // sequence present in the emitted text, not just "happens to work".
    let src = r#"
        fn add(a: i8, b: i8) -> i8 {
            let c: i8 = a + b
            return c
        }
        fn main() -> i64 {
            return 0
        }
    "#;
    let program = parse_checked(src);
    let report = analyze(&program);
    let ir = codegen::emit_llvm_ir(&program, &report).expect("should compile");
    assert!(
        ir.contains("range_trap"),
        "a + b for two full-range i8 params is genuinely unprovable -- a Tier-2 trap block \
         must be present:\n{ir}"
    );
}

// ---- negative literal arguments (the second real bug this module hit) -

#[test]
fn negative_literal_call_argument_compiles_and_runs_correctly() {
    // The second real bug found by testing: a negated literal argument
    // (`-3`) was being computed via a real `sub i64 0, 3` instruction and
    // then passed where a narrower parameter type was declared -- a
    // genuine LLVM type mismatch. Literals (including negated ones) now
    // get emitted directly at the callee's declared width instead.
    let src = r#"
        fn offset(base: i32, delta: i32) -> i32 {
            return base + delta
        }
        fn main() -> i32 {
            let r: i32 = offset(10, -3)
            return r
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_eq!(code, 7); // 10 + (-3)
}

#[test]
fn bool_valued_if_expression_compiles_and_runs_correctly() {
    // The gap `if_expr`'s doc comment used to flag: a genuinely
    // `bool`-valued if-expression whose branches both fall through (not
    // the "both return" or "side-effect only" shapes every existing
    // example happened to use) needed the result slot to actually be
    // `i1`, not a hardcoded `i64`. Fixed by inferring the slot's type
    // from the `then` branch's trailing expression (`typeck.rs` already
    // proved both branches agree).
    let src = r#"
        fn main() -> i64 {
            let c: bool = true
            let ok: bool = if c { true } else { false }
            if ok {
                return 1
            }
            return 0
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_eq!(code, 1);
}

// ---- -O0 vs -O2: both must agree with each other and the interpreter --

#[test]
fn optimized_and_unoptimized_builds_agree_on_every_example() {
    // The real point of this test: -O2 is an aggressive optimizer, and
    // it treats every `unreachable` this backend emits (for provably-
    // dead code, e.g. a definitely-returning function's fallthrough, or
    // an if-expression whose branches both terminate) as a hard
    // guarantee it's free to optimize around. A subtly wrong
    // `unreachable` might produce correct output at -O0 by accident and
    // silently misbehave at -O2 -- comparing both levels against the
    // same expected output is what would actually catch that, not
    // reading the code again.
    for (src, expected_stdout) in [
        (include_str!("../examples/hello.nir"), "8\n"),
        (include_str!("../examples/factorial.nir"), "3628800\n"),
        (include_str!("../examples/loop.nir"), "20\n19\n18\n17\n16\n11\n"),
    ] {
        let (o0_stdout, o0_code) = compile_and_run_opt(src, codegen::OptLevel::O0);
        let (o2_stdout, o2_code) = compile_and_run_opt(src, codegen::OptLevel::O2);
        assert_eq!(o0_stdout, expected_stdout, "-O0 output should match the interpreter");
        assert_eq!(o2_stdout, expected_stdout, "-O2 output should match the interpreter");
        assert_eq!(o0_code, o2_code, "-O0 and -O2 must exit the same way");
    }
}
