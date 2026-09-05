---
state: draft
shepherd: (unassigned)
---

# RFC 0005: The Nirdosha↔Rust plugin boundary — safety and performance

## Motivation

Nirdosha's whole value proposition is a *static-proof* story: ownership/
affine types, the effect system, `smt.rs`'s bounds proving, no mutex in
the grammar at all. `crates/compiler/src/plugin.rs` (Kind A, RFC 0003/
0004) is the one place a `.nir` program's guarantees hand off to
arbitrary Rust — so the plugin boundary isn't an integration detail,
it's part of the safety and performance model, and deserves the same
rigor as any other row in `docs/goal.md`.

This RFC does three things, in the order the evidence actually
justifies them, not the order a plugin-boundary essay would predict:

1. **Characterizes what Kind A actually is today**, precisely — it
   turns out to matter a great deal for how much of the classic
   FFI-safety literature even applies (§1).
2. **Closes a real, already-named gap**: `Ty::Handle`, a compiler-
   enforced affine type for plugin-held resources, replacing the
   untyped `i64` `nirdosha-plugin-support::HandleRegistry` disclosed as
   its own honest limitation. Built, tested, all 911 pre-existing tests
   still green (§2).
3. **Runs the spike `rfcs/0004-native-plugin-sandboxing.md` explicitly
   deferred** — rot13 compiled to WASM, called through `wasmtime`,
   real numbers — to give Kind C (WASM-sandboxed plugins) a real
   evidence base instead of a placeholder (§3).

Everything below is either a citation to code that exists on `main`
today, a change built and tested on this RFC's own branch
(`rfc/plugin-boundary-safety-perf`), or a number from a benchmark whose
source is included and reproducible. Nothing is estimated without being
labeled as an estimate.

## Design

### 0. What Kind A actually is (read this before anything else)

The single most consequential fact about `crates/compiler/src/plugin.rs`,
easy to miss and central to everything that follows: **a Kind A plugin
is not across a foreign-function boundary in the classic sense at all.**

```rust
pub type PluginFn = Arc<dyn Fn(&[Value], Span) -> Result<Value, RuntimeError> + Send + Sync>;
```

`Value`, `RuntimeError`, `Span` are Nirdosha's own Rust types. A plugin
crate is "an ordinary Rust dependency, compiled and statically linked"
(`plugin.rs`'s own doc comment) into the exact same binary, checked by
the exact same `rustc`. There is no `dlopen`, no C ABI, no
serialization, no separate address space. Consequences that a plugin
author writing against `PluginBuiltin` gets *for free*, with **zero
`unsafe`** in `crates/plugin-example-rot13` or any of the five gallery
plugins:

- **Ownership/borrowing**: ordinary Rust borrow-checking already governs
  everything a plugin's `call` closure touches. There is no "does Rust
  retain a reference past Nirdosha's own lifetime" question, because
  Nirdosha's own `Value` (an `Arc`-based enum, `interpreter.rs`) *is*
  the value — a plugin holding an `Arc<str>` clone alongside Nirdosha's
  own copy is exactly as sound as any two Rust closures sharing an
  `Arc`, because that is literally what's happening.
- **Aliasing**: `Value::Str(Arc<str>)`, `Value::Json(Arc<serde_json::Value>)`
  etc. are already immutable-after-construction and `Arc`-shared by
  design (`interpreter.rs`) — a plugin can alias one freely, the same
  way any other part of the interpreter already does, with the same
  guarantee (no data race, because nothing is ever mutated through a
  shared `Arc`).
- **Unsafe Rust**: the honest answer to "where is `unsafe` allowed" is
  **nowhere, today, and none of the six existing plugin crates in this
  repo use it.** This is worth stating as a real, checked fact, not an
  aspiration — see Evidence §E0.

**What this means for the rest of this RFC**: most of the classic
FFI-safety literature (stable-ABI layout, calling-convention mismatches,
manual lifetime bookkeeping across a boundary, `unsafe extern "C"`
contracts) is answering a question Kind A doesn't ask, because Kind A
isn't foreign in the sense that literature means. Applying it uncritically
here would be solving problems this architecture doesn't have while
missing the two it does — which is exactly what §1 and §2 found instead.

The real, live gaps are narrower and (once you know Kind A's actual
shape) more tractable:

- Nothing analogous to `Ty::Db`'s own affine discipline exists for a
  *plugin's* stateful resources — §1.
- The dispatch mechanism itself has one measurable, fixable cost, not
  from `Arc<dyn Fn>` (negligible) but from something upstream of it —
  Evidence §E1.
- The moment a plugin needs genuine isolation from a plugin author who
  *isn't* trusted (RFC 0004's own stated non-goal for Kind A), none of
  Kind A's free lunch survives, and the real cost of that isolation
  needed real numbers RFC 0004 didn't have yet — §3.

