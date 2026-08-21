# Nirdosha `transact` — durable, verified, compensating side effects

This is the Nirdosha-native answer to what Argus/Avalon/Atomos each built as
a whole distributed-OS-sized runtime (guardians, topactions, open nesting,
commit/abort handlers — see the design conversation this doc grew out of).
The finding that matters: none of that needs to become ten new keywords
here. Nirdosha's "external effect" already *is* a named function call
(`sandbox f(args)`, `connect(...)`, `tcp` send/recv) — so durable-transaction
support is a rollback-and-replay discipline layered over calls that already
exist, not a new subsystem. One keyword, five slots.

## What it brings to the table

**1. Names the actual failure mode, instead of assuming "no exception ==
success."** A network call returning `200 OK` is not the same fact as "the
business operation actually happened" — that's exactly the gap that
motivated this whole design. `verify` is a mandatory slot, not optional: you
cannot write a `transact` that skips inspecting whether `network` really
succeeded.

**2. Crash-recoverability without a `guardian`/`stable` type-former.**
Argus needed `stable` vs. `volatile` fields as a language-visible
distinction because *any* guardian state could need to survive a crash.
Nirdosha only needs one thing to survive a crash: "did `network` already
run for this `transact`, and what did it return." That's narrow enough to
be an interpreter-owned runtime service (an append-only log), invisible in
syntax — the same way `rand_seed` is a per-`Interpreter` field, not a
language construct. Row 6 (goal.md) stays honored: no notational cost until
a program actually uses `transact`.

**3. Retry and timeout as "fundamentally expected," not bolted on later.**
A network call that can time out or transiently fail is the normal case,
not the exception — so `retry`/`timeout` are part of the `network` slot's
own grammar, not a separate wrapper the programmer has to remember.

**4. Reuses the existing affine-call discipline instead of inventing
first-class functions.** Every slot takes exactly one named `call` —
`Expr::Call`-only, same restriction `spawn call`/`sandbox call` already
enforce. This was a real fork (see the conversation this doc grew out of):
true first-class function values were the other option, and were rejected
as a much bigger structural addition to the language (a whole new type
category) for no semantic gain over named slots.

## The construct, locked

```ebnf
stmt          ::= ... | transact_stmt

transact_stmt ::= "transact" "{"
                     "network"    ":" call ("retry" int_lit)? ("timeout" int_lit)?
                     "verify"     ":" call
                     "commit"     ":" call
                     ("compensate" ":" call)?
                     ("log"        ":" call)?
                   "}"
```

- Slot order is fixed (`network`, `verify`, `commit`, optional `compensate`,
  optional `log`) — no permutation parsing, keeps this LL(1) the same way
  every other fixed-arity form in this grammar already is.
- `network` and `verify` become implicit local bindings, typed exactly like
  a `let` would type them from the call's return type, visible to every
  slot after them (`verify: check(network)`, `log: write_log(amount,
  verify)`), scoped only inside the `transact` block.
- `verify`'s call must return `bool` — a new, specific `TypeErrorKind`
  (matching the precision of existing ones like `WrongIndexArity`,
  `SingularMatrix`), not a generic type mismatch.
- `transact { ... }` is itself a value: `true` if it committed, `false` if
  it compensated (or if there was no `compensate` slot and `verify` was
  `false`) — same block-value convention `if` already uses.
- `timeout N` is whole seconds, plain `int_lit`, no unit suffix — no new
  duration-literal lexer syntax invented here on purpose (see Decisions).

```nir
fn call_api(amount: i64) -> i64 { ... }          // network
fn check(resp: i64) -> bool { ... }              // verify
fn update_db(amount: i64) -> i64 { ... }         // commit
fn refund(amount: i64) -> i64 { ... }            // compensate
fn write_log(amount: i64, ok: bool) -> unit { ... }

fn checkout(amount: i64) -> bool {
    return transact {
        network:    call_api(amount) retry 3 timeout 5
        verify:     check(network)
        commit:     update_db(amount)
        compensate: refund(amount)
        log:        write_log(amount, verify)
    }
}
```

## Runtime protocol

1. Before `network` runs, the interpreter appends a pending record
   `{txn_id, site, args}` to the durability log (Layer 3+ — see below;
   Layer 1 has no log at all and this step is simply absent).
2. `network`'s call runs, retried up to `retry` times (default 1 — i.e. no
   retry) on a trap, aborting the whole attempt if `timeout` seconds elapse
   first. Its result is appended to the log **before** `verify` runs — this
   is the actual crash-safety boundary: a restart that finds "network ran,
   result X, no terminal marker" resumes at step 3, never re-invoking
   `network`.
