---
state: draft
shepherd: (unassigned)
---

# RFC 0006: Structured concurrency for native threads — Pillars 1-4

## Motivation

Two documents landed proposing how Nirdosha should close its remaining
concurrency gap, and they are not proposing the same thing:

1. **A "Thread/Channel/Sandbox" methodology brief** — nine phases of
   design questions and adversarial tests for adding native
   concurrency while preserving Nirdosha's guarantees. Most of its
   Phase 1 ownership questions ("can a `box` cross a thread boundary,"
   "can the sender access it after") are **already answered, shipped,
   and working** in `ownership.rs`/`interpreter.rs` today — `box` is
   affine, `spawn` moves it, `thread` is affine and `join`-consumed
   once. This document is best read as a **verification methodology**
   (its Phases 5-9 especially), not a from-scratch design.
2. **`nirdosha_concurrency_spec.md`** — a genuine redesign proposal:
   capability-typed boxes (`iso`/`froze`/`lend`), non-blocking
   mailbox-based `send`, structured `scope` blocks replacing bare
   `spawn`, and (Pillar 5, reserved for a later version) a real,
   compile-time deadlock-freedom proof via lexical scope levels,
   correctly citing real prior art (Pony, Erlang/OTP, Go, Trio) and a
   real 2026 OOPSLA result (Fowler & Hu, on deadlock-freedom not
   composing in actor systems).

**The gap worth naming plainly**: `README.md`'s own comparison table
claims "Deadlock freedom: No mutex primitive exists at all." True, but
incomplete — a mutex-free circular wait is still fully constructible
today (two threads each blocked on `recv` from a channel the other is
supposed to send on first). Closing that gap for real is exactly what
`nirdosha_concurrency_spec.md`'s Pillar 5 targets; nothing in Nirdosha's
shipped design does yet.

Per the user's own direction, this RFC does not adopt either document
on paper. It builds a standalone evidence prototype of Pillars 1-4
first (`rfcs/evidence/0006-structured-concurrency/`), runs the brief's
own adversarial test list against it for real, and reports what
actually happened — including one assumption in the brief's own test
list that turned out to be wrong when checked.

## Design

### 0. What Nirdosha has today (the actual starting point)

- `box T` — affine, moved on use. `spawn worker(h)` moves it; the
  sender cannot reference it again (a real `UseAfterMove` compile
  error).
- `thread T` — affine; `join` consumes it exactly once.
- `chan T` — the *handle* is deliberately the one freely-copyable
  concurrency type (many holders); the *payload* is where ownership
  transfer happens, matching R1-R4 in `nirdosha_concurrency_spec.md`
  already, by coincidence of a sound design rather than by having read
  that document.
- No mutex/lock primitive exists in the grammar — already true, already
  the README's headline claim, and preserved by every option
  considered below.
- Not yet true: full deadlock-freedom (the gap above), and native
  compilation of any of this (`Ty::Thread`/`Ty::Channel`/`Ty::Sandbox`
  are still hard `codegen.rs` rejections — see
  `rfcs/0005-plugin-boundary-safety-and-performance.md` §0's own
  difficulty ranking, which already named this the one genuinely hard,
  foundational remaining Track B item).

### 1. The prototype, and the central finding

`rfcs/evidence/0006-structured-concurrency/concurrency_proto/` — a
standalone Rust crate (own `[workspace]`, same isolation reasoning as
`crates/runtime-kernels/`), **not** a change to Nirdosha's real
grammar/typechecker. It implements Pillars 1-4's runtime mechanics in
plain Rust and runs the brief's Phase 6 adversarial list against it.

**The finding that matters most**: Rust's own ownership/move semantics
and standard library already provide most of Pillars 1-4 natively.

