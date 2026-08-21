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
fn ownership_example_compiles_and_matches_interpreter() {
    let src = include_str!("../examples/ownership.nir");
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
}

#[test]
fn borrow_example_compiles_and_matches_interpreter() {
    let src = include_str!("../examples/borrow.nir");
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
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

#[test]
fn sandbox_example_is_rejected_by_codegen() {
    let program = parse_checked(include_str!("../examples/sandbox.nir"));
    let report = analyze(&program);
    let result = codegen::emit_llvm_ir(&program, &report);
    assert!(
        result.is_err(),
        "codegen doesn't support `sandbox`/`stop` yet -- must reject, not mis-compile"
    );
}

#[test]
fn sandbox_channels_example_is_rejected_by_codegen() {
    let program = parse_checked(include_str!("../examples/sandbox_channels.nir"));
    let report = analyze(&program);
    let result = codegen::emit_llvm_ir(&program, &report);
    assert!(
        result.is_err(),
        "codegen doesn't support `sandbox`/`chan` yet -- must reject, not mis-compile"
    );
}

// ---- `str` -- literals, escapes, param/return pass-through, `==`/`!=` --

#[test]
fn strings_example_compiles_and_matches_interpreter() {
    let src = include_str!("../examples/strings.nir");
    let (stdout, code) = compile_and_run(src);
    // `1`/`0`, not `true`/`false` -- a pre-existing, documented cosmetic
    // difference from the interpreter's `render()` (a comparison result
    // printed as a bare `bool` takes the same `i1`-as-`i64` path any
    // other `print(x > y)` already does; see `call()`'s print-arg
    // dispatch), not something this phase introduces.
    assert_eq!(stdout, "hello, nirdosha\nline one\nline two\ttabbed\nworld\n1\n0\n");
    assert_eq!(code, 0);
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn empty_string_literal_compiles_and_prints_a_blank_line() {
    let src = r#"
        fn main() {
            let s: str = ""
            print(s)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(stdout, "\n");
    assert_eq!(code, 0);
}

#[test]
fn same_length_different_content_strings_compare_unequal() {
    let src = r#"
        fn main() {
            let a: str = "abc"
            let b: str = "abd"
            print(a == b)
            print(a != b)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(stdout, "0\n1\n");
    assert_eq!(code, 0);
}

#[test]
fn tcp_client_example_compiles_now_that_tcp_codegen_landed() {
    // Stale since Phase B1: `tcp`/`connect`/`send`/`recv`/`stop` all
    // compile now — this only checks that `emit_llvm_ir` itself succeeds
    // (valid IR), not that the program *runs* correctly against a real
    // service, since `examples/tcp_client.nir`'s own doc comment says it
    // needs `python3 -m http.server 8000` running externally — a real
    // end-to-end round trip against a self-contained loopback server is
    // covered instead by the dedicated tests just above.
    let program = parse_checked(include_str!("../examples/tcp_client.nir"));
    let report = analyze(&program);
    let result = codegen::emit_llvm_ir(&program, &report);
    assert!(result.is_ok(), "tcp/connect/send/recv/stop should all compile: {:?}", result.err());
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

// ---- Phase 4: f64 scalar codegen -------------------------------------------
//
// `f64` maps directly to LLVM's `double` and needs no width story
// (`is_integer()` is false for it, so the existing `guard_in_range`/
// `narrow_from_i64`/`widen_to_i64` machinery already treats it as a
// no-guard, no-narrow passthrough -- see each function's doc comment).
// `Vector`/`Matrix` are **not** covered by this phase: they need a
// pointer/alloca-based codegen strategy (every other value in this file
// is a single SSA register), a distinct, larger increment deferred
// honestly rather than rushed -- see `llvm_ty`'s `Ty::Vector`/`Ty::Matrix`
// arm. `matrices_example_is_rejected_by_codegen`/`linalg_example_is_
// rejected_by_codegen` below pin that this is a real, checked rejection,
// not silent mis-compilation.

#[test]
fn floats_example_compiles_and_matches_interpreter() {
    let src = include_str!("../examples/floats.nir");
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    // `%f`'s default 6-decimal formatting (this backend) vs Rust's
    // shortest-round-trip `f64` formatting (the interpreter) are a
    // documented, honest cosmetic difference -- compare the *values*,
    // not the exact bytes, the same way the rest of this test file
    // trusts exit code + stdout content for integers where the two
    // formatters happen to already agree.
    let lines: Vec<f64> = stdout.lines().map(|l| l.parse().expect("each line should be a float")).collect();
    let expected = [5.0, 2.0, 5.25, 2.3333333333333335, -3.5, 1.0, 1.0, 3.0];
    assert_eq!(lines.len(), expected.len());
    for (got, want) in lines.iter().zip(expected.iter()) {
        assert!((got - want).abs() < 1e-6, "expected {want}, got {got}");
    }
}

#[test]
fn float_arithmetic_and_negation_compile_correctly() {
    let src = r#"
        fn main() {
            let a: f64 = 3.5
            let b: f64 = -a
            print(a + b * 2.0)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    // a=3.5, b=-3.5, a + b*2.0 = 3.5 + (-7.0) = -3.5
    let v: f64 = stdout.trim().parse().expect("stdout should be a float");
    assert!((v - (-3.5)).abs() < 1e-6, "expected -3.5, got {v} (code {code})");
    assert_eq!(code, 0);
}

/// A previously-latent bug (fixed alongside `f64` support): the OS-level
/// `main` wrapper converted `nir_main`'s return value to a process exit
/// code via `sext`, an integer-only instruction -- invalid LLVM IR for
/// `double`. Exercises `fn main() -> f64` directly, the one shape that
/// would have hit it.
#[test]
fn a_float_returning_main_compiles_and_sets_a_sane_exit_code() {
    let src = r#"
        fn main() -> f64 {
            return 3.9
        }
    "#;
    let (_, code) = compile_and_run(src);
    // `fptosi` truncates toward zero, same as Rust's `as i32` -- 3.9 -> 3.
    assert_eq!(code, 3);
}

#[test]
fn float_comparisons_compile_correctly() {
    let src = r#"
        fn main() {
            let a: f64 = 1.5
            let b: f64 = 2.5
            print(a < b)
            print(a == a)
            print(a > b)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "1\n1\n0\n");
}

// ---- Vector/Matrix codegen, Phase 0+1: value representation, the new
// by-pointer ABI, and `ArrayLit` -- everything needed to bind, reassign,
// and pass/return a Vector/Matrix value. Indexing, elementwise
// operators, and every dense-linalg builtin are still interpreter-only
// (later phases) -- `matrices_example_is_rejected_by_codegen` and
// `linalg_example_is_rejected_by_codegen`, right below, are the tests
// proving that boundary still holds.
//
// There's no way to extract a single scalar out of a Vector/Matrix in
// Nirdosha source without indexing (Phase 2) or a builtin (Phase 4) --
// so unlike every other test in this file, these can't cross-check a
// compiled value against `nirdosha::run`'s stdout. Instead: (a) inspect
// the emitted LLVM IR text directly for the exact hex-encoded bit
// pattern each literal element should store, in the right order/shape,
// and (b) confirm `codegen::build` actually produces a real binary that
// runs to completion (clang would refuse genuinely malformed IR at
// assembly time, so a successful build is itself a meaningful signal for
// the new pointer/sret ABI shape).

fn emit_ir(src: &str) -> String {
    let program = parse_checked(src);
    let report = analyze(&program);
    codegen::emit_llvm_ir(&program, &report).expect("codegen should succeed for this program")
}

fn f64_hex(v: f64) -> String {
    format!("0x{:016X}", v.to_bits())
}

#[test]
fn vector_literal_let_compiles_and_stores_every_element() {
    let src = r#"
        fn main() -> i64 {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            return 0
        }
    "#;
    let ir = emit_ir(src);
    assert!(ir.contains("alloca [3 x double]"), "expected a flat [3 x double] alloca for `v`:\n{ir}");
    for val in [1.0, 2.0, 3.0] {
        assert!(ir.contains(&f64_hex(val)), "expected the bit pattern for {val} in the IR:\n{ir}");
    }
    let (_, code) = compile_and_run(src);
    assert_eq!(code, 0);
}

#[test]
fn matrix_literal_flattens_row_major_and_compiles() {
    let src = r#"
        fn main() -> i64 {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            return 0
        }
    "#;
    let ir = emit_ir(src);
    // Flattened to one [4 x double] buffer (row-major), not a [2 x [2 x
    // double]] nested array -- matching `interpreter.rs`'s own
    // `Value::Matrix(Arc<[Value]>, rows, cols)` flattening exactly.
    assert!(ir.contains("alloca [4 x double]"), "expected the matrix's own flat [4 x double] alloca:\n{ir}");
    for val in [1.0, 2.0, 3.0, 4.0] {
        assert!(ir.contains(&f64_hex(val)), "expected the bit pattern for {val} in the IR:\n{ir}");
    }
    assert!(ir.contains("@llvm.memcpy"), "row construction should go through llvm.memcpy:\n{ir}");
    let (_, code) = compile_and_run(src);
    assert_eq!(code, 0);
}

#[test]
fn vector_reassignment_compiles_and_runs() {
    let src = r#"
        fn main() -> i64 {
            let v: Vector(f64, 2) = [1.0, 2.0]
            let w: Vector(f64, 2) = [3.0, 4.0]
            v = w
            return 0
        }
    "#;
    let ir = emit_ir(src);
    assert!(ir.contains("@llvm.memcpy"), "reassigning an aggregate should copy via memcpy, not alias:\n{ir}");
    let (_, code) = compile_and_run(src);
    assert_eq!(code, 0);
}

#[test]
fn vector_param_and_return_round_trip_through_the_new_abi() {
    let src = r#"
        fn identity_vec(v: Vector(f64, 3)) -> Vector(f64, 3) {
            return v
        }
        fn main() -> i64 {
            let a: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let b: Vector(f64, 3) = identity_vec(a)
            return 0
        }
    "#;
    let ir = emit_ir(src);
    // The by-pointer calling convention: an aggregate return becomes an
    // implicit `ptr %sret.ret` first argument and a `void`-returning
    // `define`; an aggregate param becomes a plain `ptr`, not a value.
    assert!(
        ir.contains("define void @identity_vec(ptr %sret.ret, ptr %arg.v)"),
        "expected the sret+pointer-param signature for identity_vec:\n{ir}"
    );
    // The callee's prologue copies its incoming pointer into its own
    // local storage before ever touching it (copy-in, not aliasing).
    assert!(
        ir.contains("call void @llvm.memcpy.p0.p0.i64(ptr %v.addr, ptr %arg.v"),
        "expected a copy-in memcpy from the incoming param pointer:\n{ir}"
    );
    // The call site in `main` allocates its own destination and passes
    // it as the first (sret) argument, plus `a`'s own pointer as the
    // second.
    assert!(ir.contains("call void @identity_vec(ptr %call_result.addr"), "expected the sret call convention at the call site:\n{ir}");
    let (_, code) = compile_and_run(src);
    assert_eq!(code, 0, "the new pointer/sret ABI should produce a real, runnable binary");
}

#[test]
fn matrices_example_is_rejected_by_codegen() {
    // As of Phase 3, indexing, elementwise ops, `*`, and `==`/`!=` are
    // all codegen-supported -- `check_supported`'s own structural
    // pre-pass now accepts this whole program, since it has no type
    // information to know `print(v)` is printing an aggregate (its
    // "printable" check is purely syntactic -- not a bool *literal* --
    // not type-aware; see `is_printable_expr`'s doc comment). The real
    // remaining boundary is `print(v)`/`print(m)` (printing a whole
    // Vector/Matrix directly, rather than one indexed element) still
    // being unsupported, caught by `call()` during actual IR emission,
    // not by the early pre-pass -- so this test now exercises the full
    // `build` pipeline instead of `check_supported` alone.
    let program = parse_checked(include_str!("../examples/matrices.nir"));
    assert!(
        codegen::check_supported(&program).is_ok(),
        "every construct in this example except `print`-of-a-whole-aggregate is codegen-supported now"
    );
    let report = analyze(&program);
    let mut out_path = std::env::temp_dir();
    out_path.push(format!("nirdosha_test_{}_{}", std::process::id(), unique_suffix()));
    let result = codegen::build(&program, &report, &out_path, codegen::OptLevel::O2);
    let _ = std::fs::remove_file(&out_path);
    assert!(result.is_err(), "print(v)/print(m) on a whole Vector/Matrix is still unsupported");
}

#[test]
fn linalg_example_is_rejected_by_codegen() {
    // As of Phase 5, every dense-linalg builtin this example calls (`dot`,
    // `cross`, `zeros`/`ones`/`identity`, `sum`/`len`/`norm`, `transpose`/
    // `trace`/`det`/`solve`, `is_square`/`is_symmetric`) is codegen-
    // supported — `check_supported`'s structural pre-pass now accepts the
    // whole program, same reasoning as `matrices_example_is_rejected_by_
    // codegen` above. The real remaining boundary is identical to that
    // test's: `print(v)`/`print(m)` on a whole Vector/Matrix result
    // (`cross`, `transpose`, `zeros`, `ones`, `identity`, `solve` are all
    // printed directly here) is still unsupported, caught by `call()`
    // during actual IR emission, not the early pre-pass.
    let program = parse_checked(include_str!("../examples/linalg.nir"));
    assert!(
        codegen::check_supported(&program).is_ok(),
        "every construct in this example except `print`-of-a-whole-aggregate is codegen-supported now"
    );
    let report = analyze(&program);
    let mut out_path = std::env::temp_dir();
    out_path.push(format!("nirdosha_test_{}_{}", std::process::id(), unique_suffix()));
    let result = codegen::build(&program, &report, &out_path, codegen::OptLevel::O2);
    let _ = std::fs::remove_file(&out_path);
    assert!(result.is_err(), "print(v)/print(m) on a whole Vector/Matrix is still unsupported");
}

/// Performance smoke test (unified plan §4.3): compiled `f64` arithmetic
/// measurably beats the interpreter over the same workload. The plan's
/// literal wording names a 3x3 matmul specifically -- not built this
/// phase (see the module note above) -- so this is the same claim
/// (compiled numeric code is faster) over the numeric feature this phase
/// actually shipped: a tight `f64` accumulate loop, run for enough
/// iterations that process-spawn overhead can't dominate the
/// measurement.
#[test]
fn compiled_float_arithmetic_is_faster_than_interpreted() {
    let src = r#"
        fn main() {
            let acc: f64 = 0.0
            let i: i64 = 0
            while i < 2000000 {
                acc = acc + 1.5
                acc = acc * 0.9999
                i = i + 1
            }
            print(acc)
        }
    "#;
    let program = parse_checked(src);

    let interpreted_start = std::time::Instant::now();
    nirdosha::run(src).expect("should run");
    let interpreted_elapsed = interpreted_start.elapsed();

    let report = analyze(&program);
    let mut out_path = std::env::temp_dir();
    out_path.push(format!("nirdosha_perf_{}_{}", std::process::id(), unique_suffix()));
    codegen::build(&program, &report, &out_path, codegen::OptLevel::O2).expect("should compile");
    let compiled_start = std::time::Instant::now();
    let output = Command::new(&out_path).output().expect("compiled binary should run");
    let compiled_elapsed = compiled_start.elapsed();
    let _ = std::fs::remove_file(&out_path);

    assert_eq!(output.status.code(), Some(0));
    // The fixed point of `x = (x + 1.5) * 0.9999` is 14998.5 -- both
    // paths should converge to it.
    let compiled_val: f64 = String::from_utf8_lossy(&output.stdout).trim().parse().expect("stdout should be a float");
    assert!((compiled_val - 14998.5).abs() < 0.1, "expected convergence near 14998.5, got {compiled_val}");

    assert!(
        compiled_elapsed < interpreted_elapsed,
        "expected compiled ({compiled_elapsed:?}) to beat interpreted ({interpreted_elapsed:?})"
    );
}

// ---- Phase 2: real dynamic `Expr::Index` codegen -----------------------

#[test]
fn vector_literal_index_read_compiles_and_matches_interpreter() {
    let src = r#"
        fn main() {
            let v: Vector(f64, 3) = [10.0, 20.0, 30.0]
            print(v[0])
            print(v[2])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "10.000000\n30.000000\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn matrix_literal_index_read_compiles_and_matches_interpreter() {
    let src = r#"
        fn main() {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            print(m[0, 1])
            print(m[1, 1])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "2.000000\n4.000000\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn dynamic_vector_index_reads_the_right_element() {
    // A genuinely runtime (loop-variable, non-literal) index, not just a
    // literal one -- the general case `typeck.rs` actually allows
    // (`Expr::Index`'s index only has to be `is_integer()`, no literal
    // restriction), and the reason this phase needs real `getelementptr`
    // with a runtime offset rather than an unroll-only shortcut.
    let src = r#"
        fn main() {
            let v: Vector(f64, 4) = [100.0, 200.0, 300.0, 400.0]
            let i: i64 = 0
            let sum: f64 = 0.0
            while i < 4 {
                sum = sum + v[i]
                i = i + 1
            }
            print(sum)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "1000.000000\n");
}

#[test]
fn dynamic_out_of_bounds_vector_index_traps_at_runtime() {
    let src = r#"
        fn main() {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let i: i64 = 5
            print(v[i])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "an out-of-bounds dynamic index must not exit 0");
}

#[test]
fn dynamic_negative_vector_index_traps_at_runtime() {
    let src = r#"
        fn main() {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let i: i64 = 0
            let j: i64 = i - 1
            print(v[j])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "a negative dynamic index must not exit 0");
}

#[test]
fn matrix_out_of_bounds_column_index_traps_at_runtime() {
    let src = r#"
        fn main() {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            let j: i64 = 9
            print(m[0, j])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "an out-of-bounds column index must not exit 0");
}

#[test]
fn unproven_dynamic_index_has_a_trap_block_in_the_ir() {
    let src = r#"
        fn main() {
            let v: Vector(f64, 4) = [1.0, 2.0, 3.0, 4.0]
            let i: i64 = 0
            let sum: f64 = 0.0
            while i < 4 {
                sum = sum + v[i]
                i = i + 1
            }
            print(sum)
        }
    "#;
    let ir = emit_ir(src);
    assert!(
        ir.contains("idx_trap"),
        "a loop-variable index into a fixed-size Vector isn't provable by this pass -- a \
         Tier-2 trap block must be present:\n{ir}"
    );
}

#[test]
fn proven_in_bounds_literal_index_has_no_trap_block_in_the_ir() {
    // `v[0]` on a `Vector(f64, 3)` -- trivially in-bounds, and exactly
    // the `ident[literal]` shape both `refine.rs` and `smt.rs` already
    // proved bounds for before this phase existed (their
    // `proven_index_bounds` sets were populated but unconsumed). This
    // phase's `guard_index_in_bounds` is the first codegen-side consumer
    // of that proof -- Tier 1, silent, no runtime check at all.
    let src = r#"
        fn main() {
            let v: Vector(f64, 3) = [1.0, 2.0, 3.0]
            print(v[0])
        }
    "#;
    let ir = emit_ir(src);
    assert!(
        !ir.contains("idx_trap"),
        "v[0] on a Vector(f64,3) is proven in-bounds by smt.rs -- no Tier-2 trap block should \
         be emitted:\n{ir}"
    );
}

// ---- Phase 3: elementwise ops, `*` in all three shapes, `==`/`!=` -----

#[test]
fn vector_elementwise_add_sub_match_interpreter() {
    let src = r#"
        fn main() {
            let a: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let b: Vector(f64, 3) = [10.0, 20.0, 30.0]
            let sum: Vector(f64, 3) = a + b
            let diff: Vector(f64, 3) = b - a
            print(sum[0])
            print(sum[1])
            print(sum[2])
            print(diff[0])
            print(diff[2])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "11.000000\n22.000000\n33.000000\n9.000000\n27.000000\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn matrix_elementwise_sub_matches_interpreter() {
    let src = r#"
        fn main() {
            let a: Matrix(f64, 2, 2) = [[5.0, 6.0], [7.0, 8.0]]
            let b: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            let d: Matrix(f64, 2, 2) = a - b
            print(d[0, 0])
            print(d[0, 1])
            print(d[1, 0])
            print(d[1, 1])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "4.000000\n4.000000\n4.000000\n4.000000\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn hadamard_multiply_and_divide_match_interpreter() {
    let src = r#"
        fn main() {
            let a: Vector(f64, 3) = [2.0, 4.0, 9.0]
            let b: Vector(f64, 3) = [3.0, 2.0, 3.0]
            let prod: Vector(f64, 3) = a .* b
            let quot: Vector(f64, 3) = a ./ b
            print(prod[0])
            print(prod[1])
            print(prod[2])
            print(quot[0])
            print(quot[2])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "6.000000\n8.000000\n27.000000\n0.666667\n3.000000\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn plain_scalar_hadamard_is_unaffected_by_the_aggregate_path() {
    // `.*`/`./` on two plain scalars is legal too (`infer_hadamard`'s doc
    // comment: two matching scalars are trivially "the same shape") and
    // must still take the ordinary scalar `binary()` path, not get
    // dragged into the new aggregate unrolling this phase adds.
    let src = r#"
        fn main() {
            let a: f64 = 6.0
            let b: f64 = 3.0
            print(a .* b)
            print(a ./ b)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "18.000000\n2.000000\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn scalar_times_matrix_both_orders_match_interpreter() {
    let src = r#"
        fn main() {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            let a: Matrix(f64, 2, 2) = 2.0 * m
            let b: Matrix(f64, 2, 2) = m * 3.0
            print(a[0, 0])
            print(a[1, 1])
            print(b[0, 0])
            print(b[1, 1])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "2.000000\n8.000000\n3.000000\n12.000000\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn matrix_times_vector_matches_interpreter_accumulation_order() {
    // A 3x3 matrix chosen so the dot-product accumulation order actually
    // matters (not a shape where any summation order gives the same
    // float bits).
    let src = r#"
        fn main() {
            let m: Matrix(f64, 3, 3) = [
                [0.1, 0.2, 0.3],
                [1.1, 2.2, 3.3],
                [7.0, 0.001, 100000.0]
            ]
            let v: Vector(f64, 3) = [1.0, 10.0, 100.0]
            let r: Vector(f64, 3) = m * v
            print(r[0])
            print(r[1])
            print(r[2])
        }
    "#;
    let (compiled_stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);

    let interpreted_stdout = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled_stdout, &interpreted_stdout, "Matrix*Vector");
}

#[test]
fn matrix_times_matrix_matches_interpreter_accumulation_order() {
    let src = r#"
        fn main() {
            let a: Matrix(f64, 3, 3) = [
                [0.1, 0.2, 0.3],
                [1.1, 2.2, 3.3],
                [7.0, 0.001, 100000.0]
            ]
            let b: Matrix(f64, 3, 3) = [
                [9.0, 8.0, 7.0],
                [0.001, 0.5, 12.25],
                [3.0, 2.0, 1.0]
            ]
            let c: Matrix(f64, 3, 3) = a * b
            print(c[0, 0])
            print(c[1, 2])
            print(c[2, 2])
        }
    "#;
    let (compiled_stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);

    let interpreted_stdout = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled_stdout, &interpreted_stdout, "Matrix*Matrix");
}

/// Parses each line of both outputs as `f64` and asserts near-exact
/// numeric equality, line by line.
///
/// This is *not* a true bit-pattern check, and it would be dishonest to
/// call it one: the compiled path prints via `printf`'s fixed
/// six-decimal `%f` (e.g. `"1.800200"`), which *rounds* — a value like
/// `1.8001999999999998` (needing 16+ significant digits to round-trip)
/// prints identically to `1.8002` (a different `f64`, one ULP away).
/// Parsing either rounded string back to `f64` recovers the nearest
/// `f64` to the *rounded decimal*, not the original computed bits, so
/// comparing `.to_bits()` after a round trip through `%f` is comparing
/// noise, not the computation — this was tried first and produced a
/// false positive (a 1-ULP "mismatch" that vanished under direct
/// instruction-level inspection, see below), which is why this function
/// exists instead.
///
/// The actual verification that `agg_mul`'s Matrix*Vector/Matrix*Matrix
/// loop order matches `interpreter.rs::eval_binary` bit-for-bit was done
/// once, by hand, at the instruction level: `objdump -d` on a compiled
/// binary computing the same accumulation chain as these tests showed
/// genuinely separate `mulsd`/`addsd` (no hardware `fma`, so no
/// contraction-related rounding difference), operating on the exact
/// literal constants in the exact source order — which is what this
/// unrolled codegen is designed to produce, and matches `interpreter.rs`
/// exactly by construction (module doc, design decision 3). What this
/// helper checks at the integration-test level, given `print`'s only
/// observable precision is six decimals, is that nothing *grosser* than
/// that (wrong operand, wrong order, an accidentally-swapped shape)
/// slipped in — a tolerance far tighter than any real reordering bug
/// would produce, not a "close enough" shrug.
fn assert_floats_match_interpreter(compiled_stdout: &str, interpreted_stdout: &str, label: &str) {
    let compiled: Vec<f64> = compiled_stdout.lines().map(|l| l.parse().expect("compiled output line should be a float")).collect();
    let interpreted: Vec<f64> =
        interpreted_stdout.lines().map(|l| l.parse().expect("interpreted output line should be a float")).collect();
    assert_eq!(compiled.len(), interpreted.len(), "{label}: compiled and interpreted printed a different number of lines");
    for (i, (c, p)) in compiled.iter().zip(interpreted.iter()).enumerate() {
        assert!(
            (c - p).abs() < 1e-6,
            "{label} line {i}: compiled {c:?} vs interpreted {p:?} -- differ by more than printf's own \
             %f precision, a real mismatch, not rounding noise"
        );
    }
}

#[test]
fn vector_and_matrix_equality_true_and_false_cases() {
    let src = r#"
        fn main() {
            let a: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let b: Vector(f64, 3) = [1.0, 2.0, 3.0]
            let c: Vector(f64, 3) = [1.0, 2.0, 99.0]
            print(a == b)
            print(a == c)
            print(a != c)
            print(a != b)
            let m1: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            let m2: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]]
            let m3: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 9.0]]
            print(m1 == m2)
            print(m1 == m3)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "1\n0\n1\n0\n1\n0\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn integer_element_vector_elementwise_ops_match_interpreter() {
    let src = r#"
        fn main() {
            let a: Vector(i64, 3) = [10, 20, 30]
            let b: Vector(i64, 3) = [3, 4, 7]
            let sum: Vector(i64, 3) = a + b
            let quot: Vector(i64, 3) = a ./ b
            print(sum[0])
            print(sum[2])
            print(quot[0])
            print(quot[1])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "13\n37\n3\n5\n");
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Unit));
}

#[test]
fn integer_hadamard_divide_by_zero_traps_at_runtime() {
    let src = r#"
        fn main() {
            let a: Vector(i64, 2) = [10, 20]
            let b: Vector(i64, 2) = [2, 0]
            let quot: Vector(i64, 2) = a ./ b
            print(quot[1])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "an elementwise integer divide by zero must not exit 0");
}

// ---- Phase 4: shape-driven Vector/Matrix builtins ---------------------
//
// Every builtin here has loop trip counts that depend only on
// compile-time-known shape, never on runtime data values, so each fully
// unrolls into straight-line IR (codegen.rs's `PHASE4_BUILTINS`, design
// decision 3). `det`/`inv`/`solve`/`rank`/`kf_update_state`/
// `kf_update_cov` have genuine data-dependent control flow (partial-pivot
// search) and stay interpreter-only until Phase 5 (a linked runtime call,
// not unrolled IR) -- `linalg.rs`'s
// `codegen_rejects_data_dependent_linalg_builtin_calls` pins that.

#[test]
fn len_and_is_square_are_compile_time_constants() {
    let src = r#"
        fn main() {
            let v: Vector(f64, 4) = [1.0, 2.0, 3.0, 4.0]
            print(len(v))
            let a: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            let b: Matrix(f64, 3, 3) = [[1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]]
            print(is_square(a))
            print(is_square(b))
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    // `print` on a `bool` result prints `0`/`1`, not `true`/`false` --
    // an existing, documented cosmetic gap (`call()`'s `print` arm
    // `zext`s `i1` to `i64` and prints via `%lld`), not something Phase 4
    // introduces or should paper over.
    assert_eq!(stdout, "4\n0\n1\n");
}

#[test]
fn zeros_ones_identity_produce_the_right_shapes() {
    let src = r#"
        fn main() {
            let z: Vector(f64, 3) = zeros(3)
            print(z[0])
            print(z[2])
            let o: Matrix(f64, 2, 2) = ones(2, 2)
            print(o[0, 0])
            print(o[1, 1])
            let i: Matrix(f64, 3, 3) = identity(3)
            print(i[0, 0])
            print(i[0, 1])
            print(i[2, 2])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, "0.000000\n0.000000\n1.000000\n1.000000\n1.000000\n0.000000\n1.000000\n");
}

#[test]
fn transpose_swaps_rows_and_columns() {
    let src = r#"
        fn main() {
            let m: Matrix(f64, 2, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0]]
            let t: Matrix(f64, 3, 2) = transpose(m)
            print(t[0, 0])
            print(t[0, 1])
            print(t[1, 0])
            print(t[2, 1])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "transpose");
}

#[test]
fn dot_computes_the_inner_product() {
    let src = r#"
        fn main() {
            let a: Vector(f64, 4) = [1.3, -2.7, 3.1, 0.4]
            let b: Vector(f64, 4) = [0.9, 4.2, -1.1, 2.6]
            print(dot(a, b))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "dot");
}

#[test]
fn cross_computes_the_cross_product() {
    let src = r#"
        fn main() {
            let a: Vector(f64, 3) = [1.3, -2.7, 3.1]
            let b: Vector(f64, 3) = [0.9, 4.2, -1.1]
            let c: Vector(f64, 3) = cross(a, b)
            print(c[0])
            print(c[1])
            print(c[2])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "cross");
}

#[test]
fn sum_over_vector_and_matrix() {
    let src = r#"
        fn main() {
            let v: Vector(f64, 4) = [1.3, -2.7, 3.1, 0.4]
            print(sum(v))
            let m: Matrix(f64, 2, 3) = [[1.3, -2.7, 3.1], [0.4, 5.5, -6.6]]
            print(sum(m))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "sum");
}

#[test]
fn norm_family_and_frobenius_norm() {
    let src = r#"
        fn main() {
            let v: Vector(f64, 4) = [1.3, -2.7, 3.1, 0.4]
            print(norm(v))
            print(norm1(v))
            print(norm_inf(v))
            let m: Matrix(f64, 2, 3) = [[1.3, -2.7, 3.1], [0.4, 5.5, -6.6]]
            print(frobenius_norm(m))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "norm family");
}

#[test]
fn trace_sums_the_diagonal() {
    let src = r#"
        fn main() {
            let m: Matrix(f64, 3, 3) = [[1.3, -2.7, 3.1], [0.4, 5.5, -6.6], [7.1, 8.2, -9.3]]
            print(trace(m))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "trace");
}

#[test]
fn is_symmetric_and_is_diag_true_and_false_cases() {
    let src = r#"
        fn main() {
            let sym: Matrix(f64, 3, 3) = [[1.0, 2.0, 3.0], [2.0, 4.0, 5.0], [3.0, 5.0, 6.0]]
            let not_sym: Matrix(f64, 3, 3) = [[1.0, 2.0, 3.0], [9.0, 4.0, 5.0], [3.0, 5.0, 6.0]]
            print(is_symmetric(sym))
            print(is_symmetric(not_sym))
            let diag: Matrix(f64, 2, 2) = [[7.0, 0.0], [0.0, 8.0]]
            let not_diag: Matrix(f64, 2, 2) = [[7.0, 1.0], [0.0, 8.0]]
            print(is_diag(diag))
            print(is_diag(not_diag))
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    // See `len_and_is_square_are_compile_time_constants`'s note --
    // `print` on a `bool` prints `0`/`1`, not `true`/`false`.
    assert_eq!(stdout, "1\n0\n1\n0\n");
}

#[test]
fn distance_computes_euclidean_distance() {
    let src = r#"
        fn main() {
            let a: Vector(f64, 3) = [1.3, -2.7, 3.1]
            let b: Vector(f64, 3) = [0.9, 4.2, -1.1]
            print(distance(a, b))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "distance");
}

#[test]
fn bearing_computes_initial_great_circle_bearing() {
    let src = r#"
        fn main() {
            let a: Vector(f64, 3) = [37.7749, -122.4194, 0.0]
            let b: Vector(f64, 3) = [40.7128, -74.0060, 0.0]
            print(bearing(a, b))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "bearing");
}

#[test]
fn lla_to_ecef_and_back_round_trips() {
    let src = r#"
        fn main() {
            let lla: Vector(f64, 3) = [37.7749, -122.4194, 15.0]
            let ecef: Vector(f64, 3) = lla_to_ecef(lla)
            print(ecef[0])
            print(ecef[1])
            print(ecef[2])
            let back: Vector(f64, 3) = ecef_to_lla(ecef)
            print(back[0])
            print(back[1])
            print(back[2])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "lla_to_ecef/ecef_to_lla");
}

#[test]
fn ecef_to_enu_and_back_round_trips() {
    let src = r#"
        fn main() {
            let ecef: Vector(f64, 3) = [-2706179.0, -4261066.0, 3885731.0]
            let refp: Vector(f64, 3) = [37.7749, -122.4194, 0.0]
            let enu: Vector(f64, 3) = ecef_to_enu(ecef, refp)
            print(enu[0])
            print(enu[1])
            print(enu[2])
            let back: Vector(f64, 3) = enu_to_ecef(enu, refp)
            print(back[0])
            print(back[1])
            print(back[2])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "ecef_to_enu/enu_to_ecef");
}

#[test]
fn kf_predict_state_and_cov_match_interpreter() {
    let src = r#"
        fn main() {
            let x: Vector(f64, 4) = [1.0, 2.0, 0.5, -0.3]
            let p: Matrix(f64, 4, 4) = [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0]
            ]
            let f: Matrix(f64, 4, 4) = [
                [1.0, 0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0, 1.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0]
            ]
            let q: Matrix(f64, 4, 4) = [
                [0.01, 0.0, 0.0, 0.0],
                [0.0, 0.01, 0.0, 0.0],
                [0.0, 0.0, 0.01, 0.0],
                [0.0, 0.0, 0.0, 0.01]
            ]
            let x1: Vector(f64, 4) = kf_predict_state(x, p, f, q)
            let p1: Matrix(f64, 4, 4) = kf_predict_cov(x, p, f, q)
            print(x1[0])
            print(x1[1])
            print(x1[2])
            print(x1[3])
            print(p1[0, 0])
            print(p1[1, 1])
            print(p1[0, 2])
            print(p1[3, 3])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "kf_predict_state/kf_predict_cov");
}

/// Runs `src` through the interpreter and returns whatever it printed to
/// real stdout, for the tests above that need a byte-for-byte comparison
/// against the compiled binary's output rather than just comparing the
/// interpreter's *return value* (which, for a `fn main()` with no
/// `return`, is always `Value::Unit` either way and proves nothing about
/// what got printed).
fn capture_interpreted_stdout(src: &str) -> String {
    use std::io::Read;
    // `nirdosha::run` prints straight to the process's real stdout with
    // no capture hook (documented gap, `bench/README.md`) -- shell out to
    // the actual `nirdosha` binary and capture it externally instead,
    // the same technique `bench/README.md` itself names as the fallback.
    let mut src_file = std::env::temp_dir();
    src_file.push(format!("nirdosha_test_src_{}_{}.nir", std::process::id(), unique_suffix()));
    std::fs::write(&src_file, src).expect("should write temp source file");

    let exe = env!("CARGO_BIN_EXE_nirdosha");
    let mut child = Command::new(exe)
        .arg(&src_file)
        .stdout(std::process::Stdio::piped())
        .spawn()
        .expect("interpreter should launch");
    let mut stdout = String::new();
    child.stdout.take().unwrap().read_to_string(&mut stdout).expect("should read interpreter stdout");
    let status = child.wait().expect("interpreter should exit");
    let _ = std::fs::remove_file(&src_file);
    assert!(status.success(), "interpreter run should succeed for this program");
    stdout
}

// ---- Phase 5: det/inv/solve/rank/kf_update_state/kf_update_cov, via a
// linked native call into runtime_kernels.rs's staticlib (not unrolled
// IR — genuine data-dependent partial-pivot control flow, module doc's
// `PHASE5_BUILTINS` note). Same cross-check-against-the-interpreter
// discipline as every other builtin test in this file, plus a
// deliberate-singular-matrix trap test per fallible builtin (`inv`/
// `solve`/`kf_update_state`/`kf_update_cov` — `det`/`rank` never fail).

#[test]
fn det_builtin_compiles_and_matches_interpreter() {
    // First-column pivot is 0 -- forces a real row swap, not just
    // straight-line elimination, exercising the actual partial-pivot
    // branch this phase exists for.
    let src = r#"
        fn main() {
            let m: Matrix(f64, 3, 3) = [[0.0, 2.0, 3.0], [4.0, 5.0, 6.0], [7.0, 8.0, 10.0]]
            print(det(m))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "det");
}

#[test]
fn inv_builtin_compiles_and_matches_interpreter() {
    let src = r#"
        fn main() {
            let m: Matrix(f64, 3, 3) = [[2.0, 0.0, 1.0], [1.0, 3.0, 2.0], [0.0, 1.0, 4.0]]
            let inv_m: Matrix(f64, 3, 3) = inv(m)
            print(inv_m[0, 0])
            print(inv_m[1, 2])
            print(inv_m[2, 1])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "inv");
}

#[test]
fn solve_builtin_compiles_and_matches_interpreter() {
    let src = r#"
        fn main() {
            let a: Matrix(f64, 3, 3) = [[2.0, 0.0, 1.0], [1.0, 3.0, 2.0], [0.0, 1.0, 4.0]]
            let b: Vector(f64, 3) = [5.0, 10.0, 15.0]
            let x: Vector(f64, 3) = solve(a, b)
            print(x[0])
            print(x[1])
            print(x[2])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "solve");
}

#[test]
fn rank_builtin_compiles_and_matches_interpreter() {
    // Row 2 = 2 * row 0 -- a genuinely rank-deficient 3x3 (rank 2, not 3).
    let src = r#"
        fn main() {
            let m: Matrix(f64, 3, 3) = [[1.0, 2.0, 3.0], [4.0, 5.0, 6.0], [2.0, 4.0, 6.0]]
            print(rank(m))
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(compiled, "2\n");
    let interpreted = capture_interpreted_stdout(src);
    assert_eq!(compiled, interpreted);
}

#[test]
fn kf_update_state_and_cov_compile_and_match_interpreter() {
    let src = r#"
        fn main() {
            let x: Vector(f64, 4) = [1.0, 2.0, 0.5, 0.5]
            let p: Matrix(f64, 4, 4) = [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0]
            ]
            let z: Vector(f64, 2) = [1.1, 2.2]
            let h: Matrix(f64, 2, 4) = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]]
            let r: Matrix(f64, 2, 2) = [[0.25, 0.0], [0.0, 0.25]]
            let x2: Vector(f64, 4) = kf_update_state(x, p, z, h, r)
            let p2: Matrix(f64, 4, 4) = kf_update_cov(x, p, z, h, r)
            print(x2[0])
            print(x2[1])
            print(x2[2])
            print(p2[0, 0])
            print(p2[2, 2])
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0);
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "kf_update_state/cov");
}

#[test]
fn inv_of_a_singular_matrix_traps_at_runtime() {
    let src = r#"
        fn main() {
            let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [2.0, 4.0]]
            let inv_m: Matrix(f64, 2, 2) = inv(m)
            print(inv_m[0, 0])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "inv of a singular matrix must not exit 0");
}

#[test]
fn solve_of_a_singular_matrix_traps_at_runtime() {
    let src = r#"
        fn main() {
            let a: Matrix(f64, 2, 2) = [[1.0, 2.0], [2.0, 4.0]]
            let b: Vector(f64, 2) = [1.0, 2.0]
            let x: Vector(f64, 2) = solve(a, b)
            print(x[0])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "solve against a singular matrix must not exit 0");
}

#[test]
fn kf_update_state_with_singular_innovation_covariance_traps_at_runtime() {
    // P and R both all-zero -> S = H P H^T + R is the all-zero (singular)
    // 2x2 matrix.
    let src = r#"
        fn main() {
            let x: Vector(f64, 4) = [0.0, 0.0, 0.0, 0.0]
            let p: Matrix(f64, 4, 4) = [
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0]
            ]
            let z: Vector(f64, 2) = [1.0, 1.0]
            let h: Matrix(f64, 2, 4) = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]]
            let r: Matrix(f64, 2, 2) = [[0.0, 0.0], [0.0, 0.0]]
            let x2: Vector(f64, 4) = kf_update_state(x, p, z, h, r)
            print(x2[0])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "kf_update_state with a singular innovation covariance must not exit 0");
}

