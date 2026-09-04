//! Tests for `docs/ROADMAP.md` Track E2's `visual`/`render:` DSL — a
//! `dashboard` item (graph/heatmap/timeline, no naming-convention
//! equivalent) plus the same `render:` key reused inside a `workspace`
//! `panel` (Track E1). See `docs/GRAMMAR.md`'s `dashboard_item`'s `visual`
//! alternative and `examples/ctms/UI_CONSTRUCTS.md` §2 for the full
//! design. Mirrors `screen_dsl.rs`/`workspace_dsl.rs`'s own style.

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
    fn graph_wallet_clusters() -> Result(json, i64) requires(public) {
        return match json_parse("{}") {
            Ok(v) => Ok(v),
            Err(e) => Err(0),
        }
    }
    fn heatmap_alerts() -> Result(json, i64) requires(public) {
        return match json_parse("[]") {
            Ok(v) => Ok(v),
            Err(e) => Err(0),
        }
    }
    fn timeline_events() -> Result(json, i64) requires(public) {
        return match json_parse("[]") {
            Ok(v) => Ok(v),
            Err(e) => Err(0),
        }
    }
    fn stat_open_cases() -> i64 { return 3 }

    dashboard {
        tile "Open Cases" -> stat_open_cases
        visual "Wallet Clusters" -> graph_wallet_clusters {
            render: "graph"
        }
        visual "Alert Density" -> heatmap_alerts {
            render: "heatmap"
        }
        visual "Case Events" -> timeline_events {
            render: "timeline"
        }
    }

    fn main() {}
"#;

#[test]
fn well_formed_visuals_parse_and_typecheck_cleanly() {
    let program = parse_ok(WELL_FORMED);
    let dash = program.dashboard.as_ref().expect("dashboard block present");
    assert_eq!(dash.tiles.len(), 1);
    assert_eq!(dash.visuals.len(), 3);
    assert_eq!(dash.visuals[0].target_fn, "graph_wallet_clusters");
    assert_eq!(dash.visuals[0].entries.len(), 1);
    assert_eq!(dash.visuals[0].entries[0].0, "render");
    assert!(matches!(&dash.visuals[0].entries[0].1, nirdosha::ast::Expr::Str(s, _) if s == "graph"));

    typecheck(&program).expect("well-formed visuals should typecheck cleanly");
}

#[test]
fn visual_with_no_body_at_all_is_allowed_and_defaults_at_the_ui_gen_layer() {
    // `("{" kv_entry* "}")?` -- a `visual` naming no `render` at all
    // parses fine (typeck has nothing to flag; `ui_gen.rs::MetricRender
    // ::from_kv` is what actually supplies the `BarChart` default).
    let src = r#"
        fn graph_wallet_clusters() -> Result(json, i64) requires(public) {
            return match json_parse("{}") { Ok(v) => Ok(v), Err(e) => Err(0), }
        }
        dashboard {
            visual "Wallet Clusters" -> graph_wallet_clusters
        }
        fn main() {}
    "#;
    let program = parse_ok(src);
    assert_eq!(program.dashboard.as_ref().unwrap().visuals[0].entries.len(), 0);
    typecheck(&program).expect("a body-less visual should typecheck cleanly");
}

#[test]
fn visual_target_fn_that_does_not_resolve_is_rejected() {
    let src = r#"
        dashboard {
            visual "Ghost" -> no_such_fn { render: "graph" }
        }
        fn main() {}
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::UnknownDashboardFn { metric_kind, fn_name } if metric_kind == "visual" && fn_name == "no_such_fn"
    ));
}

#[test]
fn visual_render_value_not_in_the_closed_set_is_rejected() {
    let src = r#"
        fn graph_wallet_clusters() -> Result(json, i64) requires(public) {
            return match json_parse("{}") { Ok(v) => Ok(v), Err(e) => Err(0), }
        }
        dashboard {
            visual "Wallet Clusters" -> graph_wallet_clusters { render: "pie_chart" }
        }
        fn main() {}
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::UnknownRenderValue { render, .. } if render == "pie_chart"
    ));
}

#[test]
fn visual_render_value_that_is_not_a_string_literal_is_rejected() {
    let src = r#"
        fn graph_wallet_clusters() -> Result(json, i64) requires(public) {
            return match json_parse("{}") { Ok(v) => Ok(v), Err(e) => Err(0), }
        }
        dashboard {
            visual "Wallet Clusters" -> graph_wallet_clusters { render: 1 }
        }
        fn main() {}
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::InvalidFieldValidationExpr { key } if key == "render"
    ));
}

#[test]
fn panel_render_reuses_the_same_closed_vocabulary() {
    let src = r#"
        struct Case { id: i64 }
        fn list_x(case_id: i64) -> Result(json, i64) {
            return match json_parse("[]") { Ok(v) => Ok(v), Err(e) => Err(0), }
        }
        workspace W {
            subject: Case
            panel "P" {
                source: list_x
                render: "timeline"
            }
        }
        fn main() {}
    "#;
    let program = parse_ok(src);
    typecheck(&program).expect("panel { render: \"timeline\" } should typecheck cleanly");
}

#[test]
fn panel_render_value_not_in_the_closed_set_is_rejected() {
    let src = r#"
        struct Case { id: i64 }
        fn list_x(case_id: i64) -> Result(json, i64) {
            return match json_parse("[]") { Ok(v) => Ok(v), Err(e) => Err(0), }
        }
        workspace W {
            subject: Case
            panel "P" {
                source: list_x
                render: "pie_chart"
            }
        }
        fn main() {}
    "#;
    assert!(matches!(
        first_type_error(src),
        TypeErrorKind::UnknownRenderValue { render, .. } if render == "pie_chart"
    ));
}

#[test]
fn a_dashboard_with_no_visual_items_typechecks_exactly_as_before() {
    let src = r#"
        fn stat_open_cases() -> i64 { return 3 }
        dashboard {
            tile "Open Cases" -> stat_open_cases
        }
        fn main() {}
    "#;
    let program = parse_ok(src);
    assert!(program.dashboard.as_ref().unwrap().visuals.is_empty());
    typecheck(&program).expect("a dashboard with no visual items should typecheck as before");
}
