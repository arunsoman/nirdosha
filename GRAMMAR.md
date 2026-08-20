# Nirdosha — grammar

Scope: the "Core language" slice from `goal.md` §3/§6, plus Phase 1's first
increment — `box`/`*` and the ownership discipline they make meaningful
(`compiler/src/ownership.rs`) — plus a first slice of concurrency (rows
2–3): `spawn`/`join`/`thread T` (real OS threads; see `PHASE0.md`'s
"Eleventh update") and `chan`/`send`/`recv` (see its "Twelfth update").
Still no effects or refinement types; those remain later-phase additions
layered on top of this grammar without changing it, which was the point
of getting the grammar's shape right early (§6 Phase 0 note: retrofitting
parseability later is expensive). Note that `spawn`/`join`/`thread`/
`chan`/`send`/`recv` are, so far, interpreter-only — `codegen.rs` rejects
them explicitly, the same "reject, don't mis-compile" treatment `box`/`&`
got before their own codegen support existed.

## Row 7 claim, stated precisely

The parser (`compiler/src/parser.rs`) is hand-written recursive descent with
**strictly one token of lookahead and no backtracking**, anywhere. That is
the operational definition of LL(1): at every point, the next token alone
determines which production applies. Binary-operator precedence is handled
by precedence climbing inside expression parsing, not by grammar
left-recursion — which is what keeps the expression grammar LL(1)-parseable
without a separate transformation step.

**Update — now cross-checked, and the check found something real.**
`grammar_check/` (a separate crate; see its README for the full story) ran
this grammar through `lalrpop`, an independent LALR(1) generator. It does
not build cleanly, and the reason is worth stating as a rule rather than
leaving buried in a build log: **this language has no statement
separator — no semicolons, no significant newlines — so wherever an
operator token could either extend the current expression or start a new
statement, the grammar is genuinely ambiguous as a plain CFG.** The
parser resolves every one of these cases the same deterministic way —
**always prefer to extend the current expression over ending the
statement** (equivalently: shift over reduce, always) — but that rule
was previously implicit in `parser.rs`'s control flow only, never stated
here. It's real and load-bearing: `return x` immediately followed on the
next line by `-y`, with nothing between them, parses as `return (x - y)`
— one statement, a subtraction — not as `return x` followed by a
separate `-y` statement, checked directly against the running
interpreter (`grammar_check/README.md` has the transcript). Every
`stmt ::= ...` alternative below should be read with this rule attached,
not as free-standing productions a parser could combine in whatever
order first succeeds.

This is the LALR(1) claim's honest final form: the *hand-written parser*
is unambiguous (deterministic, single-token lookahead, no backtracking —
the original claim above still holds, and still matters for row 7). The
*grammar as an abstract CFG*, independent of any particular parser
implementing it, is not unambiguous without this rule stated explicitly
— a distinction that only became visible by actually running a second,
independent tool against it, not by re-reading the hand-written parser
more carefully.

## EBNF

**Disambiguation rule this EBNF alone doesn't state** (found by the
LALR(1) cross-check above, not designed in up front): a `block`'s
`stmt*` (below) has no separator between statements. Wherever a token
could either extend the previous statement's expression or begin a new
statement, **always extend the previous one** — shift over reduce, with
no exception. Two concrete cases, the simplest one first:

```
let x: i64 = 1
-2
```
parses as one statement, `let x: i64 = (1 - 2)` (`x` is `-1`) — never as
`let x: i64 = 1` followed by a separate `-2` expression-statement.

```
return x
-y
```
parses as `return (x - y)` — one statement, a subtraction — never as
`return x` followed by a separate `-y` statement.

```ebnf
program     ::= item*

item        ::= fn_decl

fn_decl     ::= "fn" ident "(" params? ")" ("->" type)? block

// No trailing comma — `params`/`args` (below) both require a following
// item after every comma, so `fn f(a: i64,)` and `f(1, 2,)` are both
// parse errors, checked directly, not assumed. A real, small ergonomic
// gap (trailing commas are usually a courtesy for editing/diffing), not
// a deliberate design stance — worth a cheap fix if it comes up again.
params      ::= param ("," param)*
param       ::= ident ":" type

// `usize` exists (Rust-style: for sizes/indices, unsigned) with no
// `isize` counterpart — intentional, not an oversight: `i64` already
// covers the signed pointer-width case this language would want `isize`
// for, and nothing here indexes anything yet (no arrays — see
// omissions), so a second signed-width-of-pointer type has no use to
// motivate it yet. `codegen.rs` doesn't support any `u8..usize` type at
// all yet regardless (needs a signed-vs-unsigned instruction choice not
// yet made — see its module doc).
//
// `unit` is a type keyword only — there is no expression-level literal
// for constructing a `unit` *value* explicitly. `primary` above has no
// `()`-as-empty-group alternative, so a `unit`-typed value only ever
// arises implicitly (a function with no declared return type running to
// completion, or the result of calling one) — you cannot write `let x:
// unit = <something>` except by assigning the result of such a call;
// there's no direct literal to put on the right of `=`.
// `thread T` and `chan T` (goal.md rows 2-3) follow `box`'s shape exactly
// — a prefix type-former wrapping another `type`, not a separate grammar
// category. `thread T` is affine (a spawned computation has exactly one
// owner, `join` consumes it); `chan T` is **not** — see `Ty::Channel`'s
// doc comment in `ast.rs` for why a channel handle needs to stay freely
// copyable while its *payload* still moves through `send`.
type        ::= "&" type
              | "box" type
              | "thread" type
              | "chan" type
              | "i8" | "i16" | "i32" | "i64"
              | "u8" | "u16" | "u32" | "u64" | "usize"
              | "bool" | "unit"

