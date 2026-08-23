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
}
