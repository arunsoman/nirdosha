//! `typeck::collect_role_claim_strings` — demo mode's "what can I try"
//! catalog (`ui_gen.rs`'s `IDENTITY_CATALOG`, `nirdosha serve` with no
//! `--jwks-file`/`--issuer`/`--audience`). Walks three unrelated places
//! role/claim strings appear in the grammar — a `fn`'s own
//! `requires(...)`, a `screen` field's `view`/`edit` gate, and a
//! `workflow` state's `owner` gate — and dedupes/sorts the result.

fn parse_ok(src: &str) -> nirdosha::ast::Program {
    let toks = nirdosha::token::Lexer::new(src).tokenize().expect("lex should succeed");
    nirdosha::parser::Parser::new(toks).parse_program().expect("parse should succeed")
}

fn catalog(src: &str) -> (Vec<String>, Vec<(String, String)>) {
    let program = parse_ok(src);
    nirdosha::typeck::typecheck_optional_main(&program).expect("should typecheck cleanly");
    nirdosha::typeck::collect_role_claim_strings(&program)
}

#[test]
fn a_requires_role_and_requires_claim_are_both_collected() {
    let (roles, claims) = catalog(
        r#"
        fn a() -> i64 requires(role: "admin") { return 1 }
        fn b() -> i64 requires(claim: "department", "cardiology") { return 1 }
        "#,
    );
    assert_eq!(roles, vec!["admin".to_string()]);
    assert_eq!(claims, vec![("department".to_string(), "cardiology".to_string())]);
}

#[test]
fn duplicate_roles_across_functions_are_deduped() {
    let (roles, _) = catalog(
        r#"
        fn a() -> i64 requires(role: "admin") { return 1 }
        fn b() -> i64 requires(role: "admin") { return 1 }
        fn c() -> i64 requires(role: "staff") { return 1 }
        "#,
    );
    assert_eq!(roles, vec!["admin".to_string(), "staff".to_string()], "should dedupe and sort, got {roles:?}");
}

#[test]
fn a_role_call_naming_more_than_one_role_collects_every_name() {
    let (roles, _) = catalog(
        r#"
        struct Widget { id: i64, name: str }
        fn list_widget() -> i64 { return 0 }
        screen Widget {
            field name {
                view: role("admin", "analyst")
            }
        }
        fn main() {}
        "#,
    );
    assert_eq!(roles, vec!["admin".to_string(), "analyst".to_string()]);
}

#[test]
fn a_screen_field_edit_claim_gate_is_collected() {
    let (_, claims) = catalog(
        r#"
        struct Widget { id: i64, name: str }
        fn list_widget() -> i64 { return 0 }
        screen Widget {
            field name {
                edit: claim("department", "eng")
            }
        }
        fn main() {}
        "#,
    );
    assert_eq!(claims, vec![("department".to_string(), "eng".to_string())]);
}

#[test]
fn a_workflow_state_owner_role_is_collected() {
    let (roles, _) = catalog(
        r#"
        workflow Approval {
            data { amount_cents: i64 }
            state Pending {
                owner: role("manager")
                on Approved -> Done
            }
            state Done terminal {}
        }
        fn main() {}
        "#,
    );
    assert!(roles.contains(&"manager".to_string()), "workflow state owner role should be collected, got {roles:?}");
}

#[test]
fn a_program_with_no_role_or_claim_gates_at_all_yields_two_empty_lists() {
    let (roles, claims) = catalog("fn f() -> i64 requires(public) { return 1 }");
    assert!(roles.is_empty(), "got {roles:?}");
    assert!(claims.is_empty(), "got {claims:?}");
}
