//! `nirdosha-presence-gateway` — the small, standalone WebSocket relay
//! `docs/WORKFLOW.md`'s "notify presence bridge" section and `docs/ROADMAP.md`'s
//! Track A5 both describe: it terminates real browser WebSocket
//! connections (something `nirdosha serve` itself deliberately never
//! does — `docs/WORKFLOW.md`'s own "Deliberate non-goals" section, "this
//! repository does not terminate WebSocket connections and adds none"),
//! reports who's actually online via `POST /api/_presence_connect`/
//! `_disconnect`, and relays `notify()`'s Redis `PUBLISH`es on
//! `nirdosha:push:<subject>` to the right live connection.
//!
//! Deliberately its own crate, not folded into `compiler/`: it's meant to
//! be deployed as a lightweight sidecar/Deployment next to `nirdosha
//! serve`, not a second copy of the whole interpreter — see `Cargo.toml`'s
//! own doc comment on why it depends on `nirdosha` only under
//! `[dev-dependencies]`.
//!
//! Split into: [`jwt`] (independent JWT/JWKS verification for inbound
//! clients), [`presence`] (the HTTP client for the two presence routes),
//! [`registry`] (per-subject connection ref-counting, for correct
//! multi-tab behavior), and [`gateway`] (the WebSocket server itself,
//! wiring the other three together).

pub mod gateway;
pub mod jwt;
pub mod presence;
pub mod registry;
