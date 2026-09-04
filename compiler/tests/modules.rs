//! Tests for the real module/package system (`ROADMAP.md` Track F, F2;
//! `NEXT_GEN.md` §F2) — namespacing (`module Ident { ... }`),
//! visibility (`pub`), and separate compilation (`use "path.nir"`).
//! Three things get exercised end to end, not just typechecked: the
//! two documented collision bugs this closes (`struct Pair` vs. the
//! prelude's own, and two enums sharing a variant name — see
//! `ast::scope_key`'s doc comment), that the legacy string-named
//! `module "Display Name" { ... }` form is completely unaffected, and
//! that `pub`/`use` actually gate/merge what they claim to at runtime,
//! not just at typecheck time.

use nirdosha::ast::Program;
use nirdosha::interpreter::Value;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;

fn parse(src: &str) -> Program {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    Parser::new(toks).parse_program().expect("parse should succeed")
}

fn first_type_error_message(src: &str) -> String {
    let program = parse(src);
    match typecheck(&program) {
        Ok(()) => panic!("expected a type error, but the program type-checked cleanly"),
        Err(errors) => errors.into_iter().next().unwrap().to_string(),
    }
}

/// A fresh directory under the OS temp dir, unique per test — same
/// "real files on disk, unique per test run" discipline
/// `tests/file_io.rs::temp_path` already establishes, needed here
/// because `loader::load_program`/`use` resolution is inherently a
/// real-filesystem concern (relative-path imports).
fn temp_dir(name: &str) -> std::path::PathBuf {
    let mut p = std::env::temp_dir();
    p.push(format!("nirdosha_modules_test_{}_{}", std::process::id(), name));
    std::fs::create_dir_all(&p).unwrap();
    p
}

// ---- Legacy `module "Display Name" { ... }` is completely unaffected ----

#[test]
fn legacy_string_named_module_still_typechecks_and_runs_unchanged() {
    let src = r#"
        module "Reporting" {
            struct Row {
                id: i64,
            }
            fn make_row(id: i64) -> Row {
                return Row(id)
            }
        }
        fn main() -> i64 {
            let r: Row = make_row(7)
            return r.id
        }
    "#;
    let program = parse(src);
    assert_eq!(typecheck(&program), Ok(()));
    assert_eq!(nirdosha::run(src), Ok(Value::Int(7)));
}

// ---- Namespacing: the documented `struct Pair` vs. prelude collision ----

#[test]
fn namespaced_struct_sharing_a_name_with_the_prelude_does_not_error() {
    let src = r#"
        module Mine {
            pub struct Pair {
                first: i64,
                second: i64,
            }
        }
        fn main() -> i64 {
            let p: Mine::Pair = Mine::Pair(3, 4)
            return p.first
        }
    "#;
    let program = parse(src);
    assert_eq!(typecheck(&program), Ok(()));
    assert_eq!(nirdosha::run(src), Ok(Value::Int(3)));
}

#[test]
fn a_bare_reference_to_a_name_that_also_exists_namespaced_still_resolves_to_the_top_level_one() {
    // `scope_key`'s core invariant: a bare reference can only ever
    // match a `ns: None` registration, never a namespaced one, even
    // when a namespaced item of the same short name exists elsewhere
    // in the very same program — so this constructs the *prelude's*
    // `Pair`, not `Mine::Pair`, with zero ambiguity error.
    let src = r#"
        module Mine {
            pub struct Pair {
                first: i64,
                second: i64,
            }
        }
        fn main() -> i64 {
            let p: Pair(i64, i64) = Pair(9, 10)
            return p.first
        }
    "#;
    let program = parse(src);
    assert_eq!(typecheck(&program), Ok(()));
    assert_eq!(nirdosha::run(src), Ok(Value::Int(9)));
}

// ---- Namespacing: the documented `CurrencyCode::SAR`-shaped collision ----

#[test]
fn two_enums_sharing_a_variant_name_compile_and_run_when_one_is_namespaced() {
    let src = r#"
        module Mine {
            pub enum ReportType {
                SAR,
                STR,
            }
        }
        fn classify(r: Mine::ReportType) -> i64 {
            return match r {
                Mine::ReportType::SAR => 1,
                Mine::ReportType::STR => 2,
            }
        }
        fn main() -> i64 {
            let r: Mine::ReportType = Mine::ReportType::SAR()
            let c: CurrencyCode = CurrencyCode::SAR()
            return classify(r)
        }
    "#;
    let program = parse(src);
    assert_eq!(typecheck(&program), Ok(()));
    assert_eq!(nirdosha::run(src), Ok(Value::Int(1)));
}

#[test]
fn two_top_level_enums_sharing_a_bare_variant_name_is_still_a_duplicate_constructor_error() {
    // The genuinely-ambiguous case (neither enum namespaced) is exactly
    // as strict as before F2 — this isn't a new hole, only the
    // namespaced escape hatch above is new.
    let src = r#"
        enum ReportType {
            SAR,
        }
        fn main() -> i64 { return 0 }
    "#;
    let msg = first_type_error_message(src);
    assert!(msg.contains("SAR"), "expected a DuplicateConstructor-shaped error mentioning `SAR`, got: {msg}");
}

// ---- Visibility (`pub`) ----

#[test]
fn a_private_namespaced_item_referenced_qualified_from_another_module_is_rejected() {
    let src = r#"
        module Audit {
            struct Secret {
                code: i64,
            }
        }
        module Other {
            pub fn peek(s: Audit::Secret) -> i64 {
                return s.code
            }
        }
        fn main() -> i64 { return 0 }
    "#;
    let msg = first_type_error_message(src);
    assert!(msg.contains("private"), "expected a PrivateItem error, got: {msg}");
}

