//! Layer 1 of native observability/APM (the approved design plan's
//! rollout layer 1): hook points + a local stdout/file exporter, proving
//! the tracing *mechanism* and the "zero cost when disabled" claim.
//! Layer 2a (dynamic, client-gated enablement over `--otel-port`) is
//! also built — see "Rollout layers 2-4" below. Still missing: real
//! OTLP export, real metrics-to-collector, a blocking-op watchdog
//! (layers 2b-4, same section). Every later layer only ever adds a new
//! `Tracer` constructor and a new `Event` variant; the
//! `Interpreter::tracer: Option<Arc<Tracer>>` field and the
//! "cheap-check-then-bail when disabled" hook-point shape
//! (`interpreter.rs::Interpreter::traced`) don't change.
//!
//! ## Zero-cost when disabled
//!
//! `Tracer` is only ever reached through `Option<Arc<Tracer>>`. Every
//! hook point is `let Some(tracer) = &self.tracer else { return
//! <original, untraced path>; };` — when `None` (the default, and the
//! only state that exists in layer 1), that `Option` check is the
//! *entire* added cost: no `Instant::now()`, no allocation, no channel
//! send. See `interpreter.rs`'s `Interpreter::traced` and
//! `Interpreter::call` for the one hook-point shape every call site
//! reuses. Layer 2a (below) adds a second, still-cheap check on top of
//! this — see that section for what "zero-cost" narrows to once a
//! `Tracer` can exist but sit dormant.
//!
//! ## Fail-open
//!
//! The channel to the background exporter thread is bounded
//! (`CHANNEL_CAPACITY`) and every send is `try_send`, never `send`: full
//! or disconnected both just increment the dropped-events counter and
//! drop the event. A traced program never blocks on its own tracing, and
//! a tracing failure never becomes a `RuntimeError`.
//!
//! ## No payload capture
//!
//! `SpanRecord` carries a fixed vocabulary only — effect tag, construct/
//! builtin name, calling function name, timing, and `outcome`'s
//! `ErrorKind` *variant name* (`error_kind_name`), never `RuntimeError`'s
//! `Display`/message, which can embed raw paths/hostnames via
//! `ErrorKind::ChannelIoError`. No argument values, no return values, no
//! SQL params, no HTTP bodies.
//!
//! ## Attack surface
//!
//! Outbound-only by construction, in layer 1: this module never opens a
//! listening socket (layer 1 doesn't open any socket at all —
//! `new_console`/`new_file` only ever write to stdout or a local file).
//! Enablement is host-controlled only (`main.rs`'s `--otel-console`,
//! scanned the same way `--format=json` already is) — no `.nir`-source
//! construct exists or is planned to turn this on or redirect it. Layer
//! 2a (`--otel-port`, below) is the one deliberate exception to "never
//! opens a listening socket," built with that in mind: loopback-only, no
//! bind-address flag to widen it, and a mandatory bearer token — see
//! that section for the reasoning.
//!
//! ## Rollout layers 2-4
//!
//! Same discipline `TRANSACT.md`/`SANDBOXING.md` use: each layer ships
//! and is tested on its own before the next starts. Layers 1 and 2a are
//! built (`tests/observability.rs`, `tests/observability_layer2a.rs`,
//! `tests/serve.rs`'s otel-port tests); 2b-4 are not.
//!
//! - **Done — Layer 2a: dynamic, client-gated enablement over a
//!   dedicated port.** Motivation: `nirdosha serve` runs continuously,
//!   but an APM consumer is watching it only sometimes; paying tracing's
//!   cost (however small) the rest of the time is pure waste. `serve`
//!   gains `--otel-port` (a second listener, always loopback-only —
//!   `127.0.0.1`, hardcoded, no flag to widen it — separate from the
//!   app's own `--port`/`--host`) and a mandatory `--otel-token`:
//!   `cmd_serve` refuses to start if `--otel-port` is set without it,
//!   the same posture the presence gateway takes for `--presence-token`
//!   (`ROADMAP.md`'s A5), except mandatory rather than optional — an
//!   unauthenticated APM port would leak call timing/error-rate data to
//!   anyone who can reach it. Protocol: the client sends one line,
//!   `Bearer <token>\n`; the server replies `ok\n` and streams
//!   newline-delimited `render_span_json` lines from then on, or
//!   `err <reason>\n` and closes (`handle_otel_client`). A `Tracer`
//!   (`new_dynamic`) is constructed once at `serve` startup — dormant —
//!   and flows through `Arc::clone` exactly as it always has into every
//!   request's `Interpreter` and every `spawn`ed thread; no new
//!   propagation mechanism. It carries a `ClientRegistry`: an
//!   `enabled: AtomicBool`, an `epoch: AtomicU64`, and
//!   `Mutex<Vec<ClientSlot>>` (one bounded `SyncSender<Arc<str>>` per
//!   connected client — `CLIENT_QUEUE_CAPACITY`, N concurrent consumers,
//!   not just one; `Destination::Fanout` `try_send`s each rendered line
//!   to every slot). Each hook point's cheap-check-then-bail grows one
//!   more branch, checked *before* `Instant::now()`/span construction:
//!   ```text
//!   let Some(tracer) = &self.tracer else { return untraced };
//!   if !tracer.enabled() { return untraced }; // one relaxed atomic load
//!   ```
//!   A connection is a held-open stream, not a scrape/poll — a poll
//!   model would flip `enabled` on/off every scrape interval and defeat
//!   the point, since "connected" is standing in for "someone is
//!   actually watching." `register_client` flips `enabled` true
//!   immediately, no debounce. `deregister_client` only starts a
//!   `DISABLE_DEBOUNCE` (2.5s) timer once the registry is empty, and
//!   that timer checks `epoch` before actually disabling — a reconnect
//!   inside the window bumps `epoch` and makes the stale timer a no-op,
//!   so a restarting APM agent never causes a visible tracing gap.
//!   Exporter thread and channel are always alive once `serve` starts
//!   (no per-connection thread spawn/teardown) — `enabled` gates whether
//!   hook points do any work at all, not whether the thread exists.
//!   Known, disclosed gap: a per-client queue overflow (a slow APM
//!   viewer) isn't counted anywhere — only the shared
//!   `dropped_count()`/`emitted_count()` exist, which track the
//!   producer-side channel, a distinct concern.
//! - **Layer 2b — real OTLP export.** The actual collector/backend wire
//!   format, over the layer-2a transport above (today's feed is this
//!   project's own flat JSON-lines shape, not OTLP). `main.rs`'s current
//!   `--otel`/`--otel-endpoint` (parsed, rejected with "not implemented
//!   yet") land here.
//! - **Layer 3 — real metrics**, not just spans: `nirdosha.tracer.
//!   dropped_events` (already exposed via `dropped_count()`) plus
//!   throughput/latency histograms, exported the same layer-2a/2b way.
//! - **Layer 4 — blocking-op watchdog**: flag an effectful call that's
//!   run far longer than its own span history suggests it should,
//!   surfaced as its own `Event` variant. Depends on 2/3 existing so
//!   there's a baseline to compare against.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TryRecvError, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::ast::Effect;
use crate::token::Span;