#[test]
fn kf_update_cov_with_singular_innovation_covariance_traps_at_runtime() {
    let src = r#"
        fn main() {
            let x: Vector(f64, 4) = [0.0, 0.0, 0.0, 0.0]
            let p: Matrix(f64, 4, 4) = [
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0],
                [0.0, 0.0, 0.0, 0.0]
            ]
            let z: Vector(f64, 2) = [1.0, 1.0]
            let h: Matrix(f64, 2, 4) = [[1.0, 0.0, 0.0, 0.0], [0.0, 1.0, 0.0, 0.0]]
            let r: Matrix(f64, 2, 2) = [[0.0, 0.0], [0.0, 0.0]]
            let p2: Matrix(f64, 4, 4) = kf_update_cov(x, p, z, h, r)
            print(p2[0, 0])
        }
    "#;
    let (_, code) = compile_and_run(src);
    assert_ne!(code, 0, "kf_update_cov with a singular innovation covariance must not exit 0");
}

#[test]
fn runtime_kernels_staticlib_link_produces_a_standalone_binary() {
    // Confirms the build.rs/embedded-staticlib/link mechanism itself: the
    // compiled binary runs correctly when copied away from wherever it
    // was built, ruling out an accidental dependency on a file that only
    // exists during `codegen::build` itself (the temp `.ll`/`.a` files
    // `build()` writes and best-effort deletes right after linking).
    let src = r#"
        fn main() {
            let m: Matrix(f64, 2, 2) = [[3.0, 0.0], [0.0, 4.0]]
            print(det(m))
        }
    "#;
    let program = parse_checked(src);
    let report = analyze(&program);
    let mut built_path = std::env::temp_dir();
    built_path.push(format!("nirdosha_test_{}_{}", std::process::id(), unique_suffix()));
    codegen::build(&program, &report, &built_path, codegen::OptLevel::O2).expect("build should succeed");

    let mut moved_path = std::env::temp_dir();
    moved_path.push(format!("nirdosha_test_moved_{}_{}", std::process::id(), unique_suffix()));
    std::fs::rename(&built_path, &moved_path).expect("should be able to move the built binary");

    let output = Command::new(&moved_path).output().expect("moved binary should still run standalone");
    let _ = std::fs::remove_file(&moved_path);
    assert!(output.status.success(), "moved binary should exit 0");
    assert_eq!(String::from_utf8_lossy(&output.stdout), "12.000000\n");
}

