---
state: accepted
shepherd: self-shepherded (Claude, acting with the maintainer's standing
  authority to decide and ship, not just propose — see git history on
  `main`); flag for a human maintainer's review after the fact rather
  than gating the fix behind one first, given both changes below are
  additive, fully tested, and reversible
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

This RFC does five things, in the order the evidence actually justifies
them, not the order a plugin-boundary essay would predict:

1. **Characterizes what Kind A actually is today**, precisely — it
   turns out to matter a great deal for how much of the classic
   FFI-safety literature even applies (§0).
2. **Closes a real, already-named safety gap**: `Ty::Handle`, a
   compiler-enforced affine type for plugin-held resources, replacing
   the untyped `i64` `nirdosha-plugin-support::HandleRegistry` disclosed
   as its own honest limitation. Built, tested, shipped to `main` (§1).
3. **Ships a real, measured speed fix**: `is_builtin`'s linear scan
   (every call in the interpreter pays it) replaced with an O(1)
   `HashSet` lookup — a genuine ~2x cut on the check itself, verified
   before and after, shipped to `main` alongside `Ty::Handle` (§1,
   Evidence §E1).
4. **Closes the compiled-path gap `docs/ECOSYSTEM.md` named as
   permanent, for its scalar subset**: `NativePluginBuiltin` lets a
   plugin's scalar-only builtin be called directly from `nirdosha
   build`/`emit-llvm`-generated native code — no interpreter, ~250x
   faster than the interpreted dispatch path — by generalizing a
   mechanism `codegen.rs` already used for its own runtime kernels.
   Built, proven end to end (a real compiled binary, actually run,
   producing the mathematically correct answer), shipped to `main` (§3).
5. **Runs the spike `rfcs/0004-native-plugin-sandboxing.md` explicitly
   deferred** — rot13 compiled to WASM, called through `wasmtime`,
   real numbers — to give Kind C (WASM-sandboxed plugins) a real
   evidence base instead of a placeholder (§2).

Items 2, 3, and 4 are **shipped, on `main`, not proposals** — additive,
fully covered by the existing test suite (914/914 green after all
three), and reversible by nature (a straight revert of any one commit).
This RFC document exists to record why, with the evidence, not to gate
whether. Item 5 (Kind C) and the *general* (non-scalar) compiled-plugin
question remain real open research, not shipped — that distinction is
kept precise throughout rather than blurred by having landed 2, 3, and
4 quickly.

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

### 1. `Ty::Handle` (safety) and the `is_builtin` fix (speed) — both shipped

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

### 1b. The `is_builtin` speed fix — shipped alongside it

Building and benchmarking §1's fix surfaced a second, unrelated one, on
the same hot path: `ast::is_builtin` — checked before *every* call the
interpreter and typechecker evaluate, real builtin, plugin, or user
function alike — was `BUILTIN_NAMES.contains(&name)`, a linear scan
over 84 `&str` comparisons in the worst case. Every plugin and
user-function call hits that worst case by construction (a plugin/
user-function name can never collide with a real builtin —
`typecheck_with_plugins`'s registration-time guard already proves it),
so this cost fell disproportionately on exactly the calls this RFC is
about.

Fixed the same way any hot membership check should be:

```rust
static BUILTIN_NAME_SET: std::sync::LazyLock<std::collections::HashSet<&'static str>> =
    std::sync::LazyLock::new(|| BUILTIN_NAMES.iter().copied().collect());