// A block's *value* (relevant wherever a block sits in an expression
// position — an `if`'s branches, most concretely) is its last
// statement's expression, if that last statement is a bare `expr_stmt`
// — the same convention Rust's blocks use. A block that's empty, or
// whose last statement is `let`/`return`/`while`, has value `unit`.
// This governs `if`-as-a-value (`let x: i64 = if c { 1 } else { 2 }`)
// and is load-bearing for every pass that walks a block (`typeck.rs`,
// `ownership.rs`, `refine.rs`, `smt.rs`, `codegen.rs` all implement it
// identically) — stated here because nothing in the EBNF below implies
// it on its own; `block ::= "{" stmt* "}"` alone reads as purely
// imperative, no value implied.
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
// shape as C/Rust's assignment-expression. The `ident` restriction on the
// left side is a real *grammar* restriction, not merely an artifact of
// how the parser happens to be written — there is no production anywhere
// that lets a general expression (`foo.bar`, `foo[0]`, ...) appear as an
// assignment target, and in fact neither of those exists as an
// expression *at all* yet (no field access, no indexing — see the
// omissions list). If/when either is added, extending `assignment`'s
// left side to more than `ident` is a real grammar change, not just a
// parser one.
//
// Implementation note, distinct from the grammar restriction above: the
// parser doesn't try `ident "="` as a distinct alternative (that would
// need two tokens of lookahead at the start). It parses a full
// `logic_or` first — which for a bare name yields an `Ident` expression
// — and only *then* checks whether the current token is `=` and the
// thing it just built was exactly an `Ident`. That's still a
// single-token decision at the point the decision is made; it just
// happens after, not before, parsing the left side. If the grammar
// restriction above ever widens, this parsing technique doesn't
// automatically follow — it would need its own redesign.
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
//
// `box`/`*`/`!`/`-` all apply to a full `unary`, not just a `primary` —
// so `box f()` boxes the *result of calling* `f`, `*g()` dereferences a
// call's result, and so on. This is intentional, not a surprising
// consequence of the grammar's shape: `box`/`&` wrap an arbitrary
// expression (see `Expr::Box`/`Expr::Ref` in ast.rs — neither is
// restricted to a `Primary` operand), the same way `!`/`-` do. `box`
// specifically is not a type constructor with special primary-only
// syntax the way it might read in some languages; it's an ordinary
// prefix operator over expressions.
//
// `&expr`'s operand is restricted, after parsing, to exactly `Expr::Ident`
// — see `Expr::Ref`'s doc comment in ast.rs for why. Two independent,
// separately-checked limitations stack here, not one: (1) `&&x` lexes as
// one `AndAnd` token (needed for the boolean operator), so a
// reference-to-a-reference can't even be *written* with `&&` — the same
// ambiguity early C-family lexers have historically had. (2) Writing it
// with a space instead (`& &x`) *does* lex as two separate `&` tokens —
// checked directly (`& &n` for `n: i64` produces two `Amp` tokens, not
// one `AndAnd`) — but is *still* rejected, by the Ident-only operand
// restriction just above: `&x`'s own operand is `Expr::Ref(...)`, not a
// bare `Expr::Ident`, so parsing fails with "`&` can only borrow a plain
// variable name" regardless of the lexer question. Fixing the lexer
// wouldn't be enough on its own to make `& &x` legal; both limitations
// would need to be addressed together.
// `spawn`'s operand is restricted, after parsing, to exactly `Expr::Call`
// — the same "parse normally, then validate what came out" technique
// `&`'s `Expr::Ident` restriction above uses. `spawn` runs a *named
// function*, not an arbitrary expression, so `spawn f()()` (were it even
// legal — see `call`'s own arity restriction below) or `spawn (1 + 2)`
// are both rejected with a specific message, not a generic parse error.
// `chan` takes no operand at all — it's a nullary keyword in expression
// position (see `Expr::Chan` in ast.rs for why: unlike `box`/`spawn`, it
// has no sub-expression to infer a payload type from, so it only
// type-checks against an already-known `chan T` expectation).
// `send`/`recv` don't fit this file's "prefix keyword wraps a `unary`"
// shape at all — `send` needs *two* operands (the channel, the payload),
// so both use an explicit, fixed-arity `"(" ... ")"` form instead, closer
// in shape to `call` below than to `spawn`/`join`.
unary       ::= ("!" | "-" | "*" | "box" | "&") unary
              | "spawn" call
              | "join" unary
              | "chan"
              | "send" "(" expr "," expr ")"
              | "recv" "(" expr ")"
              | call

