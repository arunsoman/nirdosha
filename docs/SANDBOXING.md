# Nirdosha sandboxing — what it brings, and our approach

This is not the ProtoBox PRD (`../Nirdosha_Sandboxing_PRD.md`, kept as reference,
not adopted wholesale). That document is a good prompt — the core instinct
(sandbox handles as affine, ownership-tracked resources) is exactly right and
worth keeping — but it's written in aspirational future-Nirdosha syntax
(structs, enums, traits, generics, an effects system) none of which exists
yet, and it invents a second, incompatible channel primitive alongside the
`chan T` this project already shipped. This document says what the *idea*
buys us, independent of that syntax, and how we intend to get there without
duplicating work already done.

## What it brings to the table

**1. Sandboxes as a third tier of the safety story, not a bolt-on.**
Rows 1–3 of `docs/goal.md` are all versions of the same claim: the type system,
not discipline, owns a resource's lifecycle and a computation's isolation.
`box` does this for memory. `thread T`/`chan T` do it for concurrent
computation. A `box Container`/`box MicroVM` extends the *identical*
mechanism — deterministic, ownership-driven teardown, no new checker logic
— to a resource that happens to be a whole isolated process or VM instead
of a heap allocation. That's not a new capability bolted onto Nirdosha; it's
the same proof, applied one level up. Nothing about `ownership.rs` needs to
change to say this — it already treats affine handles as opaque.

**2. A real, narrower-than-it-sounds interop story.** What this actually
buys is *orchestration-level* interop, not in-process FFI: spawn a
workload written in any language, unmodified, inside a container or
microVM; talk to it over a typed channel. The foreign code needs a small
protocol-speaking shim, and every round trip pays a real IPC cost — this is
not "call a Python function inline." What it *is*: a way to bring an
existing polyglot codebase or a third-party binary under Nirdosha's
ownership/isolation discipline as a supervised, hardware-isolated neighbor,
without a line of it ever being ported to `.nir`. That is genuinely useful
and well-precedented (it's roughly how Firecracker-based FaaS platforms and
WASM sandboxes already do polyglot isolation) — we should market it as
exactly that, not oversell it as universal interop.

**3. A differentiated niche, not another item on a crowded list.**
"Systems language with ownership" competes head-on with Rust. "Systems
language where sandbox lifecycle is compiler-enforced, not
library-and-discipline" is a much smaller field — Rust gets you there with
a `Drop` impl you have to write correctly yourself; Nirdosha would get you
there from the same checker that already refuses to compile a use-after-move
`box`. Worth treating as a real positioning angle, not just a feature.

**4. Hardware isolation as the honest answer to "what about untrusted
code."** Everything rows 1–4 prove is a proof *about well-typed Nirdosha
programs*. It says nothing about code Nirdosha doesn't control — a
third-party binary, a different language's runtime, a tenant's own upload.
MicroVMs are the honest mechanism for that gap: hardware-enforced isolation
where the type system's writ doesn't reach. Framed this way, sandboxing
isn't a competing safety story to rows 1–4, it's what covers the boundary
where they necessarily stop.

## What we're keeping from the PRD, and what we're not

**Keeping:** affine sandbox handles with scope-based teardown; microVM as
the primary/preferred isolation primitive (Firecracker's boot-time and
density numbers are real and the right target); the general threat-model
framing (hardware VT-x isolation, seccomp on the VMM, separate guest
kernel).

**Not keeping as-is:** the PRD's `Channel<T>` is a *separate*,
single-use-per-direction primitive (`send(self, ...)` and `recv(self)`
both consume the channel). We already shipped `chan T` this session — an
unbounded, reusable, multi-message queue with a non-affine handle, race-
freedom proved by consuming the *payload* at `send`, not the handle.
**Decision:** one primitive, multiple transports — give `ChannelInner`
a transport abstraction behind the same `Value`-level `send`/`recv`
semantics: today's in-process backing (`Mutex<VecDeque<Value>>` +
`Condvar`) is transport #1, a unix-socket/vsock-backed transport for
cross-process sandbox IPC is transport #2. Not a second, incompatible
channel concept. If a real use case needs single-use request/response
semantics, that's a constraint expressed on top of `chan T` later (a
convention, or a distinct affine wrapper), not a reason to fork the
primitive.

