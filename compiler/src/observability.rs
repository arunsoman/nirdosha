//! Layer 1 of native observability/APM (the approved design plan's
//! rollout layer 1): hook points + a local stdout/file exporter, proving
//! the tracing *mechanism* and the "zero cost when disabled" claim — no
//! OTLP export, no real metrics-to-collector, no blocking-op watchdog
//! yet (layers 2-4). Every later layer only ever adds a new `Tracer`
//! constructor and a new `Event` variant; the `Interpreter::tracer:
//! Option<Arc<Tracer>>` field and the "one branch when disabled"
//! hook-point shape (`interpreter.rs::Interpreter::traced`) don't change.
//!
//! ## Zero-cost when disabled
//!
//! `Tracer` is only ever reached through `Option<Arc<Tracer>>`. Every
//! hook point is `let Some(tracer) = &self.tracer else { return
//! <original, untraced path>; };` — when `None` (the default), that
//! `Option` check is the *entire* added cost: no `Instant::now()`, no
//! allocation, no channel send. See `interpreter.rs`'s `Interpreter::
//! traced` and `Interpreter::call` for the one hook-point shape every
//! call site reuses.
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
//! Outbound-only by construction: this module never opens a listening
//! socket (layer 1 doesn't open any socket at all — `new_console`/
//! `new_file` only ever write to stdout or a local file). Enablement is
//! host-controlled only (`main.rs`'s `--otel-console`, scanned the same
//! way `--format=json` already is) — no `.nir`-source construct exists
//! or is planned to turn this on or redirect it.

use std::io::Write;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::Arc;
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
        AlreadyJoined => "AlreadyJoined",
        SandboxSpawnFailed { .. } => "SandboxSpawnFailed",
        AlreadySandboxStopped => "AlreadySandboxStopped",
        ChannelIoError { .. } => "ChannelIoError",
        IndexOutOfBounds { .. } => "IndexOutOfBounds",
        SingularMatrix => "SingularMatrix",
        RngNotSeeded => "RngNotSeeded",
        CallStackOverflow { .. } => "CallStackOverflow",
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

/// Where the background exporter thread's JSON lines actually go —
/// `Stdout` for `--otel-console`, `File` for `tests/observability.rs`:
/// capturing a live process's own stdout reliably from inside the same
/// test binary is awkward, so the test asserts against a temp file
/// instead (same tradeoff noted in the design plan's verification
/// section).
enum Destination {
    Stdout,
    File(std::fs::File),
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
/// every thread's spans land in the same exporter/file.
pub struct Tracer {
    tx: SyncSender<Event>,
    counters: Arc<Counters>,
}

impl Tracer {
    fn spawn(dest: Destination) -> Arc<Tracer> {
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
        Arc::new(Tracer { tx, counters })
    }

    /// `--otel-console`'s tracer: every span, one JSON object per line,
    /// to stdout.
    pub fn new_console() -> Arc<Tracer> {
        Tracer::spawn(Destination::Stdout)
    }

    /// Same shape, to a file instead — see `Destination`'s doc comment
    /// for why `tests/observability.rs` uses this one, not
    /// `new_console`.
    pub fn new_file(path: PathBuf) -> std::io::Result<Arc<Tracer>> {
        let file = std::fs::File::create(path)?;
        Ok(Tracer::spawn(Destination::File(file)))
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