// Exactly zero or one call, not "zero or more" — `f()()` is a **parse
// error**, checked directly against the real parser, not assumed:
//
//     parse error: expected an expression, found RParen
//
// A `*` here (as an earlier revision of this EBNF had it, claiming a
// call's *result* could itself be called again — currying-style) would
// be wrong twice over: `parser.rs`'s `parse_call` only ever consumes one
// `"(" args ")"` and returns immediately, and the language has no
// function-value concept for a second call to even mean anything against
// — `Expr::Call` names its callee by a plain identifier, resolved by
// lookup, not evaluated as a first-class value. Found the same way the
// statement-separator ambiguity was: by writing the case out and running
// it, not by re-reading the code more carefully.
call        ::= primary ("(" args? ")")?
args        ::= expr ("," expr)*

// `"(" expr ")"` requires a real `expr` inside — `()` alone (an empty
// parenthesized group) is not a valid `primary`, and isn't a way to
// spell a `unit` *value* either; see the omissions list for what that
// means for `unit`.
primary     ::= int_lit | "true" | "false" | ident | "(" expr ")"

// Decimal digits only — no `0x`/`0b` prefixes, no `_` digit-group
// separators (`1_000`). Not needed for Phase 0's examples; listed in
// omissions rather than silently absent.
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
- **Superseded, kept for history — split out since the original single
  paragraph made it easy to miss that two of these are real, working
  passes, not aspirations:**
  - *What used to be true:* type checking was purely dynamic (checked as
    the interpreter executed, Python-style), and assignment had "no
    ownership model, explicitly not what row 1 asks for."
  - *What's true now:* both are false. `compiler/src/typeck.rs` is a
    real static pass (a program that fails it is never executed).
    `compiler/src/ownership.rs` statically enforces single ownership for
    `box`-typed bindings, including branch-merge and loop-reassignment
    cases (see `PHASE0.md`'s ownership updates for exactly what's proved
    and what isn't). Shared borrows (`&`) exist too — a function can
    read a value without consuming it.
  - *What's still true:* assignment (`x = expr`) reassigns any binding
    in place with no borrowing discipline applied to *it* — a scalar
    local is exactly as freely mutable as a Python local, and even a
    `box` binding can be freely reassigned (fine: reassignment isn't
    aliasing, it just clears the moved-from flag; see `ownership.rs`'s
    doc comment). `&mut` (exclusive/mutable borrows) doesn't exist yet —
    it needs real liveness tracking to enforce "aliasing xor mutability"
    that shared `&` didn't need. Reading *through* `&box T` to scalar
    content inside is also still unsupported (no place-expression
    semantics yet — see `ownership.rs`'s module doc).

### Known soundness bug, found and fixed during development

`box box i64`-style nested boxes work, but tripped a real soundness gap
while this was being built: `ownership.rs`'s first draft exempted *every*
deref from move-checking, which is only correct when what comes out is a
scalar — `*bb` for `bb: box box i64` hands out the affine inner `box i64`
by value, so it has to consume `bb`, and the first draft didn't. Fixed
(see `ownership.rs`'s `Expr::Deref` handling) and pinned by
`tests/ownership.rs::dereferencing_a_nested_box_twice_is_use_after_move`
— worth its own heading, not a trailing bullet, because it's a concrete
example of exactly the kind of bug this whole checker exists to rule
out, caught in the checker's *own* code during development, not in a
user's program.

## Independent cross-check

`grammar_check/` (top-level, sibling to `compiler/` — see
[`../grammar_check/README.md`](../grammar_check/README.md)) runs this
EBNF through `lalrpop`, an independent LALR(1) generator, as a second
check beyond "the hand-written parser is single-token-lookahead by
construction." It's what found the statement-separator ambiguity
documented above. It does not build cleanly, on purpose and by design —
see its README for why that's the actual, informative result, not a
broken build waiting to be fixed.
