//! Tests for `docs/ROADMAP.md` Track E1's `workspace`/`panel` DSL — a
//! composite, multi-panel screen scoped to one instance of a `subject`
//! struct. See `docs/GRAMMAR.md`'s `workspace_decl`/`panel_decl` productions
//! and `examples/ctms/UI_CONSTRUCTS.md` §1 for the full design this
//! implements the existence/shape-checking core of. Mirrors
//! `screen_dsl.rs`'s own style/coverage shape for the analogous `screen`
//! DSL.

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

// `str` is banned as a function param/return type everywhere (docs/LANGUAGE.md
// §6b) -- `json_parse`'s own `Result(json, str)` (a builtin, exempt) has
// to be converted into a real enum-favoring carrier before a *user* fn
// can return it. `Text` is the same one-field free-text carrier
// `screen_dsl.rs`'s own `WELL_FORMED` const already uses for this.
const PRELUDE: &str = r#"
    struct Text {
        value: str,
    }
"#;

const WELL_FORMED: &str = r#"
    struct Text {
        value: str,
    }

    struct Case {
        id: i64,
        status: str,
    }

    fn list_transaction_for_case(case_id: i64) -> Result(json, Text) {
        return match json_parse("[]") {
            Ok(v) => Ok(v),
            Err(e) => Err(Text(e)),
        }
    }
    fn list_case_note(case_id: i64) -> Result(json, Text) {
        return match json_parse("[]") {
            Ok(v) => Ok(v),
            Err(e) => Err(Text(e)),
        }
    }
    fn add_case_note(case_id: i64, body: Text) -> Result(json, Text) {
        return match json_parse("{}") {
            Ok(v) => Ok(v),
            Err(e) => Err(Text(e)),
        }
    }

    workspace CaseInvestigation {
        title: "Investigation Workspace"
        subject: Case

        panel "Transactions" {
            source: list_transaction_for_case
        }
        panel "Notes" {
            source: list_case_note
            action "Add Note" -> add_case_note {
                style: "filled"
            }
        }
    }

    fn main() {}
"#;

#[test]
fn well_formed_workspace_parses_and_typechecks_cleanly() {
    let program = parse_ok(WELL_FORMED);
    assert_eq!(program.workspaces.len(), 1);
    let ws = &program.workspaces[0];
    assert_eq!(ws.name, "CaseInvestigation");
    assert_eq!(ws.panels.len(), 2);
    assert_eq!(ws.panels[0].title, "Transactions");
    assert_eq!(ws.panels[1].title, "Notes");
    assert_eq!(ws.panels[1].actions.len(), 1);
    assert_eq!(ws.panels[1].actions[0].target_fn, "add_case_note");

    typecheck(&program).expect("well-formed workspace should typecheck cleanly");
}

#[test]
fn workspace_with_no_subject_entry_is_rejected() {
    let src = format!(
        r#"
        {PRELUDE}
        struct Case {{ id: i64 }}
        fn list_x(case_id: i64) -> Result(json, Text) {{
            return match json_parse("[]") {{ Ok(v) => Ok(v), Err(e) => Err(Text(e)), }}
        }}
        workspace W {{
            panel "P" {{ source: list_x }}
        }}
        fn main() {{}}
    "#
    );
    assert!(matches!(first_type_error(&src), TypeErrorKind::WorkspaceMissingSubject(name) if name == "W"));
}

#[test]
fn workspace_subject_naming_an_unknown_struct_is_rejected() {
    let src = format!(
        r#"
        {PRELUDE}
        fn list_x(case_id: i64) -> Result(json, Text) {{
            return match json_parse("[]") {{ Ok(v) => Ok(v), Err(e) => Err(Text(e)), }}
        }}
        workspace W {{
            subject: NoSuchStruct
            panel "P" {{ source: list_x }}
        }}
        fn main() {{}}
    "#
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::UnknownWorkspaceSubject { workspace, struct_name }
            if workspace == "W" && struct_name == "NoSuchStruct"
    ));
}