/// Bounded — see the module doc's "fail-open" section. Generous enough
/// that a real traced program's normal burst of effectful calls doesn't
/// spuriously drop, small enough that a wedged/slow exporter thread
/// can't turn this into an unbounded queue.
const CHANNEL_CAPACITY: usize = 4096;

/// One hook point's outcome — `Ok`, or the `ErrorKind` *variant name*
/// only (see `error_kind_name`), never the interpolated message.
#[derive(Debug, Clone, Copy)]
pub enum Outcome {
    Ok,
    Err(&'static str),
}

/// One traced span — a `call()` invocation, or one effectful `eval_expr`/
/// `eval_builtin` operation. `effect: None` only happens for a pure user
/// function's own `call()` span: hotspot attribution still wants to see
/// it (a slow pure helper nested inside a traced function should still
/// show up — `interpreter.rs::Interpreter::effect_of_fn`'s doc comment),
/// just with no effect tag to reuse from `ast::Effect`. Every other hook
/// point always carries `Some`.
#[derive(Debug, Clone)]
pub struct SpanRecord {
    pub effect: Option<Effect>,
    pub site: Span,
    /// The builtin/construct name (`"spawn"`, `"chan.send"`,
    /// `"http_get"`, `"call"`, ...) — always a literal at every call
    /// site, never built from user input, hence `&'static str`.
    pub name: &'static str,
    /// `Some` only for a `call()` span (the user function's own name);
    /// every other hook point has no enclosing-function name available
    /// at the point it's recorded — no call-stack tracking exists yet
    /// (an honest layer-1 gap, not a claim otherwise).
    pub fn_name: Option<Arc<str>>,
    pub start: Instant,
    pub duration: Duration,
    pub outcome: Outcome,
}

/// At minimum a span — the only event kind layer 1 needs. Metrics
/// (layer 3) and watchdog events (layer 4) each add their own variant
/// later.
pub enum Event {
    Span(SpanRecord),
}

/// `RuntimeError::kind`'s bare variant name, never its `Display` message
/// — see the module doc's "no payload capture" section. Exhaustively
/// matched (no `_ =>` catch-all): a new `ErrorKind` variant is a compile
/// error here, not a silently-mislabeled span.
pub fn error_kind_name(kind: &crate::interpreter::ErrorKind) -> &'static str {
    use crate::interpreter::ErrorKind::*;
    match kind {
        UnknownFn(_) => "UnknownFn",
        UnknownVar(_) => "UnknownVar",
        ArityMismatch { .. } => "ArityMismatch",
        TypeMismatch { .. } => "TypeMismatch",
        OutOfRange { .. } => "OutOfRange",
        DivByZero => "DivByZero",
        MissingReturn { .. } => "MissingReturn",
        ThreadPanicked { .. } => "ThreadPanicked",
        ThreadSpawnFailed { .. } => "ThreadSpawnFailed",
        AlreadyJoined => "AlreadyJoined",
        SandboxSpawnFailed { .. } => "SandboxSpawnFailed",
        AlreadySandboxStopped => "AlreadySandboxStopped",
        ChannelIoError { .. } => "ChannelIoError",
        IndexOutOfBounds { .. } => "IndexOutOfBounds",
        SingularMatrix => "SingularMatrix",
        RngNotSeeded => "RngNotSeeded",
        CallStackOverflow { .. } => "CallStackOverflow",
        TransactLogUnavailable { .. } => "TransactLogUnavailable",
        TransactCommitPending { .. } => "TransactCommitPending",
        TransactCompensatePending { .. } => "TransactCompensatePending",
        TransactNetworkTimedOut { .. } => "TransactNetworkTimedOut",
        WorkflowLogUnavailable { .. } => "WorkflowLogUnavailable",
        WorkflowActionPending { .. } => "WorkflowActionPending",
        Deadlock { .. } => "Deadlock",
        ContractViolation { .. } => "ContractViolation",
    }
}

