//! The fidelity corpus for `crates/compiler/nirdosha.gbnf` (unified plan
//! §4.2.3): two independent checks, neither of which alone would be
//! trustworthy.
//!
//! 1. `validate_gbnf` (a dev-dependency wrapping llama.cpp's *actual*
//!    grammar parser, via the `llama-cpp-gbnf` crate) confirms the file
//!    is syntactically legal, non-left-recursive GBNF with a `root`
//!    rule — the real engine's own well-formedness check, not a
//!    self-report.
//! 2. `nirdosha_grammar_export::matches_fully` (this crate's own,
//!    general-purpose GBNF interpreter — see its module doc for why a
//!    second implementation, not the real engine, does the matching
//!    here) is run over every shipped `.nir` example (parsed
//!    successfully by the real `nirdosha` lexer+parser — the crate's own
//!    test suite already proves that) and a set of deliberately
//!    malformed snippets the real parser rejects. Agreement on both
//!    directions is the actual fidelity claim.

fn grammar_text() -> String {
    std::fs::read_to_string(concat!(env!("CARGO_MANIFEST_DIR"), "/../compiler/nirdosha.gbnf"))
        .expect("crates/compiler/nirdosha.gbnf should exist")
}

fn real_parser_accepts(src: &str) -> bool {
    let Ok(toks) = nirdosha::token::Lexer::new(src).tokenize() else {
        return false;
    };
    nirdosha::parser::Parser::new(toks).parse_program().is_ok()
}

// `examples/` is two nested directories now (`syntax/`, `features/`),
// not a flat pile of `.nir` files -- a plain `read_dir` would silently
// see zero of them, so this walks the tree instead of assuming it's flat.
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

#[test]
fn the_grammar_file_is_valid_gbnf_per_llama_cpps_real_parser() {
    let grammar = grammar_text();
    assert_eq!(llama_cpp_gbnf::validate_gbnf::validate_gbnf(&grammar, "root"), Ok(()));
}

#[test]
fn every_shipped_example_is_accepted_by_both_the_real_parser_and_the_gbnf_grammar() {
    let grammar = nirdosha_grammar_export::parse(&grammar_text());
    let examples_dir = concat!(env!("CARGO_MANIFEST_DIR"), "/../../examples");
    let mut checked = 0;
    let mut paths = Vec::new();
    collect_nir_files(std::path::Path::new(examples_dir), &mut paths);
    for path in paths {
        let src = std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("failed to read {path:?}: {e}"));
        assert!(real_parser_accepts(&src), "{path:?} should be accepted by the real parser (it's a shipped example)");
        assert!(
            nirdosha_grammar_export::matches_fully(&grammar, "root", &src),
            "{path:?} was accepted by the real parser but rejected by nirdosha.gbnf -- the grammar has drifted"
        );
        checked += 1;
    }
    assert!(checked >= 10, "expected to check at least 10 example files, found {checked}");
}

/// Small, self-contained snippets covering constructs not exercised by
/// the full example files above (or exercised in combinations too large
/// for a quick, targeted regression) -- each one independently valid
/// Nirdosha, confirmed against the real parser before being checked
/// against the grammar.
const POSITIVE_SNIPPETS: &[&str] = &[
    "fn main() {}",
    "fn f(a: i64, b: bool) -> i64 { return a }",
    "fn main() { let x: f64 = 3.14 }",
    "fn main() { let v: Vector(f64, 3) = [1.0, 2.0, 3.0] }",
    "fn main() { let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]] }",
    "fn main() { let v: Vector(f64, 2) = [1.0, 2.0] let x: f64 = v[0] }",
    "fn main() { let m: Matrix(f64, 2, 2) = [[1.0, 2.0], [3.0, 4.0]] let x: f64 = m[0, 1] }",
    "fn main() { let a: Vector(f64, 2) = [1.0, 2.0] let b: Vector(f64, 2) = [3.0, 4.0] let c: Vector(f64, 2) = a .* b }",
    "fn main() { let n: box i64 = box 1 let m: i64 = *n }",
    "fn main() { let n: i64 = 1 let r: &i64 = &n }",
    "fn main() { if true { print(1) } else { print(2) } }",
    "fn main() { while true { print(1) } }",
    "fn main() { let s: str = \"hello, world\\n\" }",
    "fn main() { let c: chan i64 = chan }",
    "fn main() { let t: thread i64 = spawn f() } fn f() -> i64 { return 1 }",
    "// a leading comment\nfn main() {\n    // a body comment\n    print(1)\n}\n",
];

/// Structurally malformed Nirdosha -- unbalanced braces, a bare `let`
/// with no value, a dangling operator, an unterminated string. Each one
/// confirmed to be a real parser rejection first: the grammar's job is
/// to reject the same shapes, not to catch *type* errors (`typeck.rs`'s
/// job, out of scope for a syntax grammar) -- so nothing here is merely
/// ill-typed, every one is a genuine parse failure.
const NEGATIVE_SNIPPETS: &[&str] = &[
    "fn main() {",
    "fn main() { let x: i64 = }",
    "fn main() { let x: i64 = 1 + }",
    "fn main() { let s: str = \"unterminated }",
    "fn main() { let v: Vector(f64,) = [1.0] }",
    "fn main() { let x: i64 = (1 + 2 }",
    "fn f(a: i64 b: i64) {}",
];

#[test]
fn positive_snippets_are_accepted_by_both() {
    let grammar = nirdosha_grammar_export::parse(&grammar_text());
    for src in POSITIVE_SNIPPETS {
        assert!(real_parser_accepts(src), "expected the real parser to accept: {src}");
        assert!(
            nirdosha_grammar_export::matches_fully(&grammar, "root", src),
            "real parser accepted but nirdosha.gbnf rejected: {src}"
        );
    }
}

#[test]
fn negative_snippets_are_rejected_by_both() {
    let grammar = nirdosha_grammar_export::parse(&grammar_text());
    for src in NEGATIVE_SNIPPETS {
        assert!(!real_parser_accepts(src), "expected the real parser to reject: {src}");
        assert!(
            !nirdosha_grammar_export::matches_fully(&grammar, "root", src),
            "real parser rejected but nirdosha.gbnf accepted: {src}"
        );
    }
}