3. `verify(network)` runs.
4. `verify == true` → `commit` runs. Success appends a terminal `committed`
   marker, then `log` runs (best-effort only — never logged to the
   durability log itself, never replayed). If `commit` traps, the record
   stays non-terminal; a restart retries **only `commit`**, never
   `network` again.
5. `verify == false` → `compensate` runs if present, then a terminal
   `compensated` marker, then `log`.

If `network` still traps after `retry` attempts (or times out), the whole
`transact` block traps and propagates — there is nothing to compensate,
since `network` never returned a result to act on.

## How we're going to get there — layers, not a syntax spec

Same discipline `SANDBOXING.md` already committed to: each layer ships,
gets its own tests and example, before the next starts. The full design
above is locked as the *target*; it is not the first patch.

1. **`transact` in-process, no durability, no retry/timeout.** Parser,
   typeck (implicit `network`/`verify` bindings, the `bool`-return check on
   `verify`), ownership (slots are ordinary calls — nothing new for
   `ownership.rs` to reason about, same as `SANDBOXING.md`'s observation
   that `spawn`/`chan` cost zero new checker machinery), interpreter
   execution of the five-step protocol minus steps that need the log.
   Proves the grammar and control flow are right before anything durable
   is at stake.
2. **`retry`/`timeout` on `network`.** Needs a real wall-clock read for the
   timeout — an explicit, called-out departure from the determinism story
   (`goal.md`'s determinism section only covers `rand_seed`'s RNG stream;
   this is new, unavoidable nondeterminism, same honesty `SANDBOXING.md`
   already gives "`recv` can block forever"). Retry re-invokes the exact
   same call expression with the same evaluated arguments; a trap is the
   only thing "retry" reacts to — `verify` returning `false` is not a
   retry condition, it's the compensate path.
3. **The durability log itself.** An append-only WAL owned by the
   `Interpreter` (or a real file-backed store — TBD when this layer is
   actually built, deliberately not fixed here). This is the narrow,
   concrete slice of `goal.md` row 10's aspirational `ledger.rs` this
   feature actually needs — not the full capability-manifest/provenance
   system.
4. **Crash replay.** On interpreter startup, scan the log for non-terminal
   records and resume each at the correct step (skip `network` if its
   result was already recorded; retry only `commit` if `network`
   succeeded but no terminal marker exists).
5. **Cross-process `network`/`commit`.** Once 1–4 are proven against
   in-process functions, let a slot's named function internally use
   `sandbox`/`connect`/`tcp` (already legal today — no grammar change
   needed, since the restriction is "one named `Expr::Call`," and what
   that function's body does is already unconstrained).

## Decisions

Resolved now rather than left open, matching `SANDBOXING.md`'s own
practice:

- **Slot operand is exactly `Expr::Call`, never a bare `sandbox`/`connect`
  expression directly.** Keeps `transact`'s own grammar simple and
  consistent with `spawn`/`sandbox`'s existing restriction; a slot that
  needs a real network/process effect wraps it in a named function, which
  every worked example above already does.
- **Retry only reacts to a trap, never to `verify == false`.** Conflating
  "the call itself failed" with "the call succeeded but the business
  outcome was negative" is exactly the bug this whole feature exists to
  prevent — collapsing them back together in the retry logic would
  reintroduce it one layer down.
- **`commit`'s and `compensate`'s and `log`'s return types are
  unconstrained and discarded** — same treatment `expr_stmt` already gives
  a bare expression statement, not a new rule.
- **No duration-literal syntax (`5s`) invented for `timeout`** — plain
  `int_lit` seconds. A real duration literal is future work if/when it's
  needed elsewhere too, not a one-off invented here.
- **Durability is an interpreter-owned runtime service, not a `stable`/
  `guardian` type-former.** Narrower surface, zero new type-checking
  machinery, and it's the only thing Nirdosha's version of "guardian
  state" actually needs (whether `network` already ran) — unlike Argus,
  which needed the distinction on arbitrary user fields.
- **Compiled backend (`codegen.rs`) is out of scope until the interpreter
  version is proven** — same "reject, don't mis-compile" treatment every
  other unimplemented construct gets today (`box`, `thread`, `chan`,
  `sandbox`, `tcp` are all still interpreter-only per `LANGUAGE.md` §10;
  `transact` joins that list, not an exception to it).