/// `eval_expr`'s single builtin-dispatch hook point looks a builtin name
/// up here rather than re-deriving its own table — reuses
/// `effects::builtin_effect` (the exact table `effects.rs` already
/// maintains for `typeck.rs`'s effect enforcement) so there's exactly
/// one place that says "`http_get` is `Network`", not two that could
/// drift apart. Returns `None` for a pure builtin (most of them, e.g.
/// every dense-linear-algebra one) — those aren't traced at all,
/// matching `pure`'s "nothing to check" status everywhere else in this
/// codebase.
pub fn traced_builtin(name: &str) -> Option<(Effect, &'static str)> {
    let effect = crate::effects::builtin_effect(name).into_iter().next()?;
    let static_name = crate::ast::BUILTIN_NAMES.iter().find(|&&n| n == name).copied()?;
    Some((effect, static_name))
}

/// The dropped/emitted event counters — shared (via `Arc`) between
/// `Tracer` itself and the background exporter thread, since the thread
/// needs to bump `emitted` independently of whatever `Tracer::record`
/// call is (or isn't) happening concurrently on the producer side.
struct Counters {
    dropped: AtomicU64,
    emitted: AtomicU64,
}

/// Layer 2a only. One connected APM client's outbound queue — bounded
/// and lossy on purpose, the same fail-open stance `CHANNEL_CAPACITY`
/// itself takes: a slow APM viewer misses a line, it never backs up the
/// exporter thread or any other connected client. Not counted in
/// `Tracer::dropped_count()` — that counter is specifically about the
/// producer-side channel (span recorded but the exporter couldn't keep
/// up); a full per-client queue is a distinct, currently-uncounted gap,
/// disclosed here rather than silently conflated with the other one.
const CLIENT_QUEUE_CAPACITY: usize = 256;

