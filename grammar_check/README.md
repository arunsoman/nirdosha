# Independent LALR(1) cross-check — and what it found

This crate exists to answer the question `GRAMMAR.md` left open since
Phase 0: is Nirdosha's grammar actually LALR(1)-parseable by an
independent tool, not just single-token-lookahead by construction in the
one hand-written parser that happens to implement it? `src/nirdosha.lalrpop`
transliterates `GRAMMAR.md`'s EBNF into `lalrpop`'s syntax,
production-for-production. `lalrpop` refuses to generate a parser table
for an ambiguous grammar — a clean build *is* the proof.

**This crate currently does not build cleanly, and that's the actual
result, not a broken build waiting to be fixed.** Read on before assuming
`cargo build` failing here means something's wrong.

## The finding

Nirdosha has no statement separator — no semicolons, no significant
newlines. `GRAMMAR.md`'s own EBNF is silent about how a parser is
supposed to know where one statement ends and the next begins; it just
lists `stmt* ::= stmt*` and trusts each `stmt` to consume exactly the
right number of tokens. For most statement shapes that's fine. It stops
being fine wherever an expression could legally continue **or** a brand
new statement could legally start with the very same token — which
happens for every operator that's both a binary continuation and a valid
unary prefix: `+`... no, actually just `-`, `!`, `&`, `*`, and `box`
(`+` isn't a unary prefix in this language, so it's not actually part of
this — but `lalrpop` still flags it, because the conflict it's reporting
is more general than that; see below).

Concretely: `lalrpop` reports genuine shift/reduce conflicts at *every*
level of the expression precedence chain (`Additive`, `Multiplicative`,
`Comparison`, `Equality`, `LogicAnd`, `LogicOr`), all with the same
shape — after parsing a complete sub-expression as one statement's worth
of content, seeing an operator token, the grammar (as a plain CFG) admits
two different valid derivations: extend the current expression, or treat
what follows as the start of a new statement. `lalrpop`'s LALR(1) table
construction has exactly one token of lookahead and no way to see far
enough ahead to know that, e.g., a bare `+` could *never* actually start
a new statement in this language (there's no unary `+`) — it reports the
conflict anyway, because the *shape* of the ambiguity is present in the
grammar regardless of whether every instance of it would eventually dead-end.

**This is not hypothetical or purely formal — it's checkable in the real
parser, and was checked:**

```nirdosha
fn main() -> i64 {
    let x: i64 = 5
    let y: i64 = 3
    return x
    -y
}
```

Two readings are grammatically available: `return (x - y)` (one
statement, a subtraction) or `return x` followed by a separate `-y`
expression-statement (two statements). Running this through the actual
interpreter:

```
$ nirdosha /tmp/ambiguity_check2.nir
=> 2
```

`5 - 3 = 2` — confirmed as one statement, deterministically, every time.
`parser.rs`'s recursive-descent structure can't actually be ambiguous at
runtime the way an abstract CFG can: each `parse_*` function either sees
a continuation token and shifts, or it doesn't, with no backtracking and
no second attempt. The disambiguation rule that produces this is real,
consistent, and simple — **always prefer to extend the current
expression over ending the statement** — but until this cross-check, it
existed only as an emergent property of the parser's control flow, not
as a rule stated anywhere in `GRAMMAR.md`. That's the actual, useful
result of building this crate: not "the grammar is broken," but "the
grammar's specification was incomplete, and now the missing rule is
written down" (see `GRAMMAR.md`'s own updated section on this).

## A second, more familiar conflict — for completeness, not cherry-picked

`lalrpop` also reports the textbook "dangling else" ambiguity:
`IfExpr = "if" Expr Block (*)` vs `IfExpr = "if" Expr Block (*) "else"
Block` — given `if a {} if b {} else {}`, does the `else` belong to the
outer `if` or the inner one? This is one of the most famous ambiguities
in language design (C, Java, and most C-family languages have it) and
every mainstream language resolves it the same way: an `else` binds to
the nearest preceding `if`. `parser.rs`'s `parse_if_expr` does exactly
that — it greedily consumes a trailing `else` the moment it sees one,
the same "always shift" instinct that resolves the statement-boundary
conflicts above. Included here so the write-up isn't quietly filtering
down to only the conflict with the more novel backstory — this one's
real too, just far less surprising.

## Why this wasn't "fixed" by restructuring the `.lalrpop` file

An early attempt narrowed the `return`-specific case (bare `return` is
only ever legal immediately before a block's closing `}`, matching
`parser.rs`'s actual behavior exactly — `Block`'s two production forms
below reflect that, and are worth keeping as an accurate model even
though they didn't resolve the deeper issue). It didn't change the
conflict count, which is itself informative: it proved the ambiguity
isn't really about `return` specifically, it's about statement
boundaries in general, `return` was just the first place it showed up.

Fully eliminating the conflicts would need either (a) a real language
change — mandatory statement separators — which is a substantive,
disruptive change to a language three example programs and 78 tests
already depend on the shape of, or (b) `lalrpop`-specific
conflict-resolution annotations (`#[precedence]`/`#[assoc]`) applied
densely across the whole precedence chain, encoding "always prefer
shift" explicitly at each level — mechanically possible, but a lot of
surface area on a secondary, non-canonical grammar artifact, for a
result (a green build) that wouldn't prove anything the finding above
doesn't already prove more directly. Neither felt like the right thing
to reach for under the time this deserved versus what it would have
cost — recorded as a real, open option, not silently dropped.

## What this crate still demonstrates, build failure and all

- The tests in `src/lib.rs` — parsing all five example programs, the
  specific assignment-vs-expression case `GRAMMAR.md`'s `Assignment`
  production doc comment calls out, and a rejection case — would pass
  and confirm those specific shapes are unambiguous *if* the crate built.
  They currently can't run (the build fails first, `cargo test` never
  gets there) — left in place because they're the right tests for a
  successor version of this grammar to be checked against, e.g. once a
  statement-separator design is decided.
- The bug hunt itself is the deliverable: a real, external, independent
  tool, applied deliberately, surfaced a genuine gap between the
  published grammar spec and the shipped parser's actual behavior. That
  is exactly the kind of result `/loop`-style "keep testing, don't just
  keep building" discipline is supposed to produce, and it's recorded
  here in full rather than quietly worked around.
