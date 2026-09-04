//! Tests for `screen <Struct> { layout { ... } }` — the composable
//! container/layout system (`docs/ROADMAP.md` Track F, F1 Phase A;
//! `docs/NEXT_GEN.md` §F1). The first genuinely recursive DSL construct
//! in this grammar (`parser::parse_layout_node` calls itself for a
//! container's children) — several tests here exist specifically to
//! prove the recursion actually composes, not just that one level
//! parses.

use nirdosha::ast::{LayoutNode, Program};
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::{typecheck, TypeErrorKind};

fn parse_ok(src: &str) -> Program {
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

const BASE: &str = r#"
    struct Text {
        value: str,
    }

    struct Case {
        id: i64,
        case_number: str,
        priority: i64,
    }

    fn list_case() -> Text { return Text("[]") }
    fn create_case(c: Case) -> Text { return Text(c.case_number) }
    fn update_case(c: Case) -> Text { return Text(c.case_number) }
    fn delete_case(id: i64) -> i64 { return id }
    fn escalate_case(id: i64) -> i64 { return id }
    fn main() {}
"#;

// ---- a screen with no `layout` block is completely unaffected ----

#[test]
fn a_screen_with_no_layout_block_typechecks_exactly_as_before() {
    let src = format!("{BASE}\nscreen Case {{ }}\n");
    let program = parse_ok(&src);
    assert_eq!(typecheck(&program), Ok(()));
    let screen = program.screens.iter().find(|s| s.struct_name == "Case").unwrap();
    assert!(screen.layout.is_none());
}

// ---- the core shape: containers, field/action refs, a widget leaf ----

#[test]
fn a_well_formed_layout_referencing_real_fields_and_actions_typechecks() {
    let src = format!(
        "{BASE}
        screen Case {{
            action \"Escalate\" -> escalate_case
            layout {{
                row {{
                    column {{
                        group \"Details\" {{
                            field case_number
                            field priority
                        }}
                        divider {{}}
                    }}
                    column {{
                        tabs {{
                            tab \"Actions\" {{
                                action \"Escalate\"
                            }}
                        }}
                    }}
                }}
            }}
        }}
        "
    );
    let program = parse_ok(&src);
    assert_eq!(typecheck(&program), Ok(()));
    let screen = program.screens.iter().find(|s| s.struct_name == "Case").unwrap();
    assert!(screen.layout.is_some(), "layout block should have parsed into ScreenDecl.layout");
}

// ---- recursion actually composes (row > column > group, not just one level) ----

#[test]
fn nested_containers_three_levels_deep_parse_into_a_real_tree() {
    let src = format!(
        "{BASE}
        screen Case {{
            layout {{
                row {{
                    column {{
                        group \"Details\" {{
                            field case_number
                        }}
                    }}
                }}
            }}
        }}
        "
    );
    let program = parse_ok(&src);
    let screen = program.screens.iter().find(|s| s.struct_name == "Case").unwrap();
    let root = screen.layout.as_ref().expect("layout should be present");
    // Root is the synthesized column wrapping every top-level item.
    let LayoutNode::Column { children, .. } = root else { panic!("root should be a Column") };
    let LayoutNode::Row { children, .. } = &children[0] else { panic!("expected a Row one level in") };
    let LayoutNode::Column { children, .. } = &children[0] else { panic!("expected a Column two levels in") };
    let LayoutNode::Group { children, .. } = &children[0] else { panic!("expected a Group three levels in") };
    let LayoutNode::Field { name, .. } = &children[0] else { panic!("expected a Field leaf four levels in") };
    assert_eq!(name, "case_number");
}

#[test]
fn a_grid_carries_its_columns_entry_through_to_the_manifest() {
    let src = format!(
        "{BASE}
        screen Case {{
            layout {{
                grid {{
                    columns: 3
                    field case_number
                    field priority
                }}
            }}
        }}
        "
    );
    let program = parse_ok(&src);
    assert_eq!(typecheck(&program), Ok(()));
    let screen = program.screens.iter().find(|s| s.struct_name == "Case").unwrap();
    let root = screen.layout.as_ref().unwrap();
    let LayoutNode::Column { children, .. } = root else { panic!("root should be a Column") };
    let LayoutNode::Grid { entries, children, .. } = &children[0] else { panic!("expected a Grid") };
    assert_eq!(children.len(), 2);
    assert!(entries.iter().any(|(k, _)| k == "columns"));
}

