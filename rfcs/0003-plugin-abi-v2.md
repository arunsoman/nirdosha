---
state: draft
shepherd: (unassigned)
---

# RFC 0003: Plugin ABI v2 — effect declarations, async/sync policy, versioning

## Motivation

`crates/compiler/src/plugin.rs`'s `PluginBuiltin`/`NirdoshaPlugin` (RFC-
adjacent per `GOVERNANCE.md`: "the plugin ABI" needs this process
before an ordinary PR) has one real, live correctness gap and two
undecided policy questions, all surfaced while building a real gallery
of five reference plugins (`crates/plugin-example-{mysql,activemq,
cassandra,neo4j,hbase}/`) against actual external systems instead of
just the trivial `rot13` reference.

**The correctness gap**: `effects.rs`'s `Expr::Call` arm attributes an
effect tag (`Effect::Rng`/`Io`/`Concurrent`/`Network`) to a call by
checking, in order, whether the name is `db_connect`, a real builtin
(`is_builtin`), a known user function, or a local `Ty::Fn` binding. A
plugin builtin's name is deliberately kept out of `ast::BUILTIN_NAMES`
(`plugin.rs`'s own doc comment) and isn't a user function either — so
it matches **none** of these branches and contributes the empty set,
unconditionally. `rot13` never exposed this because it does nothing
effectful. The moment a plugin does real network I/O (every builtin in
this gallery's five plugins does), a function declaring `effect(pure)`
that calls one typechecks clean:

```
fn definitely_pure() -> i64 effect(pure) {
    let h: i64 = cassandra_connect("attacker-controlled-or-just-buggy")
    // ...
}
```

This is a real, demonstrable unsoundness in the effect system as it
stands today, not a hypothetical.

**The two policy questions**: (1) whether/how Nirdosha ever adopts
async as a language feature, given every async-backed plugin in this
gallery (`cassandra`, `neo4j`) has to bridge into a shared Tokio
runtime from inside a synchronous `PluginFn` closure; (2) what "plugin
ABI compatibility across `nirdosha` versions" actually means, given
static linking. Both are cheap to decide now, before any published
third-party plugin exists to be broken by getting them wrong later.

## Design

### 1. `PluginBuiltin.effects: BTreeSet<Effect>` (required field)

```rust
pub struct PluginBuiltin {
    pub name: String,
    pub params: Vec<Ty>,
    pub ret: Ty,
    pub effects: std::collections::BTreeSet<crate::ast::Effect>,
    pub call: PluginFn,
}
```

A plugin author declares which of the four existing `Effect` tags
(`Rng`/`Io`/`Concurrent`/`Network`) its builtin produces — the empty
set for a pure function like `rot13`, `{Effect::Network}` for a
network-backed connect/query, etc. **Never a new tag**: `effects.rs`'s
own module doc already states the four are deliberately closed
("adding a tag with no builtin that produces it yet would be notation
with nothing to check") — a plugin effect is always a new *producer*
of an existing kind, never a new kind.

`plugin.rs` gains one more free-function helper alongside `signatures`/
`implementations`:

```rust
pub(crate) fn effect_map(plugins: &[PluginBuiltin]) -> HashMap<String, BTreeSet<Effect>> {
    plugins.iter().map(|p| (p.name.clone(), p.effects.clone())).collect()
}
```

### 2. Threading it into `effects.rs`, additive-only

`effects.rs`'s `walk_stmts`/`walk_block`/`walk_stmt`/`walk_expr` (all
private to the module — zero external callers) each gain one more
parameter, `plugin_effects: &HashMap<String, EffectSet>`. `walk_expr`'s
`Expr::Call` arm gets one new branch, checked in the same position a
plugin name would otherwise fall through both `is_builtin` and
`known.get`:

```rust
} else if let Some(fx) = plugin_effects.get(name) {
    acc.extend(fx.iter().copied());
} else if let Some(callee) = known.get(name) {
```

**The public `infer_effects(program, registry)` function's signature
does not change** — it has 20 call sites across `main.rs`, `serve.rs`,
`interpreter.rs`, and two test files, none of which currently pass
plugins at all (that's Track B of the plugin-ecosystem plan, not this
RFC). Instead, a new sibling is added, mirroring the `_with_plugins`
pattern `lib.rs`/`typeck.rs` already establish:

```rust
pub fn infer_effects_with_plugins(
    program: &Program,
    registry: &TypeRegistry,
    plugin_effects: &HashMap<String, EffectSet>,
) -> HashMap<String, FnEffects> { /* the real body, now parameterized */ }

pub fn infer_effects(program: &Program, registry: &TypeRegistry) -> HashMap<String, FnEffects> {
    infer_effects_with_plugins(program, registry, &HashMap::new())
}
```

`typeck.rs`'s private `typecheck_impl` (three existing call sites:
`typecheck`, `typecheck_with_plugins`, `typecheck_optional_main`, none
public) gains one more parameter, `plugin_effects: &HashMap<String,
EffectSet>`, passed as `&HashMap::new()` by the two plugin-free callers
and as `crate::plugin::effect_map(plugins)` by `typecheck_with_plugins`
— which already receives the full `&[PluginBuiltin]` slice, so no new
public parameter is needed there either. `typecheck_impl`'s own call
to `effects::infer_effects` becomes `effects::infer_effects_with_plugins`.

Net effect: the only genuinely breaking change in this whole RFC is
`PluginBuiltin` gaining a required field — every other change is
additive (new sibling functions, new parameters on functions with zero
external callers). The six existing `PluginBuiltin`-constructing crates
in this repo (`plugin-example-{rot13,mysql,activemq,cassandra,neo4j,
hbase}`) each get one line added to their struct literals.

### 3. Sync/async: an explicit, dated non-goal

**Nirdosha plugin builtins are synchronous, blocking FFI calls. There
is no plan to add async as a language feature.** The interpreter's
call path has no `.await` point and isn't getting one — `pool.rs`'s
own doc comment already states the interpreter is "no async runtime,
no tokio dependency," a considered choice, not an oversight.

A plugin wrapping an async-only client library (most modern Cassandra/
Neo4j/Kafka drivers) bridges it itself: `nirdosha-plugin-support`'s
`block_on` against one process-wide shared Tokio runtime, called from
inside the synchronous `PluginFn` closure. This is the sanctioned
pattern, demonstrated end-to-end by this gallery's Cassandra and Neo4j
plugins — not a stopgap pending a future async story.

Consequence, stated plainly: a slow plugin call inside `serve`'s
request handling blocks that request's thread for its duration — the
same property a slow `Ty::Db`/`Ty::Mq` call already has today. Not a
new class of problem; if "web service at real internet scale" ever
demands more than this, that's a much larger, separate async-interpreter
research question (a cooperative-yield primitive, or a genuine rewrite),
evaluated then, against real load numbers — explicitly out of scope
here.

### 4. ABI/version compatibility: Cargo/semver already is the answer

Plugins are statically linked (no `dlopen`, `plugin.rs`'s own doc
comment), so there is no dynamic-ABI-mismatch scenario the way there
would be with a `dlopen`'d shared object compiled against a different
header — Cargo's ordinary dependency resolution unifies a plugin crate
and its consuming project onto one identical build of the `nirdosha`
crate. **Policy**: `plugin.rs`'s public surface (`PluginBuiltin`,
`NirdoshaPlugin`, `PluginFn`, the relevant `Ty`/`Effect` variants)
follows ordinary Rust semver on the `nirdosha` crate's own version —
a breaking change (like this RFC's own `effects` field addition) bumps
accordingly (minor pre-1.0, major post-1.0), and a plugin crate pinning
`nirdosha = "0.1"` simply fails to compile against an incompatible
`nirdosha = "0.2"` until updated — a compile error, the correct, safe
failure mode, not a silent miscompile.

One separate, lighter-weight addition earns its keep for tooling, not
ABI compatibility: a `nirdosha_schema = "1"` field in a plugin's
`[package.metadata.nirdosha]` block (already used by every plugin in
this gallery), versioned independently of and much slower-moving than
the `nirdosha` crate's own semver — so a future auto-discovery step
(RFC 0001) can give a fast, friendly "I don't understand this
metadata shape" diagnostic before attempting a full `cargo build`,
rather than a raw compile error surfacing from deep inside generated
glue code.

## Effect on the permission model

This *is* the permission-model fix: today a plugin builtin is
invisible to `effect(...)` checking entirely (the bug this RFC closes).
After this RFC, a plugin builtin's declared effects are checked
exactly like a real builtin's — `plugin.rs`'s own doc comment already
states a plugin builtin isn't a new capability class, and this RFC is
what actually makes that true for the effect system, not just for
type/arity checking (which `typecheck_with_plugins` already handled
correctly).

## Compatibility

`PluginBuiltin` gains a required field — breaking for any existing
`PluginBuiltin` literal (all six in this repo, fixed in the same
change). Everything else is additive: no existing `.nir` program's
behavior changes, and no function with an external caller outside this
RFC's own scope has its signature changed.

## Rejected alternatives

- **Optional `effects: Option<BTreeSet<Effect>>`, defaulting to
  conservative-all-four when absent.** Rejected: silently degrades
  every existing plugin call's precision (a `.nir` caller declaring
  `effect(network)` around a `rot13` call would newly be *required* to
  declare `concurrent`/`rng`/`io` too) and doesn't fix the actual bug
  for a plugin author who forgets to update `effects` after adding a
  new capability — a required field with no default forces the
  question at every plugin's own compile time instead.
- **A fifth `Effect::Plugin` tag`** covering all plugin effects
  uniformly, sidestepping the need for a plugin author to map onto the
  existing four. Rejected: throws away the actual information
  (`effect(network)` vs. `effect(io)` are different, checkable
  guarantees today) and reintroduces exactly the "notation with
  nothing [specific] to check" problem `effects.rs`'s own doc already
  argues against for a from-scratch tag.

## Open questions

- Whether `Ty::Handle(HandleKind)` — a first-class, compiler-enforced
  affine handle type for stateful plugin resources (today handled by
  `nirdosha-plugin-support`'s `HandleRegistry<T>`, a plain `i64` with
  none of `ownership.rs`'s guarantees) — is worth building, and if so,
  whether its payload should be typed (`Ty::Handle(HandleKind,
  Box<Ty>)`) or left nominal like `Ty::Db`. Deliberately not decided
  here — real plugins now exist (this gallery) to inform that decision
  with actual use cases, which didn't exist when this question was
  first raised.
