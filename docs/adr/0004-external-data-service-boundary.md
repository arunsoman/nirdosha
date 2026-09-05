# 0004: External Data & Service Boundary — plugin-backed `db`/`mq` connections by URL scheme

Date: 2026-09-05
Status: accepted

## Context

`Ty::Db` and `Ty::Mq` already exist as first-class affine handle types
(`dbconn.rs`, `interpreter.rs`'s `MqConn`), each with exactly one
built-in backend: SQLite/Postgres for `db_connect`, Redis for
`mq_connect`. The plugin system (Track F, `rfcs/0005-plugin-boundary-
safety-and-performance.md`) already lets a Kind-A Rust crate add
*new, differently-named* builtins (`mysql_connect`/`mysql_query`/...,
`activemq_connect`/`activemq_send`/...) and already has a working,
process-wide `HandleRegistry<T>` for opaque `i64` handles to
plugin-owned resources — `crates/plugin-example-mysql` and
`crates/plugin-example-activemq` proved both of those independently,
each against a real server.

What was missing was *generic dispatch*: a `.nir` program written
against `db_connect`/`db_query`/`db_execute`/`stop` (or
`mq_publish`/`mq_consume`/`stop`) had no way to have that call routed
to a plugin-provided backend — MySQL, ActiveMQ, or anything else —
without the language, typechecker, or interpreter knowing that
backend's name in advance. The alternative on the table (a proposal
this ADR responds to directly) was to invent a new `Value` boundary
type and a new typed plugin-registration API specifically for
"external data & service" capabilities. Both already substantially
exist (`Value::Json` is already the dynamic boundary type every
`db_query`/plugin JSON round-trip already uses; `HandleRegistry` is
already the opaque-handle mechanism) — the real gap was routing, not
representation.

## Decision

A `scheme://...` connection string is dispatched to a plugin builtin
by **naming convention**, not a new registration API:

- `db_connect("mysql://...")` → looks up `db_provider_mysql_connect`
  in `self.plugins: HashMap<String, PluginFn>` (the same map
  `run_with_plugins`/`serve.rs` already build from `&[PluginBuiltin]`,
  unchanged) before falling through to `dbconn::connect` for anything
  without a matching plugin builtin, and unchanged for anything that
  isn't a `scheme://` URL at all (`:memory:`, a bare file path).
- `db_query`/`db_execute` on a plugin-backed connection look up
  `db_provider_<scheme>_query`/`_execute`, passing `(handle, sql,
  params_json)` — `params_json` is `dbconn::params_to_json(&params)`,
  the exact same bind-parameter encoding `sql_bind_params` already
  produces, now serialized to a JSON array string so it can cross the
  plugin boundary as an ordinary `Value::Str` argument. A provider's
  `_query` may return either a JSON `str` (parsed on this side) or a
  `Value::Json` directly.
- A new builtin, `mq_connect_via(url: str) -> Result(mq, str)`, is the
  MQ equivalent of `db_connect`'s scheme-sniffing — added as a new
  entrypoint rather than overloading `mq_connect`'s existing
  `(host, port)` two-argument shape, which stays exactly as it was for
  Redis. `mq_publish`/`mq_consume` gained a `MqConn::PluginBacked`
  match arm exactly like `DbConn::PluginBacked`, dispatching to
  `mq_provider_<scheme>_publish`/`_consume`.
- `stop` on either handle type now explicitly calls
  `db_provider_<scheme>_close`/`mq_provider_<scheme>_close` before
  dropping a plugin-backed connection, rather than relying on `Drop` —
  the plugin owns the real resource (a MySQL `Conn`, a STOMP
  `TcpStream`) and `HandleRegistry::remove` is the only thing that
  actually frees it.

No new `Value` variant, no new `Ty`, no new entrypoint into
`run_with_plugins`/`serve.rs` (both still just take `&[PluginBuiltin]`)
— a plugin author who wants their crate reachable through the generic
surface adds four more `PluginBuiltin` entries under the
`db_provider_<scheme>_*`/`mq_provider_<scheme>_*` names, alongside
whatever bespoke names they already exposed. Both are additive: this
ADR removed nothing from either plugin's original surface.

## Evidence

Proved against two real, independently-chosen backends, not mocks:

- **MySQL** (`crates/plugin-example-mysql`, `mysql:8` in Docker):
  `db_provider_mysql_connect/query/execute/close` added alongside the
  original `mysql_connect/query/execute/close`, reusing the same
  `HandleRegistry<Conn>` and `mysql_value_to_json`. A new test,
  `generic_db_surface_transparently_dispatches_to_the_mysql_plugin`,
  runs a `.nir` program that calls only `db_connect`/`db_execute`/
  `db_query`/`stop` (never `mysql_*` by name) against the live
  container, including a real bind-parameter round trip
  (`db_execute(conn, "...VALUES (?, ?)", id, label)`) — passes.
- **ActiveMQ over STOMP** (`crates/plugin-example-activemq`,
  `apache/activemq-classic` in Docker) — chosen over standing up a new
  Kafka broker as the second reference implementation, since it was
  already real, tested infrastructure in this repo proving the same
  point (streaming/pub-sub, not request/response, the second of the
  three shapes the original proposal wanted proven): `mq_provider_
  stomp_connect/publish/consume/close` added alongside the original
  `activemq_connect/send/receive/close`. One new wrinkle:
  `activemq_receive`'s own convention (empty string = timeout) doesn't
  match `mq_consume`'s contract (`Result(str, str)`, timeout is an
  `Err`, matching Redis' `BLPOP` semantics already in
  `crates/compiler/tests/mq.rs`) — so `mq_provider_stomp_consume` is a
  distinct function, not a reused one, that turns a timeout into a
  real `RuntimeError` instead of `Ok("")`. A new test,
  `generic_mq_surface_transparently_dispatches_to_the_activemq_plugin`,
  runs `.nir` source using only `mq_connect_via`/`mq_publish`/
  `mq_consume`/`stop` against the live broker — passes.
- Full workspace build (`cargo build --workspace --exclude nirdosha-
  grammar-check`) and `cargo test -p nirdosha --no-fail-fast` (75 test
  binaries, all `ok`) both clean after these changes, on top of the
  two plugin crates' own test suites (7/7 and 6/6 respectively,
  `--include-ignored` against the live containers).

The third shape from the original proposal — dynamic/self-describing
data (JSON) — was already closed before this ADR: `Value::Json` is the
type every `db_query` result and every plugin round-trip already uses;
nothing new was needed there. What this ADR actually closes is
*routing* an arbitrary `scheme://` connection string to a plugin, for
both the request/response+transactional shape (MySQL) and the
streaming/pub-sub shape (ActiveMQ), through the one existing `Ty::Db`/
`Ty::Mq` surface — proving one dispatch mechanism generalizes across
both without weakening either type's affine-handle guarantees (a
plugin-backed handle is exactly as single-use as a built-in one; nothing
about `PluginBacked` bypasses `ownership.rs`).

## Consequences

**Easier now**: any future Kind-A plugin (Cassandra, Neo4j, HBase — the
other three in this gallery — or a genuinely new one, Kafka included)
gets the generic `db_connect`/`mq_connect_via` surface for free by
adding four `db_provider_<scheme>_*`/`mq_provider_<scheme>_*` builtins
under this convention; no compiler change is needed per new backend.

**Harder, or at least new**: the convention itself (`db_provider_
<scheme>_connect` etc.) is a load-bearing string match with no static
check that a plugin author spelled it correctly — a typo silently
produces "no provider registered for scheme X" at runtime rather than
a compile-time or registration-time error. This is the same category
of disclosed cost `HandleRegistry`'s own doc comment already accepts
for opaque handles in general, not a new kind of risk introduced here.
`publish_push_event` (the workflow real-time-push helper) explicitly
rejects `MqConn::PluginBacked` rather than silently degrading, since
Redis' `PUBLISH` pub-sub semantics don't generalize to arbitrary
backends — an honest gap, not a silent one.

**Also true, stated plainly**: this ADR is additive and backward
compatible (no existing `.nir` program's behavior changes; every
existing `mysql_*`/`activemq_*` builtin is untouched), which is why
it's recorded as an ADR and shipped directly rather than gated behind
an RFC (`docs/adr/README.md`'s "a judgment call made while implementing
something" scope, `GOVERNANCE.md`'s RFC gate being for breaking/
cross-cutting/language-surface changes specifically — no `.nir` grammar
or `Ty` changed here).
