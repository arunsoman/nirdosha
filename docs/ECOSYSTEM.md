# Nirdosha — developer/production ecosystem (design discussion)

**Status: discussion only. Nothing in this document is implemented.**
This is the spec `docs/ROADMAP.md` Track G points to and will execute
against, written *before* the first line of it lands — the same order
`docs/NEXT_GEN.md`/`docs/MOBILE.md`/`docs/WORKFLOW.md`/`docs/TRANSACT.md`
were written in for their own features, not after.

## Why this came up

An outside read of the repo (2026-09-04) found the compiler core
unusually well documented — README/wiki, roadmap, issue/PR templates,
CI, releases, installers, signed/SBOM'd container workflows — but
named a real gap: the *adoption* infrastructure around the compiler is
thin. Five pieces, in the order this doc treats them:

1. **Package/stdlib economy** — no Nirdosha package manager/registry,
   no third-party `.nir` library ecosystem. Distribution today is
   "download a prebuilt CLI from GitHub Releases"; the Rust
   `Cargo.toml` is the *compiler's own* build manifest, not a `.nir`
   package system.
2. **Editor/tooling ecosystem** — no LSP, no tree-sitter grammar, no
   formatter, no debugger. The `cie` repo already documents this gap
   from the outside, describing Nirdosha as handled via AST dump for
   lack of either.
3. **Independent LLM validation** — `crates/bench/` (pass@1 +
   self-repair rate, 23 tasks) is real, but per `docs/ROADMAP.md`'s own
   compliance table it has only ever run against mock models, never a
   real one. The flagship "an LLM can write this" claim is unverified.
4. **Production/ops ecosystem** — largely *already* tracked under
   `docs/ROADMAP.md` Track A (containerization, health probes, presence
   gateway are `[DONE]`/`[PARTIAL]`); what's still genuinely open there
   (OTLP export, compat/versioning policy, real Windows verification,
   macOS Z3 vendoring) is Track A's job, not a new one — see G4 below
   for why this doc doesn't re-litigate it.
5. **Community/governance depth** — the project is effectively
   solo-maintained (GitHub contributors: `arunsoman` only, 94
   contributions as of this writing). No RFC process, no bus-factor
   resilience.

Each gets its own section below, sized to how much *new* design each
actually needs — G1 gets the deepest treatment since it's the one with
a concrete proposal on the table ("use Cargo itself"); G4 is
deliberately short because Track A already owns it.

---

## G1 — Package/stdlib economy: can Cargo itself be the package manager?

### The proposal as stated

> Can we use the existing Rust package manager itself, where people
> could install the required package from Rust/crates.io and then
> Nirdosha's compiler will take care of the rest?

### Why this is more promising than "build a `nirpkg` registry"

A bespoke Nirdosha registry (host a service, run a CLI like `nirpkg
publish`, own the uptime/security/abuse-moderation of a package index)
is the expensive option — exactly the kind of new stateful service a
solo-maintained project (see G5) can least afford. Cargo/crates.io
already solved dependency resolution, semver, lockfiles, yanking, and
hosting. Reusing it is the right instinct — but "use Cargo" actually
covers two different kinds of package that get conflated by the one
sentence above, and they need different treatment.

### Two kinds of package, one word