#[test]
fn a_matrix_constructed_inside_a_tight_loop_does_not_blow_the_stack() {
    // Regression test for a real bug found after Phases 0-5 all landed
    // and passed their own tests: every `Vector`/`Matrix` alloca's
    // address is always taken (passed to `memcpy`/GEP/a `call`), so
    // LLVM's `mem2reg` can never promote it to a register the way it
    // does for a scalar `let`. An alloca emitted inline at its point of
    // use inside a loop body (rather than hoisted once to the function's
    // entry block and reused) allocates fresh, never-reclaimed stack
    // space on every iteration -- `benchmarks/nirdosha/{matmul,det,dot,
    // kalman}.nir` (each a 200,000-iteration loop constructing a
    // Matrix(f64,4,4)/Vector(f64,8) per iteration) all segfaulted before
    // `Codegen::entry_allocas` existed to fix this; no test in Phases
    // 0-5 caught it because every one of them used small, few-iteration
    // examples. 100,000 iterations here reliably blew the default 8MB
    // stack (`ulimit -s`) under the old inline-alloca codegen; this test
    // exists so that regressing back to inline allocas fails loudly
    // instead of only showing up against real benchmark-scale workloads.
    let src = r#"
        fn main() {
            let n: i64 = 0
            let n_max: i64 = 100000
            let t: f64 = 0.0
            let checksum: f64 = 0.0
            while n < n_max {
                let a: Matrix(f64, 4, 4) = [
                    [t, t + 1.0, t + 2.0, t + 3.0],
                    [t + 4.0, t + 5.0, t + 6.0, t + 7.0],
                    [t + 8.0, t + 9.0, t + 10.0, t + 11.0],
                    [t + 12.0, t + 13.0, t + 14.0, t + 15.0]
                ]
                let b: Matrix(f64, 4, 4) = [
                    [t + 1.0, t, t + 3.0, t + 2.0],
                    [t + 5.0, t + 4.0, t + 7.0, t + 6.0],
                    [t + 9.0, t + 8.0, t + 11.0, t + 10.0],
                    [t + 13.0, t + 12.0, t + 15.0, t + 14.0]
                ]
                let c: Matrix(f64, 4, 4) = a * b
                checksum = checksum + c[0, 0]
                t = t + 0.0001
                n = n + 1
            }
            print(checksum)
        }
    "#;
    let (compiled, code) = compile_and_run(src);
    assert_eq!(code, 0, "should exit cleanly, not SIGSEGV from unbounded per-iteration stack growth");
    let interpreted = capture_interpreted_stdout(src);
    assert_floats_match_interpreter(&compiled, &interpreted, "tight-loop matrix checksum");
}

