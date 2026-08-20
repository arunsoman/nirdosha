# Nirdosha — grammar

Scope: the "Core language" slice from `goal.md` §3/§6, plus Phase 1's first
increment — `box`/`*` and the ownership discipline they make meaningful
(`compiler/src/ownership.rs`). Still no effects or refinement types, and
still no concurrency; those remain later-phase additions layered on top of
this grammar without changing it, which was the point of getting the
grammar's shape right early (§6 Phase 0 note: retrofitting parseability
later is expensive).

## Row 7 claim, stated precisely

The parser (`compiler/src/parser.rs`) is hand-written recursive descent with
**strictly one token of lookahead and no backtracking**, anywhere. That is
the operational definition of LL(1): at every point, the next token alone
determines which production applies. Binary-operator precedence is handled
by precedence climbing inside expression parsing, not by grammar
left-recursion — which is what keeps the expression grammar LL(1)-parseable
without a separate transformation step.

This is *not yet* cross-checked against an independent LALR(1) parser
generator (e.g. `lalrpop`) — that's a deliberate deferral, not an oversight:
the grammar is still small and expected to change through Phase 1–2, and a
generator cross-check is worth doing once it stabilizes, not on every
edit. Track that as an open item, not a closed claim.

## EBNF

```ebnf
program     ::= item*

item        ::= fn_decl

fn_decl     ::= "fn" ident "(" params? ")" ("->" type)? block

params      ::= param ("," param)*
param       ::= ident ":" type

type        ::= "&" type
              | "box" type
              | "i8" | "i16" | "i32" | "i64"
              | "u8" | "u16" | "u32" | "u64" | "usize"
              | "bool" | "unit"

block       ::= "{" stmt* "}"

stmt        ::= let_stmt
              | return_stmt
              | while_stmt
              | expr_stmt

let_stmt    ::= "let" ident ":" type "=" expr
return_stmt ::= "return" expr?
while_stmt  ::= "while" expr block
expr_stmt   ::= expr

expr        ::= if_expr
              | assignment

if_expr     ::= "if" expr block ("else" (block | if_expr))?

// Right-associative, lowest precedence among non-`if` expressions — same
// shape as C/Rust's assignment-expression. Implementation note: the parser
// doesn't try `ident "="` as a distinct alternative (that would need two
// tokens of lookahead at the start). It parses a full `logic_or` first —
// which for a bare name yields an `Ident` expression — and only *then*
// checks whether the current token is `=` and the thing it just built was
// exactly an `Ident`. That's still a single-token decision at the point
// the decision is made; it just happens after, not before, parsing the
// left side.
assignment  ::= ident "=" assignment
              | logic_or

logic_or    ::= logic_and ("||" logic_and)*
logic_and   ::= equality ("&&" equality)*
equality    ::= comparison (("==" | "!=") comparison)*
comparison  ::= additive (("<" | ">" | "<=" | ">=") additive)*
additive    ::= multiplicative (("+" | "-") multiplicative)*
multiplicative ::= unary (("*" | "/") unary)*
// `*` is unary deref here, not multiplication — `multiplicative` only ever
// sees `*` in infix position, after a full `unary` is already parsed, so
// there's no ambiguity: which meaning applies is determined purely by
// which production is asking, never by extra lookahead.
// `&expr`'s operand is restricted, after parsing, to exactly `Expr::Ident`
// — see `Expr::Ref`'s doc comment in ast.rs for why. Known limitation:
// `&&x` lexes as one `AndAnd` token (needed for the boolean operator), so
// a reference-to-a-reference isn't expressible at all right now — the
// same ambiguity early C-family lexers have historically had, not solved
// here, not silently pretended away either.
unary       ::= ("!" | "-" | "*" | "box" | "&") unary | call
call        ::= primary ("(" args? ")")*
args        ::= expr ("," expr)*

primary     ::= int_lit | "true" | "false" | ident | "(" expr ")"

int_lit     ::= digit+
ident       ::= alpha (alpha | digit | "_")*
```

## Deliberate omissions (Phase 0 boundary, not forgotten)

- No `for` loop yet — `while` is the one structured-iteration primitive
  until the design decides whether `for` is sugar over `while` (compositional,
  row 8) or a separate construct (simpler to read, row 6). Undecided on
  purpose rather than guessed.
- No structs/enums/arrays yet — `type` is a closed keyword set, not a
  grammar production, because refinement types (row 4, Phase 2) will change
  how array/index types are spelled and it's cheaper to add that once than
  to add arrays now and redesign them in Phase 2.
- No `unsafe`/`audited` block syntax yet — that's the Tier-3 escape valve
  from `goal.md` §4, which presupposes Tier 1/2 (the SMT-discharged
  refinement layer) existing first.
- **Superseded, kept for history:** the two bullets that used to live here
  said type checking was purely dynamic and that assignment had "no
  ownership model, explicitly not what row 1 asks for." Both are now false
  — `compiler/src/typeck.rs` is a real static pass, and
  `compiler/src/ownership.rs` statically enforces single ownership for
  `box`-typed bindings (see PHASE0.md's ownership section for what that
  does and doesn't prove). What's still true: assignment (`x = expr`)
  reassigns any binding in place with no *borrowing* discipline at all — a
  scalar local is exactly as freely mutable as a Python local, and even a
  `box` binding can be freely reassigned (which is fine — reassignment
  isn't aliasing, it just clears the moved-from flag; see
  `ownership.rs`'s doc comment). There is still no way to *borrow* a value
  without moving or copying it (no `&`/`&mut`), which is the next real gap
  once refinement types (row 4) are further along.
- `box box i64`-style nested boxes work, but tripped a real soundness gap
  during testing: `ownership.rs`'s first draft exempted *every* deref from
  move-checking, which is only correct when what comes out is a scalar —
  `*bb` for `bb: box box i64` hands out the affine inner `box i64` by
  value, so it has to consume `bb`, and the first draft didn't. Fixed (see
  `ownership.rs`'s `Expr::Deref` handling) and pinned by
  `tests/ownership.rs::dereferencing_a_nested_box_twice_is_use_after_move`
  — recorded here because it's a good example of exactly the kind of bug
  this whole checker exists to rule out, caught in the checker's own code
  during development, not in a user's program.