### 1. `Ty::Handle` — closing rfcs/0003's own open question

**The gap, in the maintainers' own words** (`nirdosha-plugin-support/
src/lib.rs`, shipped 2026-09-05): *"a handle minted by
`HandleRegistry::insert` is just a `Value::Int` (an opaque `i64`) once
it crosses into `.nir` source — `ownership.rs` gives it none of the
affine 'one owner, closed exactly once' guarantees a real `Ty::Db`/
`Ty::Mq`/`Ty::Sandbox` handle gets today. A `.nir` program can call a
plugin's own `close(id)` builtin twice, or drop the id and leak the
underlying resource, and nothing in this crate or the compiler catches
either at compile time."*

That same file explains why the obvious "just add a generic
compiler-enforced handle type" fix wasn't built: it would need a plugin
author to get `Box<dyn Any>` downcasting and Nirdosha's own affine
semantics right, "a pattern with zero public precedent."

**The finding this RFC makes**: that concern is real for a *generic*
handle type, but doesn't apply to the actual fix, because it conflates
two separable questions.

- **Question A** (what `ownership.rs` needs): is this *type* affine —
  single-owner, moved on use, an error to touch after it's consumed?
  `Ty::is_affine()` (`ast.rs`) answers this with a flat, syntactic
  `matches!` over type *tags* — `Ty::Box(_) | Ty::Thread(_) | ... |
  Ty::Db | Ty::Mq`. It has never once looked at what's *inside* a
  handle. A `Ty::Db` connection's real driver (`rusqlite::Connection`
  vs. a Postgres `r2d2::PooledConnection`, `dbconn.rs`'s `DbConn` enum)
  is invisible to it, on purpose.
- **Question B** (what a plugin author needs): given a raw handle id
  crossing back into their own Rust code, how do I get back a concrete
  `T`? `HandleRegistry<T>: Mutex<HashMap<u64, T>>`, already shipped,
  already solves this with zero `Any`/downcasting, because `T` is
  monomorphized per `HandleRegistry<T>` instance.

These were never actually coupled. `Ty::Handle(String)` — the `String`
naming a plugin-chosen resource kind, e.g. `"MysqlConnection"` — answers
Question A alone, mechanically, with the *runtime* representation
staying exactly what `HandleRegistry::insert` already returns: a plain
`Value::Int`. Question B's answer (`HandleRegistry<T>`) is completely
unchanged — a plugin author's `call` closures need **zero** edits to
adopt this.

**Built, on this RFC's branch** (`ast.rs`, `token.rs`, `parser.rs`,
`codegen.rs` — four files, ~50 lines total):

```rust
// ast.rs -- new Ty variant, and one line in is_affine()
Handle(String),
// ...
matches!(self, Ty::Box(_) | Ty::Thread(_) | ... | Ty::Db | Ty::Mq | Ty::Handle(_))
```

```rust
// token.rs -- one new keyword, mirroring Vector/Matrix's Name(args) shape
"handle" => Tok::HandleKw,
```

```
// New .nir syntax, parser.rs -- handle(KindName), a bare identifier,
// not looked up against the struct/enum registry (it's a plugin's own
// nominal tag, never a constructible type):
let h: handle(MysqlConnection) = mysql_connect(url)
```

`codegen.rs::check_supported` rejects it with a named reason, joining
`Db`/`Mq`/`Json` — plugins are already interpreter-only for the
compiled path (`docs/ECOSYSTEM.md` §G1), so this is consistency, not a
new limitation.

**Verified, not just written** — `crates/compiler/tests/
plugin_handle_ownership.rs`, a mock stateful plugin
(`widget_connect`/`widget_close`/`widget_query`), three tests, all
passing:

```
running 3 tests
test double_close_on_a_plugin_handle_is_a_compile_time_ownership_error ... ok
test a_struct_holding_a_handle_is_affine_too ... ok
test single_use_then_close_still_works_and_runs ... ok
```

The first test is the actual claim: `widget_connect()` → `widget_close(h)`
→ `widget_close(h)` again is rejected **before the program ever runs**,
with a real `ownership error: ...: use of `h` after it was moved` —
exactly the class of bug that, before this fix, only a plugin's own
runtime bookkeeping could catch, one call too late. The second test
confirms `Ty::is_affine`'s existing struct-recursion (`ast.rs::
TypeRegistry::is_affine_visiting`, unchanged) picks up a handle stored
in a struct field for free — no new code needed, it already generalizes.

**A second real finding, surfaced only by actually building this, not
by designing it on paper**: the third test failed on the first attempt.
`widget_query(h)` (a *read*, no reason to consume) followed by
`widget_close(h)` was rejected too — because `Ty::Handle` args, being
ordinary `Expr::Call` arguments, are consumed on every call by default,
same as any user function's arguments. `Ty::Db`'s own `db_query`/
`db_execute` avoid this via a hardcoded, per-builtin-**name** exemption
in `ownership.rs`:

```rust
let consume = !(i == 0 && matches!(name.as_str(), "db_query" | "db_execute" | "mq_publish" | "mq_consume"));
```

That doesn't scale to third-party plugins — a plugin author can't add
their own builtin's name to a match arm inside the compiler. The fix
that *does* generalize, with zero `ownership.rs` changes: declare a
read-only builtin's handle parameter as `&handle(Kind)`
(`Ty::Ref(Box::new(Ty::Handle(...)))`) instead of a bare `handle(Kind)`.
`Ty::Ref`'s own existing rule — a shared borrow of affine content is
always freely, repeatedly readable — already covers this with
infrastructure that predates this RFC entirely. Fixed the test
accordingly; **this is now the documented convention** a `Ty::Handle`-
using plugin author should follow: mutating/consuming operations
(`connect`, `close`) take `handle(Kind)`; read-only operations (`query`,
`is_open`) take `&handle(Kind)`.

**Regression check**: `cargo test -p nirdosha --no-fail-fast` — 911
tests passed, 0 failed, across every existing test file. This is a real,
additive, backward-compatible change: `handle` was previously usable
only as an identifier substring (`handle_authorized`, etc. — checked
against every `.nir` file in the repo, none use bare `handle` as an
identifier), so reserving it introduces no breakage.

### 2. Kind C — the WASM spike RFC 0004 deferred, with real numbers

RFC 0004 §3 named the right next step and explicitly didn't do it: *"a
narrow spike compiling `rot13` (already pure, no I/O, the simplest
possible case) to WASM and measuring call overhead through `wasmtime`
— explicitly not designed here, flagged as its own future RFC once that
spike has real numbers."* This section is that RFC, with those numbers.

**What was built** (source included with this RFC's evidence, not
merged into the main tree — see Compatibility): the identical `rot13`
transform, compiled to `wasm32-unknown-unknown`, with the minimal
explicit calling convention any WASM-sandboxed plugin needs (no
Component Model tooling, scoped to exactly what the spike needs to
measure):

```rust
// Guest side
#[no_mangle] pub extern "C" fn alloc(len: usize) -> *mut u8;   // host writes input here
#[no_mangle] pub extern "C" fn dealloc(ptr: *mut u8, len: usize);
#[no_mangle] pub extern "C" fn rot13_inplace(ptr: *mut u8, len: usize);
```

Host side: `wasmtime` 49.0.0-rc.1, `Engine::default()`, real
`Instance::get_typed_func`/`Memory::read`/`Memory::write` calls — no
shortcuts, every real cross-boundary step measured.

**Results** (i7-8550U, Linux 7.0.10-zen1, best of 5, full methodology
in Evidence §E2):

| | 55-byte payload | 61 KB payload |
|---|---:|---:|
| Kind A: full plugin dispatch (`Arc<dyn Fn>`, in-process) | ~390 ns | O(1) in payload size — see below |
| Kind C: full round trip (alloc+copy-in+call+copy-out+dealloc) | ~217–241 ns | ~118,691 ns |
| Kind C: call only, buffer reused (no copy) | ~104–121 ns | ~118,502 ns |
| Kind C: copy-in+copy-out only (no call) | ~22–26 ns | ~2,343 ns |

**The actual finding, stated precisely**: at a small, fixed payload,
Kind C's *call* overhead alone (~104–121 ns) is already comparable to
or larger than Kind A's *entire* dispatch-plus-work cost (~390 ns
includes real work; Kind A's dispatch tax alone is ~30–90 ns, Evidence
§E1) — WASM's own call-dispatch mechanism (crossing into Cranelift-
compiled code through `wasmtime`'s `Store`/`Instance` machinery) is not
free, independent of any copying. **The much larger, structural
difference is what happens as payload grows**: Kind A's argument-passing
cost is an `Arc::clone` — a fixed-cost atomic refcount bump, *provably*
independent of the string's length (a basic property of `Arc`, not
something this RFC needed its own benchmark to establish). Kind C's
copy-in/copy-out cost measured **~2,343 ns at 61 KB vs. ~22–26 ns at 55
bytes — scaling with payload size**, because a linear-memory sandbox
boundary fundamentally cannot share Nirdosha's `Arc<str>`; every byte
must be copied in and back out. For a large JSON blob, file, or query
result, this gap has no ceiling; Kind A's stays flat.

**Also real, and the dominant cost at 61 KB**: the guest-side
computation itself (~1.9 ns/byte in this spike's Cranelift-compiled
loop) is not free either, and this spike didn't attempt to separate
"WASM sandboxing tax" from "Cranelift-vs-`rustc`-optimized-native
codegen tax" — both are real costs of choosing Kind C, reported
together, honestly, rather than a cleaner-looking number produced by
attributing this cost to the wrong cause.

**What Kind C would need, based on this evidence** (design, not yet
built — scoped here for a real follow-up RFC once/if a genuine
untrusted-plugin need materializes, per RFC 0004's own recommendation
this stays a separate Kind, not a Kind A retrofit):

- A typed, generated shim per plugin signature (the `alloc`/`copy`/
  `call`/`copy`/`dealloc` protocol above, mechanized — this is exactly
  what `extism`'s PDK/`convert` crate and `wit-bindgen`'s Component
  Model bindings already do in the broader Rust/WASM ecosystem; neither
  needed to be reinvented here, both are real prior art worth building
  on rather than around).
- An explicit **payload-size-aware** cost model in any future capacity
  planning: Kind C is the right choice when isolation matters more than
  large-payload throughput (an untrusted third-party transform on a
  short string); Kind A remains categorically better for anything
  passing large, shared, `Arc`-backed data — a distinction Kind A vs.
  Kind C's design should make legible to a plugin *consumer*, not just
  its author.
- `Ty::Handle` (§1) generalizes cleanly to a Kind C handle too: a WASM
  guest's own "connection id" is exactly as opaque an `i64` as
  `HandleRegistry`'s already is, crossing the *linear-memory* boundary
  instead of an in-process one — the ownership-checker-side fix is
  identical, only the runtime plumbing on the far side differs.

### 3. The harder, still-open question: compiled (`build`/`emit-llvm`) plugin calls

`docs/ECOSYSTEM.md` §G1 already discloses this precisely: *"plugins
stay permanently interpreter-only for the compiled path (no stable
calling convention from generated LLVM IR into an opaque `Arc<dyn Fn>`
exists), a deliberate limit, not an oversight."* This RFC does not
close this gap — it's real research, not a spike-sized question — but
frames it precisely, since the brief that prompted this RFC asked
directly about LLVM/cross-boundary optimization:

**Why it's hard, specifically**: `PluginFn` is a Rust trait object —
`Arc<dyn Fn(&[Value], Span) -> Result<Value, RuntimeError>>`. LLVM IR
`codegen.rs` emits has no notion of `Value`, `RuntimeError`, or a Rust
vtable — those are `rustc`-generated, not stable, and not meaningful
from IR generated by a *different* compilation (`nirdosha build`'s own
`clang`-backed pipeline, `benchmarks/RESULTS.md`'s own methodology
note). Closing this for real needs one of:

1. **A stable, `#[repr(C)]` plugin ABI** *in addition to* the existing
   `Arc<dyn Fn>` one — e.g. a plugin optionally also exports
   `extern "C" fn(*const CValue, usize) -> CResult` for scalar-only
   signatures, where `CValue` is a flat, `#[repr(C)]` tagged union
   `codegen.rs`'s LLVM IR can construct and call directly, no interpreter
   involved. Real cost: every plugin author pays this twice, or the
   compiler generates the second form mechanically from the first
   (feasible for `Ty::I8..Ty::Handle`'s scalar cases; `Ty::Json`/
   compound types would need to stay interpreter-only, same category
   as `Ty::Db` today).