// ---- Phase B1: tcp/tcp_listener codegen -----------------------------------
//
// Mirrors `tests/tcp.rs`'s own discipline: every test here spins up its
// own loopback `std::net::TcpListener` in the Rust test harness itself,
// no dependency on an external service. `free_port` avoids a fixed port
// number that could collide with something else already listening.

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0").expect("binding a fresh loopback listener should never fail").local_addr().unwrap().port()
}

#[test]
fn compiled_connect_send_recv_stop_round_trips_real_bytes() {
    use std::io::{Read, Write};
    let port = free_port();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let n = stream.read(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"ping");
        stream.write_all(b"pong").unwrap();
    });

    let src = format!(
        r#"
        fn main() {{
            let conn: tcp = connect("127.0.0.1", {port})
            send(conn, "ping")
            let reply: str = recv(conn)
            stop conn
            print(reply)
        }}
    "#
    );
    let (stdout, code) = compile_and_run(&src);
    server.join().unwrap();
    assert_eq!(code, 0);
    assert_eq!(stdout.trim_end(), "pong");
}

#[test]
fn compiled_recv_payload_matches_interpreter_byte_for_byte() {
    use std::io::{Read, Write};
    let port = free_port();
    let listener = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let server = std::thread::spawn(move || {
        let (mut stream, _) = listener.accept().unwrap();
        let mut buf = [0u8; 1024];
        let _ = stream.read(&mut buf).unwrap();
        stream.write_all(b"hello from server").unwrap();
    });

    let src = format!(
        r#"
        fn main() {{
            let conn: tcp = connect("127.0.0.1", {port})
            send(conn, "hi")
            let reply: str = recv(conn)
            stop conn
            print(reply)
        }}
    "#
    );
    let (compiled, code) = compile_and_run(&src);
    server.join().unwrap();
    assert_eq!(code, 0);
    assert_eq!(compiled.trim_end(), "hello from server");
}

