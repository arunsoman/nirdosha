# Phase 0 status

**Update:** the static type checker flagged below as "suggested next
milestone" is now built (`compiler/src/typeck.rs`) and wired into `run()` —
a program that fails type checking is never executed. 28/28 tests pass. The
row table and "what's deferred" notes below are the original Phase-0-only
snapshot; treat the type-checker line in the row table as superseded by
this note, not edited in place, so the history of what shipped when stays
legible.

**What the checker actually proves**, not just "type checking exists":
- "No implicit conversions" (goal.md §3) is now real — an `i32` and an
  `i64` variable can't be added without an explicit cast existing (there
  isn't one yet, which is itself honest: the language currently has no way
  to convert between integer widths at all, on purpose, until that's
  designed rather than defaulted into).
- Integer *literals* stay flexible against a declared width (`n - 1` needs
  no annotation on `1`), so the strictness above doesn't make ordinary
  arithmetic unusable.
- `if` used as a statement vs. used as a value are genuinely different
  static contexts — branches must agree in type (and an `else` must exist)
  only when the value is actually read. Getting this wrong would have
  made the checker reject `examples/loop.nir`, which is why it has its own
  test (`if_with_no_else_used_as_a_statement_is_fine`).
- `return` nested inside a value-producing `if`-branch (e.g. inside a
  `let`'s initializer) type-checks correctly against the *function's*
  return type, independent of the `let`'s declared type — matching what
  the interpreter could already run via `Signal` propagation. See
  `return_nested_inside_a_value_position_if_still_typechecks` in
  `tests/basic.rs`.
- Definite-return analysis: a function declared to return non-`unit` must
  provably return on every path, checked structurally, not discovered at
  runtime.
- Error recovery: a program with two independent mistakes gets both
  reported in one pass, not just the first (`unknown_variable_is_caught_
  statically_before_any_output`) — the shape goal.md row 9 asks for.

**Second update:** a static move-checker now exists too
(`compiler/src/ownership.rs`), giving row 1 ("no GC, no manual `free()`")
its first real content. Before this, the language had no heap-allocated
value at all — ownership had nothing to say anything about. Now there's
`box <type>` (a single-value heap cell) and `*expr` (deref-read), and a
proper move-checker: using an affine (`box`-typed) binding by name
transfers ownership; a later use of the same binding on the same
control-flow path is a compile-time "use after move" error. 43/43 tests
pass, including the two design decisions worth calling out specifically:

- **Branch merging.** `if c { moves b } else { doesn't }` has to treat `b`
  as moved either way afterward, since the checker can't know at compile
  time which branch ran — the same conservative merge Rust's own borrow
  checker does. Verified by `moving_in_only_one_if_branch_still_poisons_
  later_use`.
- **Loop double-pass.** A `while` body might run more than once, and a
  variable it moves on iteration 1 is gone by iteration 2 — checking the
  body only once (from the state *before* the loop) would miss that.
  This was an actual bug in the first draft, caught while writing the
  module doc, not a hypothetical: the fix checks the body once, silently,
  to compute what one iteration produces, merges that with the pre-loop
  state, then checks the body again for real from there. Verified by
  `moving_a_pre_loop_variable_inside_the_body_is_rejected`.
- **Nested boxes exposed a real soundness gap, found by testing, not by
  design review.** `*bb` for `bb: box box i64` hands out the *inner* `box
  i64` by value — itself affine — so extracting it has to consume `bb`,
  the same as any other move. The first draft exempted *every* deref from
  move-checking (correct for `box <scalar>`, wrong for `box box T`); it
  shipped, ran, and returned the right answer for a single dereference,
  and only the *second* dereference of the same nested box revealed the
  gap. Fixed and pinned by
  `dereferencing_a_nested_box_twice_is_use_after_move`. Worth keeping as a
  concrete example of why "it ran and gave the right answer" isn't the
  same as "it's sound" — the bug was invisible until a test specifically
  tried to reuse the outer binding.

**What this does and doesn't prove.** The interpreter clones a `Value` on
every variable read, so right now aliasing a `box` couldn't actually
corrupt anything at runtime even without this checker — two "owners" just
end up with independent Rust-owned trees. The checker's value is entirely
prescriptive: it proves the single-ownership discipline a real (future,
LLVM-compiled, arena/region-based) backend would need in order to free
memory deterministically with no garbage collector, before there's a real
backend that needs it. Read this as "the proof exists, not yet
load-bearing" — see `ownership.rs`'s module doc for the full reasoning.

**Third update:** shared borrows (`&type`, `&expr`) now exist. A function
can read a value without consuming it — `fn peek(r: &i64) -> i64 { return
*r }` — and the same binding can be borrowed any number of times, since a
reference is never affine (`Ty::is_affine` returns `false` for `Ty::Ref`;
unlimited simultaneous readers is always sound, the same reason Rust
allows it). 49/49 tests pass. `&mut` (exclusive/mutable borrows) is still
not built — it needs real liveness tracking to enforce "aliasing xor
mutability" (at most one mutable borrow, or any number of shared ones,
never both at once), which is a materially bigger undertaking than shared
borrows turned out to be, so it stayed out of this increment rather than
being rushed.

