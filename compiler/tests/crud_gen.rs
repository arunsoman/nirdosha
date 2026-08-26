//! Tests for `nirdosha gen-crud`: the deterministic struct+CRUD renderer
//! (`nirdosha::crud_gen`, pure text — same lex/parse/typecheck/ownership
//! harness pattern as `compiler/tests/init.rs`/`emit_ui.rs`). The point of
//! this module is "always compiles, no LLM needed" — every happy-path test
//! below asserts real typecheck success, not just string matching, and the
//! two edge-case tests assert the specific SQL-generation bugs found while
//! building this (`INSERT INTO t () VALUES ()`, `UPDATE t SET  WHERE ...`)
//! stay fixed.

use nirdosha::crud_gen::{render_entity, render_plan, EntityPlan, FieldSpec, KpiSpec, ScreenPlan};
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck_optional_main;

fn typechecks(src: &str) {
    let toks = Lexer::new(src).tokenize().unwrap_or_else(|e| panic!("lex should succeed: {e:?}\n---\n{src}"));
    let program =
        Parser::new(toks).parse_program().unwrap_or_else(|e| panic!("parse should succeed: {e:?}\n---\n{src}"));
    typecheck_optional_main(&program).unwrap_or_else(|e| panic!("typecheck should succeed: {e:?}\n---\n{src}"));
    check_ownership(&program).unwrap_or_else(|e| panic!("ownership check should succeed: {e:?}\n---\n{src}"));
}

fn field(name: &str, ty: &str) -> FieldSpec {
    FieldSpec { name: name.to_string(), ty: ty.to_string() }
}

fn full_entity() -> EntityPlan {
    EntityPlan {
        struct_name: "TradeRecord".to_string(),
        fields: vec![
            field("po_id", "i64"),
            field("credit_rating", "i64"),
            field("bank_guarantee_required", "bool"),
            field("notes", "str"),
            field("score", "f64"),
        ],
        crud_slots: vec!["list".into(), "get".into(), "create".into(), "update".into(), "delete".into()],
        screen_title: Some("Trade Records".to_string()),
        field_labels: [("po_id".to_string(), "PO ID".to_string())].into_iter().collect(),
    }
}

/// `render_entity` alone omits the shared `struct Text { value: str }`
/// wrapper (that's `render_plan`'s job, declared once per file, not once
/// per entity) -- so any test that needs the result to actually
/// typecheck goes through a one-entity `ScreenPlan` instead of calling
/// `render_entity` directly. Tests that only check emitted *content*, or
/// that expect an `Err`, use `render_entity` directly.
fn render_one(entity: &EntityPlan, db: &str) -> String {
    render_plan(&ScreenPlan { entities: vec![entity.clone()], kpis: vec![] }, db, "").expect("should render")
}