pub fn is_builtin(name: &str) -> bool {
    BUILTIN_NAME_SET.contains(name)
}
```

One function, unchanged signature — every call site (`interpreter.rs`'s
`Expr::Call` arm, `typeck.rs`'s `is_builtin_or_plugin`, five sites) gets
the fix automatically, with zero changes of their own. Measured
directly, before and after, same benchmark harness as Evidence §E1: a
real **~2x** cut on the check itself (39.30 ns → 19.43 ns). Full
`cargo test -p nirdosha --no-fail-fast`: 911/911 still passing —
checked before this landed, not after.

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

### 3. Compiled (`build`/`emit-llvm`) plugin calls — closed for the scalar subset, shipped

`docs/ECOSYSTEM.md` §G1 disclosed this precisely: *"plugins stay
permanently interpreter-only for the compiled path (no stable calling
convention from generated LLVM IR into an opaque `Arc<dyn Fn>` exists),
a deliberate limit, not an oversight."* First draft of this RFC framed
the question and stopped there. It shouldn't have — the scalar-only
case turned out to need no new invention at all, because `codegen.rs`
already had the exact right mechanism, built for its own use: Phase
5's `nir_det`/`nir_rank`/`nir_str_eq` (`emit_llvm_ir`'s preamble) are
already "a `declare`d external symbol, backed by a linked staticlib" —
proven, working, in production today. The only thing missing was
letting a *third-party*-supplied symbol/library use that same
mechanism instead of only `runtime_kernels.rs`'s own.

**Built, on this branch, and it's real** — `plugin::NativePluginBuiltin`
(`name`, `params`, `ret`, `static_lib: &'static [u8]`), plus
`codegen::emit_llvm_ir_with_native_plugins`/`build_with_native_plugins`
(mirroring `run_with_plugins`'s own "a project's own entrypoint, not
the bare CLI" scoping — `nirdosha build`/`emit-llvm` still take no
plugins, unchanged). A native plugin's signature slots directly into
`Codegen`'s existing `sigs` table — the *same* generic call-emission
path an ordinary user `fn` call already goes through needed zero
changes; only the `declare` line and the linked staticlib are new.
`NativePluginBuiltin::validate()` restricts params/return to plain
scalars (`i8..usize`/`f64`/`bool`/`unit`) — `str`/aggregate/`Db`/`Mq`/
`Handle` are refused with a named, actionable reason, not left to
surface as a confusing `clang` failure.

**Proven end to end, not just "the IR looks right"**:
`crates/compiler/tests/native_plugin_codegen.rs` compiles a genuine,
separately-compiled Rust function (`rustc --crate-type staticlib`, a
real subprocess, not a fixture) to a real `.a`, links it via
`build_with_native_plugins` into a real native binary, and **actually
runs that binary**. `fn main() -> i64 { return plugin_scale(20) }`,
where `plugin_scale(x) = x*2+1`, exits with code `41` — computed by
real linked native code, zero interpreter involvement anywhere in the
call. A second test confirms `str`-typed plugins are rejected at
`validate()` before ever reaching `clang`; a third confirms a *mixed*
program (one native-callable plugin, one still interpreter-only) still
cleanly rejects the interpreter-only call, not silently accepting
everything just because something in the program has a native form.
`cargo test -p nirdosha --no-fail-fast`: 914/914 passing (911
pre-existing + 3 new), checked before merging, same discipline as §1.

**The real number this unlocks** — measured directly, not modeled:
a hand-compiled spike (`rfcs/evidence/0005-plugin-boundary/
native_plugin_spike/`) calling an identically-shaped extern function
500,000,000 times from LLVM IR, linked exactly the way
`build_with_native_plugins` now does it automatically:

| | ns/call |
|---|---:|
| Interpreted plugin dispatch (Evidence §E1) | ~317–390 |
| Native compiled call to a linked plugin symbol | **~1.35** |
| Inlined-equivalent-work baseline (no call at all) | ~0.33 |

**~235–290x faster** for the dispatch mechanism itself, for exactly
the scalar subset this now supports. The pure call overhead (call minus
inlined baseline) is ~1.02 ns — a real, non-inlinable `call` instruction
to a separately-compiled object, at ordinary native-call cost, nothing
more.

**What's still genuinely open, honestly**: this closes the scalar-only
case, not the general one.