**Deferred, not rejected:** refinement-typed channel payloads (needs
refinement types, which don't exist). See "Decisions" below for the
effects-marker and error-model questions — those are resolved now, not
left open.

## How we're going to get there — layers, not a syntax spec

Deliberately not specifying struct layouts, method signatures, or exact
keywords here — that's real design work we do together when we're ready to
build each layer, not something to freeze in a vision document. What's
worth fixing now is the *order*, because each layer should be buildable and
testable on its own, the same discipline every feature this project has
shipped so far has followed (implement → test → example → document →
commit, one coherent slice at a time):

1. **Done — `sandbox`/`stop` (docs/PHASE0.md's "Thirteenth update").** An
   affine handle around a bare OS child process (`std::process::Command`,
   re-execing the `nirdosha` binary itself as a `--sandbox-worker`), no
   isolation, no IPC. Deterministic teardown is real, not aspirational: a
   `SandboxChild`'s own Rust `Drop` impl kills and reaps the process even
   if a program never calls `stop`, verified directly (spawn an infinite-
   loop sandbox, confirm it's alive via `kill -0`, drop the handle,
   confirm it's gone) rather than assumed from the ownership proof alone.
   One real bug found by testing, not review: the child re-exec used
   `std::env::current_exe()`, which resolves to whatever binary is
   actually running this interpreter — correct for the real `nirdosha`
   CLI, silently wrong under `cargo test` (resolves to the test harness).
   Fixed with an explicit override (`Interpreter::with_sandbox_exe`),
   which is also the honest answer for any *other* host process embedding
   this interpreter later, not just a test-only workaround. See docs/PHASE0.md
   for the full writeup and `examples/sandbox.nir`/`tests/sandbox.rs` for
   what's actually covered, including what deliberately isn't yet (no
   wait-for-natural-completion, only kill; no isolation backend).
2. **Done — `chan T` over a real cross-process transport (docs/PHASE0.md's
   "Fourteenth update").** Built exactly as decided above: `ChannelInner`
   gained a `TransportState` (in-memory, unchanged; or a Unix domain
   socket), not a second primitive. A `chan`-typed `sandbox` argument now
   gives the spawned process a genuine live channel back to the parent —
   `send`/`recv` work identically on both sides of the process boundary,
   same syntax, same `Ty::Channel`. The one real blocking step (accepting
   the child's connection) is deferred to the first actual `send`/`recv`,
   so spawning stays non-blocking either way. New, honest failure mode:
   `ErrorKind::ChannelIoError` when the socket transport's I/O itself
   fails (the peer died) — impossible for the in-process transport, so
   this is new territory, not a gap in the old one. Two deliberate scope
   limits, both enforced with real errors, not just documented: a channel
   can only cross into `sandbox` while empty (no replay of already-queued
   messages), and only into *one* sandboxed process, ever. See
   docs/PHASE0.md, `examples/sandbox_channels.nir`, `tests/sandbox_channels.rs`
   for what's covered.
3. **A typed serialization boundary.** Bytes in, bytes out, checked
   against the declared payload type at both ends — no refinement-witness
   propagation until refinement types themselves exist.
4. **Docker as the first real isolation backend.** Once 1–3 are proven
   against a bare process, swap the backend for a namespace-isolated
   container — the handle/channel shape shouldn't need to change, which
   is itself the test that the abstraction was cut in the right place.
5. **Firecracker last**, once the shape has survived two backend swaps
   already (process → Docker) and we're not simultaneously debugging the
   abstraction *and* a VMM. The PRD's own comparison table is right that
   microVMs are strictly more engineering for strictly more isolation —
   we just don't want to pay that cost while still finding bugs in the
   ownership/channel plumbing itself.
6. **Effects, snapshot/restore, quotas, multi-tenant density.** All real,
   all later — each is additive once 1–5 exist and none of them changes
   the shape of what's below it.

**2b. Done — `str`/`tcp`/`connect` (docs/PHASE0.md's "Fifteenth update"), not
one of the six layers above, added out of sequence.** Working through
layer 2 surfaced a real gap in the plan itself: every layer through 2
only ever spawns *another copy of the `nirdosha` binary*, so there was
never a need to name an external thing or speak a foreign protocol. The
concrete goal this whole document opens with — orchestrate *any*
containerized workload, a real Spring Boot app was the example given —
needs both, and neither existed. `str` (minimal UTF-8 literals, just
enough to name a host or an image) and `tcp`/`connect` (a real, raw TCP
client, reusing `send`/`recv`/`stop` rather than inventing new keywords)
close that gap. Verified against something genuinely external and
unaware of Nirdosha: a raw HTTP GET over `connect` to this machine's
already-running Neo4j Docker container, real JSON back. This is now the
actual prerequisite for layer 4 (a `sandbox` variant that launches a
pre-existing image by name, rather than one of the program's own
functions) — see docs/PHASE0.md for what's still missing before that's
buildable (`is_sandbox_safe` doesn't accept `str`/`tcp` yet; that's the
next concrete step, not yet started).

## Decisions

Resolved now rather than left open, so the plan above has a fixed
foundation to build against:

- **A sandbox gets its own structured error family, not a reuse of
  `ThreadPanicked`.** A spawned thread panicking is an in-language Rust
  unwind caught at `join()`; a sandboxed guest failing is categorically
  different — it can exit non-zero, fail to boot, or simply never
  respond. That last case is the same "`recv` can block forever" gap
  already documented for `chan T` generally (see `docs/PHASE0.md`'s row 3
  entry), except sharper here: an untrusted guest hanging is far more
  likely than a bug in our own spawned function. New variants (exited /
  failed-to-start / channel-closed, shape TBD when we build this layer),
  same "checker is the real gate, this is the backstop" discipline as
  `ThreadPanicked`/`AlreadyJoined` — and this is the case that actually
  justifies giving sandbox `recv` a timeout, even before ordinary `chan T`
  gets one.
- **No bespoke "spawns a sandbox" effect marker before the real effects
  system.** `spawn`/`chan` could ship years ahead of docs/goal.md's own phase
  plan because they cost zero new checker machinery — pure reuse of
  `ownership.rs`. An effect marker is the opposite: an entirely new
  checking discipline with nothing to reuse. A narrow one-off version now
  would be either real unchecked-adjacent machinery, or something that
  merely *looks* checked without being proven — and this project's
  standing discipline is "either it's really proven, or the doc says
  plainly it isn't" (see every `docs/PHASE0.md` row-status entry). Treat "can
  this function spawn a sandbox" the same as "can this function spawn a
  thread" today: an unchecked fact about the implementation, until the
  real effects system exists and covers it for free as one more case,
  not a special one.
- **The "any tech stack" claim is always paired with its qualifier,
  everywhere it's said publicly.** Never "any language" alone — always
  "any language, as an isolated message-passing neighbor; the sandboxed
  side needs to speak our wire protocol." The actual fix for the honesty
  gap isn't wording, though — it's a first-party shim/SDK for a couple of
  common languages (Python, Node are the obvious first two) so "no
  porting required" gets much closer to literally true for the common
  case, instead of every user hand-rolling a protocol client. Worth
  scoping as its own small deliverable once layer 2 (the real transport)
  exists to write a shim against.
- **Backend order is process → Docker → Firecracker** (see the layered
  plan above), not "container vs. microVM first" as a binary choice —
  there's a cheaper, more honest first step than either.