#[test]
fn all_five_crud_slots_render_and_typecheck() {
    let src = render_one(&full_entity(), "shop.db");
    typechecks(&src);

    assert!(src.contains("struct TradeRecord {\n    id: i64,\n    po_id: i64,"));
    assert!(src.contains("fn list_trade_record() -> Result(json, Text)"));
    assert!(src.contains("fn get_trade_record(id: i64) -> Result(TradeRecord, Text)"));
    assert!(src.contains("fn create_trade_record(x: TradeRecord) -> Result(i64, Text)"));
    assert!(src.contains("fn update_trade_record(x: TradeRecord) -> Result(i64, Text)"));
    assert!(src.contains("fn delete_trade_record(id: i64) -> Result(i64, Text)"));
    assert!(src.contains(r#"screen TradeRecord {"#));
    assert!(src.contains(r#"title: "Trade Records""#));
    assert!(src.contains(r#"field po_id { label: "PO ID" }"#));

    // The one `db_connect` literal is reused byte-for-byte everywhere
    // (`PROTOBOX_INTEGRATION.md` SS3's contract) -- every CRUD fn opens
    // the same connection string, never a fresh one per function.
    assert_eq!(src.matches(r#"db_connect("shop.db")"#).count(), 5);

    // INSERT never mentions `id` -- it's autoincrement, not caller-set.
    let insert_line = src.lines().find(|l| l.contains("INSERT INTO")).expect("has an INSERT");
    assert!(!insert_line.contains(" id,") && !insert_line.contains("(id"), "INSERT should never include id column");

    // UPDATE's WHERE clause binds `x.id`, and every other field is bound
    // by name -- this is the exact shape a hand-written/LLM-written
    // `b2b.nir` uses.
    let update_line = src.lines().find(|l| l.contains("UPDATE trade_record")).expect("has an UPDATE");
    assert!(update_line.contains("WHERE id = ?") && update_line.contains("x.id)"));
}

#[test]
fn get_decode_chain_is_cleanly_nested_one_level_per_field() {
    let src = render_one(&full_entity(), "shop.db");
    typechecks(&src);
    // Regression test for the indentation bug found while building this:
    // an early version's post-hoc `indent()` pass left inconsistent
    // column alignment across the nested match chain. Each successive
    // `Ok(...) => match ...` line should be indented exactly one level
    // (4 spaces) deeper than the one before it.
    let lines: Vec<&str> = src
        .lines()
        .skip_while(|l| !l.starts_with("fn get_trade_record"))
        .skip(1) // the fn signature line itself
        .take_while(|l| !l.trim_start().starts_with("fn "))
        .filter(|l| l.contains("Ok(") && l.contains("=> match"))
        .collect();
    assert!(lines.len() >= 5, "expected one Ok(...) => match line per decoded field, got: {lines:?}");
    let mut prev_indent: Option<usize> = None;
    for l in &lines {
        let this_indent = l.len() - l.trim_start().len();
        if let Some(p) = prev_indent {
            assert_eq!(this_indent, p + 4, "expected exactly one level of extra indentation per nested field, line: {l:?}");
        }
        prev_indent = Some(this_indent);
    }
}

#[test]
fn zero_field_create_uses_default_values_not_empty_parens() {
    let entity =
        EntityPlan { struct_name: "Ping".into(), fields: vec![], crud_slots: vec!["create".into()], screen_title: None, field_labels: Default::default() };
    let src = render_one(&entity, "x.db");
    typechecks(&src);
    assert!(src.contains(r#"db_execute(conn, "INSERT INTO ping DEFAULT VALUES")"#));
    assert!(!src.contains("()  VALUES ()") && !src.contains("() VALUES ()"), "should never emit the empty-parens form SQLite/Postgres both reject");
}

#[test]
fn zero_field_update_is_rejected_at_generation_time_not_emitted_broken() {
    let entity =
        EntityPlan { struct_name: "Ping".into(), fields: vec![], crud_slots: vec!["update".into()], screen_title: None, field_labels: Default::default() };
    let err = render_entity(&entity, "x.db").expect_err("update with nothing to set should be rejected");
    assert!(err.contains("Ping") && err.contains("update"), "error should name the entity and the offending slot: {err:?}");
}

#[test]
fn enum_typed_field_is_rejected_with_a_clear_v1_scope_message() {
    let entity = EntityPlan {
        struct_name: "Order".into(),
        fields: vec![field("status", "OrderStatus")],
        crud_slots: vec!["create".into()],
        screen_title: None,
        field_labels: Default::default(),
    };
    let err = render_entity(&entity, "x.db").expect_err("non-scalar field type should be rejected in v1");
    assert!(err.contains("OrderStatus"), "error should name the unsupported type: {err:?}");
}

#[test]
fn whole_plan_with_multiple_entities_and_a_kpi_typechecks() {
    let plan = ScreenPlan {
        entities: vec![
            full_entity(),
            EntityPlan {
                struct_name: "Document".into(),
                fields: vec![field("title", "str")],
                crud_slots: vec!["list".into(), "create".into()],
                screen_title: None,
                field_labels: Default::default(),
            },
        ],
        kpis: vec![KpiSpec { name: "open_trades".into(), label: "Open Trades".into() }],
    };
    let src = render_plan(&plan, "shop.db", "").expect("should render");
    typechecks(&src);
    assert!(src.contains("struct Text {"), "Text wrapper declared once for the whole file");
    assert_eq!(src.matches("struct Text {").count(), 1, "declared exactly once, not per entity");
    assert!(src.contains("fn stat_open_trades() -> i64"));
    assert!(src.contains(r#"tile "Open Trades" -> stat_open_trades"#));
}

#[test]
fn plan_with_no_crud_slots_at_all_skips_the_unused_text_struct() {
    let plan = ScreenPlan {
        entities: vec![EntityPlan {
            struct_name: "Empty".into(),
            fields: vec![],
            crud_slots: vec![],
            screen_title: None,
            field_labels: Default::default(),
        }],
        kpis: vec![],
    };
    let src = render_plan(&plan, "x.db", "").expect("should render");
    typechecks(&src);
    assert!(!src.contains("struct Text {"), "no CRUD fn needs Text, so it shouldn't be declared");
}
