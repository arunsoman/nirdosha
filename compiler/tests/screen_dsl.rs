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
    struct Text {
        value: str,
    }

    struct Product {
        id: i64,
        name: str,
        price: i64,
    }

    fn list_product() -> Text { return Text("[]") }
    fn create_product(p: Product) -> Text { return Text(p.name) }
    fn update_product(p: Product) -> Text { return Text(p.name) }
    fn delete_product(id: i64) -> i64 { return id }
    fn restock_product(id: i64) -> i64 { return id }

    fn stat_product_count() -> i64 { return 0 }
    fn chart_products_by_price() -> Text { return Text("[]") }

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

const WITH_VALIDATION_FIELDS: &str = r#"
    struct Widget {
        id: i64,
        name: str,
        quantity: i64,
    }
    fn list_widget() -> i64 { return 0 }

    screen Widget {
        field name {
            pattern: "^[A-Za-z ]+$"
        }
        field quantity {
            min: 0
            max: 1000
        }
    }

    fn main() {}
"#;

#[test]
fn pattern_and_min_max_on_appropriate_fields_typecheck_cleanly() {
    let program = parse_ok(WITH_VALIDATION_FIELDS);
    typecheck(&program).expect("pattern on a str field + min/max on a numeric field should typecheck cleanly");
}

#[test]
fn pattern_on_non_str_field_is_rejected() {
    let src = r#"
        struct Widget {
            id: i64,
        }
        fn main() {}

        screen Widget {
            field id {
                pattern: "^[0-9]+$"
            }
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::FieldValidationTypeMismatch {
            struct_name: "Widget".to_string(),
            field_name: "id".to_string(),
            key: "pattern".to_string(),
            field_ty: "I64".to_string(),
        }
    );
}

#[test]
fn min_on_non_numeric_field_is_rejected() {
    let src = r#"
        struct Widget {
            id: i64,
            name: str,
        }
        fn main() {}

        screen Widget {
            field name {
                min: 0
            }
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::FieldValidationTypeMismatch {
            struct_name: "Widget".to_string(),
            field_name: "name".to_string(),
            key: "min".to_string(),
            field_ty: "Str".to_string(),
        }
    );
}

#[test]
fn pattern_value_must_be_a_string_literal() {
    let src = r#"
        struct Widget {
            id: i64,
            name: str,
        }
        fn main() {}

        screen Widget {
            field name {
                pattern: 5
            }
        }
    "#;
    assert_eq!(first_type_error(src), TypeErrorKind::InvalidFieldValidationExpr { key: "pattern".to_string() });
}

#[test]
fn min_value_must_be_a_number_literal() {
    let src = r#"
        struct Widget {
            id: i64,
            quantity: i64,
        }
        fn main() {}

        screen Widget {
            field quantity {
                min: "zero"
            }
        }
    "#;
    assert_eq!(first_type_error(src), TypeErrorKind::InvalidFieldValidationExpr { key: "min".to_string() });
}

#[test]
fn invalid_regex_pattern_is_rejected() {
    let src = r#"
        struct Widget {
            id: i64,
            name: str,
        }
        fn main() {}

        screen Widget {
            field name {
                pattern: "["
            }
        }
    "#;
    match first_type_error(src) {
        TypeErrorKind::InvalidRegexPattern { struct_name, field_name, .. } => {
            assert_eq!(struct_name, "Widget");
            assert_eq!(field_name, "name");
        }
        other => panic!("expected InvalidRegexPattern, got {other:?}"),
    }
}

#[test]
fn format_expands_and_typechecks_cleanly() {
    let src = r#"
        struct Contact {
            id: i64,
            email: str,
        }
        fn list_contact() -> i64 { return 0 }

        screen Contact {
            field email {
                format: "email"
            }
        }

        fn main() {}
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("a known `format` on a str field should typecheck cleanly");
}

#[test]
fn unknown_format_is_rejected() {
    let src = r#"
        struct Contact {
            id: i64,
            email: str,
        }
        fn main() {}

        screen Contact {
            field email {
                format: "not-a-real-format"
            }
        }
    "#;
    assert_eq!(first_type_error(src), TypeErrorKind::UnknownFieldFormat { format: "not-a-real-format".to_string() });
}

#[test]
fn pattern_and_format_together_is_rejected() {
    let src = r#"
        struct Contact {
            id: i64,
            email: str,
        }
        fn main() {}

        screen Contact {
            field email {
                pattern: "^.+@.+$"
                format: "email"
            }
        }
    "#;
    assert_eq!(
        first_type_error(src),
        TypeErrorKind::ConflictingPatternAndFormat { struct_name: "Contact".to_string(), field_name: "email".to_string() }
    );
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
        fn list_plain() -> i64 { return 0 }
        fn main() -> i64 { return list_plain() }
    "#;
    let program = parse_ok(src);
    assert!(program.screens.is_empty());
    assert!(program.dashboard.is_none());
    typecheck(&program).expect("a program with no screen/dashboard block should typecheck as before");
}
