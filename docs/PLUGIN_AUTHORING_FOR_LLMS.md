# Writing a Nirdosha plugin: a recipe for an LLM (or a human) to follow

**Status**: reference documentation, describing a real, working pattern
— not a proposal. Every claim here is demonstrated by the five plugins
in `crates/plugin-example-{mysql,activemq,cassandra,neo4j,hbase}/`,
each built following exactly this recipe, each with a real test suite
proving it works.

## Why this doc exists

A Kind-A Nirdosha plugin (`docs/ECOSYSTEM.md` §G1) is small enough that
an LLM can write a working one today, given the right template and
constraints. This doc *is* that template: the same shape, the same
five files, the same decision points, spelled out explicitly enough
that generating a sixth plugin (Kafka, Redis Streams, Elasticsearch,
whatever) should be a mostly mechanical exercise, not a fresh design
problem each time.

This is also a real, near-term mitigation for "Nirdosha doesn't have a
package registry" — instead of `cargo add some-nirdosha-plugin` from a
public index (which doesn't exist yet, see `docs/ECOSYSTEM.md`), a
project can have an agent scaffold a bespoke plugin from this recipe on
demand. That's not a substitute for a real registry (everyone
re-generating their own slightly-different Kafka plugin is exactly the
reuse problem a registry solves) — but it makes the gap far less
painful for a single project today.

## The five files every plugin needs

