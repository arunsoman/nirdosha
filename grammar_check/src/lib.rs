//! Independent LALR(1) cross-check of Nirdosha's grammar — see the
//! top-level README in this crate's directory (or PHASE0.md's relevant
//! "update" section) for what a clean build here actually proves and
//! what it doesn't. This crate is not part of the Nirdosha compiler
//! pipeline; nothing here parses real programs into a usable AST — the
//! grammar's own semantic actions are all `()`. The only thing that
//! matters is whether `lalrpop_mod!` below expands and compiles at all:
//! `lalrpop` refuses to generate a parser table for an ambiguous
//! (LALR(1)-conflicting) grammar, so a successful build of this crate
//! *is* the proof, not a description of one.

#[allow(clippy::all)]
lalrpop_util::lalrpop_mod!(pub nirdosha);

#[cfg(test)]
mod tests {
    use super::nirdosha::ProgramParser;

    fn parses(src: &str) -> bool {
        ProgramParser::new().parse(src).is_ok()
    }

    #[test]
    fn hello_example_parses() {
        assert!(parses(include_str!("../../examples/hello.nir")));
    }

    #[test]
    fn factorial_example_parses() {
        assert!(parses(include_str!("../../examples/factorial.nir")));
    }

    #[test]
    fn loop_example_parses() {
        assert!(parses(include_str!("../../examples/loop.nir")));
    }

    #[test]
    fn ownership_example_parses() {
        assert!(parses(include_str!("../../examples/ownership.nir")));
    }

    #[test]
    fn borrow_example_parses() {
        assert!(parses(include_str!("../../examples/borrow.nir")));
    }

    #[test]
    fn assignment_still_parses_as_assignment() {
        // The exact case the grammar's `Assignment` production doc
        // comment calls out: IDENT "=" ... vs. IDENT alone as the start
        // of `LogicOr`. If this line parses, the grammar wasn't
        // ambiguous about which alternative to take here.
        assert!(parses("fn main() { let x: i64 = 1 x = 2 }"));
    }

    #[test]
    fn bare_identifier_expression_still_parses_as_an_expression() {
        assert!(parses("fn main() { let x: i64 = 1 x }"));
    }

    #[test]
    fn garbage_is_rejected() {
        assert!(!parses("fn ( ) { ="));
    }

    // ---- Row 11/12/`WORKFLOW.md`/Track E1 declaration-level shapes ----
    // Added alongside `nirdosha.lalrpop`'s own `StructDecl`/`EnumDecl`/
    // `ScreenDecl`/`DashboardDecl`/`ModuleDecl`/`WorkflowDecl`/
    // `WorkspaceDecl` productions -- same "would pass and confirm this
    // specific shape is unambiguous *if* the crate built" caveat the
    // rest of this file already carries (see the module doc comment and
    // README's "What this crate still demonstrates" section).

    #[test]
    fn struct_and_enum_decls_parse() {
        assert!(parses("struct Point(A) { x: A, y: A, } enum Status { Open, Closed(i64), }"));
    }

    #[test]
    fn screen_and_dashboard_decls_parse() {
        assert!(parses(
            r#"struct Product { id: i64, name: i64, }
               screen Product {
                   title: 1
                   paginate { page_size: 1 }
                   field name { label: 1 }
                   action "Restock" -> restock_product { style: 1 }
               }
               dashboard {
                   tile "Count" -> stat_count
                   chart "By Price" -> chart_price
               }"#
        ));
    }

    #[test]
    fn module_decl_parses() {
        assert!(parses(r#"module "Vendors" { struct Vendor { id: i64, } fn list_vendor() {} }"#));
    }

    #[test]
    fn workflow_decl_parses() {
        assert!(parses(
            r#"workflow Approval {
                   data { amount: i64, }
                   state Draft {
                       on_entry { notify(1) }
                       on Submit -> Review
                   }
                   state Review terminal {
                       on link Approve -> Draft
                   }
               }"#
        ));
    }

    #[test]
    fn workspace_and_panel_decl_parses() {
        assert!(parses(
            r#"workspace CaseInvestigation {
                   title: 1
                   subject: 1
                   panel "Transactions" {
                       source: 1
                   }
                   panel "Notes" {
                       source: 1
                       action "Add Note" -> add_case_note { style: 1 }
                   }
               }"#
        ));
    }
}
