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

**Sixth update:** row 5 (native, hardware-speed codegen) now has real
content. `compiler/src/codegen.rs` emits textual LLVM IR — not a Rust
binding to the LLVM C API (`inkwell`/`llvm-sys`), because this
environment's LLVM 22 is recent enough that a binding crate's supported-
version list might not cover it; textual IR is a stable format, and the
system `clang` compiling it is *the same* LLVM 22, so there's no version
skew between what's emitted and what assembles it. Several real compilers
use exactly this strategy. `nirdosha build <file.nir> -o <out>` produces
an actual native executable; `nirdosha emit-llvm <file.nir>` prints the
generated IR.

**Honestly scoped, not silently narrowed.** `check_supported` rejects,
with a specific reason, anything outside signed integers (`i8`..`i64`),
`bool`, `unit`, and `print` on integer-typed arguments only — no
`u8`..`usize` (needs a signed-vs-unsigned instruction choice this pass
doesn't make), no `box`/`&`/`*` (compiling real heap allocation and move
semantics to native code is separate, larger work than proving the
discipline statically — `ownership.rs`'s proof exists, nothing executes
on it yet). `tests/codegen.rs` confirms `examples/ownership.nir` and
`examples/borrow.nir` are rejected outright, not silently mis-compiled.

**Tier 1 vs Tier 2 finally means something, for the first time in this
codebase.** `refine.rs` and `smt.rs` both said, explicitly, "not wired to
elide the runtime check — no backend exists yet to spend the payoff on."
One does now: a `let`/assignment whose span is in `smt::analyze`'s
`proven_in_range` gets no runtime bounds check at all in the compiled
binary (Tier 1); one that isn't gets a real compare-and-trap sequence
(Tier 2). `tests/codegen.rs`'s `proven_safe_arithmetic_has_no_trap_
block_in_the_ir` / `unproven_arithmetic_does_have_a_trap_block_in_the_ir`
check this at the IR text level, not just "it happened to work."

**Three real bugs, found by running compiled binaries, not by reading
the code — worth recording in full, because each one is exactly the kind
of mistake this whole project's discipline exists to catch, and each one
*did* get caught, not shipped silently:**

1. **Silent wraparound defeated the overflow check entirely.** The first
   draft computed arithmetic directly at the declared narrow LLVM width
   (`add i8`), which wraps on overflow exactly like real two's-complement
   hardware. `100 + 100` (overflows `i8`, max 127) wrapped to `-56` —
   still "in range" for `i8` by construction — so the guard could
   *never* fire; a deliberately-overflowing test program compiled,
   ran, and exited 0 instead of aborting. Caught by writing that exact
   test program and running the compiled binary, not by review. Fixed by
   matching what `interpreter.rs`'s `Value::Int(i64)` already did all
   along: every integer-typed value is computed at `i64` internally,
   range-checked at `i64` width (before any wraparound could happen),
   and only narrowed to its declared width at storage/parameter/return
   boundaries, after the check passes. `narrow_type_overflow_actually_
   traps_at_runtime` in `tests/codegen.rs` pins this as a permanent
   regression test.
2. **A negated literal call argument type-mismatched.** `-3` was
   computed via a real `sub i64 0, 3` instruction, then passed where a
   narrower parameter type (e.g. `i32`) was declared — LLVM requires a
   call site's argument types to match the callee's signature exactly.
   Fixed by reusing the same `literal_value` helper `typeck.rs` already
   used to decide literal flexibility (factored into `ast.rs` as a
   shared function specifically so the two modules can't silently
   disagree about what counts as a literal) and emitting literal
   arguments directly at the callee's declared width, no instruction
   needed.
3. **A process-wide temp-file race.** `codegen::build`'s temp `.ll`
   filename used only `process::id()`, which is identical across every
   thread in one process — three genuinely-correct compiles, run in
   parallel by `cargo test`'s default threading, raced on the same file
   and came back with empty output. Fixed with a process-wide atomic
   counter alongside the pid. A real robustness bug for any caller doing
   concurrent builds in one process, not a test-only artifact.

**What's still not done, honestly.** No optimization passes (`-O0`
equivalent throughout — correctness over performance was the explicit
priority for a first backend). `if`-as-a-value's result slot is
hardcoded `i64`; a genuinely `bool`-valued `if` whose branches both fall
through (not the common "both return" or "side-effect only" shapes) would
mis-type the store — not hit by any current example, flagged in
`codegen.rs`'s `if_expr` rather than silently shipped. `Stmt::Return`'s
guard is always Tier 2 (neither `refine.rs` nor `smt.rs` records a proof
for a `return` site yet) — a real, scoped follow-up, not a fundamental
limitation.

**Seventh update:** row 7's grammar cross-check, deferred since Phase 0
("worth doing once it stabilizes"), finally happened — a new top-level
crate, `grammar_check/`, transliterates `GRAMMAR.md`'s EBNF into
`lalrpop` syntax and asks an independent LALR(1) generator whether it's
actually conflict-free. It isn't, and the *reason* is a genuine finding,
not a build-tooling problem: Nirdosha has no statement separator (no
semicolons, no significant newlines), so anywhere an operator token could
either extend the current expression or start a new statement, the
grammar is ambiguous as a plain CFG. `lalrpop` reports this at every
level of the precedence chain.

**Checked against the real interpreter, not left as a formal curiosity.**
`return x` immediately followed on the next line by `-y` (nothing
between them) genuinely could parse two ways — `return (x - y)`, or
`return x` followed by a separate `-y` statement. Running it:

```
$ nirdosha /tmp/ambiguity_check2.nir   # let x=5; let y=3; return x \n -y
=> 2
```

`5 - 3 = 2` — confirmed as one statement, deterministically, every
single time (`parser.rs` has no backtracking and no second attempt to
try the other reading). The rule that produces this — **always prefer to
extend the current expression over ending the statement** — was real,
consistent, and load-bearing all along, but existed only as an emergent
property of the hand-written parser's control flow, never written down
anywhere. `GRAMMAR.md` now states it explicitly, both as prose and
attached directly to the EBNF.

**An early attempt to eliminate the conflicts by narrowing the
`return`-specific case (matching `parser.rs`'s actual rule — bare
`return` only immediately before `}`) didn't change the conflict count —
which is itself informative,** not a failed fix to hide: it proved the
ambiguity was never really about `return` specifically, `return` was
just the first place it became visible. Fully eliminating the conflicts
would need either mandatory statement separators (a real, disruptive
language change) or dense `lalrpop`-specific precedence annotations
across the whole expression grammar for a green build that wouldn't
prove anything the finding above doesn't already prove more directly —
recorded as a real, open option in `grammar_check/README.md`, not
pursued given what it would have cost against what it would have added.

**The honest bottom line, stated the way row 0's Rice's-theorem framing
asks every claim in this project to be stated:** the *parser* is
unambiguous — deterministic, single-token lookahead, no backtracking,
the original claim stands. The *grammar as an abstract specification*,
independent of any one parser implementing it, was not unambiguous
without a rule that lived only in code until this check surfaced it.
That gap — spec looser than implementation — is exactly the kind of
thing an independent cross-check exists to find, and it found one.

**Eighth update:** `Stmt::Return` now gets real Tier-1 treatment in both
`refine.rs` and `smt.rs` — the one documented gap left over from the
Sixth update, closed the same way `codegen.rs` already had (a
`current_fn_ret` field threaded into the checker so a `return` site has
something to check its value against). Small, bounded fix — except it
immediately broke a test, and the reason why is worth recording in full.

**A real, structural bug in `refine.rs`, invisible until `Return` sites
were checked at all.** `factorial_multiplication_is_not_proven_in_range`
started failing — not because the fix was wrong, but because it exposed
that `refine.rs`'s `Interval` was `i64`-backed, and `Interval::unknown()`
was defined as *exactly* `[i64::MIN, i64::MAX]` — `Ty::I64`'s own legal
range. That makes "is this interval within `i64`'s range" vacuously true
for *every* interval, since no `i64`-backed bound can ever fall outside
`i64`'s own range in the first place. `refine.rs` could prove an
`i64`-typed value safe but could never actually catch a real one that
wasn't — a blind spot specific to the language's widest type, hidden the
whole time `Return` sites went unchecked, surfaced the moment they
weren't. `smt.rs` never had this bug: Z3's `Int` sort is a genuinely
unbounded mathematical integer, not `i64`-backed, so it was never
capable of this particular vacuous truth.

**Fixed by widening, not by special-casing.** `Interval`'s `lo`/`hi`
fields moved from `i64` to `i128`, giving real headroom: a computation
that actually overflows `i64` now produces bounds outside
`i64::MIN..=i64::MAX`, which the range check can genuinely detect.
Pinned directly with a new regression test,
`two_unconstrained_i64_params_multiplied_is_not_proven_in_range` — two
unconstrained `i64` parameters multiplied and returned as `i64`, which
obviously can overflow in reality and, before the fix, was being
claimed as proven safe. `i128` isn't a perfect ceiling either (chained
extreme operations could in principle approach *its* limit too), but
`saturating_*` arithmetic keeps that sound — a wider blind spot than
`i64`'s, not a reintroduction of the same bug, and honestly noted as a
residual limitation in `Interval`'s own doc comment rather than assumed
away.

**Two existing tests had to be corrected, not just re-passed — a
distinction worth being precise about.** Both `refine.rs`'s and
`smt.rs`'s versions of `factorial_multiplication_is_not_proven_in_range`
had asserted "nothing in factorial is proven," which was accidentally
too broad: `return 1` in the `n <= 1` branch is genuinely, trivially
safe (`1` always fits `i64`), and correctly *does* get proven now that
`Return` sites are checked at all. The tests were rewritten to check the
*specific* multiplying `return` instead — the real claim the test names
were always meant to make. This is a real improvement surfacing an
over-broad test, not a regression papered over.

84/84 tests pass. Row 4's status line above and this section together
are the full, current picture — the row-4 line hasn't been re-edited to
say "84/84" since the running count would only go stale again next
session; treat this update as the current authority on the number.

**Ninth update:** row 5's codegen now actually delivers on "hardware
speed," not just "produces a binary that runs." `codegen::build` takes
an `OptLevel` (`O2` by default, `O0` via `nirdosha build ... --opt0`) —
the generated IR is unoptimized either way (still "alloca everywhere,"
module doc), but `clang` is now asked to optimize it afterward, the same
as it would for C source, matching what goal.md row 5 actually asks for
rather than settling for the weaker "compiles and runs" bar the earlier
milestone cleared.

**This was also, deliberately, a stress test — and it passed.** `-O2` is
an aggressive optimizer, and LLVM treats every `unreachable` this
backend emits (for provably-dead code — a definitely-returning
function's fallthrough, an if-expression whose branches both terminate)
as a hard guarantee it's free to optimize around. A subtly wrong
`unreachable` could produce correct output at `-O0` by luck and silently
misbehave at `-O2`. `tests/codegen.rs`'s new `optimized_and_
unoptimized_builds_agree_on_every_example` runs all three core examples
at both levels and checks both against the interpreter's own output —
and the overflow-trap and division-by-zero tests now run at `-O2` by
default too. All of it passed on the first attempt: no latent
`unreachable` bug turned up. Worth recording as a real, checked absence
of a bug, not just silence — the difference between "nobody looked" and
"someone looked and it held."

85/85 tests pass.

**Tenth update:** `codegen.rs`'s one remaining documented gap is closed —
a genuinely `bool`-valued `if`-expression whose branches both fall
through (`let ok: bool = if c { true } else { false }`) now gets a real
`i1` result slot instead of a hardcoded `i64` one. Fixed by inferring the
slot's type from the `then` branch's trailing expression
(`block_trailing_ty`, reusing `local_ty_of`) — sound because
`typeck::check_if` already proved both branches agree in type at any
real value-position use, so only the `then` side needs inspecting. A
`unit`-valued `if` (no LLVM value to hold at all — `alloca void` isn't
legal IR) skips the slot entirely now, running both branches purely for
side effects, rather than forcing every case through the same
one-size-fits-all `i64` slot the old code used.

86/86 tests pass. This closes out every gap the Sixth update's codegen
milestone originally flagged.

**Eleventh update:** rows 2-3 (no data races, no deadlocks) now have a
first, real implementation — `spawn`/`join` and a new `thread <T>` type,
wired through every pass (lexer, parser, `typeck.rs`, `ownership.rs`,
`refine.rs`/`smt.rs`, `interpreter.rs`; `codegen.rs` honestly rejects it,
the same "reject, don't mis-compile" treatment `box`/`&` got before their
own codegen support existed).

*Execution model, chosen deliberately.* The user asked for the API to be
framed like Java's virtual threads — a cheap, language-level concurrency
abstraction, decoupled from what actually backs it. The honest first
implementation is real OS threads (`std::thread::spawn`/`JoinHandle`):
`spawn f(args)` runs `f` on a genuine new OS thread and returns a
`thread T` handle immediately (`T` = `f`'s return type); `join h` blocks
until it finishes, consumes the handle, and produces its `T`-typed result.
Nothing about the *language-level* API (the `spawn`/`join` surface, or
`Ty::Thread`'s affine-ness) assumes OS threads specifically — a future
M:N/lightweight scheduler can replace what backs a `thread T` without
changing any program's observable semantics. That's the actual point of
naming it after virtual threads now, before the cheaper backing exists.

*Race-freedom, and where it actually comes from.* No new concurrency-
specific safety logic was written. `ownership.rs`'s `Expr::Spawn` arm
reuses the exact move-checking a normal function call already does on its
arguments — an argument passed to `spawn` is consumed, exactly like an
argument passed to any function, so the spawning side can never alias a
moved value with the spawned computation. `Expr::Join` consumes the whole
handle the same way, giving `join` its single-use behavior statically (a
second `join h` is a compile-time use-after-move, not a runtime race).
Deadlock-freedom follows from the same shape: `thread T` handles are
affine, so a program's handles form a DAG by construction (no handle can
be shared to create a wait-cycle) — there is no mutex/lock primitive yet,
so this is the whole deadlock story so far, not a completed one.

*Runtime backstops, as defense in depth, not the real gate* — matching
every other check in `interpreter.rs`: joining an already-joined handle
hits `ErrorKind::AlreadyJoined` if it were ever reached at runtime (it
isn't, statically, per above); a genuine Rust panic inside a spawned
computation (e.g. undetected i64-vs-i64 overflow — i64 has no *dynamic*
range guard, only the Tier-1 static provers even attempt it, see the
Fourth/Fifth updates) is caught at the `JoinHandle::join()` boundary and
converted to a structured `ErrorKind::ThreadPanicked { message }` instead
of unwinding past the thread.

*The architectural cost.* Real `std::thread::spawn` requires `'static`
data, and the old `Interpreter<'p> { fns: HashMap<String, &'p FnDecl> }`
borrowed from the program it was interpreting — incompatible with a
spawned thread needing its own independent way to look up functions.
`Interpreter` now owns `Arc<Program>` instead of borrowing it (cheap to
clone into a spawned closure); `Value` gained a `Thread(Arc<Mutex<Option<
ThreadHandle>>>)` variant (`Arc`+`Mutex`, not `Rc`+`RefCell`, because the
value must be `Send` to cross into the spawned closure; `Mutex<Option<_>>`
so `.take()` can extract the handle exactly once, matching `join`'s
single-use semantics, while the interpreter's existing clone-on-read
`Env` model still works via cheap `Arc::clone`). `Value` lost its derived
`PartialEq` (`JoinHandle` has none) in favor of a manual impl.