/// How long after the last APM client disconnects before `enabled()`
/// actually flips back to `false`. Long enough that a reconnecting or
/// restarting APM agent doesn't cause a visible tracing gap; short
/// enough that "nobody's watching" still means "near-zero overhead"
/// within a few seconds, not minutes. Only the *disable* edge is
/// debounced — the first connection flips `enabled()` true immediately
/// (module doc's "Rollout layers 2-4" section).
const DISABLE_DEBOUNCE: Duration = Duration::from_millis(2500);

/// How often a connected client's handler thread polls its socket for a
/// client-initiated close, in between draining its own outbound queue.
/// Layer 2a's protocol is push-only (the client never sends anything
/// after its token), so this is purely a liveness check, not a data
/// path — see `handle_otel_client`.
const CLIENT_POLL_INTERVAL: Duration = Duration::from_millis(200);

struct ClientSlot {
    id: u64,
    tx: SyncSender<Arc<str>>,
}

/// Shared between `Tracer` and `spawn_otel_port_listener`'s accept loop
/// — layer 2a only. `new_console`/`new_file` never construct one of
/// these, and the exporter thread's `Destination::Stdout`/`File` arms
/// never consult it; `Tracer::enabled` returns `true` unconditionally
/// for a `Tracer` with no registry at all, preserving layer 1's existing
/// "on the moment it's attached" behavior exactly (see that method's own
/// doc comment).
struct ClientRegistry {
    enabled: AtomicBool,
    /// Bumped on every registration. A pending disable timer
    /// (`Tracer::deregister_client`) only actually disables if this
    /// hasn't moved since it captured its own snapshot — the mechanism
    /// that makes a reconnect-during-debounce cancel the disable.
    epoch: AtomicU64,
    next_id: AtomicU64,
    clients: Mutex<Vec<ClientSlot>>,
}

/// Where the background exporter thread's JSON lines actually go —
/// `Stdout` for `--otel-console`, `File` for `tests/observability.rs`
/// (capturing a live process's own stdout reliably from inside the same
/// test binary is awkward, so the test asserts against a temp file
/// instead — same tradeoff noted in the design plan's verification
/// section), `Fanout` for layer 2a's dynamically-connected APM clients.
enum Destination {
    Stdout,
    File(std::fs::File),
    Fanout(Arc<ClientRegistry>),
}

impl Destination {
    fn write_line(&mut self, line: &str) -> std::io::Result<()> {
        match self {
            Destination::Stdout => {
                let stdout = std::io::stdout();
                let mut lock = stdout.lock();
                writeln!(lock, "{line}")?;
                lock.flush()
            }
            Destination::File(f) => {
                writeln!(f, "{line}")?;
                f.flush()
            }
            Destination::Fanout(registry) => {
                // `try_send` only, per-client — see `CLIENT_QUEUE_
                // CAPACITY`'s doc comment for why a slow/dead client
                // here is just a dropped line, never a blocked exporter.
                let line: Arc<str> = Arc::from(line);
                for client in registry.clients.lock().unwrap().iter() {
                    let _ = client.tx.try_send(Arc::clone(&line));
                }
                Ok(())
            }
        }
    }
}

/// Owns the bounded channel's sending half plus the shared drop/emit
/// counters. `interpreter.rs`'s `Interpreter::tracer: Option<Arc<Tracer>>`
/// is the only way any of this is ever reached (see the module doc's
/// "zero cost when disabled" section). One `Tracer` per process (or per
/// test): every `Interpreter` instance created for a `spawn`ed thread
/// gets a cheap `Arc::clone` of the very same one (mirrors how
/// `sandbox_exe` already threads through `Expr::Spawn`'s closure), so
/// every thread's spans land in the same exporter/file/APM feed.
pub struct Tracer {
    tx: SyncSender<Event>,
    counters: Arc<Counters>,
    /// `Some` only for `new_dynamic`'s layer-2a tracer — every hook
    /// point's `enabled()` check and every APM-port connect/disconnect
    /// goes through this. `None` for `new_console`/`new_file`.
    registry: Option<Arc<ClientRegistry>>,
}