#[test]
fn a_private_namespaced_item_referenced_qualified_from_its_own_module_is_allowed() {
    // `Secret` is never `pub` — every reference to it, including its
    // own module's `make`/`round_trip` below, has to be qualified
    // (`Audit::Secret`, per `scope_key`'s "no implicit same-module
    // bare access" rule), and that qualified self-reference must
    // still succeed despite `exported: false` — `main` only ever sees
    // the `pub` `i64` `round_trip` returns, never `Secret` itself, so
    // this doesn't also (incorrectly) exercise the *rejected* cross-
    // module case `a_private_namespaced_item_referenced_qualified_
    // from_another_module_is_rejected` above already covers.
    let src = r#"
        module Audit {
            struct Secret {
                code: i64,
            }
            pub fn round_trip(code: i64) -> i64 {
                let s: Audit::Secret = Audit::Secret(code)
                return s.code
            }
        }
        fn main() -> i64 {
            return Audit::round_trip(5)
        }
    "#;
    let program = parse(src);
    assert_eq!(typecheck(&program), Ok(()));
    assert_eq!(nirdosha::run(src), Ok(Value::Int(5)));
}

// ---- Separate compilation (`use`) ----

#[test]
fn use_imports_a_pub_namespaced_item_from_another_file_and_it_runs() {
    let dir = temp_dir("use_basic");
    std::fs::write(
        dir.join("audit.nir"),
        r#"
        module Audit {
            pub struct Entry {
                id: i64,
            }
            pub fn make(id: i64) -> Audit::Entry {
                return Audit::Entry(id)
            }
        }
        "#,
    )
    .unwrap();
    let main_path = dir.join("main.nir");
    std::fs::write(
        &main_path,
        r#"
        use "audit.nir"
        fn main() -> i64 {
            let e: Audit::Entry = Audit::make(11)
            return e.id
        }
        "#,
    )
    .unwrap();
    let (program, src) = nirdosha::loader::load_program(main_path.to_str().unwrap()).expect("load should succeed");
    assert_eq!(typecheck(&program), Ok(()));
    let result = nirdosha::run_program_with_tracer_transact_and_workflow_log(program, &src, None, None, None);
    assert_eq!(result, Ok(Value::Int(11)));
}

#[test]
fn use_does_not_import_a_non_exported_item() {
    let dir = temp_dir("use_private");
    std::fs::write(
        dir.join("audit.nir"),
        r#"
        module Audit {
            struct Secret {
                code: i64,
            }
        }
        "#,
    )
    .unwrap();
    let main_path = dir.join("main.nir");
    std::fs::write(
        &main_path,
        r#"
        use "audit.nir"
        fn main() -> i64 {
            let s: Audit::Secret = Audit::Secret(1)
            return s.code
        }
        "#,
    )
    .unwrap();
    // `load_program` itself only parses+merges (typechecking the
    // *entry* file is the caller's job, same as a plain single-file
    // `parse` — see its own doc comment) — the absence of `Secret`
    // from the merge shows up as an ordinary `UnknownType`/`UnknownFn`
    // once the merged program is actually typechecked.
    let (program, _src) = nirdosha::loader::load_program(main_path.to_str().unwrap()).expect("load should succeed");
    let errors = typecheck(&program).expect_err("a non-pub item must not have been merged in");
    assert!(
        errors.iter().any(|e| e.to_string().contains("unknown")),
        "expected an unknown-type/fn error, got: {errors:?}"
    );
}

#[test]
fn use_does_not_import_a_non_namespaced_top_level_item() {
    let dir = temp_dir("use_top_level");
    std::fs::write(
        dir.join("lib.nir"),
        r#"
        struct Loose {
            n: i64,
        }
        "#,
    )
    .unwrap();
    let main_path = dir.join("main.nir");
    std::fs::write(
        &main_path,
        r#"
        use "lib.nir"
        fn main() -> i64 {
            let l: Loose = Loose(1)
            return l.n
        }
        "#,
    )
    .unwrap();
    let (program, _src) = nirdosha::loader::load_program(main_path.to_str().unwrap()).expect("load should succeed");
    let errors = typecheck(&program).expect_err("a non-namespaced top-level decl must not have been merged in");
    assert!(
        errors.iter().any(|e| e.to_string().contains("unknown")),
        "expected an unknown-type/fn error, got: {errors:?}"
    );
}

#[test]
fn an_import_cycle_is_a_clean_error_not_a_hang() {
    let dir = temp_dir("use_cycle");
    std::fs::write(dir.join("a.nir"), "use \"b.nir\"\n").unwrap();
    std::fs::write(dir.join("b.nir"), "use \"a.nir\"\n").unwrap();
    let err = nirdosha::loader::load_program(dir.join("a.nir").to_str().unwrap()).expect_err("a cycle must be a clean error");
    assert!(err.contains("cycle"), "expected an import-cycle error, got: {err}");
}

#[test]
fn two_imports_declaring_the_same_module_id_collide() {
    let dir = temp_dir("use_dup_module");
    std::fs::write(
        dir.join("one.nir"),
        r#"
        module Shared {
            pub struct Item {
                n: i64,
            }
        }
        "#,
    )
    .unwrap();
    std::fs::write(
        dir.join("two.nir"),
        r#"
        module Shared {
            pub struct Other {
                n: i64,
            }
        }
        "#,
    )
    .unwrap();
    let main_path = dir.join("main.nir");
    std::fs::write(
        &main_path,
        r#"
        use "one.nir"
        use "two.nir"
        fn main() -> i64 { return 0 }
        "#,
    )
    .unwrap();
    let err =
        nirdosha::loader::load_program(main_path.to_str().unwrap()).expect_err("two same-named modules must collide");
    assert!(err.contains("Shared"), "expected a module-collision error naming `Shared`, got: {err}");
}