#[test]
fn compiled_listen_accept_serves_a_real_client() {
    let port = free_port();
    let src = format!(
        r#"
        fn main() {{
            let l: tcp_listener = listen({port})
            let conn: tcp = accept(l)
            let msg: str = recv(conn)
            send(conn, "server saw it")
            stop conn
            stop l
            print(msg)
        }}
    "#
    );

    // The compiled binary blocks in `accept`, so it has to run in its own
    // process while a plain Rust client connects to it from this test.
    let program = parse_checked(&src);
    let report = nirdosha::smt::analyze(&program);
    let mut out_path = std::env::temp_dir();
    out_path.push(format!("nirdosha_test_{}_{}", std::process::id(), unique_suffix()));
    codegen::build(&program, &report, &out_path, codegen::OptLevel::O2).expect("codegen::build should succeed");
    let child = Command::new(&out_path).stdout(std::process::Stdio::piped()).spawn().expect("compiled binary should start");

    // Give the listener a moment to bind before connecting.
    let mut attempt = 0;
    let mut client = loop {
        match std::net::TcpStream::connect(("127.0.0.1", port)) {
            Ok(s) => break s,
            Err(_) if attempt < 50 => {
                attempt += 1;
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(e) => panic!("could not connect to the compiled listener: {e}"),
        }
    };
    use std::io::{Read, Write};
    client.write_all(b"client hello").unwrap();
    let mut buf = [0u8; 1024];
    let n = client.read(&mut buf).unwrap();
    assert_eq!(&buf[..n], b"server saw it");

    let output = child.wait_with_output().expect("compiled binary should exit");
    let _ = std::fs::remove_file(&out_path);
    assert!(output.status.success());
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim_end(), "client hello");
}