2. **Full LTO-style whole-program builds**: skip the stable-ABI question
   by compiling the plugin crate's own MIR/LLVM IR into the *same*
   `nirdosha build` LLVM module, enabling real cross-boundary inlining/
   monomorphization/DCE — the strongest possible answer to the brief's
   "cross-boundary optimization" question, and categorically
   unavailable to any dynamically-loaded (Kind C) plugin, no matter how
   the ABI is designed, because the optimizer needs the callee's IR at
   compile time, which a `dlopen`/WASM-instantiated module by
   definition doesn't have yet. This is the sharpest, most durable
   distinction in this whole RFC: **static linking is what buys
   cross-boundary optimization; genuine isolation (Kind C) forecloses
   it, permanently, independent of ABI cleverness** — not a gap to be
   engineered around, a real tradeoff to be named.

Neither is designed further here — flagged, with the concrete
trade-off named, as this RFC's own open question (below), the same
honest deferral RFC 0004 used for the WASM spike this RFC then went and
did.

## Critic (self-review — the two questions only: less safe? less fast?)

- **`Ty::Handle`'s kind name is a plain `String`, unchecked against
  anything.** Two unrelated plugins can both declare `handle(Session)`
  and `typeck.rs`'s ordinary structural equality (same as any other
  `Ty::Named` mismatch) will accept passing one where the other is
  expected, *type-checking cleanly*, then trapping or misbehaving at
  runtime inside whichever plugin's `HandleRegistry` doesn't recognize
  the id. **Less safe than it should be.** Mitigation available at
  zero further design cost: a plugin author should namespace kind names
  by crate (`"nirdosha_mysql::Connection"`, not `"Connection"`) — a
  convention, not a compiler guarantee, and this RFC should say so
  plainly rather than imply the collision is impossible. Not fixed here;
  named as a real residual gap, not glossed over.