Two things worth being precise about, both found by testing rather than
assumed correct from the design alone:

- **The real rule enforced, not an invented one.** `*r` for `r: &box i64`
  is a compile error ("cannot move `box i64` out of a shared reference") —
  you can't extract owned, affine content through a borrow, regardless of
  whether the underlying binding happens to be unmoved. This is the exact
  rule real Rust enforces too (`*r` for `r: &Box<T>` requires `T: Copy`
  or an explicit `.clone()`), not a simplification invented for this
  project.
- **Known, honestly documented limitation: no place-expression
  semantics.** Because of the rule above, there is currently *no way* to
  read the scalar *inside* a box reached only through a reference — `**r`
  doesn't help, because the inner `*r` hits the same rejection before the
  outer `*` runs at all. Real Rust avoids this because `**r` is evaluated
  as one composed *place* expression, never treating the intermediate
  `Box` as a value that has to move. Building that (tracking whether an
  expression denotes a place or a value, the way a MIR-based borrow
  checker does) is real additional work this increment didn't attempt.
  `&box T` is borrow-and-pass-around-only for now, not read-through — see
  `ownership.rs`'s module doc and
  `borrowing_a_box_repeatedly_does_not_consume_it` in `tests/ownership.rs`
  for the full reasoning and a pinned example of exactly what does and
  doesn't work.

