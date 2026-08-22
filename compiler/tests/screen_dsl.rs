//! Tests for Row 12's `screen`/`dashboard` DSL — explicit, typechecked
//! UI authoring layered on top of (never replacing) `ui_gen.rs`'s pure
//! convention-based inference. See the design doc (published as the
//! "Nirdosha UI Engine" artifact, Option 4) and `GRAMMAR.md`'s
//! `screen_decl`/`dashboard_decl` productions for the full spec this
//! implements the existence/shape-checking core of.

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

const WELL_FORMED: &str = r#"
    struct Product {
        id: i64,
        name: str,
        price: i64,
    }

    fn list_product() -> str { return "[]" }
    fn create_product(p: Product) -> str { return p.name }
    fn update_product(p: Product) -> str { return p.name }
    fn delete_product(id: i64) -> i64 { return id }
    fn restock_product(id: i64) -> i64 { return id }

    fn stat_product_count() -> i64 { return 0 }
    fn chart_products_by_price() -> str { return "[]" }

    screen Product {
        title: "Catalog"
        list: list_product
        create: create_product
        update: update_product
        delete: delete_product
        paginate {
            page_size: 25
            total: stat_product_count
        }
        field name {
            label: "Product Name"
            searchable: true
            sortable: true
        }
        field price {
            view: role("admin", "analyst")
            edit: claim("department", "sales")
        }
        action "Restock" -> restock_product {
            style: "outlined"
            confirm: "Restock this product?"
        }
    }

    dashboard {
        tile "Products" -> stat_product_count
        chart "By Price" -> chart_products_by_price
    }

    fn main() {}
"#;

#[test]
fn well_formed_screen_and_dashboard_parse_and_typecheck_cleanly() {
    let program = parse_ok(WELL_FORMED);
    assert_eq!(program.screens.len(), 1);
    assert_eq!(program.screens[0].struct_name, "Product");
    assert_eq!(program.screens[0].fields.len(), 2);
    assert_eq!(program.screens[0].actions.len(), 1);
    assert!(program.dashboard.is_some());
    assert_eq!(program.dashboard.as_ref().unwrap().tiles.len(), 1);
    assert_eq!(program.dashboard.as_ref().unwrap().charts.len(), 1);

    typecheck(&program).expect("well-formed screen/dashboard should typecheck cleanly");
}

#[test]
fn well_formed_screen_round_trips_through_json() {
    // Same shape `emit-ast` exercises (`cmd_emit_ast`, main.rs) — proves
    // the new AST nodes actually serialize, not just compile.
    let program = parse_ok(WELL_FORMED);
    let json = serde_json::to_string_pretty(&program).expect("Program should serialize with screens/dashboard");
    assert!(json.contains("\"screens\""));
    assert!(json.contains("\"dashboard\""));
    assert!(json.contains("Restock"));
}

#[test]
fn paginate_entries_are_flattened_with_a_prefix() {
    let program = parse_ok(WELL_FORMED);
    let entries = &program.screens[0].entries;
    let has = |k: &str| entries.iter().any(|(key, _)| key == k);
    assert!(has("paginate.page_size"));
    assert!(has("paginate.total"));
}

#[test]
fn screen_on_unknown_struct_is_rejected() {
    let src = r#"
        screen Ghost {
            title: "Nope"
        }
        fn main() {}
    "#;
    assert_eq!(first_type_error(src), TypeErrorKind::UnknownScreenStruct("Ghost".to_string()));
}

#[test]
fn field_override_on_unknown_field_is_rejected() {
    let src = r#"
        struct Widget {
            id: i64,
        }
        fn main() {}

        screen Widget {
            field nonexistent {
                label: "Huh"
            }
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::UnknownScreenField {
            struct_name: "Widget".to_string(),
            field_name: "nonexistent".to_string(),
        }
    );
}

#[test]
fn list_target_naming_an_undeclared_function_is_rejected() {
    let src = r#"
        struct Widget {
            id: i64,
        }
        fn main() {}

        screen Widget {
            list: list_widget_does_not_exist
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::ScreenFnNotFound {
            key: "list".to_string(),
            fn_name: "list_widget_does_not_exist".to_string(),
        }
    );
}

#[test]
fn action_target_naming_an_undeclared_function_is_rejected() {
    let src = r#"
        struct Widget {
            id: i64,
        }
        fn main() {}

        screen Widget {
            action "Do it" -> nonexistent_fn
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::ScreenFnNotFound {
            key: "action".to_string(),
            fn_name: "nonexistent_fn".to_string(),
        }
    );
}

#[test]
fn view_visibility_must_be_a_role_or_claim_call() {
    let src = r#"
        struct Widget {
            id: i64,
        }
        fn main() {}

        screen Widget {
            field id {
                view: 5
            }
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::InvalidVisibilityExpr { key: "view".to_string() }
    );
}

#[test]
fn dashboard_tile_naming_an_undeclared_function_is_rejected() {
    let src = r#"
        fn main() {}

        dashboard {
            tile "Ghost Metric" -> stat_does_not_exist
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::UnknownDashboardFn {
            metric_kind: "tile".to_string(),
            fn_name: "stat_does_not_exist".to_string(),
        }
    );
}

#[test]
fn a_second_dashboard_block_is_a_parse_error() {
    let src = r#"
        dashboard { tile "A" -> stat_a }
        dashboard { tile "B" -> stat_b }
        fn main() {}
    "#;
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let err = Parser::new(toks).parse_program().expect_err("a second dashboard block should be rejected");
    assert!(err.message.contains("only one"), "unexpected message: {}", err.message);
}

#[test]
fn struct_with_no_screen_block_is_completely_unaffected() {
    // The progressive-fallback promise: `screens`/`dashboard` start empty
    // for a program that never uses the DSL, and typeck raises nothing
    // new for it.
    let src = r#"
        struct Plain {
            id: i64,
        }
        fn list_plain() -> str { return "[]" }
        fn main() -> str { return list_plain() }
    "#;
    let program = parse_ok(src);
    assert!(program.screens.is_empty());
    assert!(program.dashboard.is_none());
    typecheck(&program).expect("a program with no screen/dashboard block should typecheck as before");
}
