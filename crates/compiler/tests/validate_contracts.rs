//! Tests for `validate <fn_name> { pre: ... post: ... }` (`docs/ROADMAP.md`
//! Track F, F3; `docs/NEXT_GEN.md` §F3) — real `.nir` syntax feeding
//! `contract_check.rs`'s already-proven Z3-backed Hoare-pair prover,
//! plus a runtime backstop (`interpreter.rs::call`) for anything that
//! prover can't statically model. Two enforcement paths, tested
//! separately, mirroring `tests/smt.rs`/`tests/refine.rs`'s own
//! discipline: several tests confirm each path *doesn't* over- or
//! under-claim, not just that it succeeds on the easy case.

use nirdosha::ast::Program;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;

fn parse(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

// ---- Build-time static path (Tier-1-provable: integer params/return, no loop/call/division) ----

const MAX_OF: &str = "
fn max_of(a: i64, b: i64) -> i64 {
    if a > b { return a }
    return b
}
fn main() -> i64 { return max_of(3, 5) }
";

#[test]
fn a_genuinely_true_contract_is_statically_proved_and_the_program_runs() {
    let src = format!("{MAX_OF}\nvalidate max_of {{ post: result >= a }}\n");
    let program = parse(&src);
    assert_eq!(nirdosha::contract_check::check_program_contracts(&program), Ok(()));
    assert_eq!(nirdosha::run(&src), Ok(nirdosha::interpreter::Value::Int(5)));
}

#[test]
fn a_genuinely_false_contract_fails_the_build_with_a_real_counterexample() {
    // `result > a` is false whenever `a >= b` (result == a then) --
    // real, not contrived: Z3 has to actually find `a >= b`, not just
    // notice a syntactic mismatch.
    let src = format!("{MAX_OF}\nvalidate max_of {{ post: result > a }}\n");
    let program = parse(&src);
    let err = nirdosha::contract_check::check_program_contracts(&program).expect_err("should be a real counterexample");
    assert_eq!(err.len(), 1);
    assert!(err[0].contains("max_of"), "error should name the fn: {err:?}");
    assert!(err[0].contains("violated"), "error should say what happened: {err:?}");
    // The whole pipeline (`nirdosha::run`, not just the checker in
    // isolation) refuses to execute a program with a proven-false
    // contract at all -- never falls through to interpreting it anyway.
    let run_err = nirdosha::run(&src).expect_err("run should refuse to execute a proven-false contract");
    assert!(run_err.contains("max_of"));
}

#[test]
fn validate_referencing_an_unknown_fn_is_a_real_type_error() {
    let src = format!("{MAX_OF}\nvalidate no_such_fn {{ post: result >= 0 }}\n");
    let program = parse(&src);
    let errors = nirdosha::typeck::typecheck(&program).expect_err("unknown fn in `validate` should be a type error");
    assert!(errors.iter().any(|e| e.to_string().contains("no_such_fn")), "errors: {errors:?}");
}

#[test]
fn validate_with_an_unrecognized_key_is_a_real_type_error() {
    let src = format!("{MAX_OF}\nvalidate max_of {{ invariant: result >= 0 }}\n");
    let program = parse(&src);
    let errors = nirdosha::typeck::typecheck(&program).expect_err("an `invariant:` key isn't `pre`/`post`");
    assert!(errors.iter().any(|e| e.to_string().contains("invariant")), "errors: {errors:?}");
}

#[test]
fn a_predicate_referencing_an_identifier_that_is_not_a_param_or_result_is_unbound() {
    let src = format!("{MAX_OF}\nvalidate max_of {{ post: result >= c }}\n");
    let program = parse(&src);
    let err = nirdosha::contract_check::check_program_contracts(&program).expect_err("`c` isn't a param of `max_of`");
    assert!(err[0].contains("`c`"), "error: {}", err[0]);
}

#[test]
fn an_unbound_identifier_in_a_predicate_is_now_caught_by_typecheck_itself() {
    // Same shape as the test above, but checking the *earlier* layer
    // added specifically to close the "malformed predicate only shows
    // up as a runtime error" gap: `typeck::check_validate` now
    // type-checks `pre`/`post` against the target fn's real signature,
    // so this is a real `TypeError` (`UnknownVar`) before contract_check
    // ever runs at all -- `nirdosha::run` must refuse to execute, not
    // silently proceed to interpretation.
    let src = format!("{MAX_OF}\nvalidate max_of {{ post: result >= c }}\n");
    let program = parse(&src);
    let errors = nirdosha::typeck::typecheck(&program).expect_err("`c` should be a real type error now");
    assert!(errors.iter().any(|e| e.to_string().contains('c')), "errors: {errors:?}");
    let run_err = nirdosha::run(&src).expect_err("run must refuse a program with an unbound validate predicate");
    assert!(run_err.contains("type error"), "run_err: {run_err}");
}

#[test]
fn a_non_bool_predicate_is_a_real_type_error_not_a_later_runtime_surprise() {
    // `result` (an `i64`) used directly as a `post`, with no comparison
    // at all -- not boolean-shaped. Must be caught here, at typecheck,
    // the same way an ordinary `if result { ... }` on an `i64` would be.
    let src = format!("{MAX_OF}\nvalidate max_of {{ post: result }}\n");
    let program = parse(&src);
    let errors = nirdosha::typeck::typecheck(&program).expect_err("a non-bool `post` should be a type error");
    assert!(!errors.is_empty());
}

#[test]
fn predicate_type_checking_also_covers_a_fn_tier_1_could_never_statically_reach() {
    // Same non-bool-predicate mistake as the test above, but on
    // `count_up` (a `while`-loop-shaped fn -- `Unsupported` to the
    // static Z3 prover). Before the typeck-level fix, this exact
    // mistake would only ever have surfaced the first time `count_up`
    // was actually called at runtime; it must now be a build-time
    // diagnostic regardless of whether the target fn is in Tier-1's
    // provable subset at all.
    let src = format!("{COUNT_UP}\nvalidate count_up {{ post: result }}\nfn main() {{}}\n");
    let program = parse(&src);
    let errors = nirdosha::typeck::typecheck(&program).expect_err("a non-bool `post` should be a type error even on an Unsupported-shaped fn");
    assert!(!errors.is_empty());
}

#[test]
fn early_return_inside_an_if_does_not_leak_into_the_statement_after_it() {
    // Regression pin for a real, previously-live bug found while
    // writing this test file: `Eval::stmts` used to keep walking
    // sibling statements after an `if` whose only branch already
    // returned, with *no* condition asserted at all -- so `max_of`'s
    // trailing `return b` got checked as reachable even when `a > b`
    // (where it never actually runs), producing a false
    // `Counterexample` against a genuinely-correct postcondition. Fixed
    // in the same session (`docs/ROADMAP.md` Track F, F3's writeup) by
    // giving `stmt` a real `Flow` result instead of a bare `EvalResult<()>`.
    let src = format!("{MAX_OF}\nvalidate max_of {{ post: result >= a }}\nvalidate max_of {{ post: result >= b }}\n");
    let program = parse(&src);
    assert_eq!(nirdosha::contract_check::check_program_contracts(&program), Ok(()));
}

// ---- Interprocedural reasoning: a proven callee's contract used as an axiom by its caller ----

const DOUBLE_AND_CALLER: &str = "
fn double(n: i64) -> i64 {
    return n * 2
}
fn double_then_add_one(n: i64) -> i64 {
    return double(n) + 1
}
fn main() -> i64 { return double_then_add_one(10) }
";

#[test]
fn a_call_to_an_unvalidated_fn_is_still_honestly_unsupported() {
    // No `validate double { ... }` at all -- exactly the pre-existing,
    // disclosed limit: a `Call` with no proven summary to use is
    // `Unsupported`, never silently approximated.
    let src = format!("{DOUBLE_AND_CALLER}\nvalidate double_then_add_one {{ post: result >= 1 }}\n");
    let program = parse(&src);
    assert_eq!(nirdosha::contract_check::check_program_contracts(&program), Ok(()));
    let notes = nirdosha::contract_check::unsupported_validate_notes(&program);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("double_then_add_one"), "note: {}", notes[0]);
}