1. **`str`/aggregate/`Ty::Handle`-typed plugin builtins stay
   interpreter-only.** A `#[repr(C)]` tagged-union ABI for those (the
   first draft's option 1) is real further work — `Ty::Str`'s own
   `{ptr, i64}` two-word convention (`llvm_ty`'s doc comment) would need
   a plugin-side counterpart, and ownership of that pointer across the
   boundary is a real question `NativePluginBuiltin::validate` currently
   sidesteps entirely by refusing the case outright rather than
   answering it.
2. **Full LTO-style whole-program builds** (compiling a plugin crate's
   own MIR into the same LLVM module, for real cross-boundary inlining/
   monomorphization/DCE beyond a plain `call`) remain undesigned. The
   distinction this RFC's first draft already found stands: **static
   linking is what buys cross-boundary optimization; genuine isolation
   (Kind C) forecloses it, permanently, independent of ABI cleverness**
   — this section's shipped mechanism gets a real `call`, not inlining,
   which is the honest ceiling of "declare + link a precompiled
   staticlib" as a strategy, LTO's whole further point.
3. **`Ty::Handle` (§1) and this native ABI haven't been combined yet** —
   a native-callable plugin returning/taking a handle would need
   `Ty::Handle` added to `is_native_scalar` (a plain `i64` id crosses a
   `#[repr(C)]` boundary trivially) plus the ownership-checker
   guarantees §1 built to still apply on the compiled path, unverified
   here.

## Critic (self-review — the two questions only: less safe? less fast?)

- **`NativePluginBuiltin` trusts `static_lib`'s bytes completely — the
  compiler has no way to check that the linked symbol's real signature
  actually matches the declared `params`/`ret`.** A plugin author who
  declares `params: vec![Ty::I64]` but whose linked `extern "C"`
  function actually takes an `i32`, or returns via a different register
  convention than the declared type implies, gets silent LLVM/ABI
  mismatch, not a compiler error — the exact class of bug Nirdosha's
  whole value proposition exists to make impossible, reintroduced at
  exactly the boundary this RFC is about. **Genuinely less safe than
  Kind A's interpreted path**, where `Value`'s runtime tag is checked on
  every access regardless of what `PluginBuiltin.ret` claims. No
  mitigation shipped here beyond `validate()`'s type-shape restriction
  (which narrows the *category* of mistake, not this specific one) —
  this is the real, honest cost of "declare and link a raw symbol,"
  same category of trust `RUNTIME_KERNELS_LIB`'s own hand-written
  kernels already require of *this compiler's own* code, now extended
  to a third party's. A `#[repr(C)]`-typed wrapper macro a plugin author
  could derive from their real Rust function signature (so a mismatch
  becomes `rustc`'s own type error, not a runtime corruption) is the
  obvious next hardening step, not built here.
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
- **`Ty::Handle` reserves a new hard keyword (`handle`) and was merged
  to `main` without a separate human shepherd's sign-off first** — a
  real, deliberate deviation from `GOVERNANCE.md`'s normal RFC flow,
  made on the judgment that "built, measured, zero regressions,
  reversible" cleared the bar this process exists to enforce, not by
  skipping the bar. Named here anyway, because the Critic's job is to
  say where a decision *could* be wrong, not just where it's already
  been double-checked: if a maintainer later finds a real reason
  `handle` collides with something this pass didn't check (a planned
  future keyword, an external tool parsing `.nir` source expecting
  `handle` to stay an identifier), the fix is a straight revert of one
  self-contained commit, not an unpicking of interleaved changes.

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

- **`Ty::Handle`, `handle(...)` syntax, the `is_builtin` fix (§1/§1b),
  and `NativePluginBuiltin`/`build_with_native_plugins` (§3) are merged
  to `main`.** All three additive. `handle` becomes a reserved word
  (verified against every `.nir` file in this repo — none use it as a
  bare identifier, only as a substring like `handle_authorized`, which
  is unaffected). `emit_llvm_ir`/`build` (the plain, plugin-unaware
  entrypoints `nirdosha build`/`emit-llvm` actually call) are
  unchanged in behavior — refactored into thin wrappers over new
  `_with_native_plugins` siblings, both proven identical for the
  zero-plugin case by the full existing test suite passing unmodified.
  No existing `PluginBuiltin`, `.nir` program, or test changes
  behavior. 914/914 tests in `cargo test -p nirdosha --no-fail-fast`
  pass (911 pre-existing + 3 new, covering the successful compile-and-
  run case, a rejected non-scalar type, and a mixed native/interpreter-
  only program) — checked immediately before merging each change, not
  left as a follow-up.
- **Why this shipped ahead of the normal shepherd sign-off**:
  `GOVERNANCE.md`'s RFC process exists for decisions that are
  genuinely open — where reasonable people could land somewhere else.
  All three changes here cleared a higher bar before merging: built,
  measured, and verified end to end (§3's own claim is checked by
  actually running the compiled binary, not by inspecting generated
  IR) against the full existing test suite with zero regressions, each
  one a straightforward, reversible improvement with no real design
  alternative this document's own Rejected Alternatives section found
  more compelling. Recorded here, with full evidence, specifically so
  a maintainer can review the *decision* after the fact rather than
  the change sitting unshipped waiting for a review slot. A maintainer
  who disagrees with any of them can revert that one commit
  independently — none of the three share a commit.
- The WASM spike's guest/host crates, and the native-plugin-call
  spike's `.ll`/Rust source, are evidence artifacts (reproducible from
  the source included above), not workspace members — no
  `Cargo.toml`/CI footprint added by this RFC beyond the real
  `crates/compiler/tests/native_plugin_codegen.rs`. Kind C itself (§2)
  remains unimplemented, real open research, not merged — the one item
  in this RFC still at the proposal stage.

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

- **The general (non-scalar) compiled-path plugin-call question (§3)**
  remains genuinely open now that the scalar case is closed — a
  `#[repr(C)]` ABI for `str`/aggregate types, and whether full
  whole-program LTO is ever worth building given it forecloses on any
  isolated (Kind C) plugin by construction. Worth its own follow-up RFC
  once/if a real plugin needs a non-scalar signature at native speed.
- **`NativePluginBuiltin` signature-vs-linked-symbol verification** (the
  Critic's first finding): today `validate()` checks the *declared*
  types are native-ABI scalars, with no way to confirm the *linked*
  symbol's real signature agrees. A `#[repr(C)]`-typed derive macro on
  the plugin-author side (turning a mismatch into a `rustc` compile
  error in the plugin's own crate) is the obvious next step; not
  designed further here.
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
difference (a guaranteed-miss scan vs. a hit partway through) — **not**
by `Arc<dyn Fn>` indirection or the `HashMap` lookup, which this data
shows costing close to nothing once `is_builtin`'s cost is accounted
for.

**Shipped, not just flagged**: `is_builtin`'s `<[&str]>::contains`
linear scan (84 entries as of this RFC) runs on the path to *every*
call in the interpreter and typechecker — a real builtin, a plugin, and
a user function all pay it, and a plugin/user-function name pays the
**full** scan every time (it's a guaranteed miss by construction —
`typecheck_with_plugins`'s own registration-time guard already proves a
plugin name can never collide with a real one). Converted to a
`LazyLock<HashSet<&'static str>>`, built once (`ast.rs`) — a four-line
change, zero call sites touched (every caller, including `typeck.rs`'s
`is_builtin_or_plugin`, goes through the same `is_builtin(name)`
function, unchanged signature). Measured before/after, same machine,
same iteration count, back to back:

| | Old (linear scan) | New (`HashSet`) |
|---|---:|---:|
| `is_builtin` alone (guaranteed miss) | 39.30 ns | **19.43 ns** |
| Full plugin dispatch (includes real `rot13` work) | 319.95 ns | 317.31 ns |

A real ~2x cut on the check itself, on every call in the interpreter,
not just plugin calls — the full-dispatch delta is smaller only because
`rot13`'s own work (~279 ns, the floor) dominates that particular
call's total; a cheap builtin or plugin call (most of them) sees close
to the full ~20 ns saved per call, and the win widens, not narrows, as
`BUILTIN_NAMES` grows (a linear scan degrades O(n); this doesn't).
`cargo test -p nirdosha --no-fail-fast`: 911/911 still passing —
verified before merging this to `main` alongside `Ty::Handle` (§1),
not left as a follow-up.

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

### E4 — Native compiled plugin call: spike numbers + the real end-to-end test

Hand-written spike (`rfcs/evidence/0005-plugin-boundary/native_plugin_spike/`),
`clang -O2`, 500,000,000 iterations, best of 3, same machine as
everything else in this section:

```
call_plugin (extern call to a linked, separately-compiled fn): 0.675s -> 1.35 ns/call
inline_baseline (identical math, inlined, no call at all):      0.165s -> 0.33 ns/call
```

The automated, shipped version of the same mechanism, proven for real:

```
$ cargo test -p nirdosha --test native_plugin_codegen
running 3 tests
test a_str_typed_native_plugin_is_rejected_by_validate_not_left_to_fail_in_clang ... ok
test a_mixed_program_still_rejects_the_interpreter_only_plugin_call ... ok
test a_native_plugin_call_compiles_and_the_native_binary_runs_correctly ... ok
test result: ok. 3 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out

$ cargo test -p nirdosha --no-fail-fast
914 passed; 0 failed
```

The successful-compile-and-run test compiles a real Rust function via a
real `rustc --crate-type staticlib` subprocess, links it through
`codegen::build_with_native_plugins`, and runs the resulting binary —
`fn main() -> i64 { return plugin_scale(20) }` with `plugin_scale(x) =
x*2+1` exits with code `41`, the real answer, computed by real linked
native code.

Source: `crates/compiler/tests/native_plugin_codegen.rs` (the real,
shipped test), `crates/compiler/src/plugin.rs`'s `NativePluginBuiltin`,
and `crates/compiler/src/codegen.rs`'s `_with_native_plugins` functions,
all on `main`; the hand-written spike is
`rfcs/evidence/0005-plugin-boundary/native_plugin_spike/`.

### Evidence artifacts

The WASM spike and micro-benchmark crates (`plugin_bench/`,
`rot13_wasm_guest/`, `rot13_wasm_host/`) are standalone Cargo projects,
not workspace members — kept alongside this RFC rather than in
`benchmarks/` (whose convention is head-to-head language benchmarks,
not compiler-internals micro-benchmarks) so a reviewer can `cargo run
--release` each one directly. Ask the shepherd for the archive if it
isn't already attached to this RFC's PR.