Explicit, honest limitations: still one OS thread per `spawn` (no M:N
scheduling yet — the API is *shaped* for it, not backed by it); `codegen`
doesn't support `spawn`/`join` at all yet (interpreter-only, like `box`/`&`
were before their own codegen work); `print` (and any other builtin) can't
be spawned — `typeck.rs` rejects it explicitly (`CannotSpawnBuiltin`)
because the interpreter's spawn machinery only knows named user functions;
no channels or other message-passing primitive exists yet, only
spawn+join's single-result-handoff shape.

`examples/threads.nir` demonstrates the accepted program (two independent
`spawn`s, both joined, matching the loose/deep-nesting-free style of the
other examples); `tests/concurrency.rs` (9 tests) covers the round trip,
two-independent-spawns, the two ownership-checker rejections above (moved-
box-reused, double-join), both runtime backstops (driven directly against
`Interpreter`, bypassing `check_ownership`, to prove the backstop fires on
its own rather than only ever being shadowed by the static check), and
both static `typeck.rs` rejections (`CannotSpawnBuiltin`,
`ExpectedThreadType`). 96/96 tests pass; `cargo clippy --all-targets` is
clean (one `type_complexity` warning on `Value::Thread`'s nested generic,
fixed with a `ThreadHandle` type alias rather than suppressed).

