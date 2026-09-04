//! Tests for `lib.rs::Diagnostic`/`run_diagnostic` -- docs/goal.md row 9's
//! structured-diagnostics surface (Phase 0.5 of the unified development
//! plan). Each test below is deliberately built the same way: run a
//! program known to fail at one specific stage, confirm `run_diagnostic`
//! reports it as that stage's `Diagnostic` variant, then round-trip that
//! diagnostic through `serde_json` -- `--format=json`'s actual contract
//! (main.rs) is exactly this serialize/deserialize pair, so a test that
//! only checked `to_string` succeeds wouldn't be testing the same thing
//! an agent parsing the CLI's output actually needs to work.

use nirdosha::ast::Ty;
use nirdosha::typeck::{TypeError, TypeErrorKind};
use nirdosha::{run_diagnostic, Diagnostic, RunFailure};

fn round_trip(diags: &[Diagnostic]) -> Vec<Diagnostic> {
    let json = serde_json::to_string(diags).expect("Diagnostic always serializes");
    serde_json::from_str(&json).expect("Diagnostic must round-trip through its own JSON")
}

#[test]
fn a_type_error_diagnostic_round_trips_through_json() {
    let src = r#"
        fn main() {
            let x: i64 = "not an int"
        }
    "#;
    let diags = match run_diagnostic(src) {
        Err(RunFailure::Diagnostics(d)) => d,
        other => panic!("expected a Diagnostics failure, got {other:?}"),
    };
    assert_eq!(
        diags,
        vec![Diagnostic::Type(TypeError {
            kind: TypeErrorKind::TypeMismatch { expected: Ty::I64, found: Ty::Str },
            span: diags[0].span(),
        })]
    );
    assert_eq!(round_trip(&diags), diags);
}

#[test]
fn an_ownership_error_diagnostic_round_trips_through_json() {
    let src = r#"
        fn main() {
            let a: box i64 = box 1
            let b: box i64 = a
            let c: box i64 = a
        }
    "#;
    let diags = match run_diagnostic(src) {
        Err(RunFailure::Diagnostics(d)) => d,
        other => panic!("expected a Diagnostics failure, got {other:?}"),
    };
    assert!(matches!(diags[0], Diagnostic::Ownership(_)), "expected an Ownership diagnostic, got {diags:?}");
    assert_eq!(round_trip(&diags), diags);
}

#[test]
fn a_runtime_error_diagnostic_round_trips_through_json() {
    let src = r#"
        fn main() -> i64 {
            let a: i64 = 1
            let b: i64 = 0
            return a / b
        }
    "#;
    let diags = match run_diagnostic(src) {
        Err(RunFailure::Diagnostics(d)) => d,
        other => panic!("expected a Diagnostics failure, got {other:?}"),
    };
    assert!(matches!(diags[0], Diagnostic::Runtime(_)), "expected a Runtime diagnostic, got {diags:?}");
    assert_eq!(round_trip(&diags), diags);
}