- `Iso<T>` needed no enforcement code of its own — a non-`Copy` value
  moved into a channel `send` is already R4 ("sending `iso` is a move;
  the compiler marks the source binding unusable"), for free, via
  `rustc`'s own borrow checker.
- `Froze<T>` is `Arc<T>` with no `DerefMut` — R3's "shared, immutable,
  any number of threads" exactly.
- Non-blocking `send` / bounded `try_send` (Pillar 2) is
  `crossbeam_channel::unbounded`/`bounded` — mature, existing library
  code, not invented.
- Structured concurrency (Pillar 4's "no orphan threads," C1) is
  `std::thread::scope` — stable since Rust 1.63, and it *already*
  refuses to let a spawned thread (or anything borrowing scope data)
  outlive the scope, and already propagates a child's panic to the
  caller.

This means the prototype's own runtime mechanics were low-risk to
build — they're thin wrappers over proven Rust primitives. **What this
does not prove**: that Nirdosha's own compiler can cheaply gain
equivalent primitives. Nirdosha has no Rust-style borrow checker or
`std::thread::scope` of its own; `ownership.rs`'s affine tracking would
need real, new work to express `froze`/`lend` (see §3), and actually
running any of this from *compiled* `.nir` code means either linking a
Rust runtime that does what this prototype does (a materially bigger
linked-kernel effort than `tcp`/`file`/`dec128` combined) or hand-rolling
these primitives from raw OS threading — neither attempted here.

### 2. Adversarial results (the brief's own Phase 6 list, run for real)

19/19 tests pass — full source in
`rfcs/evidence/0006-structured-concurrency/concurrency_proto/tests/adversarial.rs`.
Classified per the brief's own Phase 6 scheme:

| Case | Classification |
|---|---|
| Double send | **Compile-time rejection** — real `E0382`, verified by actually compiling the snippet (`counterexamples/verify_double_send.rs`) |
| Use-after-send | Same `E0382` — identical mechanism to double send, not a separate bug |
| Double receive | Runtime-safe (not a bug) — a mailbox is a queue; verified the *empty, still-open* case blocks rather than panicking |
| Dangling references | **Compile-time rejection** — real `E0597`, verified (`counterexamples/verify_dangling_ref.rs`) |
| Channel closure during send | Runtime-safe failure — a real `Err` with the payload returned, never a panic |
| Channel closure during receive | Runtime-safe failure — a real `Closed` result; buffered messages drain before it appears |
| Thread termination during ownership transfer | Runtime-safe — a value is queue-owned the instant `send` returns; the sender's later lifetime is irrelevant |
| Multiple producers | Runtime-safe, by design (real test: 8 producers × 100 messages, all 800 accounted for) |
| Multiple consumers | Runtime-safe, by design — a real, disclosed choice *beyond* the brief's own model: `crossbeam_channel`'s receiver is cloneable and multi-consumer-safe, unlike `std::sync::mpsc` |
| Nested thread spawning | Runtime-safe — inner scope provably joins before the outer statement after it runs |
| Large ownership transfers | Runtime-safe, content verified byte-for-byte after a 1MB transfer |
| Rapid thread creation/destruction | Stable across 2,000 iterations |
| Resource exhaustion | Deliberately bounded (10,000 concurrent mailboxes), not actually exhausted — a shared dev machine, disclosed rather than implied as a full stress test |
| Panic while owning a value | Runtime panic, **propagated** through the scope, not swallowed or UB |
| Panic while sending a value | N/A as literally stated (`send` itself cannot panic in this design) — the adjacent real question ("does an already-sent message survive the sender then panicking") is real and checked: yes |

**A genuine surprise, found by actually running the test, not assumed
in advance**: the brief's Phase 7 classic deadlock — "A sends on
channel1 then receives on channel2; B sends on channel2 then receives
on channel1" — taken completely literally, **does not reproduce**
under Pillars 1-4. The first version of this test asserted it would
hang and failed within milliseconds. Traced to the reason: Pillar 2
makes `send` unconditional — it never waits for a receiver, so neither
thread's `receive` ever depends on the other thread reaching some later
statement first; both sends have already landed before either
`receive` call even begins. A cycle of blocking waits requires a
blocking operation to depend on something gated behind *another*
blocking operation; two already-fired, independent sends don't create
that. **This is a real correction the brief's own Phase 7 test list
should carry forward**, not a flaw in this RFC's evidence.

The actual class of deadlock Pillar 5 exists for is **nested
reply-obligation**, constructed and verified separately
(`nested_reply_obligation_deadlocks_without_pillar_5`): A sends a
request to B and parks in exactly one blocking `receive` for B's
*reply*. B, to compute that reply, needs an answer from A — but A is
not running any code that could ever service that request (it's
blocked, waiting only on the reply channel). This **does hang** (real,
verified, bounded-timeout-checked). This is the concrete, honest
confirmation of what `nirdosha_concurrency_spec.md`'s own "what we are
NOT claiming" section already says: Pillars 1-4 alone do not deliver
deadlock-freedom; Pillar 5's level-checking (R5.3: a reply-obligation
may only target a strictly higher level) exists specifically for this
shape, not the simpler one the brief's own example describes.

### 3. Real benchmark numbers (Phase 8)

i7-8550U, Linux 7.0.10-zen1, best of 3 (`concurrency_proto/src/bench.rs`):

| | ns/iter |
|---|---:|
| Raw `std::thread::spawn` + join, no payload | 44,278 |
| `std::thread::scope` + one child, no payload | 34,859 |
| Mailbox creation | 105 |
| Send + receive round trip, same thread | 47.5 |
| `Iso<i64>` transfer (8 bytes) | 40.8 |
| `Iso<Vec<u8>>` transfer (64 MB) | 7,017 *(includes one real clone — see caveat below)* |
| 4 producers → 1 consumer, cross-thread, 1000 msgs (incl. thread spawn) | 93,062 |

**Structured concurrency costs nothing extra over raw threads** —
`std::thread::scope` (34,859 ns) is not slower than bare
`std::thread::spawn` (44,278 ns) in this measurement; the "structure"
is a compile-time/API guarantee, not a runtime wrapper, exactly the
claim C1 makes.

**Honest caveat on the 64MB row**: that number includes one real
`Vec<u8>::clone()` (a genuine 64MB allocation + memcpy) inside the
timed loop, because the benchmark harness needs a fresh value to move
each iteration. It is *not* a clean measurement of pure transfer cost.
The actual claim ("moving a `Vec<u8>` through a channel is O(1)
regardless of size") doesn't need its own benchmark to establish —
it's a basic, structural fact about what a Rust `Vec` *is* (a
`(ptr, len, cap)` triple; moving it never touches the heap-allocated
bytes), the same kind of "well-established, not re-derived" fact this
project already treats `Arc::clone`'s O(1) cost as
(`rfcs/0005-plugin-boundary-safety-and-performance.md` §2's own note).

## Critic (self-review — the two questions only: less safe? less fast?)

- **This prototype proves the runtime mechanics are sound; it does not
  prove Nirdosha's compiler can cheaply gain them.** Every primitive
  here is a thin wrapper over `std::thread::scope`/`crossbeam_channel`
  — mature Rust code Nirdosha's own compiler has no equivalent of.
  Compiling any of this from `.nir` source means either linking a real
  Rust concurrency runtime (a bigger "linked kernel" than `tcp`/`file`/
  `dec128` combined — those wrap a handful of syscalls or a pure
  arithmetic library; this would wrap an entire scheduler-adjacent
  runtime) or hand-rolling structured concurrency from raw OS threads
  inside `codegen.rs` — neither designed here. **Overclaiming "Pillars
  1-4 are basically done" from this prototype would be a real
  mistake.**
- **`lend`'s actual semantics were not tested.** This prototype only
  checked that an *unbounded* reference can't cross a channel (trivial
  — `rustc` already rejects any non-`'static` borrow in that position
  for any reason). `nirdosha_concurrency_spec.md`'s real `lend` claim
  is narrower and harder: a reference "bound to the lifetime of the
  owner's scope," held by another thread *only while the owner is
  suspended at a defined rendezvous." That's a genuinely different,
  unimplemented, and unverified mechanism — disclosed here as a real
  gap, not glossed over.
- **The one deadlock test that hangs is one hand-constructed protocol
  shape, not a general proof.** It's a real, representative instance of
  the "nested reply-obligation" class Pillar 5 targets, but this RFC
  does not claim to have enumerated every deadlock shape Pillars 1-4
  fail to prevent.
- **The brief's own Phase 7 test list needed a real correction**,
  found only by actually running it: its literal "two independent
  channels" deadlock example does not reproduce under Pillar 2. Anyone
  treating that document as a ready-to-run acceptance suite should
  know its examples aren't all correct as stated.
- **Multiple-consumer support is a real, disclosed *departure*** from
  `nirdosha_concurrency_spec.md`'s own model (which never specifies
  multi-consumer mailboxes) — chosen here because `crossbeam_channel`
  makes it free and it strengthens S3's "round-robin among mailboxes"
  fairness story, but it's an addition this RFC is making, not
  something the source document asked for.

