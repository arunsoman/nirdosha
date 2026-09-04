---
state: draft
shepherd: (unassigned)
---

# RFC 0001: Package manifest format (Cargo-based package manager)

## Motivation

`docs/ECOSYSTEM.md` §G1 works through the proposal "use Cargo itself
as Nirdosha's package manager" and splits it into two kinds of
package that need different treatment:

- **Kind A — native/builtin extension crates.** Real Rust code adding
  new builtins (crypto, PDF rendering, barcode reading), linked
  statically into the `nirdosha` binary.
- **Kind B — pure-`.nir` library packages.** No Rust code — shared
  `.nir` function/screen/workflow definitions, distributed via
  crates.io's registry protocol even though nothing in them compiles.

**Kind A's runtime mechanism already exists and is real**, not
proposed by this RFC: [`crates/compiler/src/plugin.rs`](../crates/compiler/src/plugin.rs)
defines `NirdoshaPlugin`/`PluginBuiltin`, `run_with_plugins` wires a
plugin's builtins into both `typeck.rs` and the interpreter, and
[`crates/plugin-example-rot13/`](../crates/plugin-example-rot13/) is a
working reference plugin — including the
`[package.metadata.nirdosha]` block this RFC is about to formalize,
already present in that crate's `Cargo.toml`. What's missing is
everything *around* that mechanism: a build step that assembles a
project-specific binary from a project's own declared plugin
dependencies, without a human hand-writing a `main.rs` that calls
`run_with_plugins` with a literal list. That gap is this RFC's actual
scope.

## Design

### The project manifest

A Nirdosha *project* (not a plugin crate) gains a
`[package.metadata.nirdosha]` block in its own `Cargo.toml` — reusing
the exact key namespace `plugin-example-rot13` already established for
a plugin crate describing itself, now used the other direction, for a
project declaring what it depends on:

```toml
[package.metadata.nirdosha]
plugins = ["nirdosha-plugin-rot13"]
```

`nirdosha build --with-plugins` (name open — see Open Questions) reads
this block, resolves the listed crate names against the project's
ordinary `[dependencies]` (so version/lockfile resolution is 100%
Cargo's, nothing new invented), and generates the small `main.rs`-
equivalent glue that calls `run_with_plugins` with each dependency's
`builtins()`. The merged builtin signature table is then also emitted
so `typeck.rs` can statically check calls into the plugin the same as
any other builtin — this last part is the one piece `plugin.rs` doesn't
yet do automatically (today `run_with_plugins`'s caller has to already
know the signatures to typecheck against).

### Kind B: not in this RFC's first-landed scope

`docs/ECOSYSTEM.md` §G1 recommends Kind A ship first and Kind B only
after F2 (real namespacing/`use`, `docs/LANGUAGE.md` §17) has more
mileage. This RFC's Design section above covers Kind A only, on
purpose. A Kind B design — `kind = "nir-lib"` in the same metadata
block, and teaching F2's `use` resolver to shell out to `cargo
metadata`/`cargo fetch` for a target it can't find locally — is
sketched in `docs/ECOSYSTEM.md` §G1 but deliberately left for a
follow-up RFC once Stage 1 lands, not decided here.

## Effect on the permission model

None for Kind A as designed: a linked-in plugin's builtin is subject
to exactly the same `requires(...)`/effect-annotation rules as any
other builtin — `plugin.rs`'s own doc comment already establishes a
plugin builtin isn't a new capability class. Worth restating in the
follow-up Kind B RFC once that's designed, since a fetched `.nir`
source file executing with the *importing* project's privileges (not
its own) is a real question that doesn't arise for Kind A.

## Compatibility

Fully additive — a project with no `[package.metadata.nirdosha]`
block builds exactly as it does today. No change to the grammar or to
any existing `.nir` program's behavior.

## Rejected alternatives

- **A bespoke `nirpkg` registry/CLI.** Rejected in `docs/ECOSYSTEM.md`
  §G1 already — the expensive option (hosting, uptime, abuse
  moderation) for a solo/small-team-maintained project, when
  crates.io already solves resolution, semver, lockfiles, and yanking.
- **Dynamic loading (`dlopen`) instead of static linking.** Rejected
  in `plugin.rs`'s own module doc: no stable Rust ABI to rely on.
  Static linking via ordinary Cargo dependency resolution is the same
  story every other native Rust dependency already has.

## Open questions

- **Security.** A Kind A plugin is arbitrary native code with no
  sandbox — this cuts against the language's own memory/overflow-
  safety proof story unless plugins are vetted or compiled to WASM
  instead of linked natively. `docs/ECOSYSTEM.md` §G1 flags this as
  needing its own design pass before Stage 1 ships *publicly* (the
  mechanism existing in-repo as a reference implementation is not the
  same as recommending untrusted third-party plugins to users yet).
- **`nirdosha build --with-plugins` vs. always-on.** Should a project
  with a `[package.metadata.nirdosha]` block just always link its
  declared plugins on a plain `nirdosha build`, or does that need an
  explicit opt-in flag the way this draft assumes? Affects whether
  `main.rs`/`INDEX.md`'s "`fn main`... dispatches on the first
  remaining arg to a subcommand" table needs a new flag documented.
  Actual CLI flag name (`--with-plugins`, `--plugins`, or reading the
  manifest unconditionally) is also undecided.
- **Two version resolvers**, once Kind B exists: F2's own module
  resolution and Cargo's semver would both have an opinion about
  "which version of X." Needs a stated precedence rule before Kind B's
  RFC, not silent overlap.
