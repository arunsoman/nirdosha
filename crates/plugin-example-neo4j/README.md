# nirdosha-plugin-neo4j

Reference Kind-A Nirdosha plugin #4 of 5 in the plugin-ecosystem gallery
— see `docs/PLUGIN_AUTHORING_FOR_LLMS.md` for the recipe these all
follow, and `crates/plugin-example-mysql/README.md` for the "how a
consuming project installs a plugin" walkthrough.

Async-backed, same `block_on`-bridge pattern as
`crates/plugin-example-cassandra` (see that crate's README for why) —
built to prove the connect/query/close shape generalizes past "it's
basically a database" to a genuinely different domain: graph queries
over the Bolt protocol via `neo4rs`, not tabular SQL/CQL.

## What it adds

| builtin | signature | does |
|---|---|---|
| `neo4j_connect` | `(uri: str, user: str, pass: str) -> i64` | opens a connection, returns a handle |
| `neo4j_run` | `(handle: i64, cypher: str) -> str` | runs a Cypher statement, returns a JSON array of row objects |
| `neo4j_close` | `(handle: i64) -> i64` | closes the connection |

`uri`: e.g. `"127.0.0.1:7687"`.

**Genuinely schema-agnostic row access**: `neo4j_run` deserializes each
result row into `HashMap<String, BoltType>` directly (`row.to::<HashMap
<String, BoltType>>()`), rather than requiring the Cypher query's
`RETURN` shape to be known ahead of time — `neo4rs`'s row deserializer
happens to support this generically, which is worth calling out since
it's the cleanest of this gallery's three "unknown schema" row readers
(compare `nirdosha-plugin-mysql`'s and `nirdosha-plugin-cassandra`'s,
which walk explicit column-spec lists instead).

Same handle-safety disclosure as every plugin in this gallery — see
`nirdosha-plugin-support`'s doc comment.

**`neo4j_connect` doesn't prove the server is reachable.** `neo4rs::
Graph::new` builds a lazy `deadpool`-backed connection pool underneath
— confirmed by testing it against an unreachable address, which
succeeds and hands back a live-looking handle. The real failure
surfaces on the first `neo4j_run` call instead. This is the exact same
"pool construction alone doesn't validate a connection" behavior
`crates/compiler/src/pool.rs` already documents and tests for its own
r2d2-backed pools (`get_or_create_alone_does_not_validate_the_connection
_with_the_default_lazy_min_idle`) — not a bug in this plugin, a real
property of lazy connection pooling worth knowing before you assume
`neo4j_connect` returning `Ok` means the server is actually up.

## Try it

```sh
docker run -d --name nirdosha-neo4j -p 7687:7687 -e NEO4J_AUTH=neo4j/nirdosha123 neo4j:5
# wait a few seconds for Bolt to accept connections
cargo run -p nirdosha-plugin-neo4j --example run -- \
    crates/plugin-example-neo4j/examples/query.nir
# [{"name":"sprocket"}]
```

## Tests

```sh
cargo test -p nirdosha-plugin-neo4j              # static, no server needed
docker compose -f ../plugin-examples/docker-compose.yml up -d neo4j
cargo test -p nirdosha-plugin-neo4j -- --ignored  # live round-trip against a real server
```