**Twelfth update:** `chan`/`send`/`recv` — a first `Ty::Channel(Box<Ty>)`
type-former (`chan T`) plus three new expression forms, wired through
every pass the same way the Eleventh update's `spawn`/`join` were.

*Why channels, and why now.* goal.md's own row 3 design table doesn't
just say "no deadlocks" — it says *how*: "remove blocking locks as a
primitive; concurrency = async messages only; shared-memory locks
opt-in, gated by a static lock-rank check," citing Pony's "no mutex
exists in the language" as the model. A first cut at a plain `Mutex<T>`
was the obvious next step after threads, but would have directly
contradicted that design (goal.md's own Rust comparison calls out
exactly this failure mode: "two mutexes locked in opposite order still
deadlock"). Channels are the design's actual prescribed default, so they
came first instead — a lock primitive, if one gets built later, should
be the opt-in, statically-gated exception the design describes, not the
default this update reaches for.

*Design, and why the channel handle isn't affine.* `chan T` creates an
unbounded, multi-producer multi-consumer queue (`ChannelInner` in
`interpreter.rs`: a `Mutex<VecDeque<Value>>` plus a `Condvar` so `recv`
blocks efficiently instead of spin-polling). Unlike `thread T`, a
channel handle is *not* affine — it's meant to be held by more than one
concurrent computation at once (whoever sends, whoever receives, and
there can be several of each), so every read of a `chan T` binding is
just a cheap `Arc::clone` to the same shared queue, the same "freely
copyable" treatment `&T` already gets. The actual ownership-transfer
moment isn't the handle, it's `send`'s *payload*: `ownership.rs` checks
it exactly like a call argument, so an affine `box`-typed value sent on
a channel is consumed the instant it's sent, and can never still be
touched by the sender afterward — that's what makes it sound for it to
cross to another concurrent computation, no new safety logic beyond
"touch like a call argument" required (same shape as the Eleventh
update's `spawn` reasoning). `chan` itself (the zero-argument
channel-creating expression) has no sub-expression to infer a payload
type from, unlike `box expr`/`spawn f(x)` — `typeck.rs` only accepts it
against an already-known `chan T` expected type (a `let` with an
explicit annotation), and reports a dedicated
`ChannelNeedsExplicitType` error everywhere else.

*The honest limitation, stated plainly rather than left implicit.*
`recv` is a genuine blocking wait — nothing here makes it non-blocking
or gives it a timeout. A well-typed Nirdosha program can still write
`recv(c)` on a channel nobody ever sends to and hang forever. That is a
**liveness** bug, not the **aliased-lock-order** failure goal.md's row 3
Rust comparison is specifically about, and channels *do* fully close off
that specific failure mode (there is no lock primitive in the language
at all to acquire out of order). But it means row 3's "Hard — proof by
construction" status isn't fully earned yet by this update alone — see
the row-by-row table's updated row 3 entry for the precise, narrower
claim this update actually supports. Full Pony-style proof-by-construction
would need non-blocking mailbox/actor dispatch (a behavior runs only
when a message is already available, so there's no wait primitive left
to hang on at all) — a materially bigger feature, not built here.

`examples/channels.nir` demonstrates the accepted program (a producer
sends two values, `main` receives both, matching the other examples'
loose style); `tests/channels.rs` (10 tests) covers the round trip,
FIFO ordering across multiple sends, a boxed payload surviving the
round trip, the channel handle itself being freely reusable by both
sides, the boxed-payload-reuse-after-send rejection, and four static
rejections (`chan` with no type hint, `chan` against a non-channel
annotation, `send`/`recv` on a non-channel value, sending the wrong
payload type). 107/107 tests pass; `cargo clippy --all-targets` is
clean.

**Thirteenth update:** `sandbox`/`stop` — the first slice of
`SANDBOXING.md`'s sandboxing extension ("layer 1": an affine handle
around a *real, separate OS process*, no isolation backend and no typed
IPC yet). Not one of goal.md's original ten rows; a new capability built
squarely on top of rows 1-3's existing ownership/concurrency machinery
— see `SANDBOXING.md` for the full design rationale, kept there rather
than duplicated here.