- **`&handle(Kind)`'s borrow-checking is exactly as sound as `Ty::Ref`
  already is everywhere else — which is to say, sound against
  *aliasing*, not against a plugin's own internal misuse.** A plugin
  author's `call` closure receiving `Value::Ref(Box::new(Value::Int(id)))`
  can still `.remove(id)` through `HandleRegistry` inside what's
  declared as a *read-only* (`&handle`) builtin — nothing in the type
  system stops a plugin from lying about its own operation's real
  effect on the resource, the exact same "declared effects aren't
  verified" gap RFC 0004 already named for `Effect::Network`/etc. This
  RFC's fix narrows *where* a `.nir` program can go wrong; it does
  nothing for a plugin author who's careless or adversarial inside
  their own `call` closure. Restating RFC 0004's own honesty here on
  purpose: this is defense against accidental misuse, not against a
  malicious plugin.
- **The WASM spike's numbers are a lower bound on real Kind C cost, not
  an upper one.** `Engine::default()` was used with zero WASI, zero
  fuel/epoch-interruption metering, zero real capability restriction —
  the actual sandboxing machinery a *safe* Kind C would need (blocking
  arbitrary syscalls, bounding execution time against a hung/hostile
  guest) adds more overhead on top of the pure call/copy numbers
  measured here, not less. **Less fast than these numbers suggest**,
  once real isolation is turned on — flagged so a future Kind C RFC
  doesn't cite this one's numbers as a ceiling.