#[test]
fn a_provably_true_contract_on_the_callee_lets_the_caller_be_proved_too() {
    // The real interprocedural case: `double`'s own contract is
    // provable on its own (integer, loop-free, no calls -- squarely
    // Tier-1's subset), and once it's `Proved`, `double_then_add_one`'s
    // own contract -- which calls `double` -- can *also* be proved, by
    // using `double`'s proven postcondition as a fact about the call's
    // result. Before this session's work, any `Call` at all was an
    // automatic `Unsupported`; this must now be a real `Ok(())`, not
    // just "didn't crash."
    let src = format!(
        "{DOUBLE_AND_CALLER}\nvalidate double {{ post: result == n * 2 }}\nvalidate double_then_add_one {{ post: result == n * 2 + 1 }}\n"
    );
    let program = parse(&src);
    assert_eq!(nirdosha::contract_check::check_program_contracts(&program), Ok(()));
    assert!(nirdosha::contract_check::unsupported_validate_notes(&program).is_empty());
    assert_eq!(nirdosha::run(&src), Ok(nirdosha::interpreter::Value::Int(21)));
}

#[test]
fn interprocedural_reasoning_still_finds_a_real_counterexample_in_the_caller() {
    // Same shape, but `double_then_add_one`'s own contract is actually
    // wrong (`result > n + 5`, not true for e.g. n = 0: double(0)+1 = 1,
    // not > 5) -- proves the interprocedural path doesn't just rubber-
    // stamp every caller once its callee is proven; it still has to
    // find a real violation when one exists.
    let src = format!(
        "{DOUBLE_AND_CALLER}\nvalidate double {{ post: result == n * 2 }}\nvalidate double_then_add_one {{ post: result > n + 5 }}\n"
    );
    let program = parse(&src);
    let err = nirdosha::contract_check::check_program_contracts(&program)
        .expect_err("double_then_add_one's own contract is genuinely false (n = 0 gives result = 1, not > 5)");
    assert!(err.iter().any(|e| e.contains("double_then_add_one")), "errors: {err:?}");
}

