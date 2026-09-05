//! Tests for docs/goal.md row 9's AST-export/fragment-validation surface
//! (unified plan §4.2.2): `Program`/`Expr` round-tripping through
//! `serde_json` (the same shape `nirdosha emit-ast` — main.rs — prints),
//! and `typeck.rs::validate_fragment` — a single expression fragment
//! type-checked against a caller-supplied expected type and variable
//! environment, without needing a whole enclosing function.

use nirdosha::ast::{Expr, Program, Ty};
use nirdosha::parser::Parser;
use nirdosha::token::{Lexer, Span};
use nirdosha::typeck::{FragmentEnv, TypeErrorKind};
use nirdosha::Diagnostic;

fn parse_ok(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

// `examples/` is two nested directories now (`syntax/`, `features/`),
// not a flat pile of `.nir` files -- a plain `read_dir` would silently
// see zero of them (a directory entry has no `.nir` extension, so the
// filter below would skip it and never look inside), so this walks the
// tree instead of assuming it's flat.
fn collect_nir_files(dir: &std::path::Path, out: &mut Vec<std::path::PathBuf>) {
    for entry in std::fs::read_dir(dir).unwrap_or_else(|e| panic!("failed to read {dir:?}: {e}")) {
        let path = entry.expect("dir entry should read").path();
        if path.is_dir() {
            collect_nir_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("nir") {
            out.push(path);
        }
    }
}

// ---- whole-program AST round-trip -------------------------------------------

#[test]
fn a_whole_program_round_trips_through_json() {
    let src = include_str!("fixtures/matrices.nir");
    let program = parse_ok(src);
    let json = serde_json::to_string(&program).expect("Program should serialize");
    let back: Program = serde_json::from_str(&json).expect("Program should deserialize");
    // `Program`/`FnDecl`/etc. don't derive `PartialEq` (nothing else
    // needed it before this), so compare via a second round of JSON
    // rather than adding a derive whose only consumer would be this one
    // test.
    let json2 = serde_json::to_string(&back).expect("round-tripped Program should re-serialize");
    assert_eq!(json, json2);
}

#[test]
fn every_example_program_round_trips_through_json() {
    // A broader sweep than the single-file test above: every `.nir`
    // example this repo ships exercises a different slice of the
    // grammar (str/tcp/sandbox/chan/spawn/...), so this is the cheapest
    // way to catch a variant whose `Serialize`/`Deserialize` derive
    // silently doesn't round-trip (e.g. a hand-written `impl` that
    // shadowed the derive incorrectly -- not the case here, but this is
    // the test that would catch it if it ever were).
    let mut paths = Vec::new();
    collect_nir_files(std::path::Path::new("../../examples"), &mut paths);
    assert!(!paths.is_empty(), "expected to find at least one .nir file under examples/");
    for path in paths {
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path:?}: {e}"));
        let program = parse_ok(&src);
        let json = serde_json::to_string(&program).unwrap_or_else(|e| panic!("{path:?} failed to serialize: {e}"));
        let back: Program = serde_json::from_str(&json).unwrap_or_else(|e| panic!("{path:?} failed to deserialize: {e}"));
        let json2 = serde_json::to_string(&back).unwrap_or_else(|e| panic!("{path:?} round-trip mismatch: {e}"));
        assert_eq!(json, json2, "{path:?} did not round-trip byte-for-byte");
    }
}

// ---- fragment validation ----------------------------------------------------

fn expr_json(src: &str) -> String {
    // Parses `src` as a standalone expression (via a throwaway `let`)
    // and re-serializes just the initializer -- the cheapest way to get
    // a real `Expr`'s JSON without hand-writing it.
    let wrapped = format!("fn main() {{ let x: i64 = {src} }}");
    let program = parse_ok(&wrapped);
    let nirdosha::ast::Stmt::Let { value, .. } = &program.fns[0].body.stmts[0] else {
        panic!("expected a Let statement");
    };
    serde_json::to_string(value).expect("Expr should serialize")
}

#[test]
fn a_well_typed_fragment_is_accepted() {
    let json = expr_json("a + b");
    let env = FragmentEnv::new().with("a", Ty::I64).with("b", Ty::I64);
    let result = nirdosha::typeck::validate_fragment(&json, &Ty::I64, &env);
    assert!(matches!(result, Ok(Expr::Binary(..))), "expected Ok(Binary), got {result:?}");
}

#[test]
fn a_fragment_against_the_wrong_expected_type_is_rejected() {
    let json = expr_json("a + b");
    let env = FragmentEnv::new().with("a", Ty::I64).with("b", Ty::I64);
    let result = nirdosha::typeck::validate_fragment(&json, &Ty::Bool, &env);
    match result {
        Err(diags) => assert_eq!(
            diags,
            vec![Diagnostic::Type(nirdosha::typeck::TypeError {
                kind: TypeErrorKind::TypeMismatch { expected: Ty::Bool, found: Ty::I64 },
                span: diags_span(&diags),
            })]
        ),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn a_fragment_referencing_an_unknown_variable_is_rejected() {
    let json = expr_json("unknown_var");
    let env = FragmentEnv::new();
    let result = nirdosha::typeck::validate_fragment(&json, &Ty::I64, &env);
    match result {
        Err(diags) => assert!(
            matches!(&diags[0], Diagnostic::Type(e) if matches!(e.kind, TypeErrorKind::UnknownVar(_))),
            "expected UnknownVar, got {diags:?}"
        ),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn malformed_fragment_json_is_rejected_with_its_own_diagnostic() {
    let env = FragmentEnv::new();
    let result = nirdosha::typeck::validate_fragment("not valid json", &Ty::I64, &env);
    match result {
        Err(diags) => assert!(
            matches!(&diags[0], Diagnostic::Type(e) if matches!(e.kind, TypeErrorKind::MalformedFragmentJson { .. })),
            "expected MalformedFragmentJson, got {diags:?}"
        ),
        other => panic!("expected a rejection, got {other:?}"),
    }
}

#[test]
fn a_fragment_diagnostic_round_trips_through_json() {
    let json = expr_json("a + b");
    let env = FragmentEnv::new().with("a", Ty::I64).with("b", Ty::I64);
    let Err(diags) = nirdosha::typeck::validate_fragment(&json, &Ty::Bool, &env) else {
        panic!("expected a rejection");
    };
    let serialized = serde_json::to_string(&diags).expect("Diagnostic should serialize");
    let back: Vec<Diagnostic> = serde_json::from_str(&serialized).expect("Diagnostic should deserialize");
    assert_eq!(diags, back);
}

fn diags_span(diags: &[Diagnostic]) -> Span {
    diags[0].span()
}
