//! `docs/ROADMAP.md` A10 / `docs/API_TRUST_MODEL.md` §4: `serve.rs::dispatch` is
//! default-open — any `fn` with no `requires(...)` and no
//! `VerifiedIdentity` parameter is routed and callable by anyone, with
//! no token at all. `typeck::ungated_fn_warnings` is the fix this session
//! shipped: a non-fatal diagnostic (never blocks compilation, unlike a
//! `TypeError`) surfacing exactly that case, silenced by `requires(role/
//! claim: ...)`, a `VerifiedIdentity` parameter, or the new explicit
//! `requires(public)` marker.

use nirdosha::typeck::TypeWarningKind;

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = nirdosha::token::Lexer::new(src).tokenize().expect("lex should succeed");
    nirdosha::parser::Parser::new(toks).parse_program().expect("parse should succeed")
}

fn warned_fn_names(src: &str) -> Vec<String> {
    let program = parse_ok(src);
    nirdosha::typeck::typecheck_optional_main(&program).expect("should typecheck cleanly");
    nirdosha::typeck::ungated_fn_warnings(&program)
        .into_iter()
        .map(|w| match w.kind {
            TypeWarningKind::UngatedFnReachableWithNoToken { fn_name } => fn_name,
            // `ungated_fn_warnings` only ever produces the variant above
            // — `WorkflowStateHasNoOwner` is `workflow_owner_warnings`'s
            // own, separate pass (`docs/WORKFLOW.md`'s "state ownership"
            // section).
            other => unreachable!("ungated_fn_warnings should never produce {other:?}"),
        })
        .collect()
}

#[test]
fn a_plain_fn_with_no_gate_at_all_warns() {
    let names = warned_fn_names("fn get_widget(id: i64) -> i64 { return id }");
    assert_eq!(names, vec!["get_widget".to_string()]);
}

#[test]
fn requires_role_silences_the_warning() {
    let names = warned_fn_names(
        r#"fn get_widget(id: i64) -> i64 requires(role: "admin") { return id }"#,
    );
    assert!(names.is_empty(), "requires(role: ...) should silence the warning, got {names:?}");
}

#[test]
fn requires_claim_silences_the_warning() {
    let names = warned_fn_names(
        r#"fn get_widget(id: i64) -> i64 requires(claim: "dept", "eng") { return id }"#,
    );
    assert!(names.is_empty(), "requires(claim: ...) should silence the warning, got {names:?}");
}

#[test]
fn a_verified_identity_parameter_silences_the_warning() {
    let names = warned_fn_names(
        "fn get_widget(identity: VerifiedIdentity, id: i64) -> i64 { return id }",
    );
    assert!(names.is_empty(), "a VerifiedIdentity param should silence the warning, got {names:?}");
}

#[test]
fn a_db_parameter_silences_the_warning() {
    let names = warned_fn_names(
        "fn get_widget_inner(conn: db, id: i64) -> i64 { return id }",
    );
    assert!(
        names.is_empty(),
        "a `db` param already 400s at serve.rs::decode_value regardless, so it shouldn't also warn, got {names:?}"
    );
}

#[test]
fn a_mq_parameter_silences_the_warning() {
    let names = warned_fn_names("fn get_widget_inner(q: mq, id: i64) -> i64 { return id }");
    assert!(names.is_empty(), "a `mq` param should silence the warning the same way `db` does, got {names:?}");
}

/// The explicit escape hatch this fix adds — `requires(public)` — must
/// silence the warning without gating the function the way `requires(role/
/// claim: ...)` does: a `requires(public)` fn stays a plain, directly
/// callable value (`FnDecl::requires` stays `None`), not one that needs
/// `acquire`.
#[test]
fn requires_public_silences_the_warning_without_gating_direct_calls() {
    let src = r#"
fn get_widget(id: i64) -> i64 requires(public) { return id }
fn main() -> i64 { return get_widget(7) }
"#;
    let program = parse_ok(src);
    nirdosha::typeck::typecheck(&program).expect("requires(public) must not block a direct call");
    // `main` itself has no `requires(...)`/`VerifiedIdentity` param
    // either, so it correctly warns too (confirmed against the shipped
    // trade-finance example, which ends in a no-op `fn main() {}` and
    // gets warned about by name) — this test only asserts `get_widget`
    // isn't among the warned names.
    let names = warned_fn_names(src);
    assert!(!names.contains(&"get_widget".to_string()), "requires(public) should silence the warning, got {names:?}");
}

#[test]
fn an_unknown_requirement_kind_other_than_role_claim_public_is_a_parse_error() {
    let toks = nirdosha::token::Lexer::new(r#"fn f() -> i64 requires(bogus: "x") { return 1 }"#)
        .tokenize()
        .expect("lex should succeed");
    let err = nirdosha::parser::Parser::new(toks)
        .parse_program()
        .expect_err("an unknown requirement kind should be a parse error");
    assert!(err.message.contains("role"), "error should mention the expected kinds: {}", err.message);
    assert!(err.message.contains("public"), "error should mention `public` as an option: {}", err.message);
}