Copy this shape from `crates/plugin-example-mysql/` (the simplest,
fully-synchronous one) or `crates/plugin-example-cassandra/` (if your
target's client library is async):

1. **`Cargo.toml`** — depends on `nirdosha` (path or `"0.1"`) and, if
   you need stateful/async-backed connections, `nirdosha-plugin-support`.
   Include a `[package.metadata.nirdosha]` block listing each builtin's
   name/params/ret — this is a hand-maintained *second* source of truth
   today (see "known sharp edge" below), so keep it in sync with `src/lib.rs`.
2. **`src/lib.rs`** — the plugin itself: one struct implementing
   `NirdoshaPlugin`, one `fn` per builtin.
3. **`examples/run.rs`** — a thin entrypoint calling
   `nirdosha::run_with_plugins(&src, &plugins)`. This is *how a project
   currently uses your plugin* — there's no CLI auto-discovery yet
   (`docs/ECOSYSTEM.md` §G1), so every consumer writes one of these.
4. **`tests/end_to_end.rs`** — real `.nir` source through the real
   pipeline, not a unit test of your Rust function in isolation. Split
   into two groups (see below).
5. **`README.md`** — document both the author-side ("what does this
   plugin do") and consumer-side ("how do I install it") stories. See
   `crates/plugin-example-rot13/README.md`'s "How a project consumes a
   plugin" section for the exact wording every plugin in this gallery
   reuses.

## Step-by-step

### 1. Design your builtins as flat, positional, typed functions

Every argument and return value is one of Nirdosha's `Ty` variants —
`Ty::Str`, `Ty::I64`, `Ty::Bool`, etc. No generics, no overloading. If
your target system is stateful (a database connection, a message
queue), represent the connection as an opaque `Ty::I64` handle — see
step 3.

### 2. Decide sync or async, using this exact decision rule

Nirdosha's interpreter is **permanently, deliberately synchronous** —
there is no `.await` point anywhere in its call path, and no plan to
add one. So:

- **Client library has a synchronous API** (MySQL's `mysql` crate,
  Redis, Postgres, a hand-rolled TCP protocol like this gallery's
  ActiveMQ/STOMP plugin) → write a plain synchronous `PluginFn`
  closure. Nothing special needed.
- **Client library is async-only** (most modern Cassandra/Neo4j/Kafka
  drivers) → use `nirdosha_plugin_support::block_on(async { ... })`
  *inside* your synchronous closure. This is the sanctioned bridge, not
  a workaround — see `crates/plugin-example-cassandra/README.md` for
  the full rationale. Every async-backed plugin linked into the same
  binary shares one lazily-started Tokio runtime this way, instead of
  each spinning up its own.

### 3. Stateful connections: use `HandleRegistry<T>`, not a hand-rolled `OnceLock`

```rust
fn connections() -> &'static HandleRegistry<YourClientType> {
    static REG: OnceLock<HandleRegistry<YourClientType>> = OnceLock::new();
    REG.get_or_init(HandleRegistry::new)
}

fn your_connect_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let conn = /* build your real client */;
    Ok(Value::Int(connections().insert(conn)))
}

fn your_query_call(args: &[Value], span: Span) -> Result<Value, RuntimeError> {
    let handle = int_arg(args, 0, PLUGIN, span)?;
    match connections().with(handle, |conn| /* use conn */) {
        Some(Ok(result)) => Ok(/* wrap result */),
        Some(Err(e)) => Err(plugin_error(PLUGIN, span, format!("... failed: {e}"))),
        None => Err(plugin_error(PLUGIN, span, format!("no open connection for handle {handle}"))),
    }
}
```

**Know the real cost you're accepting**: this handle is a plain `i64`,
not a compiler-enforced affine type. Nothing stops a `.nir` program
from calling your `close` builtin twice, or leaking a handle — your
`close`'s `HandleRegistry::remove` returning `None` is the *only*
place a double-close gets caught, and only at runtime. This tradeoff
(reliable, ordinary Rust now, over a `Ty::Handle` RFC that doesn't
exist yet) is deliberate — see `nirdosha-plugin-support`'s own doc
comment for the full reasoning. Write your own double-close test the
way every plugin in this gallery does (`double_close_is_a_clean_runtime_error_not_a_panic`).

### 4. Errors are real `Result`s, never panics

```rust
fn plugin_error(plugin: &str, span: Span, message: impl Into<String>) -> RuntimeError
```

is provided by `nirdosha_plugin_support`. Every builtin returns
`Result<Value, RuntimeError>` — a connection failure, a malformed
query, a missing row are all ordinary, catchable errors your `.nir`
caller sees as `run_with_plugins`'s `Err(String)`, not a crash.

### 5. Write two kinds of tests

- **Static** (no external service, always run in CI): wrong arity,
  wrong argument type, "unregistered plugin ⇒ unknown function" — all
  real type errors, proven the same way `plugin-example-rot13`'s tests
  prove them. Also test a garbage connection string / unreachable host
  fails as a clean `RuntimeError`, not a panic.
- **Live** (`#[ignore]`d, need `docker compose -f
  crates/plugin-examples/docker-compose.yml up -d <service>`): the real
  connect → use → close round trip against an actual running service.
  Run these by hand while developing (`cargo test -p your-plugin --
  --ignored`); they're wired into `.github/workflows/plugin-integration.yml`
  as a separate, opt-in (`workflow_dispatch`/scheduled) CI job — never
  the main `build.yml`, which would make ordinary PRs depend on five
  slow external services.

## Known sharp edges (disclosed, not hidden)

- **Two sources of truth**: `[package.metadata.nirdosha]`'s TOML block
  and `src/lib.rs`'s `builtins()` describe the same signatures
  independently. Nothing checks they agree yet (a `#[nirdosha_plugin]`
  proc-macro to generate one from the other is real, proposed future
  work — Track D1 of the plugin-ecosystem plan — not built). Keep them
  in sync by hand for now.
- **No CLI auto-discovery**: your plugin only works via a hand-written
  `run_with_plugins` entrypoint (or your own custom `serve`/`build`
  integration) — not through the stock `nirdosha` CLI. This is real,
  disclosed, unbuilt infrastructure (`docs/ECOSYSTEM.md` §G1), not
  something you're doing wrong.
- **`build`/`emit-llvm` (native compilation) can't use plugins at all.**
  A `PluginFn` is an opaque Rust closure with no stable calling
  convention into generated LLVM IR. Plugins are interpreter-only —
  `serve` works (it's interpreter-backed under the hood), ahead-of-time
  compilation does not, and there's no plan to change that without a
  real C-ABI plugin-calling convention, which doesn't exist.
- **Thin target-crate ecosystems happen** — see
  `crates/plugin-example-hbase/README.md`'s "why this is the hardest
  one." When the obvious crate for your target system is unmaintained
  or its API doesn't match its own docs, hand-rolling the wire protocol
  directly (this gallery's ActiveMQ/STOMP plugin) is a legitimate,
  honestly-scoped choice for a reference plugin, not a last resort to
  be ashamed of.

## Worked example: the actual process used to build this gallery

For each of MySQL, ActiveMQ, Cassandra, Neo4j, HBase, in that order
(easiest/most representative pattern first, hardest last):

1. Pick the mainstream Rust client crate for the target system (or,
   for ActiveMQ, confirm there isn't a good one and hand-roll the wire
   protocol instead — STOMP is simple enough that this was the better
   call).
2. Check the crate's real, current API before writing plugin code
   against it — a quick doc fetch or a look at its own `examples/`
   directory beats guessing, especially for a crate whose API has
   shifted across major versions (this gallery's Cassandra plugin
   needed this; MySQL/ActiveMQ's APIs were stable/simple enough not to).
3. Write the four-or-so builtins (`connect`/`query-or-run`/`execute`
   if relevant/`close`), following step 2-4 above for the sync/async
   decision and handle management.
4. Compile. Let real compiler errors — not more guessing — resolve any
   remaining API mismatches (a wrong-version dependency conflict, a
   trait method that doesn't resolve, an argument-type mismatch) one at
   a time. This gallery's Cassandra plugin needed one fix this way
   (`Row`'s real import path); HBase needed two (a duplicate-`thrift`-
   crate-version conflict from an unnecessary direct dependency, and an
   extension-trait method that didn't resolve against the published
   crate version, worked around by calling the underlying raw trait
   method instead).
5. Write the static tests first (no service needed) and get them
   green.
6. Start a real Docker container for the target service, run the
   `--ignored` live tests against it, and fix anything the *static*
   tests couldn't have caught — this gallery's Neo4j plugin needed
   this: its `connect` call turned out to succeed even against an
   unreachable server (a lazily-connecting pool underneath), a fact a
   test never touching a real network could not have surfaced.
7. Write the README, including the honest tradeoffs (rough edges,
   known limitations) — not just the happy path.
