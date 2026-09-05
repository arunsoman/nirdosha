# nirdosha-plugin-mysql

Reference Kind-A Nirdosha plugin #1 of 5 in the plugin-ecosystem gallery
(`crates/plugin-example-{mysql,activemq,cassandra,neo4j,hbase}`) — see
`docs/PLUGIN_AUTHORING_FOR_LLMS.md` for the recipe these all follow.
Built first, deliberately: MySQL's sync client is the closest in shape
to what `crates/compiler/src/dbconn.rs` already does for SQLite/
Postgres, making it the lowest-risk proof that
[`nirdosha-plugin-support`](../plugin-support)'s `HandleRegistry`
actually works end to end before the async-backed plugins build on it.

## What it adds

| builtin | signature | does |
|---|---|---|
| `mysql_connect` | `(dsn: str) -> i64` | opens a connection, returns an opaque handle |
| `mysql_query` | `(handle: i64, sql: str) -> str` | runs a `SELECT`, returns a JSON array of row objects |
| `mysql_execute` | `(handle: i64, sql: str) -> i64` | runs `INSERT`/`UPDATE`/`DELETE`/DDL, returns affected-row count |
| `mysql_close` | `(handle: i64) -> i64` | closes the connection |

`dsn` is a standard MySQL URL: `mysql://user:pass@host:port/dbname`.

**The handle is a plain `i64`, not a compiler-enforced affine type.**
Nothing stops a `.nir` program calling `mysql_close` twice, or losing
the handle and leaking the connection — `nirdosha-plugin-support`'s own
doc comment explains why this crate takes that tradeoff (reliable,
ordinary Rust now) over a first-class `Ty::Handle` (rigorous, but a
real language-surface RFC, not built yet). A double-close here is a
clean runtime error, not a panic — see `mysql_close_call` and the
`double_close_is_a_clean_runtime_error_not_a_panic` test.

## Try it

Start a real MySQL (or use `crates/plugin-examples/docker-compose.yml`
to bring up all five services at once):

```sh
docker run -d --name nirdosha-mysql -e MYSQL_ROOT_PASSWORD=nirdosha \
    -e MYSQL_DATABASE=nirdosha_test -p 3306:3306 mysql:8
```

Then:

```sh
cargo run -p nirdosha-plugin-mysql --example run -- \
    crates/plugin-example-mysql/examples/query.nir
# [{"id":"1","name":"sprocket"}]
```

(`query.nil` — [sic] `query.nir` — hardcodes the same DSN
`docker-compose.yml` uses: `mysql://root:nirdosha@127.0.0.1:3306/nirdosha_test`.)

## Tests

```sh
cargo test -p nirdosha-plugin-mysql              # static: typecheck/registration, no server needed
docker compose -f ../plugin-examples/docker-compose.yml up -d mysql
cargo test -p nirdosha-plugin-mysql -- --ignored  # live round-trip against a real server
```

## What a consuming project writes

Same one-line "installation" as every plugin in this gallery — see
`crates/plugin-example-rot13/README.md`'s "How a project consumes a
plugin" section for the full walkthrough; the only difference here is
which crate/type you name:

```rust
use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_mysql::MysqlPlugin;

let plugins = MysqlPlugin.builtins();
nirdosha::run_with_plugins(&src, &plugins)?;
```

Concatenate `.builtins()` lists to register more than one plugin at
once — the cross-cutting `crates/plugin-examples/tests/all_five.rs` test
does exactly this for all five plugins in this gallery.

## Known rough edge

`mysql_query`'s JSON output represents every column value through
`mysql::Value`'s text-protocol form — numeric columns currently come
back as JSON strings (e.g. `"id":"1"`, not `"id":1`), not re-typed
against the column's real SQL type. Functionally correct, but a real
plugin author polishing this further would want to match on
`Row::columns()`'s `ColumnType` and emit real JSON numbers/booleans —
left as-is here since it doesn't affect this crate's job of proving the
plugin *mechanism* end to end.
