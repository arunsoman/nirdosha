---
state: draft
shepherd: (unassigned)
---

# RFC 0004: Trust model for native (Kind A) plugins

## Motivation

A Kind-A plugin (`crates/compiler/src/plugin.rs`) is arbitrary native
Rust code, statically linked directly into whatever binary calls
`run_with_plugins`/`serve::run` — full process trust, no sandbox. This
cuts directly against Nirdosha's own value proposition, which is
entirely a *static-proof* story (the ownership/affine system, the
effect system, `smt.rs`'s bounds proving): a plugin can freely violate
every one of those guarantees from inside the same address space.

`docs/ECOSYSTEM.md` §G1 already flags this as needing "its own design
pass before Stage 1 ships publicly, not solved by this doc." This RFC
is that pass — for the *policy* question. It deliberately does not
design a full WASM sandboxing ABI (a separate, much larger effort,
scoped out below as its own future track).

## Design

Three options, evaluated together rather than in isolation, because
they solve different-sized slices of the same problem and aren't
mutually exclusive.

### 1. `TRUSTED_PLUGINS.md` — ship now, near-zero cost

A repo file (already added alongside this RFC) listing reviewed
plugins by crate name + version, with what a listing does and doesn't
mean stated explicitly (no runtime enforcement, no ongoing-version
guarantee — just "a maintainer read this version's source"). Modeled
on GitHub's Verified Publisher badge, not new registry infrastructure
— §G1 already rejected building a bespoke registry/hosting service for
this project's scale, and this file requires none: it's markdown, a
maintainer PR-reviews additions the same way any other PR is reviewed.

**What it actually buys**: social/process trust, the cheapest real
signal available before any auto-discovery mechanism exists. **What it
doesn't**: nothing stops a listed plugin from being wrong about its
own effects, or a future version from silently changing behavior — see
its own "isn't" section for the full, honest list.

### 2. Effect-based capability disclosure — cheap, reuses existing machinery

rfcs/0003-plugin-abi-v2.md's `PluginBuiltin.effects` field is already a
capability declaration in miniature — a plugin builtin's *type*
discloses which of `Rng`/`Io`/`Concurrent`/`Network` it touches,
checked statically today only against a *per-function* `effect(...)`
annotation (`typeck.rs`'s existing declared-vs-inferred subsumption
check, `crates/compiler/src/typeck.rs` around the `EffectNotDeclared`
site). Extending that same check to a **program-wide or
deployment-wide ceiling** — e.g. a `nirdosha serve --deny-effect
network` flag that fails startup if *any* reachable function
(including through a plugin) has `Effect::Network` in its inferred
set, reusing `effects::infer_effects_with_plugins`'s already-computed
result rather than adding a new analysis — is a small, principled
addition on top of infrastructure that already exists.

**Honest limitation, stated as plainly as `TRUSTED_PLUGINS.md`'s own**:
this is not a security boundary against an adversarial plugin. Nothing
verifies a plugin's declared `effects` are *true* — a plugin that
declares `effects: []` but performs a raw syscall defeats this
completely, the same way a listing on `TRUSTED_PLUGINS.md` doesn't
verify a plugin's *behavior* either. This is **defense-in-depth against
a well-meaning plugin's accidental scope creep** (a future version
quietly starts making network calls a deployment didn't expect), not
protection against a plugin author trying to lie. Worth building
specifically because it's cheap and reuses real infrastructure, not
because it solves the hard version of this problem.

*Not implemented by this RFC* — recorded here as the concrete shape a
follow-up PR should take, once accepted.

### 3. WASM sandboxing — explicit non-goal for Kind A, a real future Kind C

Genuine memory isolation would require compiling a plugin to
`wasm32-wasip1` and calling it through `wasmtime` instead of statically
linking it — real protection, but incompatible with Kind A's own
defining premise (`plugin.rs`'s doc comment: "a plugin crate is an
ordinary Rust dependency, compiled and statically linked... exactly the
same `cargo add`/`cargo build` story any other native Rust dependency
already has"). `PluginFn`'s shape (`Arc<dyn Fn(&[Value], Span) ->
Result<Value, RuntimeError>>`, passing Nirdosha's own Rust types
directly) has no meaning across a WASM guest boundary — it would need
an entirely different, serialized calling convention, i.e. a second,
incompatible plugin ABI existing alongside the native one.

**Position**: this is real, valuable, and **out of scope for Kind A**.
If genuine sandboxing is ever needed (e.g. a future public plugin
marketplace accepting unreviewed uploads — a scenario that doesn't
exist today), it should be designed and named as a new "Kind C"
(WASM-sandboxed extension packages), not retrofitted onto Kind A's
contract. A worthwhile first step whenever that need materializes: a
narrow spike compiling `rot13` (already pure, no I/O, the simplest
possible case) to WASM and measuring call overhead through `wasmtime`
— explicitly not designed here, flagged as its own future RFC once
that spike has real numbers.

## Recommendation

Ship (1) now — it's essentially free and is the day-one gate a future
Cargo-graph auto-discovery step (RFC 0001) should require before
linking an unfamiliar plugin automatically. Build (2) as a small,
real follow-up once a deployment actually wants it — the machinery
mostly already exists. Treat (3) as a genuinely separate, future
research track, not a checkbox this RFC can tick.

## Effect on the permission model

(1) changes nothing technical — pure process/documentation. (2), if
built, extends `effect(...)`'s existing enforcement from per-function
to per-deployment scope; no new annotation syntax, no change to what a
single function can already declare.

## Compatibility

Fully additive. No existing `.nir` program's behavior changes; (2)'s
`--deny-effect` flag is opt-in and defaults to today's behavior (no
ceiling) when absent.

## Rejected alternatives

- **A hosted plugin registry/marketplace with its own review
  pipeline.** Rejected in `docs/ECOSYSTEM.md` §G1 already, for the same
  reason RFC 0001 gives: hosting/uptime/abuse-moderation cost this
  project's scale doesn't need when Cargo/crates.io already solves
  distribution.
- **Retrofitting WASM isolation directly onto the existing
  `PluginBuiltin`/`NirdoshaPlugin` contract**, rather than a separate
  Kind. Rejected: the calling-convention mismatch is fundamental, not
  an implementation detail to paper over — see Design §3.
- **Cryptographic signing of plugin crates** (e.g. requiring a
  maintainer-signed manifest before linking). Considered, not pursued
  for this pass: solves "did a maintainer approve this exact bytes,"
  not "is this code safe to run," and Cargo/crates.io's own supply-chain
  story (checksums, yanking) already covers the former reasonably well
  for a crate published there; worth revisiting if `TRUSTED_PLUGINS.md`
  proves insufficient in practice.

## Open questions

- Whether (2)'s flag should be `nirdosha serve`-only or also apply to
  `run`/`build`/`emit-llvm` (the latter two are interpreter-only for
  plugins anyway per rfcs/0003, so the question mostly matters for
  `serve`'s deployment-time posture).
- What "unreviewed" should mean operationally once RFC 0001's
  auto-discovery exists — a hard failure if a declared plugin
  dependency isn't in `TRUSTED_PLUGINS.md`, or a warning a deployer can
  override? Left for RFC 0001's own implementation PR to decide,
  informed by this RFC's recommendation that it should be a gate, not
  silent.
