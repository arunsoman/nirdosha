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

**What's still not ownership, on purpose.** There's no borrowing (`&`/
`&mut`) — a function that wants to use a `box` without taking it still has
to take it by value and hand it back. No `Drop`-like destructor hook
exists (nothing runs "when a box goes out of scope" beyond what Rust's own
`Box<Value>` does for free). No aliasing of a box is possible at all yet,
borrowed or owned — which is actually *more* restrictive than real Rust,
not less; loosening it safely (shared read-only borrows, primarily) is a
real next increment, not a bug.

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
| 1 — No GC, no `free()` | **Started, real content.** `box`/`*` plus a static move-checker (`ownership.rs`) — see the "Second update" section above for what's actually proved and what still isn't (no borrowing, no `Drop` hook, no aliasing at all yet). Regions/bulk-arena allocation is still not started. |
| 2 — No data races | N/A yet — no concurrency exists. |
| 3 — No deadlocks | N/A yet — no concurrency exists. |
| 4 — No int/buffer overflow | **Placeholder only.** `Ty::in_range` + `check_ty` catch out-of-declared-range values at `let`/assign/return/call boundaries — but at *runtime*, dynamically, not proved absent at compile time. This is honestly labeled Tier-2-shaped, not Tier-1: see `ErrorKind::OutOfRange`'s message and `ast.rs`'s doc comment. Real Phase 2 work is an SMT-discharged static pass. |
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

Borrowing (`&`/`&mut`) for `box` values — right now a function that wants
to *use* a box without consuming it still has to take it by value and hand
it back, which makes the ownership model correct but nearly unusable for
anything beyond toy programs. That's the natural next increment on row 1.
Refinement types (row 4, needs an SMT solver integration) and effects
(rows 4/9) remain the two other prerequisite-free next milestones, chosen
by which the next session's time is better spent on rather than a fixed
order.

**Noted for Phase 3 (concurrency, rows 2–3), not relevant yet:** if actors
end up implemented as lightweight stackful coroutines multiplexed onto a
few OS threads (rather than one-OS-thread-per-actor), a *cactus stack*
(spaghetti stack — a tree-shaped call stack where many logical stacks share
common ancestor frames) is a real, established technique for making that
cheap at scale. Not applicable today: the interpreter is a plain recursive
tree-walker on Rust's own call stack, with no user-level threading or
continuation-capture for it to help with yet.