impl Tracer {
    fn spawn(dest: Destination, registry: Option<Arc<ClientRegistry>>) -> Arc<Tracer> {
        let (tx, rx) = sync_channel::<Event>(CHANNEL_CAPACITY);
        let counters = Arc::new(Counters { dropped: AtomicU64::new(0), emitted: AtomicU64::new(0) });
        let thread_counters = Arc::clone(&counters);
        std::thread::spawn(move || {
            let mut dest = dest;
            while let Ok(event) = rx.recv() {
                let Event::Span(span) = event;
                let line = render_span_json(&span);
                // A write failure here (a full disk, or stdout closed
                // out from under it) has nowhere sound to go — this is
                // already the fire-and-forget background exporter, the
                // same "never becomes the traced program's problem"
                // stance the hook points themselves take (module doc's
                // "fail-open" section). Best-effort only.
                let _ = dest.write_line(&line);
                thread_counters.emitted.fetch_add(1, Ordering::Relaxed);
            }
        });
        Arc::new(Tracer { tx, counters, registry })
    }

    /// `--otel-console`'s tracer: every span, one JSON object per line,
    /// to stdout.
    pub fn new_console() -> Arc<Tracer> {
        Tracer::spawn(Destination::Stdout, None)
    }

    /// Same shape, to a file instead — see `Destination`'s doc comment
    /// for why `tests/observability.rs` uses this one, not
    /// `new_console`.
    pub fn new_file(path: PathBuf) -> std::io::Result<Arc<Tracer>> {
        let file = std::fs::File::create(path)?;
        Ok(Tracer::spawn(Destination::File(file), None))
    }

    /// Layer 2a's tracer: built once, dormant, at `serve` startup —
    /// `enabled()` is `false` until `spawn_otel_port_listener`'s accept
    /// loop registers the first connected APM client, and back to
    /// `false` `DISABLE_DEBOUNCE` after the last one disconnects. No
    /// `Destination::Stdout`/`File` here — this tracer's only output is
    /// the live per-client feed `register_client` hands back.
    pub fn new_dynamic() -> Arc<Tracer> {
        let registry =
            Arc::new(ClientRegistry { enabled: AtomicBool::new(false), epoch: AtomicU64::new(0), next_id: AtomicU64::new(0), clients: Mutex::new(Vec::new()) });
        Tracer::spawn(Destination::Fanout(Arc::clone(&registry)), Some(registry))
    }

    /// `false` only ever means "a layer-2a tracer exists but nobody's
    /// watching" (module doc's "Rollout layers 2-4" section) — every
    /// hook point checks this *after* the `Option<Arc<Tracer>>` check
    /// and *before* doing any span work (`Instant::now()`, allocation,
    /// ...), so a dormant `Tracer` costs exactly one relaxed atomic load
    /// on top of layer 1's existing `Option` check. A `Tracer` built by
    /// `new_console`/`new_file` (no registry at all) is always `true`:
    /// layer 1's tracers were always "on" the instant they're attached,
    /// and this preserves that unchanged.
    pub fn enabled(&self) -> bool {
        match &self.registry {
            Some(r) => r.enabled.load(Ordering::Relaxed),
            None => true,
        }
    }