- **The 64 KB Cranelift-loop result (~1.9 ns/byte) was measured with
  `Engine::default()`'s default optimization settings, not verified
  against `Config::cranelift_opt_level(OptLevel::Speed)` explicitly, nor
  cross-checked against a hand-optimized native byte-loop at the same
  size.** It's reported as "the WASM guest's own compute cost," which
  is accurate, but this RFC did not isolate how much of it is
  Cranelift's own codegen quality vs. WASM's mandatory linear-memory
  bounds checks. Both are real Kind C costs either way, but a follow-up
  wanting to *optimize* Kind C specifically needs that breakdown, which
  this spike doesn't provide.
- **The `Ty::Handle` prototype adds a new hard keyword (`handle`)
  without going through this repo's own RFC-acceptance gate first** —
  it's built directly on this RFC's branch, ahead of a shepherd's
  sign-off, specifically so this document could show working, tested
  code instead of a paper design. That ordering is a deliberate choice
  for evidentiary strength (per the brief this RFC was written against:
  "Plugin proposals should be experimentally evaluated where possible"),
  not a claim that implementation should proceed before this RFC is
  accepted through the normal process (see Compatibility).

## Effect on the permission model

- `Ty::Handle` changes nothing about *what* `requires(role/claim:...)`/
  `effect(...)` can express — a handle-typed parameter typechecks like
  any other affine type. It does make one previously-invisible class of
  bug (double-close/use-after-close on a plugin resource) a real,
  named `OwnershipError` instead of a silent runtime `None`/`PluginError`
  a call late — a strict improvement to what the compiler can already
  prove, not a new annotation surface.
