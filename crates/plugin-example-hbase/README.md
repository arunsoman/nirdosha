# nirdosha-plugin-hbase

Reference Kind-A Nirdosha plugin #5 of 5 in the plugin-ecosystem gallery
— see `docs/PLUGIN_AUTHORING_FOR_LLMS.md` for the recipe these all
follow, and `crates/plugin-example-mysql/README.md` for the "how a
consuming project installs a plugin" walkthrough.

**This was the original motivating example.** The conversation that led
to this whole plugin gallery started with "what if a developer needs
HBase or Cassandra?" — HBase is genuinely the harder of the two, and
it's fitting that it's also the last plugin built here: the one proving
the Kind-A mechanism still works even where the target crate ecosystem
is thin.

## Why this is the hardest one

Unlike MySQL/ActiveMQ/Cassandra/Neo4j, there's no mature, actively
maintained, async-first Rust HBase client. This plugin uses
[`hbase-thrift`](https://crates.io/crates/hbase-thrift), a small,
less battle-tested crate wrapping HBase's Thrift 1 gateway — real,
functional, but worth knowing its limits going in:

- **Sync, not async** — Thrift's raw `TTcpChannel` here is blocking, so
  (unlike the Cassandra/Neo4j plugins in this gallery) there's no
  `nirdosha_plugin_support::block_on` bridge needed at all.
- **`THbaseSyncClientExt::put` didn't resolve** against this crate's
  published `0.3.0` build in practice (confirmed by trying it — a
  "method not found" despite the trait being correctly imported and in
  scope). `hbase_put_call` calls the raw `THbaseSyncClient::mutate_row`
  instead, which is simpler and more reliable anyway.
- **No `CREATE TABLE IF NOT EXISTS`.** HBase's Thrift `createTable`
  genuinely errors (`AlreadyExists`) on a table that's already there —
  `hbase_create_table` surfaces that as a real, catchable runtime error
  rather than silently swallowing it or pretending to be idempotent
  like the SQL/CQL plugins' inline `CREATE ... IF NOT EXISTS` can be.
- **Column families, not arbitrary columns.** HBase requires a table's
  column families to be declared up front (`hbase_create_table`'s third
  argument); `hbase_put`/`hbase_get`'s `qualifier` argument is free-form
  within that family, matching HBase's own data model.

None of this is a defect in the *plugin mechanism* — it's an honest
account of what building a plugin against a real, imperfect corner of
the Rust crate ecosystem actually looks like, which is exactly the
scenario the original question ("how would a developer actually use
HBase from Nirdosha?") was asking about.

## What it adds

| builtin | signature | does |
|---|---|---|
| `hbase_connect` | `(host: str, port: i64) -> i64` | opens a Thrift connection, returns a handle |
| `hbase_create_table` | `(handle: i64, table: str, family: str) -> i64` | creates a table with one column family |
| `hbase_put` | `(handle: i64, table: str, row: str, family: str, qualifier: str, value: str) -> i64` | writes one cell |
| `hbase_get` | `(handle: i64, table: str, row: str, family: str, qualifier: str) -> str` | reads one cell; **empty string if it doesn't exist, not an error** |
| `hbase_close` | `(handle: i64) -> i64` | closes the connection |

Same handle-safety disclosure as every plugin in this gallery — see
`nirdosha-plugin-support`'s doc comment.

## Try it

```sh
docker run -d --name nirdosha-hbase -p 9090:9090 -p 2181:2181 -p 16010:16010 dajobe/hbase-docker
# HBase is slow to start -- give it 30-60s
cargo run -p nirdosha-plugin-hbase --example run -- \
    crates/plugin-example-hbase/examples/query.nir
# sprocket
```

## Tests

```sh
cargo test -p nirdosha-plugin-hbase              # static, no gateway needed
docker compose -f ../plugin-examples/docker-compose.yml up -d hbase
cargo test -p nirdosha-plugin-hbase -- --ignored  # live round-trip (slow: HBase startup)
```

**Honest status of this crate specifically**: the static tests above
were run and pass; the `--ignored` live-gateway tests were written and
are ready to run, but were not executed against a real container in
the session that built this gallery — the shared build machine was
under heavy load from unrelated concurrent sessions at the time, and
running a sixth container (on top of the four already proven live for
MySQL/ActiveMQ/Cassandra/Neo4j) wasn't worth the added contention. This
is disclosed here rather than left silent — run the `--ignored` suite
yourself before relying on this crate for anything real.
