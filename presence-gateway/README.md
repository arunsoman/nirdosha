# nirdosha-presence-gateway

`WORKFLOW.md`'s "notify presence bridge" section and `ROADMAP.md`'s Track
A5 — the one piece of `workflow { }`'s `notify()` that `nirdosha` itself
deliberately doesn't build: a real WebSocket connection to a browser.
`nirdosha serve` publishes to Redis (`nirdosha:push:<subject>`) and reads
"who's online" (`identity_presence`); this crate is the small, standalone
process that actually terminates the browser connection, tells `nirdosha
serve` who's online via `POST /api/_presence_connect`/`_presence_disconnect`,
and relays each published event to the right live connection.

## Why a separate crate, not part of `compiler/`

`WORKFLOW.md`'s own "Deliberate non-goals" section: *"This repository does
not terminate WebSocket connections and adds no new transport."* That's
not an oversight this crate reverses — it's still true of `compiler/`.
This is meant to run as its own lightweight sidecar/Deployment next to
`nirdosha serve`, not a second copy of the whole interpreter. It depends
on `nirdosha` only under `[dev-dependencies]` (`Cargo.toml`'s own doc
comment) — used purely so its integration tests can start a real
`nirdosha serve` in-process and mint real tokens with `mock_issue_token`,
never in the shipped binary.

## Protocol

1. Client opens `ws://<gateway>/ws`.
2. Client's **first** text frame must be `{"token": "<JWT>"}` — the same
   OIDC-issued bearer token it would send `nirdosha serve` — within a
   bounded timeout (default 10s). The gateway verifies it independently
   (its own `--jwks-file`/`--issuer`/`--audience`, `src/jwt.rs`) and
   derives the subject from the token's `sub` claim; a client never gets
   to just assert who it is.
3. On success: `{"type":"connected","subject":"<sub>"}`. From here on,
   every message the server sends is `{"type":"notify","payload":{...}}`
   — `payload` is exactly what `notify()` published (`template`, `vars`,
   `sent_at`).
4. On failure (bad/expired/wrong-audience token, or a malformed first
   frame): `{"type":"error","message":"..."}"`, then the connection
   closes.
5. Server sends a WS `Ping` every 30s and expects a `Pong` back; three
   missed intervals closes the connection (a dead TCP connection with no
   FIN/RST can otherwise sit "open" indefinitely).

Presence is ref-counted per subject (`src/registry.rs`): a second tab for
the same subject doesn't re-announce, and closing one tab doesn't mark
the subject offline while another is still open — `_presence_connect`
fires only on the first connection, `_presence_disconnect` only once the
last one closes. **Ordering that actually matters, and is deliberately
enforced in this order:** the gateway `SUBSCRIBE`s to the subject's Redis
channel *before* calling `_presence_connect` or acking `"connected"` —
`notify()`'s `PUBLISH` is fire-and-forget with no persistence for a late
subscriber, so acking first (an earlier version of this code did exactly
that) can silently lose a `notify()` call issued right after the client
sees "connected". Covered by `tests/gateway_integration.rs`'s
`a_valid_token_authenticates_and_receives_a_real_notify_push`.

Graceful shutdown (`SIGTERM`/`SIGINT`, same signals `nirdosha serve`
itself handles): stop accepting new connections, send every open
connection a WS close frame, wait (default 10s) for them to actually
close and run their own presence cleanup, then exit. An ungraceful kill
(`SIGKILL`, OOM) has no way to run this at all — the same disclosed
at-least-once-cleanup limit `WORKFLOW.md`'s own non-goals section already
keeps for `notify()` itself, not hidden here either.

## Running it

```
cargo build --release
./target/release/nirdosha-presence-gateway \
    --nirdosha-base-url http://127.0.0.1:8080 \
    --presence-token-file /run/secrets/presence-token \
    --jwks-file jwks.json --issuer https://your-idp --audience your-app \
    --port 8090 \
    --redis-host 127.0.0.1 --redis-port 6379
```

`nirdosha serve` needs the *same* `--jwks-file`/`--issuer`/`--audience`
and `--presence-token`/`--presence-token-file` for this to work at all —
this gateway is the other end of a contract `nirdosha serve` already
implements (`serve.rs::handle_presence`), not a new one.

`--host`/`--port` default to `127.0.0.1`/`8090` (same "nothing binds wide
by default" posture `nirdosha serve` itself takes); `--redis-host`/
`--redis-port` default to `127.0.0.1`/`6379`. `/healthz` and `/readyz`
answer on the same port as `/ws` (`src/gateway.rs::route_connection`) —
point a Kubernetes Deployment's probes at either.

## Container image

`Dockerfile` (repo root of this crate) — a separate image from the main
`nirdosha` runtime's own `Dockerfile`, since this has none of that
image's build requirements (no Z3/C++20 vendoring). Verified live, not
just built: `docker run`/`docker stop` against a real `nirdosha serve` +
real Redis + a real browser-shaped `WebSocket` client round-trips a
`notify()` call end-to-end and shuts down gracefully within Docker's
default stop timeout.

## Testing

- `cargo test` (unit tests, `src/registry.rs`) — the ref-counting logic in
  isolation, no external services needed.
- `cargo test --test gateway_integration` — real, full-stack: a real
  `nirdosha serve` (in-process, the same technique
  `compiler/tests/serve.rs::start_server` uses), a real Redis
  (`127.0.0.1:6379` — same instance `compiler/tests/mq.rs` already
  expects running), and a real `notify()` call through the actual
  interpreter. Same "verify against something real, not a hand-rolled
  stand-in" discipline `compiler/tests/mq.rs`'s own doc comment states.

## What's *not* here — stated precisely, not silently implied

- **No TLS termination.** Same posture `nirdosha serve` itself takes
  (`KUBERNETES.md`'s compliance matrix, TLS row: `[N/A]`, "correctly
  delegated to the platform/operator") — run this behind an
  Ingress/reverse proxy/mesh sidecar in production, the same as
  `nirdosha serve`.
- **No shared/fan-out Redis subscription.** Each WebSocket connection
  opens its own dedicated Redis `SUBSCRIBE` — simple and correct, but
  O(connections) Redis subscriptions rather than one shared subscription
  fanned out in-process. Worth revisiting only if that specifically
  becomes a real bottleneck at a scale this hasn't been tested against.
- **No Helm chart / Kustomize manifests of its own yet** — unlike the
  main `nirdosha` runtime (`deploy/helm/`, `deploy/kustomize/`), this
  crate ships a `Dockerfile` only. A natural follow-up, not attempted in
  this pass so it doesn't quietly stay unowned (see `ROADMAP.md` Track
  A5).
