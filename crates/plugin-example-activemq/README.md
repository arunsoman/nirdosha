# nirdosha-plugin-activemq

Reference Kind-A Nirdosha plugin #2 of 5 in the plugin-ecosystem gallery
— see `docs/PLUGIN_AUTHORING_FOR_LLMS.md` for the recipe these all
follow, and `crates/plugin-example-mysql/README.md` for the "how a
consuming project installs a plugin" walkthrough (identical here, just
swap the crate/type name).

## Why a hand-rolled STOMP client, not a crate

The Rust STOMP crate ecosystem is thin and stale (`stomp`/`stomp-rs`'s
last real activity predates this project). Rather than depend on an
API we couldn't verify against current Rust, this plugin implements
just enough of STOMP 1.2 directly over `std::net::TcpStream`
(`src/stomp.rs`) — `CONNECT`/`SEND`/`SUBSCRIBE`/`UNSUBSCRIBE`/
`DISCONNECT`, no heart-beats, no reconnect logic. It's ~150 lines
because the protocol itself is a simple, well-specified text format;
this is a legitimate, honestly-scoped choice for a *reference* plugin,
not a production-grade STOMP client.

## What it adds

| builtin | signature | does |
|---|---|---|
| `activemq_connect` | `(url: str) -> i64` | opens a STOMP connection, returns a handle |
| `activemq_send` | `(handle: i64, queue: str, body: str) -> i64` | publishes one message |
| `activemq_receive` | `(handle: i64, queue: str, timeout_ms: i64) -> str` | waits up to `timeout_ms` for one message; **empty string on timeout, not an error** |
| `activemq_close` | `(handle: i64) -> i64` | disconnects |

`url`: `stomp://[user[:pass]@]host:port` (`stomp://` prefix optional).
A bare `queue` name like `"orders"` maps to STOMP's `/queue/orders`
convention; pass a full `/queue/...`/`/topic/...` destination directly
if you need a topic instead.

Same handle-safety disclosure as every plugin in this gallery: an
`i64`, not a compiler-enforced affine type — see
`nirdosha-plugin-support`'s doc comment.

## Try it

```sh
docker run -d --name nirdosha-activemq -p 61613:61613 -p 8161:8161 apache/activemq-classic:latest
# wait for "Connector stomp started" in `docker logs nirdosha-activemq`
cargo run -p nirdosha-plugin-activemq --example run -- \
    crates/plugin-example-activemq/examples/pubsub.nir
# hello from nirdosha
```

Default ActiveMQ Classic credentials are `admin`/`admin`.

## Tests

```sh
cargo test -p nirdosha-plugin-activemq              # static, no broker needed
docker compose -f ../plugin-examples/docker-compose.yml up -d activemq
cargo test -p nirdosha-plugin-activemq -- --ignored  # live send/receive round trip
```