## Recommendation

- **Adopt Pillars 1-4 in principle.** They're a sound, correctly-
  grounded synthesis of real prior art, and this pass replaces "sounds
  right on paper" with "19 real adversarial tests pass, two real
  compile errors verified, one wrong assumption caught and corrected."
- **Do not treat this prototype as most of the implementation work.**
  The hard, unbuilt part is making any of this run from *compiled*
  `.nir` code and extending `ownership.rs` with real `froze`/`lend`
  capability tracking — both bigger than anything shipped so far this
  cycle (`Ty::Handle`, `file`, native plugin calls, `dec128`).
- **Defer Pillar 5 exactly as its own spec stages it** — syntax
  reserved, not shipped in v1. Revisit once real Pillars-1-4 usage
  shows whether the tree-mediation constraint it would impose is
  actually livable.
- **A real, considered alternative this RFC does not recommend, but
  names honestly**: ship native codegen for `spawn`/`thread`/`chan`
  *exactly as they exist today*, without adopting the capability model
  at all. Smaller, non-breaking, and would close Track B's concurrency
  gap sooner — at the cost of leaving the deadlock-freedom question
  (and the README's slightly-overclaimed comparison-table row)
  unaddressed indefinitely. See Rejected Alternatives.

## Effect on the permission model

None of Pillars 1-4 touch `requires(role/claim:...)`/`effect(...)`.
`Effect::Concurrent` already exists and already tags `spawn`; a future
`scope`/`froze`/`lend` surface would need the same tagging, unchanged
in kind.

## Compatibility

**This is not additive — flagged plainly, unlike every other change
this cycle.** Adopting Pillars 1-4 as designed would:

- Replace bare `spawn` with structured `scope { }` blocks (or require
  retrofitting `spawn`/`join` with equivalent structured guarantees —
  an open question below).
- Change `send`'s contract from (today, implicitly, via `chan`) a
  synchronous handoff to Pillar 2's explicit non-blocking mailbox
  semantics.
- Introduce two new type-level concepts (`froze`, `lend`) alongside
  `box`'s existing affine (`iso`-equivalent) behavior.
- Break every existing concurrency example (`examples/threads.nir`,
  `channels.nir`, `sandbox_channels.nir`) and
  `examples/comparison/01-concurrent-counter.md`, which would all need
  rewriting against the new model.

This is exactly the "cross-cutting, breaking, shapes the language
surface" category `GOVERNANCE.md`'s RFC process exists for — nothing
here should land without a shepherd's sign-off and real discussion,
unlike the additive fixes in RFC 0005.

## Rejected alternatives

- **Ship native codegen for today's `spawn`/`chan` unchanged, skip the
  capability redesign entirely.** Not rejected outright — named above
  as a real, smaller alternative — but not recommended as the
  destination: it would leave the deadlock-freedom gap open
  indefinitely and require a second, later migration if Pillars 1-4
  are ever adopted anyway.
- **Implement Pillar 5 now, alongside 1-4.** Rejected for this pass:
  `nirdosha_concurrency_spec.md`'s own staging already defers it, and
  this prototype's evidence (a real, if narrow, deadlock class Pillars
  1-4 don't prevent) is exactly the kind of finding that should inform
  Pillar 5's eventual design, not be skipped past.
- **A `Box<dyn Any>`-style generic capability wrapper instead of
  distinct `Iso`/`Froze` types.** Not attempted: the whole value of
  `Iso<T>`/`Froze<T>` as *distinct* types is that `rustc` (and,
  eventually, `ownership.rs`) can tell them apart statically — an
  `Any`-erased wrapper would need runtime tag checks exactly where this
  prototype currently needs none.

## Open questions

- **`scope` vs. retrofitted `spawn`**: does Nirdosha need a new
  keyword/grammar form, or can `spawn`/`join`'s existing shape be given
  Pillar 4's guarantees without new syntax? Not resolved here.
- **`froze`/`lend` surface syntax and `ownership.rs` integration**: real
  language-surface design, not attempted.
- **Native compilation strategy**: link a Rust concurrency runtime
  (bigger scope than any linked kernel shipped so far) vs. hand-rolled
  OS-thread codegen — a real Track-B-scale question, explicitly out of
  scope for this RFC.
- **Whether `nirdosha_concurrency_spec.md`'s own Phase 7 deadlock
  examples should be corrected upstream** given this RFC's finding that
  one of them doesn't reproduce as stated.
