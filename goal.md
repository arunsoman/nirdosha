# Nirdosha (निर्दोष — "without fault")

> Research brief · Language design · 19 Aug 2026

**Goal:** a language with no garbage collector, no data races, no deadlocks,
no integer or buffer overflow, and hardware-native speed — that is *also*
easy to learn, logical enough to reason about that complex operations
visibly compose from a small set of fundamental building blocks, easy for an
LLM to write and reason about, built around AI as a first-class programmer
rather than a guest typing into a text box, and produces binaries where any
code that didn't come from the attested source is detectable, not just
"hard to hack." Ten requirements, one design — not a safety core with
usability requirements bolted on afterward. This revision folds what was
previously a "secondary" wishlist into the same requirement set the safety
properties belong to, because none of the ten actually stands independently
of the others (§1, §7).

> **Honest correction (added during the unified development plan's Phase
> 5, §4.5.3 — see `Nirdosha_Unified_Plan.md`):** this brief references
> `src/capability.rs` and `src/ledger.rs` as already existing in this
> codebase (§1 row 10, §5, §8) — an exhaustive repo search found no such
> files. Row 10's full ambition (reproducible builds, content-addressed
> source, capability manifests enforced at the kernel boundary, a signed
> provenance chain) remains **aspirational**, not built, and should be
> read that way everywhere it's mentioned below. What *is* real and
> checkable today is a narrower, concrete slice: `rand_seed`/`rand_f64`/
> `rand_gaussian` (Phase 3) make a simulation run's random draws
> byte-for-byte reproducible from a seed — no OS entropy, no hidden
> global state (see `interpreter.rs`'s `Interpreter::rng` field and
> `compiler/tests/mission_critical.rs`'s determinism tests). That's the
> honest foundation a future row-10 implementation pass would extend,
> not a claim that row 10 itself is done.

> **Row 11, added 21 Aug 2026** (see `nirdosha_row11_amendment.md`, itself
> checked against `PROTOLANG_PORT.md`'s porting exercise): the ten rows
> above are necessary but were not sufficient — Nirdosha had no product
> types, sum types, or generics, and three "Blocked" verdicts plus four
> "Rejected" ones in the porting exercise traced back to exactly that one
> missing layer, not to Rice's theorem or to any of rows 1–10. Row 11 is
> now part of the requirement set (§1's table, §3's synthesis layer, §6's
> phase plan, §7's formalization all updated below) — narrowly scoped
> (`struct`/`enum`/`match`, no traits, no HKTs, no subtyping) so it doesn't
> become the research project rows 1–10 already warn against.
>
> **Update, 21 Aug 2026:** row 11 has actually shipped, all the way
> through the items it was meant to unlock — `struct`/`enum`/`match`,
> generics on a declaration with real structural-per-instantiation type
> identity (`Pair(i64, str)` and `Pair(f64, bool)` are different,
> unrelated types — no monomorphizer pass exists or is needed), and
> `Option(T)`/`Result(T, E)` themselves as ordinary generic `enum`s
> injected into every program, with affinity propagating through struct/
> enum fields (including through a generic instantiation's own concrete
> type arguments) the same way it already does through `box`
> (`compiler/src/typeck.rs`, `ownership.rs`, `interpreter.rs`;
> `compiler/tests/structs_enums.rs`, `compiler/tests/generics.rs`;
> `nirdosha_row11_amendment.md`'s §3.6 layers 1–4, 6–7). Only layer 5
> (extending `refine.rs`/`smt.rs`'s static-proof boundary set — the
> Tier-1 bonus prover, not required for correctness) is still open.
> Codegen doesn't compile any of it — `struct`/`enum`/`match` join the
> existing interpreter-only list (`box`/`thread`/`chan`/`sandbox`/`tcp`/
> `str`), rejected explicitly, not silently mis-compiled; a program that
> never actually constructs/matches one still compiles normally, since
> the `Option`/`Result` prelude's mere presence isn't itself a use.

---

## 0. The constraint that shapes everything

Before any design: one theorem sets the ceiling here, so it's worth stating
plainly rather than discovering it three years in.

**Rice's theorem** (1953): for any non-trivial semantic property of a
program — does it terminate, does it race, does it overflow — no algorithm
can decide that property correctly for *every* program in a Turing-complete
language. Not "nobody has found a good enough algorithm yet." Provably
never, for any algorithm, ever.

This doesn't kill the project. It tells you the shape it has to take. Every
language that has actually shipped compile-time guarantees for properties
like these — Rust, SPARK Ada, F*, Pony — routes around Rice's theorem the
same way: the type system is deliberately *conservative*. It accepts a
smaller language than "everything a human could write correctly," in
exchange for being able to say, with certainty, "everything this accepts is
safe." Programs that are actually correct but that the checker can't prove
correct get rejected. That isn't a flaw in the designs below — it's the
price of the guarantee. Any pitch that promises "catches every bug, rejects
nothing valid" is promising something that doesn't exist.

So the real design question isn't "how do I catch everything." It's
**where the conservative boundary sits, and what the programmer — human or
model — does at it.** That question turns out to matter for every one of the
ten requirements below, not only the ones that look like formal-methods
problems: a boundary a human can't learn, or an LLM can't reliably work
around, is just as much a failure as a boundary that lets a race condition
through.

---

## 1. The requirements, as one set

Eleven items (row 11 added 21 Aug 2026, see the note above). Each is independently studied, with its own literature and its
own shipped examples — but they're listed together, in one table, because
that's the actual claim of this document: none of them is optional relative
to the others, and — see the **Class** column — roughly half are things a
compiler can *prove*, and half are things that can only be *measured*
against how humans and models actually behave. A design that only satisfies
the provable half is Idris2 or ATS: correct and nearly unused. A design that
only satisfies the measured half is Go or Python: usable and unsafe. Neither
half is the "real" goal; the requirement is both, at once, in one language.

| # | Requirement | Class | Mechanism | Proven / measured where | The catch |
|---|---|---|---|---|---|
| 1 | No GC, no manual `free()` | **Hard** — proof | Ownership / linear types + region inference | Rust, Austral, ATS | Cyclic structures (graphs, doubly-linked lists) fight the discipline — needs an arena escape hatch |
| 2 | No data races | **Hard** — proof | Type system rules out simultaneous mutable aliasing, statically | Rust (borrow checker + `Send`/`Sync`), Pony (reference capabilities) | Only covers what the type system can see — FFI / `unsafe` boundaries reopen the hole |
| 3 | No deadlocks | **Hard** — proof by construction | Remove blocking locks as a primitive; concurrency = async messages only; shared-memory locks opt-in, gated by a static lock-rank check | Pony — no mutex exists in the language, so a deadlock is not *expressible* | Gives up shared-memory locks by default; hot paths that want them get re-cast as messages |
| 4 | No int / buffer over- or underflow | **Hard** — SMT-discharged, tiered | Refinement types on integers and indices, discharged by an SMT solver at compile time | SPARK Ada (avionics, rail, defense, since the 1990s), F*/Low* (HACL* — ships in Firefox, Linux WireGuard), Dafny | Not every arithmetic fact is SMT-decidable — needs a defined fallback (§4), not silent failure |
| 5 | Native, hardware-speed codegen | **Hard** — engineering | AOT compile via LLVM/MLIR, no interpreter, no GC pauses, monomorphized generics | Rust, ATS, Low* (→ C), SPARK | A bespoke minimal ISA is R&D that swallows the rest of the project — LLVM already captures that instinct |
| 6 | No steep learning curve | **Soft** — measured | Small, orthogonal grammar; one idiomatic way to do a thing; Tier-1 safety machinery (§4) silent by default so its notational cost is paid only where it's load-bearing | Go, Python — both explicitly optimized for onboarding speed over expressiveness | Directly in tension with 1–4: ownership, effects, and refinement types are exactly the extra notation that raises the curve |
| 7 | Easy for an LLM to write and reason about | **Soft** — measured, one **hard** sub-property | Unambiguous grammar — LL(1)/LALR parseability is a decidable, checkable property of the grammar itself; keyword-heavy over symbol-heavy syntax; structured, not prose, diagnostics | Grammar-constrained decoding research (`outlines`, `guidance`, LALR-constrained sampling) shows markedly higher reliability under a formal grammar | Real-world LLM code quality is dominated by training-corpus volume — no grammar design fixes a brand-new language starting at zero corpus. Mitigated, not solved, by shipping a machine-readable grammar/decoder spec with the language itself |
| 8 | Logical, composable syntax — complex operations built from fundamental blocks | **Hard** — provable, as a property of the semantics | Require the semantic function to be *compositional*: `⟦compose(a,b)⟧ = F(⟦a⟧,⟦b⟧)` for one fixed `F` — a minimal orthogonal primitive set, no special-cased rule per feature | Lisp/Scheme's minimal-core-plus-macros, Bird & Meertens's algebra of programming, Smalltalk's "everything is an object" | Compositionality fights performance-motivated special cases (SIMD intrinsics, in-place mutation) — the same tension ownership types (row 1) already manage |
| 9 | AI as a first-class citizen, not a guest | **Soft** — measured, interface is **hard**-typed | Agents emit typed AST/IR fragments the compiler validates before splicing, not raw text; compiler errors return structured, machine-checkable proof obligations, not English prose | LSP-guided completion, Copilot-style compiler-feedback loops, measured self-repair gains from re-prompting with diagnostics, proof-assistant tactic states (Lean/Coq) | Breaks down exactly at Tier-3 `audited` blocks (§4), which need a natural-language justification — the one place a human review gate should stay mandatory, not delegated |
| 10 | Tamper-evidence — detect "alien" code in the binary | **Hard** — proof / hash equality | Reproducible builds (compiler is a deterministic function of source + flags) + content-addressed source (hash the AST) + capability manifests enforced at the kernel boundary + signed provenance chain | Reproducible-Builds project (Debian, Tor), SLSA / in-toto, `src/capability.rs` + `src/ledger.rs` already in this codebase | Detects *deviation from attested source*, not "hacking" in general — side channels, hardware faults, and social engineering sit outside every mechanism on this list |
| 11 | Closed product types, sum types, and generics | **Hard** — proof (decidable) | `struct`/`enum` type formers plus a `match` expression; type parameters are concrete-per-instantiation (no erasure), the same way `Vector(f64,3)` and `Vector(f64,4)` are already different types | ML family (records, tagged unions), Rust (`enum`, exhaustive `match`) — see `nirdosha_row11_amendment.md` | No traits/typeclasses, no HKTs, no subtyping, no wildcard/binding patterns in `match` (v1) — the algebra of data, and nothing past it |

Nobody has shipped a language that clears all eleven rows — not because any row
is unproven in isolation, but because the "Hard" and "Soft" columns have
historically been treated as separate projects by separate communities
(formal-methods languages vs. ergonomic mainstream ones), never assembled
into one system. That's the actual gap this brief is aimed at: a real,
scoped engineering problem, not a research fantasy — provided the two
classes are designed together from the start rather than one bolted onto
the other after the fact (§7 makes that "together" precise).

---

## 2. Prior art, honestly rated

Full languages, plus the specific techniques the newer rows above lean on —
not every entry here is a language, and that's the point: rows 6, 7, 9, and
10 are proven by tooling and process as much as by type theory.

**Languages**

- **Rust** — ownership and borrowing kill use-after-free and data races in
  safe code, no GC, shipped at massive scale since 2015. Deadlocks
  untouched — two mutexes locked in opposite order still deadlock. Overflow
  is a debug-only panic; release builds wrap silently by default.
- **Pony** — reference capabilities (`iso`/`val`/`ref`/`box`/`tag`) make race
  freedom a type-checking fact. No lock or mutex exists in the language —
  actors only exchange async messages, which is *why* deadlock becomes
  unrepresentable, not just unlikely. Largely one person's PhD-scale effort.
- **Austral** — linear types, capability-secure, deterministic destruction,
  no GC. Built explicitly for domains where certification matters —
  deliberately smaller and more auditable than Rust.
- **SPARK Ada** — a provable subset of Ada. Contracts get discharged by an
  SMT backend at compile time, proving absence of overflow, buffer overrun,
  division-by-zero. Decades old, flight-certified. The trade: no dependent
  types, restrained generics — narrow by design, which is exactly why it's
  tractable.
- **F* / Low\*** — full functional-correctness proofs, including memory
  safety and side-channel resistance. The Low* subset compiles to C with no
  runtime cost. HACL* (built this way) ships in Firefox and Linux's
  WireGuard today.
- **Idris 2** — dependent types plus Quantitative Type Theory unify
  linearity and dependent typing in one system. Ergonomics and codegen are
  still research-grade — the cautionary tale for what happens when rows
  1–5 and 8 are pursued with no budget spent on rows 6–7.
- **Vale / Verona** — region-based ownership without Rust's lifetime
  annotations (Vale's "generational references"); Verona (Microsoft
  Research) generalizes Pony's actor isolation to shared-memory regions.
  Both promising, neither past research/preview stage.
- **ATS** — dependent + linear types, compiles to C with zero overhead —
  closest existing proof that "prove nearly everything, pay nothing at
  runtime" is possible. Syntax and proof burden are brutal enough that it
  stayed niche — the direct cautionary tale for rows 6–9.
- **Go** — almost no answer to rows 1–4, 8, or 10, and not trying to have
  one — but the sharpest existing precedent for row 6: a deliberately tiny
  keyword set, one canonical formatting (`gofmt`, which ends syntax
  bikeshedding entirely), and explicit over clever, all chosen to make
  onboarding fast for large, mixed-experience teams.
- **Python** — the sharpest existing precedent for row 7's confound:
  indentation-based blocks reduce syntactic noise, but the dominant reason
  LLMs write good Python is training-corpus volume, not grammar design —
  exactly the caveat row 7 has to design around rather than assume away.

**Techniques (not languages, but load-bearing prior art for rows 7–10)**

- **Grammar-constrained decoding** (`outlines`, `guidance`, LALR-constrained
  sampling) — the concrete evidence that a model's syntactic reliability
  rises sharply when its output is constrained to a formal grammar at
  decode time, not just prompted to follow one.
- **SLSA / in-toto** — the supply-chain attestation standards row 10 leans
  on: a signed, verifiable provenance chain from source commit through
  build to artifact hash, already adopted across major package ecosystems.
- **Reproducible Builds** (Debian, Tor Project) — proof, at real
  large-project scale, that byte-identical deterministic builds are an
  engineering property you can actually ship, not just a theoretical nicety.

---

## 3. The synthesis

None of the eleven rows conflict with each other — they sit at different
layers of the same compiler and toolchain. Stacking them, from what a human
or model actually types down to the binary:

| Layer | Covers | Description |
|---|---|---|
| **Surface syntax & tooling** | rows 6, 7, 9 | LL(1)/LALR grammar (a decidable property, checked by the parser generator itself), keyword-heavy over symbol-heavy, one canonical formatting (`gofmt`-style, kills bikeshedding), structured/machine-parseable diagnostics, a shipped grammar + constrained-decoder spec so tools don't depend on training-corpus frequency to get syntax right |
| **Core language** | rows 1, 8 | Strict, expression-based; no null, no implicit conversions, structured control flow only, sized integers by default (`i8`…`i64`, `usize`) — no untyped `int` to smuggle a proof gap through |
| **Type formers** | row 11 | `struct`/`enum`/`match`, nominal, no traits or subtyping; generics are concrete-per-instantiation (no erasure) rather than a separate monomorphizer pass — the closed algebra of data the rest of user code is written in, distinct from row 8's compositional-semantics requirement on the language itself |
| **Ownership & regions** | row 1 | Austral + Vale: affine by default; region/lifetime scopes *inferred*, not hand-annotated, for the common case. Bulk region alloc/free — no per-object `malloc`, no GC, ever |
| **Concurrency** | rows 2, 3 | Pony by default: objects live in isolated domains, cross-domain traffic is only typed async messages, so races are unrepresentable. Shared-memory locks are opt-in, gated by a static lock-rank check — not the default path |
| **Refinement types** | row 4 | SPARK + Idris2 QTT: integers and byte arrays carry SMT-checked bounds (`byte[n] where n < cap`), discharged at compile time by a bundled solver (three-tier resolution, §4) |
| **Effects** | rows 4, 9 | Koka-style: function signatures declare what they touch — allocate, send, do I/O — as an algebraic effect. This is what keeps ownership and refinement tractable *and* what makes agent-facing diagnostics structured instead of prose (row 9) |
| **Backend** | row 5 | LLVM / MLIR, ahead-of-time to native code, generics monomorphized, no runtime, no GC. Codegen's only job is not to waste what every layer above already paid for statically |
| **Attestation** | row 10 | Wraps the whole pipeline rather than sitting inside it: deterministic compiler, content-addressed (AST-hashed) source, signed provenance chain, capability manifests enforced at the kernel boundary |

Four of these layers are worth a beat longer, one per requirement class that
isn't self-explanatory from the table:

**Concurrency — earn "no deadlocks" by construction, not by proof search.**
Static deadlock detection for arbitrary lock-based code is a genuinely thin
research area — it isn't that nobody proved it possible, it's that almost
nobody shipped it. Pony's answer sidesteps the search entirely: if the
language has no blocking wait primitive, there is nothing for a cycle to
form out of. Nirdosha takes that as the default, and treats shared-memory
locks the way Rust treats `unsafe` — available, rare, and visibly fenced off
by a static lock-rank check when used.

**Overflow — SPARK's discipline, F*'s ambition, scope only.** SPARK proves
the narrow, load-bearing case — array bounds and arithmetic overflow — and
has done it in production for thirty years. Nirdosha borrows SPARK's
*scope*, not F*'s ambition: refinement types on numbers and array lengths
only, discharged automatically where the solver can, with a designed
fallback where it can't (§4) rather than an unbounded proof-writing
expectation.

**Surface syntax — optimize for the two readers that actually exist.** The
readers of Nirdosha source are a human under time pressure and a model
sampling tokens — neither reads syntax the way a formal-methods paper
assumes. An LL(1)/LALR grammar isn't a nicety; it's what lets a
grammar-constrained decoder guarantee syntactic validity token-by-token
instead of hoping. One canonical formatting removes an entire axis of
disagreement (and of wasted model tokens) before either reader gets to
semantics. None of this weakens rows 1–5 — the safety machinery still
exists — it just stays silent (Tier 1, §4) until it's actually earning its
keep, so the *notational* cost of ownership/effects/refinement is paid once,
by the compiler, not on every line by the human or the model.

**Attestation — make deviation from source cheap to prove, not cheap to
hide.** "No hacking possible," read literally, isn't a claim any system can
make: side channels, hardware faults, physical access, and social
engineering sit outside what any language or compiler can touch, and Rice's
theorem (§0) already rules out "provably zero bugs of any kind." What's
actually true and worth building: rows 1–4 eliminate the specific exploit
classes (buffer overflow, use-after-free, race-condition TOCTOU) that
dominate real-world memory-unsafe-language CVEs — Microsoft's and Google's
own published breakdowns each put memory-safety issues at roughly two-thirds
of their serious security bugs — and row 10, separately, makes "this binary
was not derived from the attested source" a checkable fact rather than a
trust assumption: code that wasn't produced by the attested compiler from
the attested source either fails to reproduce the expected hash (caught at
build time) or gets refused capabilities its manifest never declared
(caught at run time, by the kernel).

---

## 4. The escape valve, spelled out

Given Rice's theorem, the solver will sometimes shrug — on rows 1–4 and 8
directly, and indirectly on row 9, since an agent's self-repair loop only
works where the compiler can say something structured back. The design
decision that actually determines whether the language is usable is what
happens next — three tiers, not a binary safe/unsafe split:

- **Tier 1 — Proved.** Solver discharges the bound at compile time. No
  runtime check is emitted; the guarantee is load-bearing, not a hint. This
  is also the tier that keeps row 6 (learning curve) and row 7 (LLM
  ergonomics) honest — it's silent, so it costs nothing to read or write.
- **Tier 2 — Checked.** Solver can't decide. The compiler demands either a
  narrower type, a proof hint, or an explicit checked-operation spelled
  differently from ordinary `+`/`[]` — so a runtime check is visible in a
  diff and in review, never silent.
- **Tier 3 — Audited.** The genuine dead end: a block marked `audited` with
  a written, greppable justification — the same role SPARK's proof
  obligations and Rust's `unsafe` already play. This is the one tier row 9
  says should stay a mandatory human gate: natural-language justification
  is exactly where an LLM's self-repair loop is weakest, not strongest, so
  an agent proposing a Tier-3 block should not also be the one clearing it.

The goal isn't zero escape hatches; it's that every one of them is visible,
searchable, and costed, rather than an implicit "the compiler trusts you
here" that nobody — human or model — can find later.

---

## 5. What this means for Sūtra specifically

Worth naming since a capability kernel is already sitting right here: this
design isn't hypothetical infrastructure to bolt on afterward, it overlaps
with what `src/capability.rs` and `src/ledger.rs` already do — for two of
the eleven rows, not one:

- **Region alloc/free** (row 1) wants one bulk syscall (`region_alloc(pages)
  → capability`, `region_free(capability)`) rather than page-by-page
  mapping — a natural extension of the existing page allocator, not a new
  subsystem.
- **Actor mailboxes** (rows 2–3) need fast async IPC — and a
  capability-secured mailbox is close to what the existing
  capability/ledger transaction system already models. Reusing what's
  already built beats designing something new here.
- **Capability manifests** (row 10) are the run-time half of
  tamper-evidence: a binary only gets to exercise the capabilities its
  manifest declares, enforced at exactly the boundary `capability.rs`
  already polices. Detecting alien code at run time is a policy change to
  an existing enforcement point, not new kernel work.
- **Lock-rank checking, SMT-discharged refinement types, the surface-syntax
  grammar/decoder, and the AI-facing effect diagnostics** (rows 3, 4, 6, 7,
  8, 9) are all purely compile-time concerns. Zero kernel-side work — they
  live entirely in `compiler/`.

Only rows 1–3 and 10 touch the kernel at all; the rest of the requirement
set is a compiler-and-tooling problem layered on top of what's already
running.

---

## 6. What it actually costs

Scale expectations against the field: Rust went from project start to 1.0
in about seven years with a growing, funded team, and its ergonomics were
still being reworked years after that. Idris2 has been active
dependently-typed research for over a decade and remains niche. Verona has
been a Microsoft Research project since roughly 2019 without a production
release. SPARK is the outlier — decades old and field-proven, but only
because its scope is deliberately narrower than anything above: no heavy
generics, no dependent types. That's the pattern worth internalizing:
**provability and expressiveness trade off directly** — the more the
compiler proves automatically, the less "clever" the surface language gets
to be. Go and Python are the mirror-image outliers: fast to learn precisely
because they gave up rows 1–4 and 8 almost entirely.

Pony is the encouraging data point, not the discouraging one: the single
hardest item on the original wishlist — deadlock freedom — was
substantially one person's dissertation-scale work, because removing a
primitive is cheaper than proving properties about it. The same logic
applies to row 10: reproducible builds and capability manifests are mostly
*assembly* of existing standards (SLSA/in-toto, the kernel's own
capability system), not new research.

| When | Phase | Covers |
|---|---|---|
| Weeks | **0 — Narrow the core, draft the grammar** | Straight-line and structured control flow only, no unbounded recursion or higher-order closures yet (keeps the checker decidable), fixed-size integers and arrays only. LL(1)/LALR-checkable grammar drafted in parallel — it's cheap to check early and expensive to retrofit later (rows 1, 6, 7) |
| Months | **1 — Ownership, no GC** | Regions + affine bindings, single-threaded, compiles via LLVM to native. The load-bearing memory-safety layer everything else sits on (row 1) |
| Months | **1.5 — Closed algebraic data types** | `struct`/`enum`/`match`, generics as concrete-per-instantiation types (no erasure). Sits between ownership and refinement types because it needs affinity propagation from Phase 1 (a struct with an affine field is affine) and feeds refinement's boundary-checking in Phase 2 (a struct/enum constructor is one more call site to check). See `nirdosha_row11_amendment.md` (row 11) |
| Months | **2 — Refinement types** | Bind a Z3-class solver at compile time for integer and array bounds. Comparable in scope to what LiquidHaskell and Dafny each took roughly a person-year or two to reach usable state on (row 4) |
| Quarters | **3 — Concurrency** | Actor model with capability-typed mailboxes, Pony-style. The one-person precedent above is the reason to attempt this rather than defer it (rows 2, 3) |
| Quarters | **3.5 — Surface & AI interface** | Grammar-constrained decoder, structured/machine-parseable diagnostics, an LLM-benchmark suite tracked as a real metric (not a launch afterthought), typed AST/IR splicing for agents. Independent of Phase 3's kernel-facing work, so it runs in parallel, not after (rows 6, 7, 9) |
| Ongoing | **4 — Harden, attest, connect** | Self-host; wire up reproducible builds, content-addressed source, and signed provenance; enforce capability manifests at the kernel boundary; connect the backend to the Sūtra kernel as a real target rather than a toy one (rows 5, 10) |

---

## 7. The eleven requirements as one optimization problem

Given the split in §1's Class column, it's worth being precise about what
"one cohesive set of requirements" actually means mathematically, rather
than leaving it as a slogan. It means: **one constrained optimization
problem**, not two lists — a single design `D` has to satisfy every hard
constraint *and* be evaluated on every soft objective, simultaneously,
because a `D` that only does one half isn't a candidate at all (it's Idris2,
or it's Go).

```
design space   D = (grammar G, type rules Σ, effect system E,
                     stdlib primitives B, capability schema M, toolchain C)

hard constraints — proof, not measurement, no partial credit:
    ⟦D⟧ ⊨ Tᵢ   for every Hard row in §1:
        T_memory        (row 1)  no well-typed program touches freed/unowned memory
        T_race          (row 2)  no well-typed concurrent program has conflicting
                                  unsynchronized access to one location
        T_deadlock      (row 3)  the wait-for graph of any well-typed program is
                                  acyclic (true by construction — no blocking primitive)
        T_overflow      (row 4)  every Tier-1 arithmetic/index op is SMT-proved in-bounds
        T_compositional (row 8)  ⟦compose(a,b)⟧ = F(⟦a⟧,⟦b⟧) for one fixed F
        T_reproducible  (row 10) compile(s, flags) is a deterministic function of (s, flags)
        T_confinement   (row 10) the kernel grants binary β only capabilities in manifest(β)
        T_data          (row 11) every struct/enum construction is field/payload-type-checked;
                                  match exhaustiveness is a decidable check over a closed,
                                  finite variant set — no undecidable residue, unlike rows 1-4

soft objective — measurement, degrees not booleans, still binding:
    maximize  Σⱼ wⱼ · mⱼ(D)   over every D that satisfies every hard constraint above

    m_learning-curve (row 6)  median novice time-to-first-correct-program (user study),
                               or Cognitive-Dimensions-of-Notations expert score
                               (Green & Petre, 1996)
    m_llm-fit        (row 7)  pass@1 on a held-out benchmark suite under a
                               grammar-constrained decoder; is G LL(1)/LALR(1)
                               (cheap, decidable, checked by the parser generator —
                               the one soft metric that's actually crisp)
    m_ai-native      (row 9)  fraction of compiler diagnostics that are
                               structured/machine-parseable vs. free prose;
                               agent self-repair success rate within N attempts
```

Two things this formulation makes explicit that a flat checklist doesn't:

1. **The hard constraints are a filter, not a target.** They define the
   *feasible set* — every `D` that survives them is a language that could
   ship without a safety regression. Row 5 (native speed) and row 5's
   codegen aren't listed as a `Tᵢ` because they're an engineering
   requirement on the backend, not a semantic theorem about `D` — but they
   still gate which `D` are admissible, the same way the others do. Row 11
   (`T_data`) is the opposite kind of exception to the pattern below: it
   *is* a `Tᵢ`, but — by design (§2.2 of `nirdosha_row11_amendment.md`,
   deliberately no traits/HKTs/subtyping) — one with no Rice's-theorem-driven
   undecidable residue, so it never needs rows 1–4's Tier-2/3 escape valve.
2. **The soft objective only ever ranks *among* feasible designs.** There is
   no way to trade a hard constraint for a better learning-curve score — a
   `D` that's easier to learn but lets a data race through isn't a
   better point in this space, it's not in the space at all. That's the
   formal content of "the secondary objectives are part of the primary
   requirements": they don't get optimized in a separate pass after safety
   is settled, they're a term in the same objective function evaluated over
   the same feasible set, from the first design iteration.

This is a real formalization, but notice what kind: a **constrained,
multi-objective optimization over a combinatorial, partly-qualitative design
space with no closed form** — there is no algebraic "solve for x," and nine
of the eleven rows already told you why (§0) — rows 5 and 11 are the two
exceptions, each decidable/engineering rather than Rice's-theorem-limited.
It's solved the way every real
engineering optimization with a non-differentiable, partly-empirical
objective landscape is solved: **iterative search, not inversion.**

Concretely, as a process rather than a metaphor: discharge the hard
constraints once, for the core calculus, as a mechanized proof (ideally in
Lean4 or Coq, the way CompCert and RustBelt did for their languages) — every
later design tweak is then a cheap re-check, not a re-proof from scratch.
Then search the soft-objective space iteratively: propose a candidate
grammar or typing-rule variant (an LLM proposing candidates here is a fair
use of the very thing being optimized for), auto-check it against the hard
constraints, run the benchmark suite for the soft metrics, keep it only if
it improves on the current Pareto frontier without violating a single hard
constraint, repeat. Every point the search keeps is itself renderable as a
concrete spec — a document structured like this one — so "translate the
math back to English" isn't a final step, it's what happens after every
iteration.

**The honest caveat, stated plainly:** this optimization never terminates at
a unique, final answer. The hard-constraint side always has an undecidable
residue by Rice's theorem — hence the Tier 2/3 escape valves (§4) stay
necessary forever, not just at launch. The soft-metric side is about human
and model behavior, which drifts as tooling and training corpora change —
"easy to learn" and "LLM-friendly" in 2026 won't mean the same measured
thing in 2030. So the right framing isn't "solve the equation once and ship
the answer" — it's standing up this search loop as a permanent part of the
language's evolution, the same role Rust's RFC process or TC39's proposal
stages already play, just made more explicit and more machine-checkable
wherever §1–§6 make that possible.

---

## 8. The recommendation

Start from **Pony's actor/capability core** for the race and deadlock
requirements (rows 2–3) — it's the one piece of this whole list with a
working, shipped answer. Layer **Austral-style linear ownership** underneath
for the no-GC memory model (row 1). Borrow **SPARK's proof-obligation
discipline**, not F*'s ambition, for overflow (row 4) — narrower scope is
why SPARK is the one that's flight-certified. Design the **surface grammar
as LL(1)/LALR from day one** and ship a formal grammar/decoder spec
alongside the compiler (rows 6–7) — retrofitting parseability onto a
grammar that grew organically is far more expensive than drafting it that
way from the start. Make the **effect system double as the agent interface**
(row 9): typed AST/IR splicing and structured diagnostics, with Tier-3
`audited` blocks kept behind a mandatory human gate rather than delegated.
Wire up **reproducible builds and capability-manifest enforcement** (row 10)
using `src/capability.rs`/`src/ledger.rs` rather than building new
supply-chain infrastructure. Target **LLVM from day one** rather than a
bespoke ISA (row 5); the "minimal instruction set" instinct is already
satisfied by everything proven statically above it, and a custom backend is
R&D that will swallow the rest of the project. Add **closed `struct`/`enum`
types with concrete-per-instantiation generics** (row 11) underneath the
ownership and refinement layers — no traits, no HKTs, no subtyping — so
that everything rows 1–5 already prove statically has a user-facing data
shape to prove it *about*, not just scalars and fixed-size arrays.

None of these seven moves is optional relative to the others — leave out the
grammar work and rows 1–4's safety machinery becomes the reason nobody
adopts the language (Idris2's fate); leave out the safety machinery and the
grammar work produces a faster Go, not Nirdosha; leave out row 11 and rows
1–4's proofs stay confined to a toy language with no records or options.

---

## 9. The honest gap, and the three things that close it

*Added 20 Aug 2026, after benchmarking the current implementation against
Julia and C (`benchmarks/RESULTS.md`) and asking plainly why anyone should
adopt Nirdosha today.* The honest answer at that point in time: they
shouldn't, not yet. Pre-alpha, no ecosystem, half the language doesn't
compile (§10 of `LANGUAGE.md`), no mechanized safety proof, no real user or
model has written real programs in it. The pitch — provable safety *and*
LLM-native design, together, which nobody has shipped — is a thesis, not a
track record. Three things turn it into one, and this is now the standing
goal, worked until each is true, not until the calendar says stop:

1. **Finish the compiler.** `Vector`/`Matrix` codegen — **done, 21 Aug
   2026**: value representation + sret/pointer calling convention, dynamic
   indexing wired to the bounds proofs `refine.rs`/`smt.rs` already
   produced (now actually consumed, not just proved), every unrollable
   operator/builtin as straight-line IR, `det`/`inv`/`solve`/`rank`/
   `kf_update_*` via a linked native runtime call. Payoff was measured, not
   assumed, and held: Group A (`matmul`/`det`/`dot`/`kalman`) went from a
   2–2 split against Julia (interpreted) to beating it 36×–441× across all
   four, compiled (`benchmarks/RESULTS.md`). One real bug found and fixed
   along the way — loop-body allocas weren't hoisted to the function entry
   block, so tight loops building `Vector`/`Matrix` values blew the stack
   past a few thousand iterations; fixed by hoisting all allocas
   unconditionally, the standard technique, with a regression test added.
   `box`/`thread`/`chan`/`sandbox`/`tcp`/`str` are still interpreter-only —
   "hardware-native speed" (row 5) now covers scalars and dense linear
   algebra, not yet those. That's the next slice of this item, not a new
   item — row 5 isn't fully closed until they compile too.
2. **Prove the safety claims formally.** Row 1–4's "no GC, no races, no
   deadlocks, no overflow" is currently "the type system is believed to
   guarantee this," backed by tests, not a mechanized proof. §7 already
   names the target: discharge the core calculus's hard constraints once,
   in Lean4 or Coq, the way CompCert and RustBelt did — after that, each
   later design change is a cheap re-check, not a re-proof from scratch.
3. **Get a real LLM benchmark and real users writing real programs.** Row
   7/9's "easy for an LLM to write" is designed-for, not measured-yet —
   `bench/`'s scoring loop is real plumbing wired to two mock models
   (`bench/README.md`), not a real model integration. Wire in an actual
   model, run the corpus, publish pass@1 and self-repair numbers; get
   people outside this project writing real Nirdosha programs and see
   where the design actually breaks under real use, not assumed use.

No deadline attached to any of the three on purpose — each is done when
it's actually true, verified the same way this project already insists on
verifying everything else (a real benchmark, a real proof, a real model,
not a claim). This section is the checkpoint to come back to and update as
each closes.

---

*Nirdosha — a design brief, not a spec. Sources are the shipped systems and
standards named above, not this document.*
