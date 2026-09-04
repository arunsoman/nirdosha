---
state: draft
shepherd: (unassigned)
---

# RFC 0002: Editor/tooling ecosystem — tree-sitter grammar + minimal LSP

## Motivation

No LSP, no tree-sitter grammar, no formatter, no debugger exist today.
`docs/ECOSYSTEM.md` §G2 records that `cie` (a related repo) already
frames this from the outside: Nirdosha is "a language with no LSP and
no tree-sitter grammar," worked around via raw AST dump
(`nirdosha emit-ast`). This is a real adoption barrier distinct from
the compiler's own correctness work — a contributor evaluating the
language today gets no inline diagnostics, no go-to-definition, no
syntax highlighting beyond whatever generic heuristic their editor
already applies to an unrecognized extension.

## Design

Build order, cheapest/highest-leverage first (`docs/ECOSYSTEM.md` §G2's
own sequencing, restated here as the thing this RFC is asking to
formally accept):

1. **Tree-sitter grammar (`grammar.js`).** [`crates/grammar_check/`](../crates/grammar_check/)
   already cross-checks the hand-written LL(1) parser
   (`crates/compiler/src/parser.rs`) against an independently-generated
   LALR(1) grammar — that pairing is the authoritative grammar source,
   per `docs/GRAMMAR.md`. `grammar.js` must be *derived from and
   checked against* that pair, not hand-authored a third time
   independently, or it drifts the same way the LL(1)/LALR(1) pair
   already once needed reconciling. Concretely: a generation script (or
   at minimum a corpus-based equivalence test reusing
   `crates/grammar_export/`'s corpus) that fails CI if `grammar.js`
   accepts/rejects something the real parser doesn't agree with.
2. **Minimal LSP — diagnostics first.** `typeck.rs`'s `TypeErrorKind`
   and `validate`'s Hoare-contract checker already produce real,
   spanned, structured errors (`--format=json`'s `Diagnostic` shape is
   the existing precedent) — wiring those through
   `textDocument/publishDiagnostics` is protocol plumbing over an
   existing result type, not new analysis. Go-to-definition next,
   riding `docs/LANGUAGE.md` §17's real module resolution (F2) once a
   program uses it. Refactors/code actions explicitly deferred to a
   later RFC, not this one.
3. **VS Code extension** as the first LSP client — largest install
   base, ships through the Marketplace, not a registry this project
   has to run itself (consistent with RFC 0001's "avoid hosting a new
   service" reasoning).
4. **Formatter — deliberately last.** There is no documented canonical
   style yet. A formatter without one is either silently opinionated
   (surprising) or does nothing (pointless) — a style decision is a
   prerequisite this RFC doesn't make, and shouldn't: it belongs in its
   own follow-up RFC once there's real multi-author code to observe
   actual style drift on.

## Effect on the permission model

None. This is tooling that reads a program and reports on it; it
doesn't change what a `.nir` program can express or how `requires`/
`acquire`/gates are enforced at runtime.

## Compatibility

Fully additive — new crates/tooling, zero changes to the compiler's
accepted grammar or runtime behavior. The one place this *could* touch
existing code is if `grammar_check`'s corpus format needs extending to
serve as `grammar.js`'s equivalence-test input; that extension must
stay backward compatible with `grammar_check`'s existing consumers.

## Rejected alternatives

- **Hand-author `grammar.js` independently.** Rejected up front — this
  is exactly the two-grammars-drift problem `docs/GRAMMAR.md` already
  had to fix once for LL(1) vs. LALR(1); a third independently-authored
  grammar is a third thing to keep in sync by hand.
- **Formatter before a style decision.** Rejected — see Design item 4.
- **A from-scratch custom protocol instead of LSP.** Not seriously
  considered: LSP already has editor-side clients for every major
  editor; a bespoke protocol would need to build those clients too.

## Open questions

- **Where does the tree-sitter grammar live?** A new
  `crates/tree-sitter-nirdosha/` in this repo, or a separate
  `nirdosha/tree-sitter-nirdosha` repo (tree-sitter's own ecosystem
  convention leans toward separate repos per grammar, for its own
  packaging/npm story). Affects CI wiring and release process either
  way.
- **LSP implementation language.** Rust (reusing `typeck.rs`/`parser.rs`
  directly, no FFI) is the obvious default given the compiler is
  already Rust, but not yet formally decided in this draft.
- **Scope of "minimal."** Diagnostics + go-to-definition is this RFC's
  stated v1; hover-for-type-info and find-references are natural next
  steps but deliberately unscoped here rather than silently assumed.