- The Kind C design sketch (§2) doesn't yet touch the permission model
  at all — no capability-bridging mechanism is proposed or built here.
  RFC 0004's own effect-based capability-disclosure design (its §2,
  "cheap, reuses existing machinery") is the right next layer once a
  real Kind C exists to gate; this RFC doesn't duplicate or supersede
  it.

## Compatibility

- **`Ty::Handle`, `handle(...)` syntax, and the four-file compiler
  change**: fully additive. `handle` becomes a reserved word (verified
  against every `.nir` file in this repo — none use it as a bare
  identifier, only as a substring like `handle_authorized`, which is
  unaffected). No existing `PluginBuiltin`, `.nir` program, or test
  changes behavior. 911/911 pre-existing tests in `cargo test -p
  nirdosha --no-fail-fast` still pass.
- **This RFC's own prototype code is on its branch
  (`rfc/plugin-boundary-safety-perf`), not proposed for merge as-is.**
  Per `GOVERNANCE.md`, a language-surface change (a new keyword, a new
  `Ty` variant) needs this RFC accepted and a shepherd assigned before
  landing on `main` — the working prototype exists so this document's
  claims are checked, not to pre-empt that process.
- The WASM spike's guest/host crates are evidence artifacts (this RFC's
  own scratch build, reproducible from the source included above), not
  a new workspace member — no `Cargo.toml`/CI footprint added by this
  RFC.

## Rejected alternatives

