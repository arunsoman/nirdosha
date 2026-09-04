# Nirdosha Amendment: Row 11 — Product Types, Sum Types, Generics

**Document:** Amendment to `docs/goal.md` (§1, §3, §6, §7)
**Date:** 21 Aug 2026
**Status:** §3.6 layers 1–4 and 6–7 shipped (21 Aug 2026) — `struct`/
`enum` declarations (with type-parameter lists — layer 6, generics),
positional construction, field access, `match` with exhaustiveness
checking, affinity propagation through struct/enum fields (including
through a generic instantiation's own concrete type arguments), and the
`Option(T)`/`Result(T, E)` prelude (layer 7) are real, tested
(`crates/compiler/tests/structs_enums.rs`, `crates/compiler/tests/generics.rs`,
`examples/structs_enums.nir`, `examples/generics.nir`),
interpreter-only per layer 8's own scoping
(`crates/compiler/src/codegen.rs::check_supported` rejects any program that
actually constructs/matches a `struct`/`enum` — including a prelude
variant like `Some(5)` — the same "reject, don't mis-compile" treatment
every other interpreter-only feature already gets; a program that never
touches Row 11 at all still compiles, since the prelude's own presence in
`Program.enums` isn't by itself a use). Layer 5 (extending
`refine.rs`/`smt.rs`'s boundary set) is still future work — see its own
entry in §3.6 below.
**Supersedes:** an earlier draft of this file. That draft described a
different, Austral/Pony-flavored language — `record ... is ... end;`,
reference capabilities (`iso`/`val`/`ref`/`box`/`tag`), an already-working
`src/capability.rs` and `src/linear.rs`, an `.aui`/`.aum` module split — and
called it Nirdosha. None of that exists in this repo (`grep` across
`crates/compiler/src/` and `docs/GRAMMAR.md`'s "Deliberate omissions" section confirm
it), and repeating an "already exists" claim about `capability.rs` is
exactly the mistake `docs/goal.md`'s own honest-correction preamble already
walked back once for `capability.rs`/`ledger.rs`. This version is checked
against the actual `crates/compiler/src/` tree, `docs/LANGUAGE.md`, and `docs/GRAMMAR.md`
line by line; every syntax example below is legal-or-proposed Nirdosha, not
borrowed from another language and relabeled.

---

## Executive summary

`docs/PROTOLANG_PORT.md` (the ProtoLang porting exercise) traced three "Blocked"
verdicts (null safety, exhaustive error handling, the I/O error hierarchy)
and four "Rejected" verdicts (config-as-code, HTTP types, DB row types, JSON)
back to one missing layer: Nirdosha has no product types, no sum types, and
no generics (`docs/LANGUAGE.md` §6: "No structs, enums, tuples, or generics";
`docs/GRAMMAR.md`'s "Deliberate omissions": "No general structs/enums yet").
None of those seven verdicts trace back to Rice's theorem (§0) or to the
rows 1–5 safety/expressiveness trade-off (§6). This amendment proposes
**Row 11** — a real addition to `docs/goal.md`'s ten-row table, not a rename of
work already implied by an existing row — scoped to exactly the three type
formers that unlock those seven items, built against what `crates/compiler/src/`
actually is today.

**The honest caveat, unchanged from last time:** Row 11 makes Nirdosha
**expressive enough** to write the programs `docs/PROTOLANG_PORT.md` described.
It does not make Nirdosha the "default language of choice" — that's an
ecosystem problem (package manager, editor/LSP, docs, real users writing
real programs), which `docs/goal.md` §9 already names as the standing gap this
doesn't close.

---

## 1. What's actually true today (checked, not assumed)

| Claim | Real? | Where |
|---|---|---|
| Types are `i8`…`i64`, `u8`…`usize`, `f64`, `bool`, `unit`, `str`, `box T`, `thread T`, `sandbox`, `chan T`, `tcp`, `tcp_listener`, `Vector(T,N)`, `Matrix(T,R,C)` | Yes | `ast.rs`'s `Ty` enum, `docs/LANGUAGE.md` §2 |
| Ownership/move-checking (`box`/`thread`/`sandbox`/`tcp`/`tcp_listener` are affine) | Yes | `ownership.rs`, `docs/LANGUAGE.md` §6 |
| Two independent bounds provers, Tier-1/2/3 escape valve | Yes | `refine.rs`, `smt.rs`, `docs/LANGUAGE.md` §8, `docs/goal.md` §4 |
| Reference capabilities (`iso`/`val`/`ref`/`box`/`tag`, Pony-style) | **No** | Not in `ast.rs`. Race-freedom comes entirely from `ownership.rs`'s affine move-checking (`docs/LANGUAGE.md` §7). |
| `src/capability.rs`, `src/ledger.rs` | **No** | `docs/goal.md`'s own honest-correction preamble already says so. |
| Koka-style effects tracked in signatures | **Not yet** | Designed, not built — `docs/PROTOLANG_PORT.md`'s "Locked design 1." |
| `record`/`union`/`generic` keywords already exist | **No** | `docs/GRAMMAR.md`: "No general structs/enums yet." |
| Field access (`expr.ident`), `match`/pattern matching | **No** | `docs/GRAMMAR.md`'s `assignment` production note: "no field access... yet"; no `match` production anywhere in the grammar. Both are new work this amendment has to bring, not details to gloss over. |
| Traits / typeclasses / interfaces | **No** | Not mentioned anywhere in `docs/LANGUAGE.md` or `docs/GRAMMAR.md`. Generic constraints in this design are unconstrained type parameters, checked ad-hoc at use (see §3.3) — the same way `Vector(T,N)`'s `T` already works (`docs/LANGUAGE.md` §2: "every dense linear-algebra builtin requires `T = f64` specifically"), not a typeclass system. |
| `where`-clause refinement syntax on user declarations | **No** | Refinement checking in Nirdosha today is automatic range-proving at existing boundaries (`let`/`return`/assign — `docs/LANGUAGE.md` §2), never a type-level annotation a program writes. Row 11 extends the boundary set (§3.5), it does not add `where` syntax. |

---

## 2. Row 11: formal proposal

### 2.1 Addition to `docs/goal.md` §1's table

| # | Requirement | Class | Mechanism | Proven / measured where | The catch |
|---|---|---|---|---|---|
| **11** | Closed product types, sum types, and generics | **Hard** — proof (decidable) | Add `struct`/`enum` type formers plus a `match` expression to the core calculus; type parameters are concrete-per-instantiation (no erasure), the same way `Vector(f64,3)` and `Vector(f64,4)` are already different `Ty`s | ML family (records, tagged unions), Rust (`enum`, exhaustive `match`) | No traits/typeclasses, no HKTs, no subtyping, no wildcard/binding patterns in `match` (v1) — narrow on purpose, see §2.2 |

### 2.2 Scope boundaries — what Row 11 is not

- **No typeclasses/trait bounds.** Nirdosha has none today; inventing one to bound generic parameters would be a second, separate, much bigger project. Generic type parameters here are unconstrained — whatever operation the body actually performs on the parameter is checked the normal way at each concrete instantiation, exactly how `Vector(T,N)`'s `T` already works.
- **No HKTs, no GADTs.** Not needed for `Option`/`Result`/`List`/`Map` or any item on `docs/PROTOLANG_PORT.md`'s unlock list.
- **No subtyping, nominal only.** Two `struct`s with identical fields are still different types — avoids the non-compositional structural-subtyping tar pit (row 8's actual requirement: one fixed semantic function, not a special case per shape).
- **No named-field construction, no wildcard/binding-only match patterns (v1).** Both are real, named limits (§3.2, §3.4), not oversights — cheap to add later, not free to guess the shape of now.

---

## 3. Design, in Nirdosha's actual grammar

### 3.1 Product types: `struct`

Extends `docs/GRAMMAR.md`'s `type` production (currently a closed list of
built-in names) with a user-defined alternative, and adds one new
declaration form alongside `fn_decl`:

```ebnf
item        ::= fn_decl | struct_decl | enum_decl

struct_decl ::= "struct" ident ("(" ident ("," ident)* ")")? "{" field ("," field)* "}"
field       ::= ident ":" type

type        ::= ... | ident                              // a declared struct/enum name
              | ident "(" type ("," type)* ")"            // applied to concrete type args
```

The parenthesized argument list is deliberate, not a shortcut: it's the
same production shape `Vector(T, N)`/`Matrix(T, R, C)` already use for
"a type name applied to arguments" (`docs/GRAMMAR.md`'s `type` production).
Nirdosha never uses `<...>` for type application anywhere, and introducing
it here would be the one genuinely new source of parsing ambiguity in an
otherwise LL(1) grammar (`docs/GRAMMAR.md`'s row-7 claim) — Rust's own
`<`-vs-less-than "turbofish" problem is exactly what this sidesteps by
reusing a convention that already exists.

```nir
struct Point {
    x: f64,
    y: f64,
}

struct Pair(A, B) {
    first: A,
    second: B,
}
```

**Construction is an ordinary call, not a new literal form.** Nirdosha's
`Expr::Call` already names its callee by a resolved identifier
(`docs/LANGUAGE.md` §6); a struct's constructor is registered under the struct's
name the same way a function is, positional-only (no named-field
construction in v1 — a real, named limit, not an oversight: it's the
simplest thing that lets every example below typecheck, and Nirdosha's
grammar has no named-argument call syntax anywhere else to be consistent
with either):

```nir
let p: Point = Point(1.0, 2.0)
let pr: Pair(i64, str) = Pair(1, "one")
```

**Field access is new** — the one genuinely new expression form this
amendment adds, since `docs/GRAMMAR.md` is explicit that no field access exists
today:

```ebnf
postfix ::= postfix "." ident        // new: struct field read
```

```nir
fn dist(p: Point) -> f64 {
    return norm([p.x, p.y])
}
```

### 3.2 Sum types: `enum`

```ebnf
enum_decl ::= "enum" ident ("(" ident ("," ident)* ")")? "{" variant ("," variant)* "}"
variant   ::= ident ("(" type ("," type)* ")")?
```

```nir
enum Option(T) {
    Some(T),
    None,
}

enum Result(T, E) {
    Ok(T),
    Err(E),
}
```

A variant is just a constructor, registered by name exactly like a
struct's — `Some(5)` and `None()` are both ordinary `Expr::Call`s (a
zero-payload variant still takes `()`, no bare-identifier special case
needed — one fewer rule, not a missing convenience). The enum's own type
(`Option(i64)`) is inferred from the expected type at the call site the
same way `chan`'s payload type already is when ambiguous
(`TypeErrorKind::ChannelNeedsExplicitType`, `typeck.rs`) — a
`let x: Option(i64) = None()` needs the annotation for the same reason a
bare `chan` does; a `Some(5)` passed where an `Option(i64)` is expected
does not.

### 3.3 Generics — no monomorphizer module needed

Nirdosha's type identity is already structural-per-instantiation, not
nominal-with-erasure: `Vector(f64,3)` and `Vector(f64,4)` are already
different `Ty` values (`docs/LANGUAGE.md` §2). A generic `struct`/`enum`
inherits this for free — `Option(i64)` and `Option(str)` are two distinct,
fully concrete `Ty`s the first time each is used in a program, not two
instantiations of one erased generic runtime representation. This means
"monomorphization" isn't a separate pass or module to build (contrast the
superseded draft's invented `src/monomorphizer.rs`): it falls out of a type
equality rule Nirdosha already has, extended to cover user type
constructors instead of just `Vector`/`Matrix`. Whenever codegen eventually
reaches `struct`/`enum` (out of scope for now — see §3.6), each distinct
instantiation actually used gets its own concrete LLVM type, the same
"fully unrolled, dimensions always compile-time literals" story
`docs/LANGUAGE.md` §10 already tells for `Vector`/`Matrix`.

Type parameters carry **no constraint syntax** (§2.2) — `Pair(A, B)`'s `A`
and `B` are unconstrained; whatever the body does with a value of type `A`
is checked normally at each concrete instantiation, exactly the way
`docs/LANGUAGE.md` §2 already says `Vector(T,N)`'s dense-linear-algebra builtins
"require `T = f64` specifically" with no separate bound declaration.

### 3.4 `match` — new, and the one place this design names a real limit

No pattern-matching construct exists in Nirdosha today (no `match`
production anywhere in `docs/GRAMMAR.md`). Sum types are useless without one, so
Row 11 has to ship both together:

```ebnf
expr       ::= ... | match_expr
match_expr ::= "match" expr "{" match_arm ("," match_arm)* ","? "}"
match_arm  ::= ident ("(" ident ("," ident)* ")")? "=>" expr
```

```nir
fn describe(o: Option(i64)) -> str {
    return match o {
        Some(n) => "got a value",
        None => "nothing",
    }
}
```

Exhaustiveness is a closed, syntactic check: the scrutinee's declared enum
lists a fixed variant set (§3.2), and `typeck.rs` verifies every arm's head
identifier names a variant of that set and every variant appears exactly
once — no wildcard, no binding-only catch-all pattern in v1. This is a
deliberate, named scope limit, not an oversight: Rust's `_`/bound-identifier
patterns need a scoping rule to tell "matches a known variant name" apart
from "binds a fresh variable," and Nirdosha's checker doesn't need to solve
that problem at all if every arm head is required to resolve to a real
variant of the scrutinee's specific enum — the same closed-identifier-
resolution discipline `Expr::Call` already applies to function names.
`match` is an `expr`, not a `stmt`, for the same reason `if` and
`transact` already are (`docs/GRAMMAR.md`'s `if_expr`/`transact_expr`): so
`return match o { ... }` works.

**Addendum (later session): the wildcard/binding-only gap named above is
now closed, narrowly.** A `str`/`i64`/`bool` scrutinee (never `f64` —
floating-point pattern equality is a footgun this form doesn't need to
inherit) can now be matched against literal-value arms plus a mandatory
trailing `_`, e.g. `match role { "admin" => .., "analyst" => .., _ =>
.. }` — see `docs/GRAMMAR.md`'s `literal_arm` production and
`ast::LiteralPattern`. This is additive, not a revision of the reasoning
above: an enum scrutinee still gets exactly the closed-variant-name
match this section describes, unchanged. The two forms are never mixed
within one `match` — `typeck.rs::check_match` dispatches on the
scrutinee's own type (`Ty::Named` enum vs. `str`/`i64`/`bool`) before
ever looking at an arm's pattern, so there's no new ambiguity to resolve
the way Rust's `_`/bound-identifier scoping rule has to. Exhaustiveness
for the literal form can't be the closed, syntactic check §3.4 relies on
(`str`/`i64` aren't finite the way an enum's declared variant set is),
so it's enforced structurally instead: exactly one `_` arm is required,
and it must be last.

### 3.5 Ownership and refinement — extend two existing passes, add nothing new

- **Affinity** (`ownership.rs`): a `struct`/`enum` is affine iff any of its
  fields/payload types are affine — computed once per declaration (mirrors
  `Ty::is_affine()`'s existing flat `matches!` predicate, `ast.rs`), not a
  new checking mechanism. `struct Handle { f: box i64 }` moves as a whole
  the same way a bare `box i64` already does; the move-checker doesn't need
  to learn anything new to see it, only to look one level through a field.
- **Refinement bounds** (`refine.rs`/`smt.rs`): no new `where` syntax (§1's
  table already flags this as invented in the superseded draft). Instead,
  the existing boundary set — `let`/`return`/assign (`docs/LANGUAGE.md` §2) — is
  extended to include struct/enum construction calls, since construction is
  already an ordinary call whose arguments get type-checked against
  declared field/payload types (§3.1). A `struct Range { low: i32, high:
  i32 }` gets exactly the bounds-checking `i32` already gets at every field,
  today's Tier-1/2 machinery unchanged, applied at one more call-site kind.

### 3.6 Rollout layers

Same discipline as `docs/TRANSACT.md` and `docs/SANDBOXING.md` — one proven layer
before the next:

1. **Shipped.** `struct`/`enum` grammar + parser, fixed concrete fields, no
   generics yet. Proved the grammar addition is unambiguous first, same
   order `docs/TRANSACT.md` and the effects design in `docs/PROTOLANG_PORT.md` both
   used — see `docs/GRAMMAR.md`'s `struct_decl`/`enum_decl`/`match_expr`
   productions and its corrected `postfix` note.
2. **Shipped.** `typeck.rs`: declaration registration (plus two
   collision-checking namespaces this document didn't originally spell
   out in code terms — type names and callable/constructor names, kept
   separate since only a struct's own name lives in both) + positional
   construction checking + field access. Struct/enum names join the
   existing name-resolution table functions already use (`docs/LANGUAGE.md`
   §6); construction is checked exactly like a function call's argument
   list (`Checker::infer_struct_construction`/
   `infer_variant_construction`).
3. **Shipped.** `match` + exhaustiveness. The closed-variant-name check
   described in §3.4 — no wildcard, no binding patterns
   (`Checker::check_match`).
4. **Shipped.** `ownership.rs`: affinity propagation (§3.5, first half) —
   `ast::TypeRegistry::is_affine` recurses "one level through" a
   field/payload, extended to field-access (`p.f` moves `p` as a whole
   only when `f`'s own type is affine, generalizing the existing `*box`
   rule) and to `match` (an N-way branch merge generalizing
   `check_if_branches`' two-way one).
5. **Not yet shipped.** `refine.rs`/`smt.rs`: extend the boundary set
   (§3.5, second half) — struct construction calls don't yet get a
   Tier-1 static bounds *proof* the way a `let`/`return`/plain call
   argument does; they still get the Tier-2 runtime check
   (`interpreter.rs::check_ty`), same as everything else before its own
   Tier-1 pass lands.
6. **Shipped.** Generics: type parameters on `struct`/`enum` declarations
   (`Ty::Named(name, args)`, `ast::substitute_ty`). No separate
   monomorphizer pass, as designed — each concrete instantiation is just
   a structurally distinct `Ty` (`ast::zip_type_params` builds the
   substitution once per construction/field-access/match site). Two
   sources resolve a construction call's type arguments, since there's no
   explicit-type-argument call syntax at all: the expected type at the
   call site (`Checker::check`'s `Expr::Call` arm, the common case — a
   `let`/return/argument annotation), or, failing that, structural
   inference from the constructor's own arguments
   (`Checker::resolve_type_args`'s fallback, `bind_type_params`) — a
   parameter that appears in neither is
   `TypeErrorKind::GenericConstructorNeedsExplicitType`. `ownership.rs`'s
   affinity/match-arm-binding and `interpreter.rs`'s runtime `check_ty`/
   match-arm-binding are all substitution-aware too (the latter recovers
   a binding's concrete type from the payload *value* itself, via
   `Interpreter::value_shape_ty`, since this file has no expected-type
   context flowing through `eval_expr` at all to substitute from
   directly — a real, narrow, accepted imprecision for a *nested*
   generic struct/enum's own further type arguments specifically, not
   silently pretended away).
7. **Shipped.** Prelude sum types: `Option(T)`, `Result(T, E)`
   (`ast::prelude_enums`). Ordinary user `enum`s, exactly as designed —
   injected into `Program.enums` at parse time
   (`Parser::parse_program`) as if hand-written, going through the same
   registration/collision-checking as any real declaration (redeclaring
   `Option`, or reusing `Some`/`None`/`Ok`/`Err` as another name, is an
   ordinary `DuplicateType`/`DuplicateConstructor` error) — no
   special-casing anywhere else in the checker, which is itself the proof
   the mechanism is general enough to earn its place in `std`.
8. **Out of scope, as designed** — `struct`/`enum`/`match` join
   `thread`/`chan`/`sandbox` on the "interpreter-only, rejected not
   mis-compiled" list (`docs/LANGUAGE.md` §10 — `box`/`tcp`/`str` compile now,
   so they've dropped off it), not an exception to it. Since the prelude
   means `Program.enums` is never actually empty,
   `codegen.rs::check_supported` can't reject on declaration presence
   alone any more — it rejects a program only if it actually *constructs*
   a struct/variant (a name lookup against the declared constructor set,
   since a constructor call is syntactically just `Expr::Call`) or uses
   `match`/field access directly, rather than letting a struct
   constructor call slip past `check_expr`'s generic `Expr::Call`
   arm and fail some other, less clear way downstream.

**Named follow-on, not designed here:** once `Result(T, E)` exists, a `?`
early-return operator (sugar over a `match` that returns `Err` immediately)
becomes cheap and well-motivated — but it needs non-local control flow
(early return out of an arbitrary expression position) that nothing in
Nirdosha has any form of today, even after steps 1–7. Flagged as the next
question to pick up once Row 11 ships, the same way `docs/PROTOLANG_PORT.md`
flagged Row 11 itself rather than guessing at its shape prematurely.

---

## 4. What Row 11 unlocks (unchanged claim, now honestly sourced)

```
Row 11
 ├── struct  → config-as-code, HTTP Request/Response, DB row types, JSON records
 ├── enum    → Option(T), Result(T, E), the file/tcp error hierarchy
 └── generics (structural, no erasure) → List(T)-shaped collections, Option/Result themselves
```

Every item on the right was a **Blocked** or **Rejected** verdict in
`docs/PROTOLANG_PORT.md`, traced there to this exact gap. None of them needed
Rice's theorem, a lattice of effects, or a capability kernel to explain why
they were stuck — they needed a `struct`, an `enum`, and a `match`, which
is what §3 actually specifies rather than assumes into existence.

---

## 5. What Row 11 does not fix

| Gap | Why Row 11 doesn't touch it |
|---|---|
| No package manager, no LSP, no real users | Ecosystem/tooling/adoption — `docs/goal.md` §9's standing gap, unrelated to the type system |
| `thread`/`chan`/`sandbox`/`file`/`db`/`mq`/`json` still interpreter-only (`box`/`tcp`/`str` compile now) | Backend codegen work already tracked (`docs/LANGUAGE.md` §10); `struct`/`enum` join that list, they don't shorten it |
| No mechanized safety proof | `docs/goal.md` §9 item 2 — Lean4/Coq work, orthogonal to adding type formers |
| `?`-propagation, effects, config-as-code's actual `env()` builtin | Real follow-ons named above and in `docs/PROTOLANG_PORT.md`, each its own design question once its prerequisite (Row 11, in this case) exists |

---

*Amendment to Nirdosha design brief. References: `docs/goal.md`, `docs/LANGUAGE.md`,
`docs/GRAMMAR.md`, `docs/PROTOLANG_PORT.md`, `crates/compiler/src/{ast,typeck,ownership,
refine,smt}.rs`.*