*What it is.* `sandbox worker(args)` launches `worker` as a genuinely
separate OS process — a fresh `nirdosha --sandbox-worker` invocation
that re-lexes/parses/typechecks its own copy of the source (written to
a fresh temp file at spawn time; there's no shared memory to hand a
parsed `Program` across a real process boundary the way a spawned
*thread* gets one) — and returns an affine `sandbox` handle. `stop`
terminates it (killing it if still running, harmless no-op if it had
already exited) and yields its OS exit code. No typed result channel
exists yet, so a sandboxed function must declare `-> unit` and take
only plain scalar parameters (`typeck.rs`'s new `SandboxFnMustReturnUnit`/
`SandboxArgMustBeScalar` gates, checked against the callee's *declared
signature*, not just what one call site happens to pass) — crossing a
real process boundary has no serialization story for `box`/`thread`/
`chan`/`sandbox` values yet (`SANDBOXING.md`'s layer 3).

*Deterministic teardown, actually backed by a real `Drop`.* This is the
one thing that makes this update different in kind from `thread`/`chan`:
those rely entirely on `ownership.rs`'s *static* proof for their safety
story, with no real cleanup obligation if a handle is silently dropped
(an unjoined thread just detaches; nothing bad happens). A leaked OS
process is a genuinely bad outcome, so `SandboxChild` (interpreter.rs)
carries a real Rust `Drop` impl that kills and reaps the child regardless
of whether `stop` was ever called — verified directly, not assumed: a
test spawns an infinite-loop sandbox, confirms the process exists via
`kill -0`, drops the handle without calling `stop`, and confirms the
process is gone. `stop` and `Drop` both call the same `stop()` method;
calling it twice (explicit `stop`, then the local binding's own drop) is
a harmless no-op on an already-reaped, already-deleted target, not a
tracked special case.

*A real bug caught by testing, not review.* `spawn_sandbox` originally
used `std::env::current_exe()` to find the binary to re-exec as — correct
for the real `nirdosha` CLI, but under `cargo test` that resolves to the
*test harness* binary, not `nirdosha`, so a sandboxed child would have
silently run the wrong program (visible as `error: Unrecognized option:
'sandbox-worker'` on stderr — libtest's own harness rejecting the flag).
A first draft of the test suite didn't check what the child actually
executed and would have passed anyway. Fixed with `Interpreter::
with_sandbox_exe`, an explicit override (defaulting to `current_exe()`)
that tests point at `CARGO_BIN_EXE_nirdosha`, the real, separately-built
binary Cargo exposes to integration tests — and the fix doubles as the
honest answer to a real embedding question: *any* host process other
than the `nirdosha` CLI itself (a language server, a test harness, a
future embedding) needs the same override, since `current_exe()` only
ever means "whatever binary is actually running." The fix didn't
initially reach every test, either: one test still called the plain
`run(src)` entry point (no override), and passed anyway on its own —
its assertion only checked that `stop` returns *some* exit code, and a
wrong-binary child happens to still be "running" long enough to get
killed most of the time. It only showed up as a real, intermittent
`cargo test` failure once every test file ran together in parallel
(the wrong-binary child, under contention, sometimes finished erroring
out *before* `stop`'s `try_wait` ran, so the "definitely still running"
assumption the test's own comment claimed was quietly false). Caught by
running the full suite repeatedly, not by re-reading the test — fixed
the same way, routed through `with_sandbox_exe`.

`examples/sandbox.nir` demonstrates the accepted program (spawn an
infinite-loop background task, `stop` it, observe the killed exit code
`-1`); `tests/sandbox.rs` (10 tests) covers a deterministic spawn+stop
round trip (an infinite-loop worker removes the "did it race" question
entirely), passing real scalar arguments across the process boundary
(verified by the sandboxed child actually printing them — real output,
not just "the program didn't error"), the drop-without-stop zombie-
prevention test described above, the double-stop backstop (both the
static `ownership.rs` proof and the dynamic `AlreadySandboxStopped`
runtime check), and four static rejections (spawning a builtin, a
non-`unit`-returning function, a non-scalar parameter, `stop` on a
non-sandbox value). 118/118 tests pass; `cargo clippy --all-targets` is
clean.

Explicit, honest limitations, beyond the ones `SANDBOXING.md` already
names: no isolation backend yet (a bare OS process, not a container or
microVM — that's layers 4-5); no wait-for-natural-completion primitive,
only kill (`stop` calling a still-running process a "kill", not a
"wait", is deliberate — see `SANDBOXING.md`'s decision on backend
ordering — but it means there's currently no way to observe a
sandboxed process's *graceful* exit code without the caller separately
burning wall-clock time first, the same "no timing primitive exists"
gap `chan`'s `recv` doesn't have this problem with since it's woken by
an event, not polled); `codegen.rs` doesn't support any of it
(interpreter-only, like every other concurrency primitive so far); the
`SandboxSpawnFailed` error path (temp-file-write or process-spawn
failure) is wired but not independently tested — hard to trigger
deterministically without an environment-fragile setup (e.g. an
unwritable temp dir), so it's exercised by code review and the type of
the error path existing, not by a dedicated test, and that gap is
recorded here rather than left silent.

---


What exists on disk right now, against `goal.md` §6's Phase 0 description
("narrow the core, draft the grammar") and §1's ten-row requirement table.
This is the first slice, not a finished language — read it next to
`GRAMMAR.md` for the formal grammar and its documented gaps.

## What runs today

Kept current, unlike the narrative "update" sections above (which are
left as history) — this tree and test count reflect the actual state of
the repo, checked, not aspirational.

```
compiler/
  Cargo.toml            depends on the z3 crate (real Z3, system libz3)
  src/
    token.rs        lexer — hand-written, single pass, structured LexError, Span: Hash
    ast.rs           AST types; Ty::bounds()/in_range shared by every pass; literal_value()
    parser.rs        recursive-descent, single-token lookahead, no backtracking
    interpreter.rs   tree-walking evaluator; Value::{Boxed,Ref,Thread,Channel,Sandbox}; dynamic Tier-2 checks
    typeck.rs        static type checker — runs before interpretation
    ownership.rs     static move-checker for box/&/spawn/join/send/recv/sandbox/stop
    refine.rs        interval-analysis Tier-1 prover (the pre-Z3 fallback)
    smt.rs           real Z3-backed Tier-1 prover (primary; condition narrowing)
    codegen.rs       LLVM IR emission + `clang` driver — real native binaries
    lib.rs           public run(src) -> Result<Value, String>
    main.rs          CLI: interpret (default), `build`, `emit-llvm`, hidden `--sandbox-worker`
  examples/
    hello.nir        functions, params, arithmetic, print
    factorial.nir    recursion, if/else-as-expression, return-in-branch unwind
    loop.nir         while, assignment/mutation, if-as-statement
    ownership.nir    box, move, consuming calls
    borrow.nir       shared borrows (&)
    threads.nir      spawn/join, thread <T>
    channels.nir     chan/send/recv
    sandbox.nir      sandbox/stop
  tests/
    basic.rs         32 tests — core language + typeck
    ownership.rs     22 tests — box/&/move-checking
    refine.rs        11 tests — interval-analysis proofs (and honest non-proofs)
    smt.rs           9 tests — SMT proofs, incl. the interval-vs-SMT flagship comparison
    codegen.rs       15 tests — real compiled binaries, run and compared to the interpreter
    concurrency.rs   9 tests — spawn/join round trips, race-freedom, panic/double-join backstops
    channels.rs      10 tests — send/recv round trips, FIFO order, payload race-freedom
    sandbox.rs       10 tests — real process spawn/stop, zombie-prevention-on-drop, static rejections
```

```
$ cd compiler && cargo run --quiet -- examples/factorial.nir
3628800
$ cargo run --quiet -- build examples/factorial.nir -o /tmp/factorial && /tmp/factorial
3628800
$ cargo test
test result: ok. 118 passed; 0 failed
```

## Row by row, against goal.md §1

| Row | Status |
|---|---|
| 1 — No GC, no `free()` | **Started, real content.** `box`/`*`, shared borrows (`&`), and a static move-checker (`ownership.rs`) — see the "Second" and "Third" update sections above for what's actually proved and what still isn't (no `&mut`, no `Drop` hook, no place expressions). Regions/bulk-arena allocation is still not started. |
| 2 — No data races | **Started, first implementation.** `spawn`/`join`, `thread <T>` (real OS threads under the hood — see the "Eleventh update" for the Java-virtual-threads framing) and `chan`/`send`/`recv` (see the "Twelfth update"). Race-freedom for both comes entirely from `ownership.rs` reusing its existing move-checker — `spawn`'s arguments and `join`'s handle for threads, `send`'s payload for channels — no new concurrency-specific safety logic exists either time. `codegen.rs` doesn't support any of it yet (interpreter-only, like `box`/`&` before their own codegen work). |
| 3 — No deadlocks | **Started, narrower than proof-by-construction — see the "Twelfth update" for the honest scope.** goal.md's own row 3 design says exactly this: default to async messages, keep shared-memory locks opt-in and gated. Channels are now that default (no `mutex`/`lock` primitive exists in the language, so the classic "two locks in opposite order" failure goal.md cites for Rust is genuinely not expressible) — but `recv` is a real blocking wait, so a well-typed Nirdosha program can still hang forever on a `recv` nobody `send`s to. That's a liveness bug, not the aliased-lock-order failure mode row 3's Pony comparison is about, but it means the *proof-by-construction* claim isn't fully earned yet — true Pony-style non-blocking mailbox dispatch would need actors/behaviors, not yet built. `thread <T>` handles being affine (forming a DAG by construction) still holds on its own, narrower terms. |
| 4 — No int/buffer overflow | **Started, and now backed by real SMT.** `Ty::in_range` + `check_ty` still catch everything dynamically (Tier 2, unchanged). Two static Tier-1 passes exist: `compiler/src/refine.rs` (interval analysis, built when this environment had no Z3) and `compiler/src/smt.rs` (real Z3 4.16, once the user installed it — see the "Fifth update" section below), the latter now the primary checker since it's strictly more capable. 68/68 tests pass, including a flagship test that runs the *same* program through both passes and confirms SMT proves something interval analysis structurally cannot (condition-based narrowing). Not wired to elide the interpreter's runtime check — see below for why. |
| 5 — Native speed | **Started, real native binaries.** `compiler/src/codegen.rs` emits textual LLVM IR and shells out to the system `clang` (LLVM 22) — see the "Sixth update" section below. Scoped to signed integers/bool/unit, no `box`/`&`/`*` yet. 78/78 tests pass; three genuine bugs were found and fixed by actually running compiled binaries, not by review — see below. |
| 6 — Learning curve | Grammar is small and keyword-heavy on purpose (`fn`/`let`/`return`/`if`/`else`/`while`, C-family operators) — no attempt yet to *measure* this against goal.md §7's proxy metrics (novice user study, Cognitive Dimensions score). Row 6 is aimed at, not yet verified. |
| 7 — LLM-friendly | The parser itself remains single-token-lookahead with no backtracking, everywhere — that claim still holds. **Now actually cross-checked** against an independent LALR(1) generator (`grammar_check/`, a real `lalrpop` build) — see the "Seventh update" below for what it found: a genuine, previously-undocumented ambiguity in the grammar-as-CFG (no statement separator, so `return x` / `-y` on separate lines could formally parse two ways), resolved deterministically by the parser's "always shift" behavior but never stated as a rule until this check surfaced it. `GRAMMAR.md` now states it explicitly. Still no benchmark suite / grammar-constrained decoder built. |
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
- **Superseded, kept for history:** this bullet used to say "no LLVM —
  codegen is a backend concern that shouldn't gate front-end iteration."
  A tree-walking interpreter was still the right first choice for
  validating the grammar and semantics quickly, and turned out to be the
  right thing to validate *against* too — every example that now compiles
  to a native binary was cross-checked against the interpreter's own
  output first. See the Sixth update for what codegen actually covers.

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
- **Non-blocking mailbox/actor dispatch** — the actual remaining gap for
  row 3's full proof-by-construction claim (see the Twelfth update):
  `recv` today is a genuine blocking wait, so a well-typed program can
  still hang on a message nobody sends. Pony's answer is that there's no
  user-facing blocking primitive at all — a behavior only runs once a
  message is already in its mailbox. Building that (not a lock primitive
  — see below for why that was deliberately *not* the next step) is
  materially bigger than `chan`/`send`/`recv` turned out to be.
- **A lock/mutex primitive, gated by a static lock-rank check** — this
  was the original candidate here, but goal.md's own row 3 design says
  shared-memory locks should be the *opt-in, gated exception*, not the
  default (channels are the default — see the Twelfth update for why
  a plain, ungated `Mutex<T>` was rejected as the next step). If this
  gets built later, the static lock-rank check is the part that actually
  keeps it from reopening "two locks in opposite order" deadlocks — a
  lock primitive without one would just be `chan`/`send`/`recv`'s
  regression, not real progress.
- **M:N thread scheduling** — replace `spawn`'s one-OS-thread-per-call
  backing with lightweight (green/virtual) threads multiplexed onto a
  fixed pool, without changing `spawn`/`join`'s observable semantics —
  the whole reason the API was framed after Java virtual threads in the
  Eleventh update. If actors/scheduling end up implemented as stackful
  coroutines multiplexed onto a few OS threads, a *cactus stack*
  (spaghetti stack — a tree-shaped call stack where many logical stacks
  share common ancestor frames) is a real, established technique for
  making that cheap at scale — now directly relevant, unlike when this
  note was first written (the interpreter was a plain recursive
  tree-walker on Rust's own call stack with no user-level threading to
  help; `spawn` still runs each call on the interpreter's ordinary Rust
  recursion, just on its own OS thread, so the note remains aspirational
  until an M:N scheduler is actually built). The non-blocking mailbox
  dispatch above would likely be built on the same scheduler.
- **`codegen.rs` support for `spawn`/`join`/`chan`/`send`/`recv`** —
  currently interpreter-only; needs an ABI decision for what a native
  `thread T`/`chan T` handle even is (an OS thread/queue handle again,
  or already the lightweight scheduler's unit) before it's worth
  building.