- **`abi_stable`/`stabby`-style stable-ABI `dlopen`, as a middle ground
  between Kind A (static, full trust) and Kind C (WASM, real isolation).**
  Real prior art (both crates do load-time layout verification, `sabi_trait`
  for FFI-safe trait objects) and worth naming precisely *because* it's
  easy to conflate with a safety win: it solves **ABI-mismatch crashes**
  (a plugin compiled against a different `rustc`/struct layout), not
  **memory isolation**. A `dlopen`'d `stabby`-verified plugin still runs
  in-process with full memory access — exactly as capable of violating
  every one of Nirdosha's guarantees as Kind A already is, with strictly
  worse tooling (no `cargo build`-time type-checking against the
  consuming project's own signatures) and none of the "explains itself
  as an ordinary Rust dependency" property `plugin.rs`'s own doc comment
  gives Kind A. Not pursued: it's a real answer to a question ("can I
  update a plugin without recompiling the host") this project hasn't
  asked yet, at real complexity cost, for zero safety benefit over Kind
  A's status quo.
- **A `Box<dyn Any>`-based generic handle type**, the option
  `nirdosha-plugin-support`'s own doc comment considered and rejected.
  This RFC's `Ty::Handle` finding (§1) is precisely that this was never
  the right comparison — the ownership-checker fix needs no downcasting
  at all. Restated here so a future reader sees both the original
  rejection and why it doesn't block the design this RFC ships.
- **Extending `ownership.rs`'s existing hardcoded `db_query`/`db_execute`-
  style per-name exemption to cover plugin "read" builtins.** Rejected:
  doesn't scale to a third-party plugin author, who cannot add their own
  builtin's name to a match arm inside the compiler. `&handle(Kind)`
  (§1) is the generalizing alternative, built on infrastructure
  (`Ty::Ref`) that already exists and needed zero `ownership.rs` changes.

## Open questions

- **The compiled-path plugin-call question (§3)** is genuinely open —
  this RFC frames the trade-off (stable scalar ABI vs. whole-program
  LTO vs. staying interpreter-only forever) without picking one. Worth
  its own follow-up RFC once/if native-speed plugin calls become a real
  deployment need, informed by real measurement of how much `db`/`json`/
  plugin-call-bound programs are actually left on the table by staying
  interpreter-only (not measured by this RFC).
- **Kind C's real design** (a generated typed shim, a capacity-planning
  story that accounts for payload-size scaling, sandboxing overhead this
  spike's numbers don't include) is sketched, not specified — this
  RFC's evidence is the argument for *why* it's worth a dedicated RFC
  once a real untrusted-plugin use case exists (still hypothetical
  today, per RFC 0004's own "no public plugin marketplace exists"), not
  the design itself.
- **Handle-kind namespacing** (the Critic's first finding): should
  `Ty::Handle`'s `String` be compiler-enforced-unique somehow (e.g.
  requiring a `nirdosha_schema`-style crate-qualified name,
  `docs/ECOSYSTEM.md`'s existing `[package.metadata.nirdosha]` convention),
  or left a convention plugin authors are documented to follow? Left
  for the shepherd/implementation PR — this RFC's prototype uses an
  unqualified name (`"Widget"`) for clarity, not as a recommendation.

## Evidence

Every number in this RFC came from a real run; methodology and raw
output below, so any of it can be independently re-run and checked
before being trusted further (same discipline `benchmarks/RESULTS.md`
already holds itself to).

**Machine**: Intel Core i7-8550U (4C/8T, 1.8 GHz base), Linux
7.0.10-zen1, `rustc`/`cargo` 1.100.0-nightly. Same machine
`benchmarks/RESULTS.md` uses.

### E0 — Kind A's `unsafe` footprint (a real, checked fact, not an aspiration)

```sh
$ grep -rn "unsafe" crates/plugin-example-*/src/ crates/plugin-support/src/
# (zero matches in every plugin crate's own logic)
```

### E1 — Dispatch-mechanism micro-benchmark

Isolates `is_builtin`'s linear scan (`ast::BUILTIN_NAMES.contains`, 48
real entries copied verbatim), the plugin path (`HashMap<String,
Arc<dyn Fn>>::get().cloned()` + indirect call — reproduced with
`plugin.rs`'s real `PluginFn` type), a real-builtin-shaped `match`
dispatch at a representative list position, and a zero-dispatch direct
call, all doing the identical real `rot13_call` body from
`crates/plugin-example-rot13/src/lib.rs`. 20,000,000 iterations per
case, best of 5, three independent runs (ns/call):

| | Run 1 | Run 2 | Run 3 |
|---|---:|---:|---:|
| `is_builtin` alone (guaranteed miss) | 61.71 | 53.52 | 66.62 |
| Plugin dispatch (miss + HashMap + `dyn Fn`) | 389.79 | 381.93 | 461.99 |
| Real-builtin dispatch (hit @23/48 + `match`) | 346.74 | 353.67 | 372.94 |
| Direct static call (floor) | 325.60 | 301.78 | 340.56 |

**Reading it**: the plugin-vs-real-builtin delta (43, 28, 89 ns across
the three runs) is small and dominated by `is_builtin`'s own scan cost
difference (a guaranteed-miss 48-entry scan vs. a hit partway through)
— **not** by `Arc<dyn Fn>` indirection or the `HashMap` lookup, which
this data shows costing close to nothing once `is_builtin`'s cost is
accounted for. The actionable, compiler-wide (not just plugin-specific)
finding: `is_builtin`'s `<[&str]>::contains` linear scan runs on the
path to *every* call in the interpreter — a `LazyLock<HashSet<&str>>`
or `phf`-generated perfect-hash set would cut this for every builtin,
user-function, and plugin call alike, and would matter increasingly as
more builtins/plugins accumulate (today's ~48-entry list is cheap
enough that this is a real but modest win, not an urgent one — flagged
as a genuine, cheap, broadly-applicable optimization this RFC's
research surfaced as a side effect, not its main subject).

Source: `plugin_bench/main.rs` (included with this RFC's evidence
directory — see below).

### E2 — WASM (Kind C) round-trip benchmark

`rot13_wasm_guest` (`wasm32-unknown-unknown`, `opt-level = 3`, `lto =
true`) called through `rot13_wasm_host` (`wasmtime` 49.0.0-rc.1,
`Engine::default()`). 55-byte payload: 2,000,000 iterations, best of 5.
61 KB payload: 200,000 iterations (full round trip) / 20,000 (repeat
runs), best of 5 — fewer iterations because each call now does real
O(n) work, not to hide variance.

```
Payload: 55 bytes
1. full round trip: alloc+copy-in+call+copy-out+dealloc   216.44–240.57 ns/call
2. call only, buffer pre-allocated + reused (no copy)     103.87–120.53 ns/call
3. copy-in + copy-out only (no call)                       22.20–25.72 ns/call

-- repeated with a 61 KB payload --
4. full round trip                                     118,690.88 ns/call
5. call only, buffer reused (no copy)                  118,501.54 ns/call
6. copy-in + copy-out only (no call)                     2,342.68 ns/call
```

Source: `rot13_wasm_guest/src/lib.rs` + `rot13_wasm_host/src/main.rs`
(included with this RFC's evidence directory).

### E3 — `Ty::Handle` prototype test results

```
$ cargo test -p nirdosha --test plugin_handle_ownership
running 3 tests
test double_close_on_a_plugin_handle_is_a_compile_time_ownership_error ... ok
test a_struct_holding_a_handle_is_affine_too ... ok
test single_use_then_close_still_works_and_runs ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p nirdosha --no-fail-fast
[... 74 test binaries ...]
911 passed; 0 failed
```

Source: `crates/compiler/tests/plugin_handle_ownership.rs`, plus the
four-file diff (`ast.rs`, `token.rs`, `parser.rs`, `codegen.rs`) on this
RFC's branch.

### Evidence artifacts

The WASM spike and micro-benchmark crates (`plugin_bench/`,
`rot13_wasm_guest/`, `rot13_wasm_host/`) are standalone Cargo projects,
not workspace members — kept alongside this RFC rather than in
`benchmarks/` (whose convention is head-to-head language benchmarks,
not compiler-internals micro-benchmarks) so a reviewer can `cargo run
--release` each one directly. Ask the shepherd for the archive if it
isn't already attached to this RFC's PR.
