//! Tests for `emit-ui`: deriving a Material-styled web UI from a
//! program's `struct` declarations and its `list_/create_/update_/
//! delete_/get_<struct>` naming convention (`ui_gen.rs`), with Row 12
//! identity types driving login/role gating. Structural/marker
//! assertions on the generated HTML, not a full-document snapshot —
//! see `ui_gen.rs`'s module doc for the manifest-driven design this is
//! checking.

use nirdosha::ast::TypeRegistry;
use nirdosha::effects::infer_effects;
use nirdosha::ownership::check_ownership;
use nirdosha::parser::Parser;
use nirdosha::token::Lexer;
use nirdosha::typeck::typecheck;
use nirdosha::ui_gen::generate;

fn emit_ui(src: &str) -> String {
    let toks = Lexer::new(src).tokenize().expect("lex should succeed");
    let program = Parser::new(toks).parse_program().expect("parse should succeed");
    typecheck(&program).expect("typecheck should succeed");
    check_ownership(&program).expect("ownership check should succeed");
    let registry = TypeRegistry::build(&program);
    let effects = infer_effects(&program, &registry);
    generate(&program, &effects, None, false, None)
}

#[test]
fn derives_a_screen_per_struct_with_a_convention_fn() {
    let html = emit_ui(include_str!("../examples/ui_todo.nir"));

    // Nav entry + manifest entry for `Todo`.
    assert!(html.contains("\"name\":\"Todo\""), "manifest should carry the Todo screen");
    assert!(html.contains("\"snake\":\"todo\""), "struct name should snake_case for routing");

    // Struct's own fields are present with the right control mapping.
    // (`manifest_json` serializes via `serde_json`'s default `BTreeMap`,
    // so object keys land in alphabetical order, not insertion order --
    // these substrings rely on that, not on insertion order.)
    assert!(html.contains(r#""control":"text","displayLabel":null,"label":"str","name":"title""#));
    assert!(html.contains(r#""control":"checkbox","displayLabel":null,"label":"bool","name":"done""#));
    assert!(html.contains(r#""control":"number","displayLabel":null,"label":"i64","name":"id""#));

    // `create_todo(t: Todo)` expands one level into Todo's own fields
    // instead of rendering a single unfillable blob.
    assert!(html.contains(r#""fn":"create_todo","kind":"create""#));
    assert!(html.contains(r#""control":"struct","displayLabel":null,"label":"Todo","name":"t""#), "struct-typed param should expand into nested fields");

    // `delete_todo(identity: VerifiedIdentity, id: i64) requires(role: "admin")`
    // -- login + role gating derived correctly, and the identity param is
    // dropped from the user-facing form.
    assert!(html.contains(r#""fn":"delete_todo","kind":"delete""#));
    assert!(html.contains(r#""requiredClaim":null,"requiredRole":"admin","requiresLogin":true"#));
    assert!(!html.contains(r#""name":"identity""#), "VerifiedIdentity param must not become a user-entered field");

    // Generic renderer scaffolding actually present (nav, login stub, snackbar).
    assert!(html.contains("class=\"nav-rail\""));
    assert!(html.contains("id=\"snackbar\""));
    assert!(html.contains("renderLogin"));
    assert!(html.contains("stub"), "login must be labeled as a stub, not presented as real auth");
}

#[test]
fn struct_with_no_convention_fn_yields_no_screen() {
    let src = r#"
        struct Untouched {
            x: i64,
        }
        fn main() -> i64 {
            return 0
        }
    "#;
    let html = emit_ui(src);
    assert!(!html.contains("\"name\":\"Untouched\""), "a struct with no list_/create_/update_/delete_/get_ fn should not become a screen");
}

#[test]
fn singular_screen_has_no_list_action() {
    let src = r#"
        struct Settings {
            volume: i64,
        }
        fn get_settings() -> i64 {
            return 0
        }
        fn update_settings(s: Settings) -> i64 {
            return s.volume
        }
        fn main() -> i64 {
            return 0
        }
    "#;
    let html = emit_ui(src);
    assert!(html.contains("\"singular\":true"), "a struct with only get_/update_ (no list_) should render as a singular form");
}

#[test]
fn option_field_is_optional_but_keeps_its_inner_control() {
    let src = r#"
        struct Text {
            value: str,
        }
        struct Note {
            body: str,
            reminder: Option(i64),
        }
        fn list_note() -> Text { return Text("[]") }
        fn create_note(n: Note) -> Text { return Text(n.body) }
        fn main() -> Text { return list_note() }
    "#;
    let html = emit_ui(src);
    assert!(html.contains(
        r#""control":"number","displayLabel":null,"label":"i64","name":"reminder","nested":[],"options":[],"required":false"#
    ));
}

#[test]
fn stat_and_chart_functions_are_derived_as_dashboard_metrics() {
    let src = r#"
        fn stat_open_cases() -> i64 {
            return 3
        }
        fn stat_total_leakage_cents() -> i64 requires(role: "analyst") {
            return 12345
        }
        enum ErrorCode {
            ParseError,
        }
        fn chart_leakage_by_service() -> Result(json, ErrorCode) {
            return match json_parse("[]") {
                Ok(v) => Ok(v),
                Err(e) => Err(ParseError()),
            }
        }
        // Not a metric: takes a param.
        fn stat_ignored(x: i64) -> i64 {
            return x
        }
        struct Text {
            value: str,
        }
        // Not a metric: wrong return type.
        fn stat_also_ignored() -> Text {
            return Text("nope")
        }
        fn main() {}
    "#;
    let html = emit_ui(src);

    // `to_title_case` derivation: "open_cases" -> "Open Cases".
    assert!(html.contains(r#""fn":"stat_open_cases","label":"Open Cases""#));
    // Gated stat carries the role.
    assert!(html.contains(r#""fn":"stat_total_leakage_cents""#));
    assert!(html.contains(r#""label":"Total Leakage Cents","requiredClaim":null,"requiredRole":"analyst","requiresLogin":true"#));
    // Chart derived from a zero-arg `json`-returning `chart_` fn.
    assert!(html.contains(r#""fn":"chart_leakage_by_service","label":"Leakage By Service""#));
    // Non-matching fns must not leak into either metric list.
    assert!(!html.contains("stat_ignored"));
    assert!(!html.contains("stat_also_ignored"));

    // Client-side dashboard scaffolding actually present.
    assert!(html.contains("const STATS = "));
    assert!(html.contains("const CHARTS = "));
    assert!(html.contains("function renderDashboard"));
    assert!(html.contains("function renderBarChart"));
    assert!(html.contains("HAS_DASHBOARD"));
}

#[test]
fn list_screen_wires_up_inline_edit_when_an_update_action_exists() {
    // `struct Ledger`, list_ + update_ (no create_/delete_) -- a table
    // screen (has list_), not the singular form, but still needs an
    // Edit path wired up (the gap this session fixed: update was
    // previously only reachable on singular screens).
    let src = r#"
        struct Text {
            value: str,
        }
        struct Ledger {
            id: i64,
            note: str,
        }
        fn list_ledger() -> Text { return Text("[]") }
        fn update_ledger(l: Ledger) -> Text { return Text(l.note) }
        fn main() -> Text { return list_ledger() }
    "#;
    let html = emit_ui(src);
    assert!(html.contains("\"singular\":false"));
    assert!(html.contains(r#""fn":"update_ledger","kind":"update""#));
    assert!(html.contains("updateValuesFromRow"), "list-screen renderer must wire up an edit path, not just delete");
}

// ---- Row 12's `screen`/`dashboard` DSL ----------------------------------
// Existence/shape correctness is `tests/screen_dsl.rs`'s job; these check
// that a declared `screen` block actually changes what `ui_gen.rs`
// renders (title/label overrides, a custom action) -- and, critically,
// that a struct with no `screen` block at all is untouched, so the DSL
// stays additive rather than a parallel code path a plain struct has to
// opt out of.

#[test]
fn declared_screen_title_and_field_label_override_inference() {
    let src = r#"
        struct Text {
            value: str,
        }
        struct Product {
            id: i64,
            name: str,
        }
        fn list_product() -> Text { return Text("[]") }
        fn create_product(p: Product) -> Text { return Text(p.name) }

        screen Product {
            title: "Catalog"
            field name {
                label: "Product Name"
            }
        }
        fn main() -> Text { return list_product() }
    "#;
    let html = emit_ui(src);
    assert!(html.contains(r#""title":"Catalog""#), "declared title should override the struct name as the screen's display title");
    assert!(
        html.contains(r#""displayLabel":"Product Name","label":"str","name":"name""#),
        "declared field label should attach as displayLabel without disturbing the inferred type label"
    );
    // Untouched field keeps displayLabel: null.
    assert!(html.contains(r#""displayLabel":null,"label":"i64","name":"id""#));
}

#[test]
fn declared_custom_action_carries_its_label_style_and_confirm() {
    let src = r#"
        struct Text {
            value: str,
        }
        struct Product {
            id: i64,
            name: str,
        }
        fn list_product() -> Text { return Text("[]") }
        fn restock_product(id: i64) -> i64 { return id }

        screen Product {
            action "Restock" -> restock_product {
                style: "outlined"
                confirm: "Restock this product?"
            }
        }
        fn main() -> Text { return list_product() }
    "#;
    let html = emit_ui(src);
    assert!(html.contains(r#""fn":"restock_product","kind":"custom""#));
    assert!(html.contains(r#""confirm":"Restock this product?""#));
    assert!(html.contains(r#""label":"Restock""#));
    assert!(html.contains(r#""style":"outlined""#));
}

#[test]
fn screen_declared_crud_target_overrides_the_inferred_fn_name() {
    // `list` points at a fn that doesn't follow the `list_<snake>`
    // convention at all -- proves the override actually replaces the
    // inferred name rather than merely supplementing it.
    let src = r#"
        struct Text {
            value: str,
        }
        struct Product {
            id: i64,
        }
        fn fetch_all_products() -> Text { return Text("[]") }

        screen Product {
            list: fetch_all_products
        }
        fn main() -> Text { return fetch_all_products() }
    "#;
    let html = emit_ui(src);
    assert!(html.contains(r#""fn":"fetch_all_products","kind":"list""#));
}

#[test]
fn struct_with_no_screen_block_is_unaffected_by_the_dsl_existing() {
    // Regression guard on the progressive-fallback promise: a *different*
    // struct in the same program having a `screen` block must not change
    // anything about a struct that doesn't.
    let src = r#"
        struct Text {
            value: str,
        }
        struct Product {
            id: i64,
        }
        fn list_product() -> Text { return Text("[]") }
        screen Product {
            title: "Catalog"
        }

        struct Plain {
            id: i64,
        }
        fn list_plain() -> Text { return Text("[]") }

        fn main() -> Text { return list_product() }
    "#;
    let html = emit_ui(src);
    assert!(
        html.contains(r#""name":"Plain","singular":false,"snake":"plain","table":"plain","title":"Plain""#),
        "a struct with no screen block should get its own name as the title, unaffected by Product's block"
    );
}

#[test]
fn no_theme_produces_no_theme_override_block() {
    let html = emit_ui("fn main() {}");
    // The placeholder is always substituted; absent a theme, it must
    // vanish rather than leak into the output.
    assert!(!html.contains("__NIRDOSHA_THEME_OVERRIDE__"));
    assert!(!html.contains("--md-primary: #ff0000"));
}

#[test]
fn theme_overrides_only_the_tokens_it_sets() {
    use nirdosha::ui_gen::Theme;
    let src = "fn main() {}";
    let toks = nirdosha::token::Lexer::new(src).tokenize().unwrap();
    let program = nirdosha::parser::Parser::new(toks).parse_program().unwrap();
    nirdosha::typeck::typecheck(&program).unwrap();
    nirdosha::ownership::check_ownership(&program).unwrap();
    let registry = TypeRegistry::build(&program);
    let effects = infer_effects(&program, &registry);
    let theme = Theme {
        primary_light: Some("#ff0000".to_string()),
        radius_sm: Some("2px".to_string()),
        ..Default::default()
    };
    let html = generate(&program, &effects, None, false, Some(&theme));

    // The theme's own tokens appear...
    assert!(html.contains("--md-primary: #ff0000;"));
    assert!(html.contains("--md-radius-sm: 2px;"));
    // ...but a token the theme never set is untouched (still the MD3
    // default, not overwritten to empty/garbage).
    assert!(html.contains("--md-radius-lg: 28px;"));
    // No dark-mode override block at all when no *_dark field was set.
    assert!(!html.contains("--md-primary: #ff0000;\n    }\n  }\n  @media (prefers-color-scheme: dark) {\n    :root {\n"));
}

#[test]
fn theme_value_containing_markup_is_dropped_not_injected() {
    use nirdosha::ui_gen::Theme;
    let src = "fn main() {}";
    let toks = nirdosha::token::Lexer::new(src).tokenize().unwrap();
    let program = nirdosha::parser::Parser::new(toks).parse_program().unwrap();
    nirdosha::typeck::typecheck(&program).unwrap();
    nirdosha::ownership::check_ownership(&program).unwrap();
    let registry = TypeRegistry::build(&program);
    let effects = infer_effects(&program, &registry);
    let theme = Theme {
        font_sans: Some("</style><script>alert(1)</script>".to_string()),
        ..Default::default()
    };
    let out = generate(&program, &effects, None, false, Some(&theme));
    // The base template legitimately has its own <script> (the baked-in
    // renderer) -- what must NOT happen is the theme's payload landing
    // verbatim in the output as a second, injected one.
    assert!(!out.contains("alert(1)"));
    assert!(!out.contains("--md-font: </style>"));
}