    /// Registers one newly authenticated APM client and flips
    /// `enabled()` true immediately (no debounce on the way up — module
    /// doc). Returns `(id, rx)`: `id` is what a later
    /// `deregister_client` call needs, `rx` is where the caller's own
    /// per-connection write loop reads rendered JSON-line spans from.
    /// `None` on a `Tracer` with no registry (`new_console`/`new_file`)
    /// — defensive, not expected to ever actually be called there.
    fn register_client(&self) -> Option<(u64, std::sync::mpsc::Receiver<Arc<str>>)> {
        let registry = self.registry.as_ref()?;
        let (tx, rx) = sync_channel(CLIENT_QUEUE_CAPACITY);
        let id = registry.next_id.fetch_add(1, Ordering::Relaxed);
        registry.clients.lock().unwrap().push(ClientSlot { id, tx });
        registry.epoch.fetch_add(1, Ordering::Relaxed);
        registry.enabled.store(true, Ordering::Relaxed);
        Some((id, rx))
    }

    /// Removes one client (by the `id` `register_client` returned). If
    /// that empties the registry, schedules the debounced disable
    /// (`DISABLE_DEBOUNCE`) on a fresh short-lived thread rather than
    /// flipping `enabled` false immediately — a reconnect before that
    /// thread wakes bumps `epoch` again, which this compares against its
    /// own snapshot, so the stale timer becomes a no-op instead of
    /// disabling a tracer someone just reconnected to.
    fn deregister_client(&self, id: u64) {
        let Some(registry) = &self.registry else { return };
        let now_empty = {
            let mut clients = registry.clients.lock().unwrap();
            clients.retain(|c| c.id != id);
            clients.is_empty()
        };
        if !now_empty {
            return;
        }
        let epoch_at_disconnect = registry.epoch.load(Ordering::Relaxed);
        let registry = Arc::clone(registry);
        std::thread::spawn(move || {
            std::thread::sleep(DISABLE_DEBOUNCE);
            if registry.epoch.load(Ordering::Relaxed) != epoch_at_disconnect {
                return; // someone (re)connected during the debounce window
            }
            if registry.clients.lock().unwrap().is_empty() {
                registry.enabled.store(false, Ordering::Relaxed);
            }
        });
    }

    /// The one send path every hook point in `interpreter.rs` goes
    /// through — `try_send`, never `send`: fail-open, never blocks the
    /// traced program (module doc's "fail-open" section).
    fn record(&self, event: Event) {
        match self.tx.try_send(event) {
            Ok(()) => {}
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// Builds and records one span — the single call every hook point in
    /// `interpreter.rs` makes once it already knows a `tracer` is
    /// attached.
    pub fn record_span(
        &self,
        effect: Option<Effect>,
        site: Span,
        name: &'static str,
        fn_name: Option<Arc<str>>,
        start: Instant,
        outcome: Outcome,
    ) {
        self.record(Event::Span(SpanRecord { effect, site, name, fn_name, start, duration: start.elapsed(), outcome }));
    }

    /// `nirdosha.tracer.dropped_events` (design plan's "Metrics shape")
    /// — how many events this tracer has silently dropped because the
    /// bounded channel was full (in practice, "disconnected" only
    /// happens once the exporter thread has already exited). Exposed for
    /// `tests/observability.rs` today and for a real metrics export
    /// (layer 3) to report later.
    pub fn dropped_count(&self) -> u64 {
        self.counters.dropped.load(Ordering::Relaxed)
    }

    /// How many spans the background thread has actually written out —
    /// `tests/observability.rs`'s synchronization point: since the
    /// exporter thread runs asynchronously, a test polls this (bounded by
    /// a timeout) rather than guessing a fixed sleep before reading the
    /// file back.
    pub fn emitted_count(&self) -> u64 {
        self.counters.emitted.load(Ordering::Relaxed)
    }
}

/// One JSON object per line — mirrors the tagged-enum JSON shape
/// `lib.rs::Diagnostic`/`interpreter::ErrorKind` already establish for
/// this codebase's structured output, kept deliberately flat (no nested
/// tagged enum) since there's only ever one `Event` variant today.
fn render_span_json(span: &SpanRecord) -> String {
    #[derive(serde::Serialize)]
    struct Line<'a> {
        #[serde(rename = "type")]
        kind: &'static str,
        name: &'static str,
        fn_name: Option<&'a str>,
        effect: Option<&'static str>,
        site: Span,
        duration_us: u128,
        outcome: &'static str,
        error_kind: Option<&'static str>,
    }
    let (outcome, error_kind) = match span.outcome {
        Outcome::Ok => ("ok", None),
        Outcome::Err(k) => ("err", Some(k)),
    };
    let line = Line {
        kind: "span",
        name: span.name,
        fn_name: span.fn_name.as_deref(),
        effect: span.effect.map(|e| e.name()),
        site: span.site,
        duration_us: span.duration.as_micros(),
        outcome,
        error_kind,
    };
    serde_json::to_string(&line).expect("Line always serializes")
}

/// Layer 2a: binds a TCP listener dedicated to APM consumers, always on
/// loopback only (`127.0.0.1`, never `host`-configurable — deliberate,
/// same reasoning as the module doc's "Attack surface" section: this is
/// an internal ops channel, not something meant to be reachable beyond
/// the box `serve` runs on) and runs the accept loop on a new background
/// thread, returning as soon as the socket is bound — mirrors
/// `serve::run`'s own "confirm the bind succeeded, then hand control to
/// a background loop" shape, so a bad `--otel-port` fails `serve`
/// startup the same way a bad `--port` already does. Each connection
/// gets its own handler thread (`handle_otel_client`).
///
/// Protocol, deliberately as small as `--presence-token`'s own bearer
/// check (`serve.rs::handle_presence`): the client sends exactly one
/// line, `Bearer <token>\n`; the server replies `ok\n` and starts
/// streaming JSON-line spans, or `err <reason>\n` and closes. No
/// re-auth, no framing beyond newline-delimited JSON, and nothing here
/// for a client to configure beyond "know the token."
pub fn spawn_otel_port_listener(tracer: Arc<Tracer>, port: u16, token: String) -> std::io::Result<()> {
    let listener = TcpListener::bind(("127.0.0.1", port))?;
    let token = Arc::new(token);
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(stream) = stream else { continue };
            let tracer = Arc::clone(&tracer);
            let token = Arc::clone(&token);
            std::thread::spawn(move || handle_otel_client(stream, &tracer, &token));
        }
    });
    Ok(())
}