**Kind A — native/builtin extension packages.** Real Rust code that
adds new builtin functions (a crypto library, a PDF renderer, a
barcode reader) — the kind of thing that has to compile *into* the
`nirdosha` binary because it's calling out to real system/library
code, the same way today's builtins already do
(`crates/compiler/src/`'s builtin registry — the "0.5: builtin
registry" line item `docs/ROADMAP.md` already marks `[DONE]`). For this
kind, Cargo is *already* the right and sufficient mechanism —
`cargo add some-nirdosha-plugin` genuinely works as stated, because
the artifact being fetched is ordinary compiled Rust. What's actually
missing is a **packaging convention**, not new package-manager
machinery:

- A `NirdoshaPlugin` trait (or equivalent) a crate implements to
  register new builtins into the same table `typeck.rs`/the
  interpreter already consult.
- A `[package.metadata.nirdosha]` block in that crate's `Cargo.toml`
  declaring the `.nir`-visible signatures it provides (name, arg
  types, return type, capability requirements — reusing whatever
  `validate`'s Hoare-contract machinery already models for
  pre/post-conditions where relevant).
- A build step — `nirdosha build --with <crate>` or a project-level
  `Cargo.toml` `[dependencies]` list the compiler reads — that
  assembles a project-specific `nirdosha` binary linking the declared
  plugin crates in, and emits the merged `.nir`-facing signature table
  so `typeck.rs` can check calls into it statically, same as any other
  builtin.

**Kind B — pure `.nir` library packages.** No Rust code at all — a
shared set of `.nir` function/screen/workflow definitions (say, a
common `audit_log` module, or a library of `field { render: ... }`
widget patterns). Cargo's registry protocol doesn't actually require
compiled Rust — a crate can legally contain nothing but data files and
a no-op `build.rs` — so crates.io *can* host these as a distribution +
versioning mechanism. But nothing today teaches Nirdosha's own module
system to look there: F2 (`docs/ROADMAP.md`'s real module/package
system — namespacing, visibility, `use`) currently resolves `use`
against local paths only. Making `use some_crate::mod` fetch a
crates.io-hosted `.nir`-source-only crate needs:

- A `[package.metadata.nirdosha] kind = "nir-lib"` marker so tooling
  can tell a Kind A (native) crate from a Kind B (source-only) crate
  at fetch time, before deciding whether to compile it or just unpack
  its `.nir` files.
- F2's resolver taught to shell out to `cargo metadata`/`cargo fetch`
  for `use` targets it can't find locally, then read the fetched
  crate's `.nir` sources directly rather than any compiled artifact.

### Recommended sequencing

1. **Stage 1 — Kind A only.** Lowest engineering cost, and it
   delivers "install a package via `cargo add`" for real, immediately.
   Define the plugin trait + metadata convention; re-package one
   existing real builtin as the reference/example plugin to prove the
   shape works end-to-end before asking any third party to build one.

   **2026-09-04 — built and verified.** `crates/compiler/src/plugin.rs`:
   `NirdoshaPlugin` trait + `PluginBuiltin`/`PluginFn`, plus real hook
   points (additive, no existing call site's behavior changed) in
   `typeck.rs` (`Checker::plugins`, `typecheck_with_plugins`,
   `is_builtin_or_plugin` at all five sites that used to gate on
   `ast::is_builtin` alone: registration-time shadow checks, spawn/
   `transact`-slot rejection, `infer_call`'s own dispatch,
   `infer_acquire`) and `interpreter.rs` (`Interpreter::plugins`,
   `with_plugins`, a dispatch arm in `Expr::Call`, propagated to every
   spawned child `Interpreter` the same way `tracer`/`sandbox_exe`
   already are) and a new `lib.rs::run_with_plugins` entrypoint. A new
   `ErrorKind::PluginError` variant gives a plugin builtin a real,
   spanned failure path (and `observability.rs`'s exhaustive
   `error_kind_name` match — a new variant is a compile error there by
   design — got its required arm).

   `crates/plugin-example-rot13/` is the one real reference plugin: a
   `[package.metadata.nirdosha]`-annotated crate contributing
   `rot13(s: str) -> str`, depending on `nirdosha` the same way a real
   third-party crate would. `tests/end_to_end.rs` runs real `.nir`
   source through the actual pipeline and checks: the call resolves and
   returns the right value; wrong arity and wrong argument type are
   both caught as real *type* errors (not runtime panics); and with no
   plugin registered, `rot13` is correctly unresolvable. `cargo test -p
   nirdosha-plugin-rot13`: 6/6 passing. Full existing suite reverified
   unaffected: `cargo test -p nirdosha --no-fail-fast` — every target
   green except `tests/mq.rs`'s 2 tests, which fail identically without
   this change too (`Connection refused` — they need a real local Redis,
   an environment gap, not a regression).

   Also runnable directly, not just tested: `cargo run -p
   nirdosha-plugin-rot13 --example run -- crates/plugin-example-rot13/
   examples/scramble.nir` prints `Aveqbfun` — the literal "install a
   package via Cargo" proposal, executed. `crates/plugin-example-rot13/
   README.md` has the full walkthrough for both sides: what a plugin
   author writes (the `NirdoshaPlugin` impl + `[package.metadata.
   nirdosha]` block) and what a consuming project writes today (a small
   entrypoint calling `run_with_plugins` — see "What Stage 1 does *not*
   cover" just below for why that's still a hand-written entrypoint,
   not a CLI flag).

   **What Stage 1 does *not* cover, honestly disclosed:** `serve`/
   `emit-ui`/`emit-llvm` never see `plugins` — only the plain-interpreter
   `run_with_plugins` path does (real future work, not silently implied).
   There's no `Cargo.toml`-driven auto-discovery yet — a project wanting
   a plugin writes a small custom entrypoint calling `run_with_plugins`
   itself, rather than the standard `nirdosha` CLI finding it
   automatically; that auto-discovery layer, and the security/sandboxing
   open question below, are what's left before this is safe to hand to
   a real third party.
2. **Stage 2 — Kind B.** Only after Stage 1 is real and F2 itself has
   had more mileage; needs the resolver work above plus a decision (see
   open questions) on whether crates.io is even the right home for
   these versus a lighter convention.

Explicitly **not** recommended as a first move: a bespoke registry or
CLI. That's the alternative this proposal is trying to avoid, and
nothing above requires it.

### Open questions (real, not hand-waved)

- **Security.** A Kind A plugin is arbitrary native code linked into
  the binary — full execution, no sandbox. That cuts directly against
  Nirdosha's own memory/overflow-safety-proof value proposition unless
  plugins are either vetted somehow or compiled to WASM and sandboxed
  instead of natively linked. Worth its own design pass before Stage 1
  ships publicly, not solved by this doc.
- **Two version resolvers.** Once F2's own module/visibility system
  and Cargo's semver both exist for Kind B packages, a project can end
  up with two overlapping notions of "which version of X am I using."
  Needs a stated precedence rule, not silent overlap.
- **Is crates.io the right home for Kind B at all?** Mixing a
  systems-language registry with an application-DSL's snippet-library
  ecosystem is a real culture mismatch (crates.io's own conventions —
  `lib.rs`, doctests, `#[no_std]` badges — don't apply). A lighter
  git-dependency convention (closer to how Go modules work) may fit
  Kind B better; worth deciding before Stage 2, not defaulting into
  crates.io by inertia.

---

## G2 — Editor/tooling ecosystem

**Problem:** no LSP, no tree-sitter grammar, no formatter, no
debugger. `cie` (a related repo) already frames this from the outside:
Nirdosha is "a language with no LSP and no tree-sitter grammar,"
worked around via AST dump.

**Plan, in build order (cheapest/highest-leverage first):**

1. **Tree-sitter grammar.** `crates/grammar_check/` already
   cross-checks the hand-written LL(1) parser against a real LALR(1)
   generator — that's the authoritative grammar source. A
   `grammar.js` should be *derived from and checked against* that, not
   hand-authored independently, to avoid the two grammars drifting
   apart the way `docs/GRAMMAR.md` already had to reconcile once for the
   LL(1)/LALR(1) pair.
2. **Minimal LSP.** Diagnostics first — the checker (`typeck.rs`) and
   `validate`'s Hoare-contract checker already produce real structured
   errors; wiring those through `textDocument/publishDiagnostics` is
   mostly plumbing, not new analysis. Go-to-definition next, riding
   F2's own module resolution. Hold off on refactors/code actions for
   a later pass.
3. **VS Code extension** as the first LSP client — largest install
   base, lowest-friction distribution (marketplace, not a registry
   this project has to run).
4. **Formatter last**, deliberately — there's no documented canonical
   style yet to format *toward*; that decision has to exist before a
   formatter can be non-controversial.

---

## G3 — Independent LLM validation

**Problem:** `crates/bench/` is real infrastructure (pass@1 +
self-repair rate, 23 tasks) but per `docs/ROADMAP.md`'s standards table
(row for "AI and ML" / ISO 42001 applicability) it has only ever been
exercised against mock models — never a real one, and with no
stdout/sandbox scoring path built out. The project's own flagship
claim ("an LLM can write Nirdosha") is currently unverified by its own
evidence.

**Plan:** this is a gap-closing task, not a new-infrastructure task —
the harness already exists. Wire one real model behind the existing
harness interface (start with a single provider/cheap-tier path, not a
multi-provider matrix), run the existing 23 tasks once, and publish
the real numbers even if they're mediocre. A real bad number is
strictly more valuable here than another mock-model run, because it's
the thing actually missing.

---

## G4 — Production/ops ecosystem

**This is deliberately the shortest section.** The outside critique's
list here (kill-mid-transaction durability, deployment story, OTLP
wiring, compat/versioning policy, real Windows verification, macOS Z3
vendoring) maps almost one-to-one onto `docs/ROADMAP.md` Track A, which
already tracks each item with real status:

- Kill-mid-transaction durability — **A1, `[DONE]`**, including the
  real bug it found and fixed.
- Deployment/containerization — **A2, `[PARTIAL]`**, most of it real
  and verified (Docker, Helm, Kustomize, health probes, graceful
  shutdown); named gaps stay named there, not repeated here.
- OTLP/observability — **A3, `[OPEN]`** past layer 2a.
- Compat/versioning policy — **A4, `[OPEN]`**.

Nothing new to design here; the outside read confirms Track A is the
right existing bucket for all of it, not that a new one is needed. Any
future work on these items goes into `docs/ROADMAP.md` Track A directly,
not this doc.

---

## G5 — Community/governance depth — `[DONE]` 2026-09-04

**Problem, as of this doc's original writing:** solo-maintained.
GitHub's own contributor graph showed `arunsoman` only, 94
contributions — no RFC process for breaking changes, no bus-factor
resilience if that one person was unavailable.

**Closed 2026-09-04.** Everything this section flagged now has a real
document or a real GitHub setting behind it, not just this design doc:

- **RFC process** — [`rfcs/README.md`](../rfcs/README.md), seeded with
  two real drafts: [`rfcs/0001-package-manifest-format.md`](../rfcs/0001-package-manifest-format.md)
  (this section's own G1) and
  [`rfcs/0002-editor-tooling-lsp-tree-sitter.md`](../rfcs/0002-editor-tooling-lsp-tree-sitter.md)
  (G2). The breaking-change policy referenced below lives in
  [`GOVERNANCE.md`](../GOVERNANCE.md) and
  [`CONTRIBUTING.md`](../CONTRIBUTING.md#breaking-changes), and the
  str-ban precedent that motivated it is recorded (after the fact) as
  [`docs/adr/0002-ban-str-in-fn-signatures.md`](./adr/0002-ban-str-in-fn-signatures.md).
- **Real maintainer access.** Confirmed via GitHub's API (not just the
  Helm chart field this section originally flagged as insufficient):
  `lekshmideepu`, `maheshmindlabs`, plus `arulrajan123` and
  `Baskarrajcodeflow`, all hold real repo write access today. See
  [`MAINTAINERS.md`](../MAINTAINERS.md) for the honest read on this —
  access exists, but three of the four have no commits/reviews on
  record yet, so the practical bus factor is still closer to 1–2 than
  5 until they're activated into an assigned area.
- **Branch protection** — enabled on `main` 2026-09-04: 1 required
  approving review, green `build`/`build-windows` CI, no force-push/
  delete, admin bypass kept for genuine emergencies. Details:
  [`GOVERNANCE.md`#branch-protection](../GOVERNANCE.md#branch-protection).
- **ADRs** for decisions made outside the RFC process:
  [`docs/adr/`](./adr/README.md).
- **Areas/ownership** — [`AREAS.md`](../AREAS.md), consumed by
  [`.github/CODEOWNERS`](../.github/CODEOWNERS).
- **Contributor funnel** — 48h triage SLA
  (`CONTRIBUTING.md`#response-time), GitHub Discussions (already
  enabled), and the full label set live on GitHub, reconciled with
  [`.github/labels.yml`](../.github/labels.yml).
- **Release credentials** — audited, not changed: `release.yml`/
  `docker.yml` already authenticate via the ephemeral `GITHUB_TOKEN`/
  OIDC, not a personal token (`gh secret list` returns none). Signed
  release tags are documented as policy in `GOVERNANCE.md` but not yet
  enforced — needs each maintainer's signing key registered first, a
  real follow-up, not done here.

---

## Suggested order

G1 Stage 1 (Cargo-plugin convention) and G2's tree-sitter grammar are
the two highest-leverage, lowest-risk starting points — both are
additive (nothing existing has to change), both have a concrete first
artifact (one reference plugin; one grammar file checked against
`crates/grammar_check/`), and both are the kind of thing a new
contributor could plausibly pick up — which itself now has somewhere
real to land, per G5's `rfcs/`. G3 (wire a real model into the
existing bench harness) is similarly small and fast, and closes a gap
the project's own docs already admit to. G4 stays inside Track A. G5
itself is `[DONE]` (see above) — what's left there is activation
(named maintainers actually using their access), not more design or
process.