#[test]
fn connecting_to_a_closed_port_traps_at_runtime() {
    let port = free_port(); // bound then immediately dropped -- nothing listens on it
    let src = format!(
        r#"
        fn main() {{
            let conn: tcp = connect("127.0.0.1", {port})
            print(0)
        }}
    "#
    );
    let (_, code) = compile_and_run(&src);
    assert_ne!(code, 0, "connecting to a closed port must not exit 0");
}

// ---- Phase C1: box/&/* (heap alloc, borrow, deref) -- no `free` yet ------

#[test]
fn box_of_a_scalar_round_trips() {
    let src = r#"
        fn main() {
            let b: box i64 = box 42
            print(*b)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout.trim(), "42");
}

#[test]
fn box_of_a_str_round_trips() {
    let src = r#"
        fn main() {
            let b: box str = box "hello box"
            print(*b)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout.trim(), "hello box");
}

#[test]
fn box_of_a_vector_derefs_and_indexes_correctly() {
    let src = r#"
        fn main() {
            let b: box Vector(f64, 3) = box [1.5, 2.5, 3.5]
            let v: Vector(f64, 3) = *b
            print(v[0])
            print(v[1])
            print(v[2])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_floats_match_interpreter(&stdout, &capture_interpreted_stdout(src), "box_of_a_vector_derefs_and_indexes_correctly");
}

#[test]
fn box_of_a_matrix_derefs_and_indexes_correctly() {
    let src = r#"
        fn main() {
            let b: box Matrix(f64, 2, 2) = box [[1.0, 2.0], [3.0, 4.0]]
            let m: Matrix(f64, 2, 2) = *b
            print(m[1, 0])
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_floats_match_interpreter(&stdout, &capture_interpreted_stdout(src), "box_of_a_matrix_derefs_and_indexes_correctly");
    assert_eq!(stdout.trim(), "3.000000");
}

#[test]
fn nested_box_double_deref_round_trips() {
    let src = r#"
        fn main() {
            let bb: box box i64 = box box 99
            print(**bb)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout.trim(), "99");
}

#[test]
fn ref_to_a_scalar_reads_without_consuming() {
    let src = r#"
        fn peek(r: &i64) -> i64 {
            return *r + 1
        }
        fn main() {
            let n: i64 = 41
            print(peek(&n))
            print(peek(&n))
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout, "42\n42\n");
}

// NOTE: `*r` for `r: &box T` is a static type error today
// (`CannotMoveOutOfReference`, `ownership.rs`'s documented "no
// place-expression semantics" limitation) -- extracting the affine `box`
// out of a shared reference isn't legal, so there's no way to read
// *through* a `&box T` at all yet, only to hold/pass it around. This test
// covers exactly that: taking `&b` more than once doesn't consume `b`
// (the non-affine-`Ref` guarantee `ownership.rs` already proves), reading
// `b` itself directly (never through `r1`/`r2`) still works.
#[test]
fn ref_to_a_boxed_value_does_not_consume_it() {
    let src = r#"
        fn main() {
            let b: box i64 = box 55
            let r1: &box i64 = &b
            let r2: &box i64 = &b
            print(*b)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout.trim(), "55");
}

// ---- Phase C2: free insertion, using ownership.rs's move data --------

/// The test C1 deliberately deferred: without free-insertion, an
/// 8-byte-per-iteration heap leak across millions of iterations grows the
/// process's RSS by hundreds of MB; with it, RSS stays flat regardless of
/// iteration count (the same physical stack slot's heap pointer is
/// replaced and its *previous* value freed every time around the loop —
/// see `Codegen::while_loop`'s free-emission point). Polls `/proc/<pid>/
/// status`'s `VmRSS` while the compiled binary is still running (a
/// `.output()`-style blocking wait can't observe this — the leak, if
/// present, is at its worst right before exit, not after).
///
/// **A real race, found and fixed while writing this test, not a
/// hypothetical**: this loop finishes in low tens of milliseconds even
/// under real load, so it's entirely possible for the child to exit and
/// its PID to be reaped and reassigned before this thread's very first
/// `/proc/<pid>/status` read. Checking the reassigned process's `PPid:`
/// alone isn't enough to catch this — every test in this file's suite
/// runs as a *thread* inside one shared test-runner process, so any
/// sibling test's own freshly-spawned child (there are dozens, all
/// spawning their own short-lived compiled binaries) shares the exact
/// same parent PID as this one's, and would pass a parent-only check.
/// Observed in practice: without a stronger check, this test measured
/// 140-180 MB reliably under the full parallel suite — not noise, but a
/// consistent misattribution to some other, genuinely larger test binary
/// that happened to inherit the reused PID. The only identity check that
/// actually closes this is comparing `/proc/<pid>/cmdline` against this
/// test's own `out_path` — nothing else running in the suite is that
/// exact freshly-built temp binary.
#[test]
fn box_in_a_tight_loop_does_not_leak_unbounded_memory() {
    let src = r#"
        fn main() {
            let n: i64 = 0
            let n_max: i64 = 20000000
            while n < n_max {
                let b: box i64 = box n
                n = n + 1
            }
            print(n)
        }
    "#;
    let program = parse_checked(src);
    let report = analyze(&program);
    let mut out_path = std::env::temp_dir();
    out_path.push(format!("nirdosha_test_{}_{}", std::process::id(), unique_suffix()));
    codegen::build(&program, &report, &out_path, codegen::OptLevel::O2).expect("codegen::build should succeed");

    let mut child =
        Command::new(&out_path).stdout(std::process::Stdio::piped()).spawn().expect("compiled binary should start");
    let pid = child.id();
    // `/proc/<pid>/cmdline` is NUL-separated argv; argv[0] here is exactly
    // `out_path` (how it was just exec'd above) — the one identity check
    // strong enough to rule out a reused PID landing on any other process
    // in this suite (see this test's own doc comment for why a `PPid`-only
    // check already turned out not to be enough).
    let expected_cmdline_prefix = out_path.to_string_lossy().into_owned();
    let mut peak_rss_kb: u64 = 0;
    loop {
        if let Ok(cmdline) = std::fs::read(format!("/proc/{pid}/cmdline"))
            && cmdline.split(|&b| b == 0).next() == Some(expected_cmdline_prefix.as_bytes())
            && let Ok(status_text) = std::fs::read_to_string(format!("/proc/{pid}/status"))
            && let Some(kb) = status_text
                .lines()
                .find(|l| l.starts_with("VmRSS:"))
                .and_then(|l| l.split_whitespace().nth(1))
                .and_then(|s| s.parse::<u64>().ok())
        {
            peak_rss_kb = peak_rss_kb.max(kb);
        }
        if child.try_wait().expect("try_wait should not error").is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    let output = child.wait_with_output().expect("compiled binary should finish");
    let _ = std::fs::remove_file(&out_path);

    assert_eq!(output.status.code(), Some(0));
    assert_eq!(String::from_utf8_lossy(&output.stdout).trim(), "20000000");
    // A correctly-freeing run measures ~2-3 MB in an idle environment; a
    // real per-iteration leak at 20M iterations would be in the hundreds
    // of MB. 50 MB is already an order of magnitude of headroom — with
    // the PID-identity check above, there's no longer a plausible source
    // of measurement noise left to pad further against.
    assert!(peak_rss_kb < 50_000, "peak RSS was {peak_rss_kb} KB — box allocations in the loop appear to be leaking");
}

/// Multiple `return` points, each with a still-live, never-moved `box`
/// (`extra`) in scope — both the early return inside the `if` and the
/// fall-through return afterward must free it independently (each is its
/// own `FreeMap::at_return` entry). A double-free (freeing the same
/// binding from two different return paths, or freeing something still
/// needed) would corrupt the allocator and typically abort/crash rather
/// than exit 0 — the exit code and matching output are the real
/// assertions here, not just "didn't crash" read informally.
#[test]
fn early_return_frees_a_live_box_without_double_freeing_it() {
    let src = r#"
        fn make(n: i64) -> i64 {
            let a: box i64 = box n
            let extra: box i64 = box 99
            if n < 0 {
                return *a
            }
            return *a + 1
        }
        fn main() {
            print(make(5))
            print(make(-5))
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout, "6\n-5\n");
}

/// `b` is declared in `run`'s own top-level scope (not inside either
/// branch), so its ownership question is only resolved once the `if`
/// merges — moved away in the `then` branch (into `consume`), never moved
/// in the `else` branch. `unused` is declared directly inside the `else`
/// block, so it's freed at that block's own close, independent of `b`.
/// Both `run(true)` (moves `b`, frees `unused`... except `unused` isn't
/// declared on that path at all) and `run(false)` (frees `unused` inside
/// the branch, then falls through to free `b` at the final `return`) have
/// to produce correct output with no crash for this to actually prove the
/// per-branch free-site split works, not just that *a* path works.
#[test]
fn box_moved_in_one_if_branch_but_not_the_other_frees_correctly_on_both_paths() {
    let src = r#"
        fn consume(b: box i64) -> i64 {
            return *b
        }
        fn run(take: bool) -> i64 {
            let b: box i64 = box 7
            if take {
                return consume(b)
            } else {
                let unused: box i64 = box 3
            }
            return 0
        }
        fn main() {
            print(run(true))
            print(run(false))
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout, "7\n0\n");
}

/// `ignore`'s body never references its own parameter and has no
/// `return` at all (`unit`-returning, falls off the end) — this is the
/// one case that exercises `FreeMap::at_fn_end` specifically (every other
/// test above goes through an explicit `Stmt::Return`), the free-site a
/// function reaches only via `function()`'s own implicit
/// `ret void`/`unreachable` fallback.
#[test]
fn an_unused_box_parameter_is_freed_at_the_implicit_function_end() {
    let src = r#"
        fn ignore(unused: box i64) {
        }
        fn main() {
            ignore(box 5)
            print(1)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout.trim(), "1");
}

/// `bb` is declared and never dereferenced or moved at all — unlike
/// `nested_box_double_deref_round_trips` above (whose `**bb` actually
/// *consumes* `bb` via `ownership.rs`'s extracting-affine-content rule,
/// so that test never exercises freeing a still-owned nested box), this
/// one reaches its free site fully owned, exercising `emit_box_free`'s
/// recursion into the inner `box i64` layer — freeing only the outer
/// allocation would leak the inner one on every run; freeing them in the
/// wrong order (outer before reading the inner pointer back out) would
/// use-after-free. Correct exit code is the observable proxy for both.
#[test]
fn nested_box_left_unused_frees_both_layers() {
    let src = r#"
        fn main() {
            let bb: box box i64 = box box 5
            print(1)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout.trim(), "1");
}

#[test]
fn boxed_function_param_and_return_round_trip() {
    let src = r#"
        fn consume(b: box i64) -> i64 {
            return *b
        }
        fn make_boxed(n: i64) -> box i64 {
            return box n
        }
        fn main() {
            print(consume(box 10))
            let b: box i64 = make_boxed(32)
            print(*b + 10)
        }
    "#;
    let (stdout, code) = compile_and_run(src);
    assert_eq!(code, 0);
    assert_eq!(stdout, capture_interpreted_stdout(src));
    assert_eq!(stdout, "10\n42\n");
}