#[test]
fn an_unproven_callee_contract_never_gets_used_as_an_axiom() {
    // `double`'s own declared contract is FALSE (`result > n * 10`) --
    // it must never reach `Proved`, and therefore must never be
    // promoted into a summary the caller could unsoundly rely on. The
    // caller's own check must still be `Unsupported` (the callee's
    // contract was never independently proven), not silently pass by
    // trusting a false premise.
    let src = format!(
        "{DOUBLE_AND_CALLER}\nvalidate double {{ post: result > n * 10 }}\nvalidate double_then_add_one {{ post: result >= 1 }}\n"
    );
    let program = parse(&src);
    let err = nirdosha::contract_check::check_program_contracts(&program).expect_err("double's own contract is false");
    assert!(err.iter().any(|e| e.contains("double") && e.contains("violated")), "errors: {err:?}");
    // `double_then_add_one`'s own (true!) contract is still only
    // `Unsupported`, not wrongly `Proved` off the back of an unproven
    // summary, and not wrongly reported as violated either.
    let notes = nirdosha::contract_check::unsupported_validate_notes(&program);
    assert!(notes.iter().any(|n| n.contains("double_then_add_one")), "notes: {notes:?}");
}

// ---- Runtime backstop (a fn Tier-1's static walker honestly can't model: here, a `while` loop) ----

const COUNT_UP: &str = "
fn count_up(n: i64) -> i64 {
    let i: i64 = 0
    while i < n {
        i = i + 1
    }
    return i
}
";