**What's still not ownership, on purpose.** No `&mut` (see above). No
`Drop`-like destructor hook exists (nothing runs "when a box goes out of
scope" beyond what Rust's own `Box<Value>` does for free). No place
expressions (see above) — `&box T` can be passed around and re-borrowed
freely, but not read through to affine content inside it.

**Fourth update:** row 4 (no int/buffer overflow) now has a real static
proof pass — `compiler/src/refine.rs`, interval (range) analysis. Checked
first: no system Z3, no `cmake` (needed for the `z3` crate's bundled
build), and installing system packages wasn't something to do without
asking — so this is a deliberate, documented substitution for what
goal.md §3/§6 Phase 2 actually specify (a Z3-class SMT solver), not a
silent downgrade. Interval analysis is the same family of technique real
safety-critical tools (Astrée, Polyspace) use for exactly this class of
proof, and it's strictly weaker than SMT: no disjunctive reasoning, no
cross-procedure inference, no nonlinear arithmetic, no condition-based
narrowing in `if` branches.

**Scoped to two proofs, deliberately, not for lack of ideas but to avoid
a wrong proof.** (1) An arithmetic expression fits its declared target
type. (2) A division's divisor is never zero. Division's *result*
interval is never computed — integer-division interval arithmetic has
real edge cases (truncation direction, sign-crossing divisors) that are
exactly the kind of place a soundness bug hides, and given how much this
whole project's credibility rests on "what's proved is really proved,"
that felt like the wrong place to cut a corner under time pressure.

**Tested for honesty, not just success.** 59/59 tests pass, and several
exist specifically to confirm the pass *doesn't* over-claim:
`two_full_range_i8_params_summed_is_not_proven_in_range` (i8+i8 can reach
254, genuinely unsafe), `division_by_an_unconstrained_parameter_is_not_
proven_nonzero`, and `factorial_multiplication_is_not_proven_in_range`
(the realistic case — recursive multiplication really can overflow, and
there's no interprocedural summary to say otherwise). A proof pass that
only ever demonstrates success hasn't demonstrated it's sound; these
tests are the other half of that claim.

**Not wired to elide the interpreter's runtime check, on purpose.** A
real Tier 1 in a compiled backend would skip emitting the check entirely,
recovering real runtime cost. This is a tree-walking interpreter with no
codegen yet (goal.md §3's Backend layer doesn't exist) — removing the
redundant check here would only remove a safety net for zero performance
benefit. `RefineReport` is the real, standalone deliverable: a genuine
proof, ready for a future backend to act on, not yet load-bearing for the
same reason `ownership.rs`'s proof isn't yet either (see the "What this
does and doesn't prove" note above) — a pattern worth naming now that
it's shown up twice: this project's static passes keep arriving before
the backend that would spend their payoff, and that's a fine order to
build things in.

**Known limitation, not yet attempted (in `refine.rs`; resolved in
`smt.rs`, see below):** no condition-based interval narrowing in `if`
branches (e.g., proving `n > 1` is known inside the `else` of `if n <= 1
{ .. } else { .. }`).

**Fifth update:** the user installed a real Z3 (system `libz3.so`
4.16.0, headers, and pkg-config module — checked directly, not assumed)
partway through the Fourth update above. `compiler/src/smt.rs` is the
result: a genuine SMT-backed Tier-1 checker using the actual `z3` crate
against the system library (no `cmake`/bundled build needed — the crate
links directly once headers and the shared library are present). This
is now the primary Tier-1 checker; `refine.rs` stays in the tree
deliberately, not deleted, as the documented fallback for an environment
without Z3 — its design reasoning didn't stop being correct just because
a stronger solver showed up, and portability to such environments is a
real, ongoing concern, not a hypothetical one (this project ran in
exactly that state for two whole increments).

**What SMT actually buys, checked by a real test, not asserted in a
comment.** `condition_narrowing_proves_what_interval_analysis_cannot` in
`tests/smt.rs` runs the *identical* program —

```
fn classify(n: i64) -> i64 {
    if n >= 0 && n <= 100 {
        let x: i8 = n
        return 0
    }
    return -1
}
```

— through both `smt::analyze` and `refine::analyze`, and asserts they
disagree in exactly the expected direction: `smt.rs` proves `x: i8 = n`
safe (it asserts `n >= 0 && n <= 100` into the solver before checking the
`let`, so `n`'s narrowed range is genuinely known there); `refine.rs`
does not and structurally cannot (interval analysis has no
representation for "this variable's range depends on a boolean condition
holding" — see its module doc). This is the single clearest, checked
demonstration in the whole codebase of why the Fourth update's
substitution was honestly labeled a substitution and not treated as
equivalent.

**The API surface itself needed real investigation, not assumption.**
The `z3` crate (0.20.2) uses a newer, simpler API than older
documentation/examples for the crate suggest — an implicit thread-local
`Context` (no explicit `Context::new`/threading required), `Solver::new()`
taking no arguments, arithmetic via ordinary Rust operators (`+`, `-`,
`*`, `!`, `&`, `|`) rather than only named methods. Discovered by reading
the crate's actual source in the local cargo registry cache rather than
guessing from memory or older examples, after an initial smoke test
based on a remembered older API failed to compile — worth naming as a
small, real instance of the same "check before assuming, then fix, don't
guess" discipline this whole project has tried to hold to elsewhere.

**What's unchanged from the Fourth update, and why.** Same two proof
targets (arithmetic-in-range, division-nonzero); division's result value
still not modeled, for the same integer-truncation-edge-case reason, not
because the solver is weaker; no interprocedural summaries (a call's
result is a fresh, only-bounds-constrained symbolic value); loops still
widen any body-reassigned variable to an unconstrained value on entry
rather than attempting loop-invariant synthesis (SPARK itself requires
programmer-supplied loop invariants for the same reason — this isn't a
shortcut unique to this project). Still not wired to elide the
interpreter's runtime check, for the same "no backend to spend the
performance payoff on yet" reason as before.

---


What exists on disk right now, against `goal.md` §6's Phase 0 description
("narrow the core, draft the grammar") and §1's ten-row requirement table.
This is the first slice, not a finished language — read it next to
`GRAMMAR.md` for the formal grammar and its documented gaps.

## What runs today

```
compiler/
  Cargo.toml
  src/
    token.rs        lexer — hand-written, single pass, structured LexError
    ast.rs           AST types; Ty::in_range is the Tier-2 bounds-check stand-in
    parser.rs        recursive-descent, single-token lookahead, no backtracking
    interpreter.rs   tree-walking evaluator, dynamic type checking
    lib.rs           public run(src) -> Result<Value, String>
    main.rs          CLI: `nirdosha <file.nir>`
  examples/
    hello.nir        functions, params, arithmetic, print
    factorial.nir     recursion, if/else-as-expression, return-in-branch unwind
    loop.nir         while, assignment/mutation, if-as-statement
  tests/
    basic.rs         14 tests: 3 example runs + 11 language/error-shape checks
```

```
$ cd compiler && cargo run --quiet -- examples/factorial.nir
3628800
$ cargo test
test result: ok. 14 passed; 0 failed
```

## Row by row, against goal.md §1

| Row | Status |
|---|---|
| 1 — No GC, no `free()` | **Started, real content.** `box`/`*`, shared borrows (`&`), and a static move-checker (`ownership.rs`) — see the "Second" and "Third" update sections above for what's actually proved and what still isn't (no `&mut`, no `Drop` hook, no place expressions). Regions/bulk-arena allocation is still not started. |
| 2 — No data races | N/A yet — no concurrency exists. |
| 3 — No deadlocks | N/A yet — no concurrency exists. |
| 4 — No int/buffer overflow | **Started, and now backed by real SMT.** `Ty::in_range` + `check_ty` still catch everything dynamically (Tier 2, unchanged). Two static Tier-1 passes exist: `compiler/src/refine.rs` (interval analysis, built when this environment had no Z3) and `compiler/src/smt.rs` (real Z3 4.16, once the user installed it — see the "Fifth update" section below), the latter now the primary checker since it's strictly more capable. 68/68 tests pass, including a flagship test that runs the *same* program through both passes and confirms SMT proves something interval analysis structurally cannot (condition-based narrowing). Not wired to elide the interpreter's runtime check — see below for why. |
| 5 — Native speed | Not started. This is a tree-walking interpreter, deliberately — LLVM codegen is Phase 1+ work once there's a stable typed AST to compile from. |
| 6 — Learning curve | Grammar is small and keyword-heavy on purpose (`fn`/`let`/`return`/`if`/`else`/`while`, C-family operators) — no attempt yet to *measure* this against goal.md §7's proxy metrics (novice user study, Cognitive Dimensions score). Row 6 is aimed at, not yet verified. |
| 7 — LLM-friendly | The one row with a real, checked claim already: the parser is single-token-lookahead with no backtracking, everywhere — see GRAMMAR.md for what that does and doesn't prove. No LALR-generator cross-check yet, and no benchmark suite / grammar-constrained decoder built yet. |
| 8 — Compositional syntax | Followed structurally: `interpreter.rs`'s `eval_expr` has exactly one match arm per `Expr` variant, none reaching into a sibling's internals. Not yet stated as a proven theorem about a formal semantics — there is no formal semantics document yet, only the implementation. |
| 9 — AI as first-class citizen | Partial groundwork only: errors are structured (`ErrorKind` enum + `Span`, matched by tests without string-parsing — see `tests/basic.rs`'s structured-error tests), which is the prerequisite for row 9, not row 9 itself. No typed AST/IR splicing interface for agents exists yet. |
| 10 — Tamper-evidence | Not started. No build/attestation pipeline exists yet — this needs Phase 4 (reproducible builds, capability manifests) and, per the earlier discrepancy check, a Sūtra kernel that doesn't exist on this machine yet either. |

## What was deliberately deferred, and why

- **Superseded, kept for history:** this section used to list "dynamic
  typing, not static" and "mutation with no ownership discipline" as
  deferred work. Both are now partially done — see the two "update"
  sections above (`typeck.rs`, `ownership.rs`). What's still genuinely true
  of mutation: scalar locals remain exactly as freely reassignable as a
  Python local (no ownership tracking applies to them at all — only
  `box`-typed bindings are ownership-tracked), and there is still no
  borrowing (`&`/`&mut`) for `box` values, so "own it or don't touch it"
  is the whole story for now.
- **No arrays, structs, or `for`.** Left out on purpose rather than guessed
  at, because refinement types (row 4) will shape how indexed/sized types
  get spelled, and it's cheaper to design that once in Phase 2 than to
  bolt it on now and redesign it later (`GRAMMAR.md`'s omissions list).
- **No LLVM.** A tree-walking interpreter was the right choice for
  validating the grammar and semantics quickly; codegen is a backend
  concern (§3's Backend layer) that shouldn't gate front-end iteration.

## Suggested next milestone

Three candidates, none blocking the others:

- **Place expressions**, to make `&box T` actually read-through (the gap
  the Third update documents) — the more foundational of the two ownership
  gaps, since `&mut` needs place-expression machinery too, just with an
  extra exclusivity check on top.
- **`&mut`** (exclusive borrows) — needs real liveness tracking to enforce
  "aliasing xor mutability," materially bigger than shared borrows turned
  out to be.
- **Extend `smt.rs`'s proof targets** — right now it proves the same two
  things `refine.rs` did (arithmetic-in-range, division-nonzero); with a
  real solver in hand, array/index-bounds proofs (goal.md row 4's other
  half, "buffer overflow") are a natural next target once arrays exist in
  the language at all (still not — see GRAMMAR.md's omissions list).
- **Effects** (rows 4/9) — still fully unstarted.
- **Bool-typed variable narrowing in `smt.rs`** — `bool_expr`'s `Ident`
  case currently falls back to an unconstrained fresh `Bool` (its doc
  comment flags this explicitly): `let ok: bool = n > 0; if ok { ... }`
  doesn't currently narrow `n` the way `if n > 0 { ... }` directly would.
  A real, scoped gap, not a hypothetical one.

**Noted for Phase 3 (concurrency, rows 2–3), not relevant yet:** if actors
end up implemented as lightweight stackful coroutines multiplexed onto a
few OS threads (rather than one-OS-thread-per-actor), a *cactus stack*
(spaghetti stack — a tree-shaped call stack where many logical stacks share
common ancestor frames) is a real, established technique for making that
cheap at scale. Not applicable today: the interpreter is a plain recursive
tree-walker on Rust's own call stack, with no user-level threading or
continuation-capture for it to help with yet.
