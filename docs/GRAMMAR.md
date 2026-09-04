# Nirdosha — grammar

Scope: the "Core language" slice from `docs/goal.md` §3/§6, plus Phase 1's first
increment — `box`/`*` and the ownership discipline they make meaningful
(`crates/compiler/src/ownership.rs`) — plus a first slice of concurrency (rows
2–3): `spawn`/`join`/`thread T` (real OS threads; see `docs/PHASE0.md`'s
"Eleventh update") and `chan`/`send`/`recv` (see its "Twelfth update") —
plus a first slice of `docs/SANDBOXING.md`'s sandboxing extension: `sandbox`/
`stop` (an affine handle around a real, separate OS process; see
`docs/PHASE0.md`'s "Thirteenth update"), `chan` wired to a real cross-process
transport for sandboxed processes ("Fourteenth update"), and `str`/`tcp`/
`connect` (a real TCP client — the prerequisite for orchestrating an
*arbitrary* containerized workload, not just another Nirdosha process;
see `docs/PHASE0.md`'s "Fifteenth update"), plus Row 11
(`docs/nirdosha_row11_amendment.md`) — `struct`/`enum` declarations (with
type-parameter lists — layer 6, generics: `Pair(A, B)`), `match`,
`expr.field` access, and the `Option(T)`/`Result(T, E)` prelude (layer
7), injected into every program at parse time — plus Row 12's `screen`/
`dashboard` declarative UI DSL (docs/LANGUAGE.md §11), consumed only by
`nirdosha emit-ui`/`nirdosha serve`. Note that
`spawn`/`join`/`thread`/`chan`/`send`/`recv`/`sandbox`/`stop` are, so
far, interpreter-only — `codegen.rs` rejects them explicitly, the same
"reject, don't mis-compile" treatment `box`/`&` (and, later,
`str`/`tcp`/`connect`) got before *their* own codegen support existed.
`box`/`&`/`*`, `str`, `tcp`/`connect`/`listen`/`accept`/`send`/`recv` (the
`tcp` case of the last two, not the `chan` one), and — as of Phase 4a —
`struct`/`enum`/`match` over **non-affine** payloads are **compiled** now
(an affine-containing `struct`/`enum`/`match` — a `box`/`&`/`tcp`/`file`/
`db`/`mq` field, transitively — is the one still-rejected Row 11 case,
Phase 4b; see `docs/LANGUAGE.md` §10). See `docs/LANGUAGE.md` §10 for the current,
verified list; this file only tracks grammar scope, not
compiled-vs-interpreter status, so check §10 directly rather than inferring
it from this paragraph's history. `screen`/`dashboard` are different in kind, not just degree:
`codegen.rs` never inspects `Program.screens`/`.dashboard` at all (there's
no expression inside either for it to walk), so `nirdosha build`/
`emit-llvm` compile a program containing them cleanly — the declarations
are simply inert to codegen, not rejected by it.

## Row 7 claim, stated precisely

The parser (`crates/compiler/src/parser.rs`) is hand-written recursive descent with
**strictly one token of lookahead and no backtracking**, anywhere. That is
the operational definition of LL(1): at every point, the next token alone
determines which production applies. Binary-operator precedence is handled
by precedence climbing inside expression parsing, not by grammar
left-recursion — which is what keeps the expression grammar LL(1)-parseable
without a separate transformation step.

**Update — now cross-checked, and the check found something real.**
`crates/grammar_check/` (a separate crate; see its README for the full story) ran
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
interpreter (`crates/grammar_check/README.md` has the transcript). Every
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
program     ::= use_decl* item*

// `use "relative/path.nir"` (`docs/ROADMAP.md` Track F, F2 piece 3;
// `docs/NEXT_GEN.md` §F2) — only legal in this leading run, before any
// other item (`parser::parse_program`); a `use` anywhere else falls
// through to `item`'s own dispatch and hits an ordinary "expected an
// item" parse error, the ordinary consequence of a keyword misplaced
// outside its one legal slot, not a special-cased rejection.
use_decl    ::= "use" string

item        ::= fn_decl | struct_decl | enum_decl | screen_decl | dashboard_decl | module_decl | workflow_decl | workspace_decl | validate_decl

// `Mod::Name` / `Mod::Enum::Variant` — a qualified reference
// (`docs/ROADMAP.md` Track F, F2). Only ever a *reference*: a declaration's
// own name (`fn_decl`/`struct_decl`/`enum_decl`/`module_decl`'s own
// `ident`) is always a single plain `ident`, never a path — nothing
// can be *declared* at a qualified name, only namespaced by the
// `module_decl` it's lexically inside. Reused wherever `ident` names
// something to look up rather than to declare: `type`'s struct/enum
// alternative, `primary`'s bare-name alternative (covers both a value
// reference and, via `call`, a callee), and `variant_arm`'s pattern —
// see `ast::scope_key`'s doc comment for the resolution rule this
// feeds: a bare reference (no `::`) can only ever match a
// non-namespaced declaration; a qualified one matches a namespaced
// declaration's own canonical key exactly.
qualified_ident ::= ident ("::" ident)*

fn_decl     ::= "fn" ident "(" params? ")" ("->" type)? effect_annotation? requires_annotation? block

// `effect(pure)` / `effect(io, network, ...)` -- `None` (fully inferred,
// the common case) if absent. `parser.rs::parse_effect_annotation`:
// `pure` denotes the empty effect set and can't be combined with any
// other name (`effect(pure, io)` is a parse error, not silently "just
// `io`") -- not expressible in this EBNF shape (a semantic check, same
// class of thing `match`'s "at most one literal domain" rule already
// isn't), enforced by the parser instead.
effect_annotation ::= "effect" "(" effect_name ("," effect_name)* ")"
effect_name ::= "pure" | "rng" | "io" | "concurrent" | "network"

// `requires(role: "admin")` / `requires(claim: "department",
// "cardiology")` / `requires(public)` -- `None` (ungated, the common
// case) if absent. `role`/`claim`/`public` are matched by identifier text
// only in this one slot, same "keyword only within this one leading
// position" treatment `screen_decl`'s own `field`/`action`/`paginate`
// names get (see below). `public` (added `docs/ROADMAP.md` A10 / `API_TRUST_
// MODEL.md` §4) is an explicit, typechecked "this fn is intentionally
// callable with no token" marker -- unlike `role`/`claim` it does not
// gate the function (`ast::FnDecl::requires` stays `None`; a
// `requires(public)` fn needs no `acquire` and is exactly as directly
// callable as one with no `requires(...)` at all); its only effect is
// silencing `typeck::ungated_fn_warnings`' warning for that one fn.
requires_annotation ::= "requires" "(" ("role" ":" str | "claim" ":" str "," str | "public") ")"

// Row 11 (`docs/nirdosha_row11_amendment.md`) — product and sum types.
// `type_params` (layer 6, generics) is an optional bare-name list, empty
// for a fully concrete struct (`Point`); `struct Pair(A, B) { .. }`'s `A`/
// `B` are just names here, never `type`s — contrast a *use* of a generic
// type (`type`'s own `ident "(" type ... ")"` alternative below), which
// takes concrete `type`s. A struct's `name` is registered in *two*
// namespaces: as a type (usable in `type` below) and, via ordinary
// `Expr::Call`, as its own positional constructor — "construction is an
// ordinary call, not a new literal form" (§3.1), so there is no separate
// struct-literal production here. `field`'s trailing comma is optional,
// unlike `params`/`args` above (a real, deliberate ergonomic difference:
// multi-line field lists are the common case, so a dangling comma from
// reordering fields shouldn't be a parse error).
struct_decl ::= "struct" ident type_param_list? "{" field ("," field)* ","? "}"
field       ::= ident ":" type
type_param_list ::= "(" ident ("," ident)* ")"

// An enum's own name is a type only — never itself callable; each
// `variant` is what's callable (`Some(5)`, `None()`), registered in the
// same flat namespace `struct_decl`'s name is. A zero-payload variant
// still takes `()` at both declaration and call site (`None`, not
// `None()`'s omission) — one fewer special case, not a missing
// convenience (§3.2). Same `type_param_list` shape and meaning as
// `struct_decl`'s, shared across every variant — there's no per-variant
// parameter list; `T` in `Some(T)` names the *enum's* own type parameter.
enum_decl   ::= "enum" ident type_param_list? "{" variant ("," variant)* ","? "}"
variant     ::= ident ("(" type ("," type)* ")")?

// Row 12's UI DSL (`nirdosha emit-ui`/`nirdosha serve` only -- docs/LANGUAGE.md
// §11): an *optional, additive* layer over `ui_gen.rs`'s pure naming-
// convention inference, never a replacement for it. `screen`/`dashboard`
// are real reserved keywords, dispatched on like `struct`/`enum` above --
// but `field`/`action`/`paginate` (inside `screen_item`) and `tile`/
// `chart` (inside `dashboard_item`) are deliberately **not** reserved
// globally. They're matched by identifier text only in this one leading
// position of their own body -- exactly the precedent `requires(role:
// ...)`'s own `role`/`claim` names already set in the real grammar
// (`parser.rs::parse_requires_annotation`, now reflected in `fn_decl`
// above alongside `effect_annotation` -- previously a documented gap in
// this file only, not in the real parser) -- so LL(1) holds with no
// second-token lookahead (dispatch is on the first token's text alone),
// and `action` stays free to be an ordinary struct field/param name
// everywhere else (`examples/trade-finance/trade_finance.nir` already
// uses it that way). A `kv_entry`'s value is an ordinary `expr` (see
// `expr`'s own production far below) -- a string, an int, a bare
// function-naming `ident`, or a `role(...)`/`claim(...)` call alike --
// deliberately reusing the general expression grammar rather than
// inventing a second value grammar just for this DSL.
screen_decl    ::= "screen" ident "{" screen_item* "}"
screen_item    ::= paginate_block | field_override | action_decl | kv_entry
paginate_block ::= "paginate" "{" kv_entry* "}"
field_override ::= "field" ident "{" kv_entry* "}"
action_decl    ::= "action" string "->" ident ("{" kv_entry* "}")?
kv_entry       ::= ident ":" expr

dashboard_decl ::= "dashboard" "{" dashboard_item* "}"
dashboard_item ::= ("tile" | "chart") string "->" ident
                  | "visual" string "->" ident ("{" kv_entry* "}")?

// `visual "<label>" -> <fn> { render: "graph"|"heatmap"|"timeline" }`
// (`docs/ROADMAP.md` Track E2, `examples/ctms/UI_CONSTRUCTS.md` §2) --
// `visual` is contextual, same "reserved only as this one leading
// keyword" treatment `tile`/`chart` already get, not a globally
// reserved token. `render`'s value is typechecked against a closed
// vocabulary (`typeck.rs::check_dashboard`) but stays an ordinary
// `kv_entry` here -- no separate mini-grammar per chart kind.

// `workspace Name { subject: Struct panel "..." { ... } }`
// (`docs/ROADMAP.md` Track E1, `examples/ctms/UI_CONSTRUCTS.md` §1) -- a
// composite, multi-panel screen scoped to one instance of `subject`,
// additive over `screen_decl`/`dashboard_decl` the same way those are
// additive over pure naming-convention inference. `workspace` is a real
// reserved keyword (like `screen`/`dashboard`/`module`/`workflow`
// above); `panel` is contextual-only, the same "keyword only within
// this one leading position" treatment `field`/`action`/`paginate`
// already get inside `screen_item` -- disambiguated from an ordinary
// `kv_entry` the same way: `panel_decl`'s second token is always a
// `string`, a `kv_entry`'s second token is always `:`, so LL(1) holds
// with no second-token lookahead beyond that same one-token check.
// `action_decl` inside `panel_item` is `screen_item`'s own production,
// reused completely unchanged -- zero new syntax for a panel's actions.
workspace_decl ::= "workspace" ident "{" workspace_item* "}"
workspace_item ::= panel_decl | kv_entry
panel_decl     ::= "panel" string "{" panel_item* "}"
panel_item     ::= action_decl | kv_entry

// Two forms, dispatched on the token right after `module` (`Tok::Str`
// vs. `Tok::Ident` -- one-token lookahead, LL(1) holds). **`string`**
// is pure nav-grouping sugar for `ui_gen.rs`, not a scoping/namespace
// construct: every `fn`/`struct`/`enum` inside still registers into the
// exact same flat global namespace a top-level declaration would --
// `typeck.rs` never even looks at it, only `ui_gen.rs` does, to group
// nav screens by it. Unchanged since before `docs/ROADMAP.md` Track F, F2 --
// a `.nir` file that only ever uses this form renders exactly as it did
// before F2 existed (nav stays flat, ungrouped; no namespace, no `pub`
// enforcement). **`ident`** (F2) is a real namespace -- every `fn`/
// `struct`/`enum` inside registers under this identifier's own
// qualified key (`ast::scope_key`), reachable from outside only via
// `Mod::Name`, never bare, and only if marked `pub` (see
// `qualified_ident`'s own doc comment for the resolution rule, and
// `pub_item` below for the visibility grammar). `nav`, if given,
// overrides the display string `ui_gen.rs` groups this module's
// screens under (same field the `string` form sets directly) --
// defaults to the identifier itself if omitted, the same
// "override defaults to inferred" pattern `screen_decl { title:
// "..." }` already establishes. `module` is a real reserved keyword
// (like `struct`/`enum`/`screen`/`dashboard` above), since no existing
// example uses "module" as an identifier. Single-level only in either
// form: a `module` nested inside a `module`, or a `screen`/`dashboard`
// inside one, is a parse error (`parser.rs::parse_module_decl`/
// `parse_namespace_module_decl`) -- the same fixed-arity/no-arbitrary-
// nesting discipline `transact` slots already have.
module_decl ::= "module" string "{" (fn_decl | struct_decl | enum_decl)* "}"
              | "module" ident "{" ("nav" ":" string)? pub_item* "}"
pub_item    ::= "pub"? (fn_decl | struct_decl | enum_decl)

// `validate <fn_name> { pre: <expr>  post: <expr> ... }` (`docs/ROADMAP.md`
// Track F, F3; `docs/NEXT_GEN.md` §F3) -- a Hoare contract on an existing
// `fn`, declared separately (mirrors `screen_decl`'s own "separate
// top-level declaration referencing an existing item" shape, not new
// per-parameter annotation syntax on the `fn` line itself). `pre`/
// `post` are `kv_entry`s reusing this file's own `expr` production
// unchanged -- no separate predicate mini-language, the exact same
// choice `contract_check.rs::check_fn_contract`'s pre-existing
// string-based entry point already made (`parser::
// parse_standalone_expr`), just fed real parsed `.nir` syntax now
// instead of a string pulled from an extraction JSON. Multiple `pre`/
// `post` entries are meaningful: every `pre` is a conjunctive
// hypothesis, every `post` is checked independently. `validate` is a
// real reserved keyword (like `screen`/`dashboard`/`module`/
// `workflow`/`workspace` above; no existing example uses "validate" as
// an identifier); `pre`/`post` are contextual-only, the same "keyword
// only within this one leading position" treatment `field`/`action`/
// `paginate` already get inside `screen_item`. Two independent
// enforcement paths consume the same `entries`, neither part of this
// grammar layer: `contract_check::check_program_contracts` (a real
// Z3-backed Tier-1 proof, hard-failing the build only on a genuine
// counterexample -- integer params/return, no loop/call/division) and
// `interpreter.rs::call`'s runtime backstop (re-checks every `pre`/
// `post` against the real concrete values on every actual call,
// unconditionally -- the only enforcement for a contract on a `fn`
// outside that static subset, true of nearly every real `fn` in a real
// app).
validate_decl ::= "validate" ident "{" kv_entry* "}"

// `docs/WORKFLOW.md`'s durable state machine — desugared by `workflow_lower.rs`
// (right after parsing, `Parser::parse_program`'s own tail call) into
// ordinary `fn_decl`/`enum_decl`/`struct_decl`, the same "pure lowering,
// zero new dispatch machinery" shape `module_decl` above already uses.
// `workflow`/`state` are real reserved keywords; `data`/`on_entry`/
// `on_exit`/`on`/`terminal`/`link` are contextual — matched by identifier
// text only inside `parse_workflow_decl`/`parse_state_decl`, the same
// treatment `transact`'s own slot names get (see `transact_expr` below).
// `action_call` reuses `transact_expr`'s "parse a call, reject anything
// else" restriction — a bare `name(args)`, never an arbitrary expression
// — except a workflow action call *may* name a builtin (`send_email`
// etc. are builtins), the opposite of `transact`'s own restriction.
// `state_item`'s trailing `kv_entry` alternative is `docs/WORKFLOW.md`'s
// "state ownership + a generated queue UI" section: `owner: role(...)`/
// `owner: claim(...)` (who may fire this state's outgoing events —
// `typeck.rs::check_visibility_expr`, the same shape `screen`'s own
// `view`/`edit` already use) and `label: "..."` (a display name for a
// generated queue UI's status badge) are the two keys given meaning
// today; any other key is parsed but ignored, the same forward-
// compatible posture `screen_item`'s own `kv_entry` fallback already has.
workflow_decl  ::= "workflow" ident "{" data_block? state_decl+ "}"
data_block     ::= "data" "{" field ("," field)* ","? "}"
state_decl     ::= "state" ident "terminal"? "{" state_item* "}"
state_item     ::= on_entry_block | on_exit_block | transition | kv_entry
on_entry_block ::= "on_entry" "{" action_call* "}"
on_exit_block  ::= "on_exit" "{" action_call* "}"
action_call    ::= ident "(" (expr ("," expr)*)? ")"
transition     ::= "on" "link"? ident "->" ident

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
// motivate it yet. `u8..usize` compile now (`nirdosha build`/
// `emit-llvm` — see `docs/LANGUAGE.md` §10) — the signed-vs-unsigned
// instruction choice this used to need turned out to be needed in
// exactly one place (`codegen.rs::widen_to_i64`), not throughout, since
// every unsigned type's legal range is capped at `[0, i64::MAX]`
// (`Ty::bounds()`) and this backend computes all arithmetic at `i64`
// width regardless of the source type's declared width.
//
// `unit` is a type keyword only — there is no expression-level literal
// for constructing a `unit` *value* explicitly. `primary` above has no
// `()`-as-empty-group alternative, so a `unit`-typed value only ever
// arises implicitly (a function with no declared return type running to
// completion, or the result of calling one) — you cannot write `let x:
// unit = <something>` except by assigning the result of such a call;
// there's no direct literal to put on the right of `=`.
// `thread T` and `chan T` (docs/goal.md rows 2-3) follow `box`'s shape exactly
// — a prefix type-former wrapping another `type`, not a separate grammar
// category. `thread T` is affine (a spawned computation has exactly one
// owner, `join` consumes it); `chan T` is **not** — see `Ty::Channel`'s
// doc comment in `ast.rs` for why a channel handle needs to stay freely
// copyable while its *payload* still moves through `send`.
// `sandbox`, unlike every other type-former above, is a **plain, bare**
// type name — not a prefix wrapping another `type`. It has no `T` to
// parameterize: this first slice (docs/SANDBOXING.md's "layer 1") has no
// typed result channel at all, only an affine handle and an OS exit
// code. Spelled `"sandbox"` in both type position (here) and expression
// position (see `unary` below, where it's likewise nullary) the same way
// `chan` is dual-use, just without `chan`'s own type parameter.
// `str`/`tcp` (docs/SANDBOXING.md layer 2's prerequisites) fit the plain
// `TypeName` alternative below, not a dedicated production — neither
// takes a type parameter, and unlike `sandbox`, `str` never needs to
// appear in *expression* position at all (`connect`, not a bare `tcp`
// keyword, is what produces a `tcp` value — see `unary` below), so it
// didn't need `sandbox`'s dual-use token treatment either.
// `Vector`/`Matrix` are deliberately capitalized, unlike every lowercase
// scalar `TypeName` — a genuinely different production (both take `(...)`
// arguments: an element type plus one or two fixed dimensions), not
// another bare keyword, and the surface syntax the unified plan's
// architecture table already uses (`Matrix(f64, 3, 3)`). Dimensions are
// plain integer literals, not arbitrary expressions — "Sized by Default"
// (the plan's §2) means the shape is fixed at compile time, the same way
// every other `type` production here is a closed grammar, not a runtime
// value.
// `ident` and `ident "(" type ("," type)* ")"` (last two alternatives)
// are Row 11's addition to this production — a declared `struct`/`enum`
// name, optionally applied to concrete type arguments (layer 6,
// generics: `Pair(i64, str)`, the same "type name applied to arguments"
// shape `Vector(T, N)`/`Matrix(T, R, C)` already use — deliberately, to
// avoid `<...>`-style type application: Nirdosha never uses it anywhere,
// and introducing it here would be the one genuinely new source of
// parsing ambiguity in an otherwise LL(1) grammar, the same "turbofish"
// problem this reuse sidesteps — `docs/nirdosha_row11_amendment.md` §3.1).
// Accepted syntactically for *any* identifier here (the parser has no
// declaration table to check against — `docs/LANGUAGE.md` §6's "functions are
// looked up by name in a table" applies equally to types now); an
// identifier that doesn't actually name a real struct/enum is
// `TypeErrorKind::UnknownType`, and a real one applied to the wrong
// number of type arguments is `TypeErrorKind::WrongTypeArity` — both
// caught by `typeck.rs`, not this grammar.
// `fn(T1, T2) -> R` (last alternative) -- a first-class function value's
// type, e.g. `apply(f: fn(i64) -> i64, x: i64)` (`examples/
// privileged_fn.nir`). Reuses the `"fn"` keyword rather than a second
// dedicated token — never ambiguous with a declaration's own `"fn"
// ident (...)`, since a type position never expects a name next.
// `parser.rs::expect_type`; previously missing from this file, not from
// the real parser.
type        ::= "&" type
              | "box" type
              | "thread" type
              | "chan" type
              | "sandbox"
              | "Vector" "(" type "," int_lit ")"
              | "Matrix" "(" type "," int_lit "," int_lit ")"
              | "i8" | "i16" | "i32" | "i64"
              | "u8" | "u16" | "u32" | "u64" | "usize" | "f64"
              | "bool" | "unit" | "str" | "tcp" | "tcp_listener"
              | qualified_ident ("(" type ("," type)* ")")?
              | "fn" "(" (type ("," type)*)? ")" ("->" type)?

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
              | audited_stmt
              | expr_stmt

let_stmt    ::= "let" ident ":" type "=" expr
return_stmt ::= "return" expr?
while_stmt  ::= "while" expr block
// docs/goal.md §4's Tier-3 escape hatch (unified plan §4.3.4): suppresses
// codegen's Tier-1/2 guard *emission* inside `body` (`guard_in_range`,
// the division-by-zero trap) — has no effect in the interpreter, which
// always runs its own runtime checks unconditionally regardless of
// `audited`. `justification` must be non-empty (checked in `typeck.rs`,
// not this grammar — an empty string is still syntactically valid here).
// `body`'s statements share the enclosing function's scope machinery
// exactly like a `block`'s would, but `body` is a bare `stmt*`, not a
// `block` — this construct has no value of its own to produce.
audited_stmt ::= "audited" str_lit "{" stmt* "}"
expr_stmt   ::= expr

expr        ::= if_expr
              | transact_expr
              | match_expr
              | assignment

if_expr     ::= "if" expr block ("else" (block | if_expr))?

// Row 11's `match` — an `expr`, not a `stmt`, for the same reason `if`/
// `transact` already are: `return match o { ... }` has to work.
// `scrutinee` is full `expr` (same as `if`'s own `cond`), not just
// `assignment` — no ambiguity from that with a following `{`, since this
// grammar has no brace-delimited struct-literal expression to confuse it
// with (construction is always `Ident(args)` — see `struct_decl` above).
//
// Two arm shapes, dispatched by the arm's own first token (LL(1), same
// discipline `call`'s callee-name resolution already gets) — never mixed
// within one `match`, which `typeck.rs` enforces by the scrutinee's type,
// not this grammar:
//   - Enum-variant arm (v1's only shape): `ident` must resolve to one of
//     the scrutinee enum's own variant names; exhaustiveness is every
//     variant covered exactly once, no wildcard.
//   - Literal-pattern arm (post-v1 addition, `str`/`i64`/`bool`
//     scrutinees only — no `f64`, floating-point pattern equality is a
//     footgun this form doesn't need): `literal` is a `str`/`int`/`bool`
//     literal, or the wildcard `_`, never both a literal *and* bindings.
//     Exhaustiveness requires exactly one `_` arm, last — a literal
//     domain isn't closed the way an enum's variant set is, so there is
//     no way to prove coverage without one.
match_expr  ::= "match" expr "{" match_arm ("," match_arm)* ","? "}"
match_arm   ::= variant_arm | literal_arm
// A zero-payload variant's pattern still takes `()`, same as its own
// construction does (`enum_decl`'s doc comment above) — the binding
// list inside is what's optional, not the parens themselves (checked
// directly against `parser.rs::parse_match_expr`: `admin()` in a match
// arm is `Tok::LParen` immediately followed by `Tok::RParen`, previously
// undocumented here).
variant_arm ::= qualified_ident ("(" (ident ("," ident)*)? ")")? "=>" expr
literal_arm ::= (str | int | "true" | "false" | "_") "=>" expr

// `docs/TRANSACT.md`'s durable-effect construct — all five layers are
// implemented (in-process control flow, a real durability log, crash
// replay, `network`'s own `retry`/`timeout`, and a real cross-process
// `network`/`commit` test). Slot order is fixed — optional `precheck`,
// then `network` (with its own optional `retry`/`timeout` modifiers),
// `verify`, `commit`, then optional `compensate`, then optional `log` —
// never permutation-parsed, the same fixed-arity discipline every other
// multi-part construct in this grammar already has.
// `precheck`/`network`/`verify`/`commit`/`compensate`/`log`/`retry`/
// `timeout` are matched by identifier *text* here, not reserved as
// `Tok` keywords (see `parser.rs::parse_transact_slot`/
// `parse_optional_int_modifier`) — the same non-keyword treatment
// `TYPE_NAMES` (token.rs) already gives scalar type names. `call` is
// `ident "(" (expr ("," expr)*)? ")"` — exactly `Expr::Call`'s own
// shape, the same "parse normally, then validate what came out"
// restriction `spawn`/`sandbox` already enforce on their own operand: a
// slot is one named call, never an arbitrary expression. `transact {
// ... }` is itself a value (`bool`) — an `expr` production, not a
// `stmt` one, specifically so `return transact { ... }` works exactly
// like `return if c {..} else {..}` already does. `network`'s call must
// pass the implicit `txn_id: str` binding as one of its arguments
// (typeck-enforced, `TransactNetworkMustUseTxnId`); `verify`'s
// arguments are further restricted to exactly `network`/`txn_id`
// (`TransactVerifyArgsMustBeImplicitBindings`) — see `docs/TRANSACT.md`'s
// "Crash replay" section for why. `retry`/`timeout` are plain `int_lit`,
// no unit suffix (no duration-literal syntax invented for this).
transact_expr ::= "transact" "{"
                     ("precheck"  ":" call)?
                     "network"    ":" call ("retry" int_lit)? ("timeout" int_lit)?
                     "verify"     ":" call
                     "commit"     ":" call
                     ("compensate" ":" call)?
                     ("log"        ":" call)?
                   "}"

// Right-associative, lowest precedence among non-`if` expressions — same
// shape as C/Rust's assignment-expression. The `ident` restriction on the
// left side is a real *grammar* restriction, not merely an artifact of
// how the parser happens to be written — there is no production anywhere
// that lets a general expression (`foo.bar`, `foo[0]`, ...) appear as an
// assignment target. **Corrected, since both now exist as plain
// expressions (postfix's `[...]`/`.ident` above), unlike when this note
// was first written:** `foo[0] = x`/`foo.bar = x` are still rejected —
// `parse_assignment` parses the left side as a full `logic_or` (see the
// implementation note below) and only accepts the result if it's exactly
// an `Expr::Ident`; an `Expr::Index`/`Expr::FieldAccess` there is
// `"left-hand side of \`=\` must be a plain variable name"`, checked
// directly. Widening `assignment`'s left side to either is a real
// grammar change this document would need to update, not just a parser
// one, if it's ever done.
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
// `.*`/`./` (Hadamard/elementwise multiply-divide) sit at the same
// precedence as `*`/`/` — Julia's own convention for broadcast operators,
// and there's no ambiguity to resolve either way since `.` never starts
// anything else at this position (a float literal's `.` is consumed
// entirely inside `float_lit` below, never left dangling for the
// expression grammar to see). No `.+`/`.-`: elementwise is already the
// *only* sensible meaning of `+`/`-` for two matching-shape operands, so
// a dotted spelling would just be a redundant second name for the same
// operation `+`/`-` already do.
multiplicative ::= unary (("*" | "/" | ".*" | "./") unary)*
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
// `sandbox`'s operand restriction is identical to `spawn`'s (`Expr::Call`
// only, same "parse then validate" technique) — `sandbox worker(x)`
// launches a *named function* as a real OS process, not an arbitrary
// expression. `stop`'s operand is an unrestricted `unary`, exactly like
// `join` — both consume a handle-typed expression (a `sandbox` *or* a
// `tcp` connection — `stop` was reused rather than inventing a second
// word that would mean the same "one-time consuming close" thing) and
// don't care how it was produced (an `Ident`, a nested `stop`/`join`,
// etc.). `send`/`recv` are likewise reused for `tcp`, not given their
// own second production — same fixed two-/one-operand shape either way,
// dispatched on the first operand's type, not the grammar. `connect`
// fits the same fixed-arity `"(" ... ")"` shape `send` already
// established, just under its own keyword (it's not consuming an
// existing handle the way `send`/`recv`/`stop` are — it *produces* one).
unary       ::= ("!" | "-" | "*" | "box" | "&") unary
              | "spawn" call
              | "join" unary
              | "chan"
              | "send" "(" expr "," expr ")"
              | "recv" "(" expr ")"
              | "sandbox" call
              | "stop" unary
              | "connect" "(" expr "," expr ")"
              // `listen(port)` binds a real TCP listening socket and
              // returns a `tcp_listener` handle — same fixed-arity shape
              // as `connect`, one operand instead of two. `accept
              // (listener)` blocks for the next client and returns an
              // ordinary `tcp` handle (unified plan §4.3.3: "same
              // `Channel<T>` semantics over TCP as over Unix sockets" —
              // no separate server-connection type). Unlike `stop`,
              // `accept` does not consume its operand: a listener
              // accepts many connections over its lifetime.
              | "listen" "(" expr ")"
              | "accept" "(" expr ")"
              // `acquire transfer_funds(proof)` -- the only way to obtain a
              // `requires`-gated function's *value* (row 12): the
              // externally-issued `RoleView`/`ClaimView` proof is the
              // argument. Same "parse a call, then restrict what came out"
              // technique `spawn`/`sandbox` use above, additionally
              // requiring exactly one argument (`parser.rs`'s own
              // `Tok::Acquire` arm) -- previously missing from this file,
              // not from the real parser (`examples/privileged_fn.nir`).
              | "acquire" call
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
call        ::= postfix ("(" args? ")")?
args        ::= expr ("," expr)*

// `v[i]` and `m[i, j]` are one bracket group each — a single `postfix`
// step, not `v[i][j]`-style chained subscripting as one production (that
// shape doesn't exist; see `ast.rs::Expr::Index`, which carries one
// `Vec<expr>` of indices per bracket group, not a nested `Index` per
// index) — though the loop below does still let `v[i][j]` *parse* as two
// separate `Index` steps chained (`typeck.rs`'s `NotIndexable` rejects it
// unless the first index's own result is itself indexable). **Corrected
// claim, checked directly against `parser.rs`, not assumed:** an earlier
// version of this doc said `f()[i]` parses — it doesn't. `postfix` runs
// once, on `primary`, *before* `call`'s own trailing `(args)` check
// (`parser.rs::parse_call`: `parse_postfix()` first, then look for `(`),
// and `parse_call` never re-enters `postfix` on the `Call` it just built
// — so a `[...]`/`.ident` immediately after a call's closing `)` is
// parsed as the start of a *new* statement instead (this grammar's "always
// prefer to extend the current expression" rule, above, doesn't reach
// across a completed `call` production) — `return f()\n[0]` type-checks
// as `return f()` followed by a second, discarded `[0]` statement, not
// `return f()[0]`, which is exactly what happens if you actually try it.
// `.ident` (Row 11 field access, `Expr::FieldAccess`) is the one new
// alternative here, chaining through this same loop exactly like `[...]`
// already does (`a.b.c`, `p.x[0]` both parse); it inherits the identical
// "doesn't chain past a `call`" limitation just corrected above, not a
// new one.
postfix     ::= primary (("[" expr ("," expr)* "]") | ("." ident))*

// `"(" expr ")"` requires a real `expr` inside — `()` alone (an empty
// parenthesized group) is not a valid `primary`, and isn't a way to
// spell a `unit` *value* either; see the omissions list for what that
// means for `unit`.
//
// `array_lit` is a `Vector` literal (`[1.0, 2.0, 3.0]`) if every element
// is a plain scalar, or a `Matrix` literal, row-major (`[[1.0, 2.0],
// [3.0, 4.0]]`), if every element is itself a same-shaped `Vector` —
// `typeck.rs::infer_array_lit` decides which, from the *value*, not the
// grammar: there is only one production here, not two. Always at least
// one element — no `[]` alternative, the same "requires a real
// sub-production" restriction `"(" expr ")"` already has; there would be
// no way to infer an element type for an empty literal. Deliberately
// **not** Julia's space-sensitive `[1 2; 3 4]` — that grammar isn't
// LL(1)/LALR-parseable (see this file's row-7 discipline above).
primary     ::= int_lit | float_lit | str_lit | "true" | "false" | qualified_ident | "(" expr ")" | array_lit
array_lit   ::= "[" expr ("," expr)* "]"

// `"` ... `"`, with a deliberately small escape set (`\"`, `\\`, `\n`,
// `\t`, `\r` — nothing else, no `\u{...}`/`\x..`/`\0`; see `token.rs`'s
// lexer). No concatenation exists at the expression-grammar level either
// — a `str` value only ever comes from a literal, a `let`/parameter
// binding, or `recv` on a `tcp` connection.
str_lit     ::= '"' (any_char | escape)* '"'

// Decimal digits only — no `0x`/`0b` prefixes, no `_` digit-group
// separators (`1_000`). Not needed for Phase 0's examples; listed in
// omissions rather than silently absent.
int_lit     ::= digit+

// Digits, a required `.`, more digits — no scientific notation (`1e10`),
// no bare trailing `.` (`1.` is a lex error, not `Int(1)` followed by
// something else), no leading-dot form (`.5`). `token.rs`'s lexer decides
// int-vs-float with one extra character of lookahead past the integer
// part: a `.` immediately followed by a digit.
float_lit   ::= digit+ "." digit+
ident       ::= alpha (alpha | digit | "_")*
```

## Deliberate omissions (Phase 0 boundary, not forgotten)

- No `for` loop yet — `while` is the one structured-iteration primitive
  until the design decides whether `for` is sugar over `while` (compositional,
  row 8) or a separate construct (simpler to read, row 6). Undecided on
  purpose rather than guessed.
- **Superseded, kept for history:** this used to say "no general
  structs/enums yet — `type` is still a closed grammar, not open to
  user-defined names." Row 11 (`docs/nirdosha_row11_amendment.md`) changed
  that: `struct_decl`/`enum_decl`/`match_expr` above are real,
  interpreter-checked and -executed (`typeck.rs`, `ownership.rs`,
  `interpreter.rs`), through layer 6 of that amendment's own rollout —
  including generics (`struct Pair(A, B) { .. }`, `type_param_list`
  above) with real structural-per-instantiation type identity
  (`Pair(i64, str)` and `Pair(f64, bool)` are different, unrelated
  `Ty`s — no monomorphizer pass exists or is needed; see
  `ast::substitute_ty`) and the `Option(T)`/`Result(T, E)` prelude
  (layer 7 — `ast::prelude_enums`, injected into every program at parse
  time, no special-casing anywhere downstream). `Vector`/`Matrix` (dense,
  fixed-shape 1-D/2-D arrays — `type`'s `"Vector" "(" ... ")"`/
  `"Matrix" "(" ... ")"` productions above) remain a separate, older
  mechanism, not unified with `struct`/`enum` — what's still missing for
  *them* specifically is dynamically-sized arrays and generic dimensions
  (`Matrix{T,N}`-style), a bigger, separate ask than Row 11 covers.
- No `unsafe`/`audited` block syntax yet — that's the Tier-3 escape valve
  from `docs/goal.md` §4, which presupposes Tier 1/2 (the SMT-discharged
  refinement layer) existing first.
- **Superseded, kept for history — split out since the original single
  paragraph made it easy to miss that two of these are real, working
  passes, not aspirations:**
  - *What used to be true:* type checking was purely dynamic (checked as
    the interpreter executed, Python-style), and assignment had "no
    ownership model, explicitly not what row 1 asks for."
  - *What's true now:* both are false. `crates/compiler/src/typeck.rs` is a
    real static pass (a program that fails it is never executed).
    `crates/compiler/src/ownership.rs` statically enforces single ownership for
    `box`-typed bindings, including branch-merge and loop-reassignment
    cases (see `docs/PHASE0.md`'s ownership updates for exactly what's proved
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

`crates/grammar_check/` (top-level, sibling to `compiler/` — see
[`../crates/grammar_check/README.md`](../crates/grammar_check/README.md)) runs this
EBNF through `lalrpop`, an independent LALR(1) generator, as a second
check beyond "the hand-written parser is single-token-lookahead by
construction." It's what found the statement-separator ambiguity
documented above. It does not build cleanly, on purpose and by design —
see its README for why that's the actual, informative result, not a
broken build waiting to be fixed.

## Machine-readable grammar artifact (docs/goal.md row 7)

`crates/compiler/nirdosha.gbnf` is a hand translation of this EBNF into GBNF, a
constrained-decoding grammar format — the artifact that lets an LLM's
sampler guarantee every token it emits stays inside Nirdosha's syntax,
not just hope the model learned the grammar from training data.
`crates/grammar_export/` (top-level, sibling to `compiler/` — see
[`../crates/grammar_export/README.md`](../crates/grammar_export/README.md)) is what
actually checked it: a real dependency on llama.cpp's own grammar parser
confirms the file is valid, loadable GBNF (and caught a real translation
bug — llama.cpp doesn't accept a bare `|` starting a continuation line
the way this EBNF's own visual style does), and a fidelity corpus
compares accept/reject behavior against the real lexer+parser for every
shipped example plus a set of hand-written positive/negative snippets.

**Currently failing, and not by this session's own changes**:
`nirdosha.gbnf` (119 lines) and `crates/grammar_check/src/nirdosha.lalrpop`
(182 lines) both predate Row 11 (`struct`/`enum`/`match`/generics)
entirely, and haven't caught up since — the fidelity corpus's "every
shipped example is accepted by both" test fails on `examples/
transact.nir` today, independent of and before this session's own
`screen`/`dashboard` addition (which also isn't reflected in either
file, for the same reason). Catching both files up to the compiler's
actual current grammar is effectively a from-scratch rewrite of both,
not an incremental sync; see `crates/compiler/UI_DSL_TODO.md` for the finding
in full rather than a claim, made here, that these stay in lockstep with
`docs/GRAMMAR.md` — they don't, today.
