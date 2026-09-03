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
    let html = emit_ui(include_str!("../../examples/ui_todo.nir"));

    // Nav entry + manifest entry for `Todo`.
    assert!(html.contains("\"name\":\"Todo\""), "manifest should carry the Todo screen");
    assert!(html.contains("\"snake\":\"todo\""), "struct name should snake_case for routing");

    // Struct's own fields are present with the right control mapping.
    // (`manifest_json` serializes via `serde_json`'s default `BTreeMap`,
    // so object keys land in alphabetical order, not insertion order --
    // these substrings rely on that, not on insertion order.)
    assert!(html.contains(r#""control":"text","displayLabel":null,"label":"str","max":null,"min":null,"name":"title""#));
    assert!(html.contains(r#""control":"checkbox","displayLabel":null,"label":"bool","max":null,"min":null,"name":"done""#));
    assert!(html.contains(r#""control":"number","displayLabel":null,"label":"i64","max":null,"min":null,"name":"id""#));

    // `create_todo(t: Todo)` expands one level into Todo's own fields
    // instead of rendering a single unfillable blob.
    assert!(html.contains(r#""fn":"create_todo","kind":"create""#));
    assert!(html.contains(r#""control":"struct","displayLabel":null,"label":"Todo","max":null,"min":null,"name":"t""#), "struct-typed param should expand into nested fields");

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
        r#""control":"number","displayLabel":null,"label":"i64","max":null,"min":null,"name":"reminder","nested":[],"options":[],"pattern":null,"required":false"#
    ));
}

#[test]
fn field_pattern_min_max_and_format_reach_the_manifest() {
    let src = r#"
        struct Contact {
            id: i64,
            name: str,
            email: str,
            age: i64,
        }
        fn list_contact() -> i64 { return 0 }
        fn create_contact(c: Contact) -> i64 { return 0 }

        screen Contact {
            field name {
                pattern: "^[A-Za-z ]+$"
            }
            field email {
                format: "email"
            }
            field age {
                min: 18
                max: 120
            }
        }

        fn main() -> i64 { return 0 }
    "#;
    let html = emit_ui(src);
    assert!(html.contains(r#""pattern":"^[A-Za-z ]+$""#), "an explicit `pattern` should reach the manifest verbatim");
    assert!(html.contains(r#""pattern":"^[^"#), "a `format` should expand into the manifest's `pattern` slot");
    assert!(html.contains("\"min\":18"), "`min` should reach the manifest");
    assert!(html.contains("\"max\":120"), "`max` should reach the manifest");
    // A field with neither key declared stays unconstrained.
    assert!(html.contains(r#""max":null,"min":null,"name":"id""#), "an unconstrained field's pattern/min/max should serialize as null, not be omitted");
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
        html.contains(r#""displayLabel":"Product Name","label":"str","max":null,"min":null,"name":"name""#),
        "declared field label should attach as displayLabel without disturbing the inferred type label"
    );
    // Untouched field keeps displayLabel: null.
    assert!(html.contains(r#""displayLabel":null,"label":"i64","max":null,"min":null,"name":"id""#));
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
fn theme_overrides_only_the_sections_it_sets() {
    use nirdosha::ui_gen::{Theme, ThemeRadius};
    let src = "fn main() {}";
    let toks = nirdosha::token::Lexer::new(src).tokenize().unwrap();
    let program = nirdosha::parser::Parser::new(toks).parse_program().unwrap();
    nirdosha::typeck::typecheck(&program).unwrap();
    nirdosha::ownership::check_ownership(&program).unwrap();
    let registry = TypeRegistry::build(&program);
    let effects = infer_effects(&program, &registry);
    let mut brand = std::collections::HashMap::new();
    brand.insert("600".to_string(), "#ff0000".to_string());
    let theme = Theme {
        brand: Some(brand),
        radius: Some(ThemeRadius { control: "2px".to_string(), card: "6px".to_string() }),
        ..Default::default()
    };
    let html = generate(&program, &effects, None, false, Some(&theme));

    // The theme's own tokens appear, both the raw ramp step and the
    // semantic role it's mapped to for a light-mode primary...
    assert!(html.contains("--brand-600: #ff0000;"));
    assert!(html.contains("--md-primary: #ff0000;"));
    assert!(html.contains("--radius-control: 2px;"));
    assert!(html.contains("--md-radius-sm: 2px;"));
    // ...but a section the theme never set is untouched: no neutral-
    // ramp custom property at all, and the baked-in MD3 default for a
    // neutral-derived role (`--md-surface`) is still the only value
    // present, not overwritten to empty/garbage.
    assert!(!html.contains("--neutral-"));
    assert!(html.contains("--md-surface: #ffffff;"), "the baked-in MD3 light default should be untouched");
    // Exactly 2: the baked-in light `:root` default plus the baked-in
    // dark `@media` default -- both pre-existing, neither theme-driven.
    // A third occurrence would mean a neutral-derived override leaked in
    // despite no `neutral` section being set.
    assert_eq!(html.matches("--md-surface:").count(), 2, "only the two baked-in MD3 defaults should be present, no theme-driven override");
    // No SECOND, theme-driven dark-mode override block: the brand ramp
    // only names step "600" (this role's light-mode step), never the
    // dark-mode step ("300"), so nothing qualifies for one -- only the
    // template's own pre-existing baked-in dark `@media` block remains.
    assert_eq!(html.matches("@media (prefers-color-scheme: dark)").count(), 1);
}

#[test]
fn theme_value_containing_markup_is_dropped_not_injected() {
    use nirdosha::ui_gen::{Theme, ThemeFonts};
    let src = "fn main() {}";
    let toks = nirdosha::token::Lexer::new(src).tokenize().unwrap();
    let program = nirdosha::parser::Parser::new(toks).parse_program().unwrap();
    nirdosha::typeck::typecheck(&program).unwrap();
    nirdosha::ownership::check_ownership(&program).unwrap();
    let registry = TypeRegistry::build(&program);
    let effects = infer_effects(&program, &registry);
    let theme = Theme {
        fonts: Some(ThemeFonts { sans: "</style><script>alert(1)</script>".to_string(), display: "Inter".to_string(), mono: None }),
        ..Default::default()
    };
    let out = generate(&program, &effects, None, false, Some(&theme));
    // The base template legitimately has its own <script> (the baked-in
    // renderer) -- what must NOT happen is the theme's payload landing
    // verbatim in the output as a second, injected one.
    assert!(!out.contains("alert(1)"));
    assert!(!out.contains("--md-font: </style>"));
    assert!(!out.contains("--font-sans: </style>"));
}

fn fixture_program() -> (nirdosha::ast::Program, std::collections::HashMap<String, nirdosha::effects::FnEffects>) {
    let src = "fn main() {}";
    let toks = nirdosha::token::Lexer::new(src).tokenize().unwrap();
    let program = nirdosha::parser::Parser::new(toks).parse_program().unwrap();
    nirdosha::typeck::typecheck(&program).unwrap();
    nirdosha::ownership::check_ownership(&program).unwrap();
    let registry = TypeRegistry::build(&program);
    let effects = infer_effects(&program, &registry);
    (program, effects)
}

fn brand_theme_with_dark_mode(dark_mode: Option<&str>) -> nirdosha::ui_gen::Theme {
    use nirdosha::ui_gen::Theme;
    let mut brand = std::collections::HashMap::new();
    brand.insert("600".to_string(), "#111111".to_string());
    brand.insert("300".to_string(), "#eeeeee".to_string());
    Theme { brand: Some(brand), dark_mode: dark_mode.map(str::to_string), ..Default::default() }
}

#[test]
fn dark_mode_media_is_the_default_strategy() {
    let (program, effects) = fixture_program();
    let theme = brand_theme_with_dark_mode(None);
    let html = generate(&program, &effects, None, false, Some(&theme));
    assert_eq!(html.matches("@media (prefers-color-scheme: dark)").count(), 2, "the baked-in default plus this theme's own dark override");
    assert!(html.contains("--md-primary: #eeeeee;"), "dark step (300) should land inside the media-query block");
}

#[test]
fn dark_mode_class_uses_root_dot_dark_not_media_query() {
    let (program, effects) = fixture_program();
    let theme = brand_theme_with_dark_mode(Some("class"));
    let html = generate(&program, &effects, None, false, Some(&theme));
    assert!(html.contains(":root.dark {"));
    assert!(html.contains("--md-primary: #eeeeee;"));
    // Only the template's own pre-existing baked-in media block remains
    // -- this theme's dark override did NOT also emit a media query.
    assert_eq!(html.matches("@media (prefers-color-scheme: dark)").count(), 1);
}

#[test]
fn dark_mode_always_writes_dark_values_into_base_root() {
    let (program, effects) = fixture_program();
    let theme = brand_theme_with_dark_mode(Some("always"));
    let html = generate(&program, &effects, None, false, Some(&theme));
    assert!(!html.contains(":root.dark"));
    assert_eq!(html.matches("@media (prefers-color-scheme: dark)").count(), 1, "only the baked-in block -- no theme-driven one");
    // The dark-step color (300 -> #eeeeee) lands directly in the base
    // override `:root` alongside the light-step color, no separate block.
    assert!(html.contains("--md-primary: #eeeeee;"));
}

#[test]
fn dark_mode_none_emits_no_dark_override_at_all() {
    let (program, effects) = fixture_program();
    let theme = brand_theme_with_dark_mode(Some("none"));
    let html = generate(&program, &effects, None, false, Some(&theme));
    assert!(!html.contains(":root.dark"));
    assert!(!html.contains("--md-primary: #eeeeee;"), "the dark-step color should never appear anywhere");
    assert_eq!(html.matches("@media (prefers-color-scheme: dark)").count(), 1, "only the baked-in block");
}

// ── Favicon: the Nirdosha brand mark, baked in at compile time ─────────

#[test]
fn every_generated_page_carries_the_nirdosha_favicon_with_no_placeholder_left_over() {
    let html = emit_ui(include_str!("../../examples/ui_todo.nir"));
    assert!(
        html.contains(r#"<link rel="icon" type="image/png" href="data:image/png;base64,"#),
        "every emit-ui page must self-contain the brand favicon, no network fetch"
    );
    assert!(
        !html.contains("__NIRDOSHA_FAVICON__"),
        "the placeholder must always be substituted, never leak into real output"
    );
}

#[test]
fn the_embedded_favicon_data_uri_decodes_to_a_real_png() {
    use base64::Engine;
    let html = emit_ui(include_str!("../../examples/ui_todo.nir"));
    let marker = r#"href="data:image/png;base64,"#;
    let start = html.find(marker).expect("favicon link tag present") + marker.len();
    let end = html[start..].find('"').expect("closing quote") + start;
    let b64 = &html[start..end];
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(b64)
        .expect("embedded favicon must be valid base64");
    // PNG magic bytes -- confirms this is a real image, not garbage text
    // that merely survived the placeholder substitution.
    assert_eq!(&bytes[0..8], &[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A, 0x1A, 0x0A]);
}
