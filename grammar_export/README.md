# GBNF export — what this proves and how

This crate produces and tests `../compiler/nirdosha.gbnf`, the
constrained-decoding grammar artifact goal.md row 7 asks for: a
machine-readable grammar spec an LLM's sampler can use to guarantee every
token it emits stays inside Nirdosha's syntax, the same way
`grammar_check/`'s `lalrpop` cross-check independently confirmed the
hand-written parser's grammar is genuinely LL(1)/LALR(1).

## This is not a mechanical export

There is no tool that converts `parser.rs`'s recursive-descent
implementation, or `GRAMMAR.md`'s EBNF, into GBNF automatically.
`../compiler/nirdosha.gbnf` is a **hand translation**, production for
production, the same discipline `grammar_check/src/nirdosha.lalrpop`
already follows for its own (differently-scoped) cross-check. A hand
translation can have its own bugs independent of whatever it's
translating from — which is exactly why this crate exists: to catch
those bugs against ground truth, not to assert the translation is
correct by construction.

## Two independent checks, on purpose

**1. Is the file valid GBNF at all?** `tests/fidelity.rs`'s first test
feeds the whole grammar to `llama-cpp-gbnf`, a Rust binding around
llama.cpp's *actual* grammar parser (a real dependency — pulling in and
building llama.cpp's C++ sources, hence the multi-minute first build).
This is a genuine, non-trivial check: it caught a real bug while this
grammar was being written. llama.cpp's parser does not treat a bare `|`
at the start of a continuation line as "keep extending the previous
rule" the way the visually similar EBNF style in `GRAMMAR.md` reads —
every multi-line alternation in `nirdosha.gbnf` has to be wrapped in
`(...)` for the real parser to accept it. Nothing about GBNF's published
spec makes this obvious in advance; it was found by actually running the
real engine against the file, not by re-reading it more carefully.

**2. Does the file accept and reject the same programs the real
compiler does?** `validate_gbnf` only proves the grammar *parses*, not
that it matches what it's supposed to match. For that, `tests/
fidelity.rs` runs a corpus — every shipped `.nir` example plus a set of
hand-written positive/negative snippets — through **two** independent
matchers and asserts they agree:

- the real `nirdosha` lexer + parser (a normal dependency on the
  `compiler/` crate), and
- this crate's own small, general-purpose GBNF interpreter (`src/
  lib.rs`) — parses whatever `.gbnf` text it's handed into rules and
  matches candidate strings against them.

That second matcher is deliberately *not* llama.cpp's engine: the public
`llama-cpp-gbnf` binding only exposes grammar validation, not
string-acceptance testing (real string matching through llama.cpp needs
a live token vocabulary and sampler loop, a much heavier dependency than
this test earns). It's an honest second implementation, not a
rubber stamp — it's generic (it interprets whatever grammar file it's
given; it isn't tuned to make `nirdosha.gbnf`'s specific rules pass) and
it's checked against the real compiler specifically so a disagreement
gets investigated, not silently trusted in either direction.

## What's still a known gap

`ident` in `nirdosha.gbnf` accepts any identifier-shaped text, including
this language's keywords (`let`, `fn`, `if`, ...) — GBNF has no clean
negative-lookahead primitive without enumerating every keyword as an
explicit exclusion. This is a deliberately accepted, safe-direction
imprecision (documented at the top of `nirdosha.gbnf` itself): a decoder
that occasionally permits `let` where the real grammar wouldn't is
harmless (the real parser/typeck still rejects it downstream), whereas a
decoder that's ever too *strict* would block a legal completion outright.
The fidelity corpus tests real programs, not adversarial
keyword-as-identifier cases, so this gap doesn't show up as a test
failure — it's a scope boundary, not something hiding a real bug.