#[test]
fn a_loop_shaped_fn_is_unsupported_statically_but_still_builds() {
    let src = format!("{COUNT_UP}\nvalidate count_up {{ post: result == n }}\nfn main() {{}}\n");
    let program = parse(&src);
    // Never a build-time error -- `Unsupported` isn't a proven defect.
    assert_eq!(nirdosha::contract_check::check_program_contracts(&program), Ok(()));
    let notes = nirdosha::contract_check::unsupported_validate_notes(&program);
    assert_eq!(notes.len(), 1);
    assert!(notes[0].contains("count_up"), "note: {}", notes[0]);
    assert!(notes[0].contains("runtime"), "note should say it's enforced at runtime instead: {}", notes[0]);
}

#[test]
fn a_true_postcondition_on_an_unsupported_fn_is_verified_for_real_at_runtime() {
    // `count_up`'s postcondition (`result == n`) is actually true --
    // Tier-1 can't prove it (the loop), but the runtime backstop
    // checks it against the real return value on the real call, and a
    // genuinely-correct fn must not be blocked by its own contract.
    let src = format!("{COUNT_UP}\nvalidate count_up {{ post: result == n }}\nfn main() -> i64 {{ return count_up(5) }}\n");
    assert_eq!(nirdosha::run(&src), Ok(nirdosha::interpreter::Value::Int(5)));
}

#[test]
fn a_false_postcondition_on_an_unsupported_fn_is_caught_at_runtime_not_silently_passed() {
    // Same loop shape, but the contract itself is wrong (`n + 1`, not
    // `n`) -- this is exactly the case a purely-static checker would
    // never catch (Tier-1 says `Unsupported`, not `Proved`); only the
    // runtime backstop can, and must.
    let src = format!(
        "{COUNT_UP}\nvalidate count_up {{ post: result == n + 1 }}\nfn main() -> i64 {{ return count_up(5) }}\n"
    );
    let err = nirdosha::run(&src).expect_err("a real postcondition violation must not be silently passed");
    assert!(err.contains("count_up"), "error: {err}");
    assert!(err.to_lowercase().contains("contract") || err.to_lowercase().contains("validate"), "error: {err}");
}

#[test]
fn a_violated_precondition_stops_the_body_from_running_at_all() {
    // Division by zero would itself be a runtime error -- this proves
    // the *precondition* is what actually fires, by checking the
    // reported error names the contract, not `DivByZero`.
    let src = "
fn reciprocal(n: i64) -> i64 {
    return 100 / n
}
validate reciprocal { pre: n != 0 }
fn main() -> i64 { return reciprocal(0) }
";
    let err = nirdosha::run(src).expect_err("n == 0 violates the precondition");
    assert!(err.contains("reciprocal"), "error: {err}");
    assert!(err.to_lowercase().contains("contract") || err.to_lowercase().contains("validate"), "error: {err}");
    assert!(!err.to_lowercase().contains("division"), "should report the *contract* failure, not fall through to DivByZero: {err}");
}

#[test]
fn a_satisfied_precondition_lets_the_body_run_normally() {
    let src = "
fn reciprocal(n: i64) -> i64 {
    return 100 / n
}
validate reciprocal { pre: n != 0 }
fn main() -> i64 { return reciprocal(4) }
";
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Int(25)));
}

#[test]
fn a_program_with_no_validate_blocks_at_all_is_completely_unaffected() {
    // No `validate` anywhere -- the whole mechanism must be a true
    // no-op, not just "usually harmless."
    let src = "fn double(n: i64) -> i64 { return n * 2 } fn main() -> i64 { return double(21) }";
    let program = parse(src);
    assert!(program.validates.is_empty());
    assert_eq!(nirdosha::contract_check::check_program_contracts(&program), Ok(()));
    assert!(nirdosha::contract_check::unsupported_validate_notes(&program).is_empty());
    assert_eq!(nirdosha::run(src), Ok(nirdosha::interpreter::Value::Int(42)));
}