/// One connected APM client's whole lifetime: token handshake, then a
/// push-only stream of rendered spans until the client disconnects (or
/// the process is shutting down). Reuses
/// `interpreter::constant_time_eq` — the same helper
/// `serve.rs::handle_presence` already checks `--presence-token`
/// against, one source of truth for "how this codebase compares a
/// bearer token," not a second copy of it.
fn handle_otel_client(mut stream: TcpStream, tracer: &Tracer, token: &str) {
    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
    let mut line = String::new();
    {
        let mut reader = BufReader::new(&mut stream);
        if reader.read_line(&mut line).is_err() || line.is_empty() {
            return;
        }
    }
    let trimmed = line.trim_end();
    let presented = trimmed.strip_prefix("Bearer ").or_else(|| trimmed.strip_prefix("bearer "));
    let Some(presented) = presented else {
        let _ = stream.write_all(b"err missing Bearer token\n");
        return;
    };
    if !crate::interpreter::constant_time_eq(presented, token) {
        let _ = stream.write_all(b"err invalid token\n");
        return;
    }
    let Some((id, rx)) = tracer.register_client() else {
        let _ = stream.write_all(b"err observability is not dynamically enabled on this server\n");
        return;
    };
    if stream.write_all(b"ok\n").is_err() {
        tracer.deregister_client(id);
        return;
    }

    // Push-only from here: drain the outbound queue for lines to write,
    // and poll the socket (short read timeout) purely to detect the
    // client hanging up -- this protocol never expects the client to
    // send anything more, so any read outcome other than "still open,
    // nothing new" ends the connection.
    let _ = stream.set_read_timeout(Some(CLIENT_POLL_INTERVAL));
    let mut discard = [0u8; 64];
    loop {
        match rx.try_recv() {
            Ok(rendered) => {
                if stream.write_all(rendered.as_bytes()).and_then(|_| stream.write_all(b"\n")).is_err() {
                    break;
                }
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => break, // exporter thread gone (process shutting down)
        }
        match stream.read(&mut discard) {
            Ok(0) => break, // client closed
            Ok(_) => {}     // unexpected data on a push-only protocol; ignore
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(_) => break,
        }
    }
    tracer.deregister_client(id);
}