// ---- references must resolve ----

#[test]
fn layout_referencing_an_undeclared_field_is_rejected() {
    let src = format!(
        "{BASE}
        screen Case {{
            layout {{ field does_not_exist }}
        }}
        "
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::UnknownScreenField { field_name, .. } if field_name == "does_not_exist"
    ));
}

#[test]
fn layout_referencing_an_action_that_does_not_exist_is_rejected() {
    let src = format!(
        "{BASE}
        screen Case {{
            layout {{ action \"Nonexistent\" }}
        }}
        "
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::UnknownLayoutAction { label, .. } if label == "Nonexistent"
    ));
}

#[test]
fn layout_referencing_an_inferred_crud_action_by_kind_is_accepted() {
    let src = format!(
        "{BASE}
        screen Case {{
            layout {{ action \"delete\" }}
        }}
        "
    );
    let program = parse_ok(&src);
    assert_eq!(typecheck(&program), Ok(()));
}

// ---- widget leaves ----

#[test]
fn an_unrecognized_widget_kind_is_rejected() {
    let src = format!(
        "{BASE}
        screen Case {{
            layout {{ not_a_real_widget {{}} }}
        }}
        "
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::UnknownRenderValue { render, .. } if render == "not_a_real_widget"
    ));
}

#[test]
fn a_timeline_widget_with_no_source_is_rejected() {
    let src = format!(
        "{BASE}
        screen Case {{
            layout {{ timeline {{}} }}
        }}
        "
    );
    assert!(matches!(first_type_error(&src), TypeErrorKind::TimelineWidgetMissingSource { .. }));
}

#[test]
fn a_timeline_widget_with_a_real_source_fn_typechecks() {
    let src = format!(
        "{BASE}
        fn list_case_history() -> Text {{ return Text(\"[]\") }}
        screen Case {{
            layout {{ timeline {{ source: list_case_history }} }}
        }}
        "
    );
    let program = parse_ok(&src);
    assert_eq!(typecheck(&program), Ok(()));
}

// ---- `field <name> { render: "badge" }` / `"searchable_select"` ----

#[test]
fn badge_render_on_a_non_enum_field_is_rejected() {
    let src = format!(
        "{BASE}
        screen Case {{
            field priority {{ render: \"badge\" }}
        }}
        "
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::FieldValidationTypeMismatch { key, .. } if key == "render"
    ));
}

#[test]
fn badge_render_on_a_real_enum_field_typechecks() {
    let src = "
        enum Status { Open, Closed }
        struct Case { id: i64, status: Status }
        fn list_case() -> Text { return Text(\"[]\") }
        struct Text { value: str }
        fn main() {}

        screen Case {
            field status { render: \"badge\" }
        }
    ";
    let program = parse_ok(src);
    assert_eq!(typecheck(&program), Ok(()));
}

#[test]
fn searchable_select_source_naming_neither_a_struct_nor_a_fn_is_rejected() {
    let src = format!(
        "{BASE}
        screen Case {{
            field priority {{ render: \"searchable_select\" source: nonexistent }}
        }}
        "
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::SearchableSelectSourceNotFound { source, .. } if source == "nonexistent"
    ));
}

#[test]
fn searchable_select_source_naming_a_real_struct_typechecks() {
    let src = format!(
        "{BASE}
        struct User {{ id: i64, name: str }}
        screen Case {{
            field priority {{ render: \"searchable_select\" source: User }}
        }}
        "
    );
    let program = parse_ok(&src);
    assert_eq!(typecheck(&program), Ok(()));
}

// ---- grammar-level guards ----

#[test]
fn two_layout_blocks_in_one_screen_is_a_parse_error() {
    let toks = Lexer::new(&format!(
        "{BASE}
        screen Case {{
            layout {{ field case_number }}
            layout {{ field priority }}
        }}
        "
    ))
    .tokenize()
    .expect("lex should succeed");
    let err = Parser::new(toks).parse_program().expect_err("a second `layout` block should be a parse error");
    assert!(err.message.contains("layout"), "expected a message naming `layout`, got: {}", err.message);
}
