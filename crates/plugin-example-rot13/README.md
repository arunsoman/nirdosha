# nirdosha-plugin-rot13

The one real reference implementation of `docs/ROADMAP.md` Track G, G1 /
`docs/ECOSYSTEM.md` §G1's Stage 1: a plugin crate that adds a new
Nirdosha builtin — `rot13(s: str) -> str` — by depending on `nirdosha`
and implementing one trait. It exists to prove "install a package via
Cargo, and the compiler takes care of the rest" actually works end to
end, not just that the idea sounds plausible.

## Run it

```sh
cargo run -p nirdosha-plugin-rot13 --example run -- \
    crates/plugin-example-rot13/examples/scramble.nir
# Aveqbfun
```

[`examples/scramble.nir`](./examples/scramble.nir) is an ordinary
`.nir` program that calls `rot13`. [`examples/run.rs`](./examples/run.rs)
is the entrypoint that wires this crate's plugin in and runs it — the
literal "installation" step is one line in that file:

```rust
let plugins = Rot13Plugin.builtins();
nirdosha::run_with_plugins(&src, &plugins)
```

## How this crate is built (the plugin-author side)

Two pieces, both in [`src/lib.rs`](./src/lib.rs):

1. A struct implementing `nirdosha::plugin::NirdoshaPlugin`, whose
   `builtins()` declares the signature — name, argument types, return
   type — and hands back the actual Rust closure that runs when
   `.nir` source calls it:

   ```rust
   impl NirdoshaPlugin for Rot13Plugin {
       fn builtins(&self) -> Vec<PluginBuiltin> {
           vec![PluginBuiltin {
               name: "rot13".to_string(),
               params: vec![Ty::Str],
               ret: Ty::Str,
               call: Arc::new(rot13_call),
           }]
       }
   }
   ```

2. A `[package.metadata.nirdosha]` block in [`Cargo.toml`](./Cargo.toml)
   — a statically-greppable declaration of the same signature, so
   tooling (and a human) can answer "what does adding this crate give
   my project?" without compiling or running anything:

   ```toml
   [package.metadata.nirdosha]
   kind = "native"
   builtins = [
       { name = "rot13", params = ["str"], ret = "str" },
   ]
   ```

## How a project consumes a plugin (the app-author side)

This is the part `docs/ECOSYSTEM.md` §G1 is honest about not having
built yet: there's no `nirdosha` CLI flag that reads a project's
`Cargo.toml` and finds a declared plugin dependency on its own. Today,
consuming a plugin means:

1. Add it as an ordinary Cargo dependency:

   ```toml
   [dependencies]
   nirdosha = "0.1"                # or path = "../compiler" locally
   nirdosha-plugin-rot13 = "0.1"   # any real plugin crate, same shape
   ```

2. Write a small entrypoint that builds the plugin list and calls
   `nirdosha::run_with_plugins` instead of the plain `nirdosha` CLI —
   [`examples/run.rs`](./examples/run.rs) *is* that entrypoint, in
   full, for this one plugin. A project depending on more than one
   plugin crate just concatenates their `builtins()` lists:

   ```rust
   let mut plugins = Rot13Plugin.builtins();
   plugins.extend(SomeOtherPlugin.builtins());
   nirdosha::run_with_plugins(&src, &plugins)?;
   ```

## What you get for free once it's registered

Everything a real builtin gets — not a looser, best-effort version of
it:

- **Static type checking.** `rot13(42)` or `rot13("a", "b")` are real
  *type errors*, caught before anything runs — same as calling any
  builtin in `ast::BUILTIN_NAMES` wrong. `rot13` used with no plugin
  registered at all is an ordinary "unknown function" error.
- **Ownership/ownership-checking coverage** — arguments are
  move-checked like any other call.
- **Spawn-safety.** `rot13` can be called from a spawned thread; the
  plugin table propagates to every child `Interpreter` the same way
  `sandbox_exe`/`tracer` do.
- **A real, spanned runtime error path** (`ErrorKind::PluginError`) if
  the plugin's own Rust function fails — not a panic.

[`tests/end_to_end.rs`](./tests/end_to_end.rs) is the proof: it runs
real `.nir` source through the real pipeline and checks all of the
above, including the two failure cases (`cargo test -p
nirdosha-plugin-rot13`).

## Full design

The two-kind split (this crate is "Kind A" — native code), the staged
plan, and the real open questions (native-code sandboxing, two
overlapping version resolvers once Kind B exists) are in
[`docs/ECOSYSTEM.md`](../../docs/ECOSYSTEM.md) §G1.
