# nirdosha-plugin-cassandra

Reference Kind-A Nirdosha plugin #3 of 5 in the plugin-ecosystem gallery
— see `docs/PLUGIN_AUTHORING_FOR_LLMS.md` for the recipe these all
follow, and `crates/plugin-example-mysql/README.md` for the "how a
consuming project installs a plugin" walkthrough.

**The first plugin in this gallery that's genuinely async-backed.** The
`scylla` driver (CQL-compatible with real Apache Cassandra, not just
ScyllaDB) is async/Tokio-only — there is no sync client. Every builtin
here bridges into [`nirdosha_plugin_support::block_on`] from inside a
plain, synchronous `PluginFn` closure. This is the sanctioned pattern,
not a workaround: Nirdosha's interpreter has no `.await` point anywhere
in its call path (it's deliberately, permanently synchronous — see
`nirdosha-plugin-support`'s own doc comment) and has no plan to add
one, so "block on a shared runtime inside your synchronous closure" is
the intended answer for any async-only client library, not a stopgap.

## What it adds

| builtin | signature | does |
|---|---|---|
| `cassandra_connect` | `(nodes: str) -> i64` | opens a session, returns a handle |
| `cassandra_query` | `(handle: i64, cql: str) -> str` | runs a `SELECT`, returns a JSON array of row objects |
| `cassandra_execute` | `(handle: i64, cql: str) -> i64` | runs any other CQL statement, returns `1` on success (CQL has no affected-row count the way SQL does) |
| `cassandra_close` | `(handle: i64) -> i64` | closes the session |

`nodes` is a comma-separated list of `host:port` contact points, e.g.
`"127.0.0.1:9042"` or `"10.0.0.1:9042,10.0.0.2:9042"`.

Same handle-safety disclosure as every plugin in this gallery — see
`nirdosha-plugin-support`'s doc comment for what an opaque `i64` handle
does and doesn't guarantee.

## Try it

```sh
docker run -d --name nirdosha-cassandra -p 9042:9042 cassandra:5
# Cassandra is slow to become ready -- give it 30-90s; `docker logs -f nirdosha-cassandra`
# until you see "Startup complete"
cargo run -p nirdosha-plugin-cassandra --example run -- \
    crates/plugin-example-cassandra/examples/query.nir
# [{"id":1,"name":"sprocket"}]
```

## Tests

```sh
cargo test -p nirdosha-plugin-cassandra              # static, no cluster needed
docker compose -f ../plugin-examples/docker-compose.yml up -d cassandra
cargo test -p nirdosha-plugin-cassandra -- --ignored  # live round-trip (slow: cluster startup)
```

## Implementation note

`cassandra_query`'s row-to-JSON conversion (`cql_value_to_json`) covers
the common scalar `CqlValue` variants (text/ascii, all integer widths,
boolean, float/double, uuid) and falls back to `{:?}`-formatted text
for anything else (collections, UDTs, etc.) — real coverage, not
claimed completeness; extend the `match` if your queries return a type
it doesn't yet handle.