#[test]
fn workspace_subject_struct_missing_an_id_i64_field_is_rejected() {
    let src = format!(
        r#"
        {PRELUDE}
        struct Case {{ name: str }}
        fn list_x(case_id: i64) -> Result(json, Text) {{
            return match json_parse("[]") {{ Ok(v) => Ok(v), Err(e) => Err(Text(e)), }}
        }}
        workspace W {{
            subject: Case
            panel "P" {{ source: list_x }}
        }}
        fn main() {{}}
    "#
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::WorkspaceSubjectMissingId { workspace, struct_name }
            if workspace == "W" && struct_name == "Case"
    ));
}

#[test]
fn panel_with_no_source_entry_is_rejected() {
    let src = r#"
        struct Case { id: i64 }
        workspace W {
            subject: Case
            panel "P" { title: "no source here" }
        }
        fn main() {}
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::PanelMissingSource { workspace, panel } if workspace == "W" && panel == "P"
    ));
}

#[test]
fn panel_source_naming_an_undeclared_fn_is_rejected() {
    let src = r#"
        struct Case { id: i64 }
        workspace W {
            subject: Case
            panel "P" { source: no_such_fn }
        }
        fn main() {}
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::ScreenFnNotFound { key, fn_name } if key == "source" && fn_name == "no_such_fn"
    ));
}

#[test]
fn panel_source_with_the_wrong_param_count_is_rejected() {
    let src = format!(
        r#"
        {PRELUDE}
        struct Case {{ id: i64 }}
        fn list_x() -> Result(json, Text) {{
            return match json_parse("[]") {{ Ok(v) => Ok(v), Err(e) => Err(Text(e)), }}
        }}
        workspace W {{
            subject: Case
            panel "P" {{ source: list_x }}
        }}
        fn main() {{}}
    "#
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::PanelSourceWrongShape { workspace, panel, fn_name }
            if workspace == "W" && panel == "P" && fn_name == "list_x"
    ));
}

#[test]
fn panel_source_returning_something_other_than_result_json_is_rejected() {
    let src = r#"
        struct Case { id: i64 }
        fn list_x(case_id: i64) -> i64 { return case_id }
        workspace W {
            subject: Case
            panel "P" { source: list_x }
        }
        fn main() {}
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::PanelSourceWrongShape { workspace, panel, fn_name }
            if workspace == "W" && panel == "P" && fn_name == "list_x"
    ));
}

#[test]
fn panel_action_target_that_does_not_resolve_is_rejected() {
    let src = format!(
        r#"
        {PRELUDE}
        struct Case {{ id: i64 }}
        fn list_x(case_id: i64) -> Result(json, Text) {{
            return match json_parse("[]") {{ Ok(v) => Ok(v), Err(e) => Err(Text(e)), }}
        }}
        workspace W {{
            subject: Case
            panel "P" {{
                source: list_x
                action "Go" -> no_such_action_fn
            }}
        }}
        fn main() {{}}
    "#
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::ScreenFnNotFound { key, fn_name } if key == "action" && fn_name == "no_such_action_fn"
    ));
}

#[test]
fn a_program_with_no_workspace_block_typechecks_exactly_as_before() {
    let src = r#"
        struct Case { id: i64 }
        fn main() {}
    "#;
    let program = parse_ok(src);
    assert!(program.workspaces.is_empty());
    typecheck(&program).expect("a program with no workspace block should typecheck as before");
}

// ── Track E4: `action { show_result: true }` reused on a panel action ──

#[test]
fn panel_action_show_result_true_requires_a_json_result_too() {
    let src = format!(
        r#"
        {PRELUDE}
        struct Case {{ id: i64 }}
        fn list_x(case_id: i64) -> Result(json, Text) {{
            return match json_parse("[]") {{ Ok(v) => Ok(v), Err(e) => Err(Text(e)), }}
        }}
        fn do_thing(case_id: i64) -> i64 {{ return case_id }}
        workspace W {{
            subject: Case
            panel "P" {{
                source: list_x
                action "Do" -> do_thing {{
                    show_result: true
                }}
            }}
        }}
        fn main() {{}}
    "#
    );
    assert!(matches!(
        first_type_error(&src),
        TypeErrorKind::ShowResultRequiresJsonResult { fn_name, .. } if fn_name == "do_thing"
    ));
}
