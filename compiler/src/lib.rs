pub mod ast;
pub mod codegen;
pub mod effects;
pub mod interpreter;
pub mod observability;
pub mod ownership;
pub mod parser;
pub mod refine;
pub mod serve;
pub mod smt;
pub mod token;
pub mod transact_log;
pub mod typeck;
pub mod ui_gen;

use interpreter::{Interpreter, Value};
use parser::Parser;
use token::Lexer;

/// Lex -> parse -> **typecheck** -> **check ownership** -> interpret. A
/// program that fails either static pass is never executed: a type error
/// and a use-after-move are both compile errors, not something to
/// discover mid-run. Ownership runs after typeck deliberately — it trusts
/// typeck to have already rejected unknown variables/functions, so it
/// only has to reason about *moves*, not about whether names resolve at
/// all. Collapses every stage's structured error into one printable
/// message; a caller that wants the structured `span`/`kind` data
/// (goal.md row 9) should drive the stages directly instead.
pub fn run(src: &str) -> Result<Value, String> {
    run_with_tracer(src, None)
}

/// Same pipeline as `run`, plus a way to hand `Interpreter::new`'s result
/// a pre-built `tracer` — the "small, named plumbing gap" the
/// observability design plan calls out (`main.rs`'s `--otel-console` is
/// the one caller that actually wants this today; every other caller
/// keeps using plain `run`, which is just this with `tracer: None`).
/// `transact`'s durability log defaults to a unique per-call temp file
/// (`Interpreter::new`'s own default); `main.rs`'s `run`/`serve` commands
/// use `run_with_transact_log` below instead, for a stable path a
/// restart's crash replay can actually find again.
pub fn run_with_tracer(src: &str, tracer: Option<std::sync::Arc<observability::Tracer>>) -> Result<Value, String> {
    run_with_tracer_and_transact_log(src, tracer, None)
}

/// Same as `run_with_tracer`, plus a caller-chosen, stable
/// `transact_log_path` (`Interpreter::with_transact_log_path`) — real
/// crash-replay continuity across separate process invocations needs
/// this; only a caller with a real source *file* to derive one from can
/// meaningfully supply it (`main.rs`'s `run`/`serve` commands do; a bare
/// `src: &str` with no file, e.g. every other caller in this codebase
/// including this file's own tests, has no such path and keeps getting
/// `Interpreter::new`'s unique-per-call temp-file default via `None`).
pub fn run_with_tracer_and_transact_log(
    src: &str,
    tracer: Option<std::sync::Arc<observability::Tracer>>,
    transact_log_path: Option<std::path::PathBuf>,
) -> Result<Value, String> {
    let toks = Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    let program = Parser::new(toks)
        .parse_program()
        .map_err(|e| format!("parse error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    if let Err(errors) = typeck::typecheck(&program) {
        let joined = errors.iter().map(|e| format!("type error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    if let Err(errors) = ownership::check_ownership(&program) {
        let joined = errors.iter().map(|e| format!("ownership error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    let mut interp = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src));
    if let Some(t) = tracer {
        interp = interp.with_tracer(t);
    }
    if let Some(p) = transact_log_path {
        interp = interp.with_transact_log_path(p);
    }
    // Crash replay (`TRANSACT.md`'s Layer 4) before `main` ever runs --
    // gated on a cheap textual pre-check (the keyword has to appear in
    // source for the construct to be used at all) so a program with no
    // `transact` never touches the durability log's filesystem path,
    // matching `Interpreter::transact_log`'s own "only opened when a
    // program actually needs it" laziness. Only a hard failure to even
    // *read* the log aborts startup; `ReplayOutcome::StillPending`/
    // `Stuck` rows are surfaced for visibility, not fatal -- they stay
    // durably recorded and eligible for the next replay regardless.
    if src.contains("transact") {
        match interp.replay_pending_transactions() {
            Ok(outcomes) => {
                for o in &outcomes {
                    eprintln!("transact replay: {o:?}");
                }
            }
            Err(e) => return Err(format!("transact durability log error during crash replay: {e}")),
        }
    }
    interp.run_main_on_big_stack().map_err(|e| format!("runtime error: {e}"))
}

/// goal.md row 9's actual content: a caller that wants the structured
/// `span`/`kind` data behind each stage's error, not `run()`'s single
/// printable string, drives this instead. One shape (`Diagnostic`) across
/// all three stages that can fail *after* parsing — typecheck, ownership,
/// interpretation — each already carrying `#[derive(Serialize,
/// Deserialize)]` all the way down to `Ty` (see `ast.rs::Ty`'s doc
/// comment for why `Deserialize` had to come this early). Lex/parse
/// errors are deliberately **not** folded into `Diagnostic`: neither
/// `LexError` nor `ParseError` has a `kind` enum the way the three later
/// stages do (just a plain message), so structuring them is honestly a
/// separate, not-yet-done increment, not silently pretended away here —
/// `RunFailure::Lex`/`Parse` stay plain strings.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "stage", content = "diagnostic")]
pub enum Diagnostic {
    Type(typeck::TypeError),
    Ownership(ownership::OwnershipError),
    Runtime(interpreter::RuntimeError),
}

impl Diagnostic {
    pub fn span(&self) -> token::Span {
        match self {
            Diagnostic::Type(e) => e.span,
            Diagnostic::Ownership(e) => e.span,
            Diagnostic::Runtime(e) => e.span,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RunFailure {
    Lex(String),
    Parse(String),
    Diagnostics(Vec<Diagnostic>),
}

/// Same lex -> parse -> typecheck -> ownership -> interpret pipeline as
/// `run()`, but failing a structured stage returns the `Diagnostic`(s)
/// themselves instead of a pre-formatted string — see `main.rs`'s
/// `--format=json` for the one caller that actually wants this today.
pub fn run_diagnostic(src: &str) -> Result<Value, RunFailure> {
    run_diagnostic_with_tracer(src, None)
}

/// Same relationship `run_with_tracer` has to `run` — `main.rs`'s
/// `--otel-console --format=json` combination is the one caller that
/// needs both at once.
pub fn run_diagnostic_with_tracer(
    src: &str,
    tracer: Option<std::sync::Arc<observability::Tracer>>,
) -> Result<Value, RunFailure> {
    run_diagnostic_with_tracer_and_transact_log(src, tracer, None)
}

/// Same as `run_diagnostic_with_tracer`, plus `run_with_tracer_and_
/// transact_log`'s own `transact_log_path` + startup crash-replay --
/// `main.rs`'s `--otel-console --format=json` combination (with
/// `--transact-log`) is the one caller that needs all three at once.
pub fn run_diagnostic_with_tracer_and_transact_log(
    src: &str,
    tracer: Option<std::sync::Arc<observability::Tracer>>,
    transact_log_path: Option<std::path::PathBuf>,
) -> Result<Value, RunFailure> {
    let toks = Lexer::new(src).tokenize().map_err(|e| {
        RunFailure::Lex(format!("lex error at {}:{}: {}", e.span.line, e.span.col, e.message))
    })?;
    let program = Parser::new(toks).parse_program().map_err(|e| {
        RunFailure::Parse(format!("parse error at {}:{}: {}", e.span.line, e.span.col, e.message))
    })?;
    if let Err(errors) = typeck::typecheck(&program) {
        return Err(RunFailure::Diagnostics(errors.into_iter().map(Diagnostic::Type).collect()));
    }
    if let Err(errors) = ownership::check_ownership(&program) {
        return Err(RunFailure::Diagnostics(errors.into_iter().map(Diagnostic::Ownership).collect()));
    }
    let mut interp = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src));
    if let Some(t) = tracer {
        interp = interp.with_tracer(t);
    }
    if let Some(p) = transact_log_path {
        interp = interp.with_transact_log_path(p);
    }
    if src.contains("transact") {
        match interp.replay_pending_transactions() {
            Ok(outcomes) => {
                for o in &outcomes {
                    eprintln!("transact replay: {o:?}");
                }
            }
            Err(e) => return Err(RunFailure::Diagnostics(vec![Diagnostic::Runtime(e)])),
        }
    }
    interp.run_main_on_big_stack().map_err(|e| RunFailure::Diagnostics(vec![Diagnostic::Runtime(e)]))
}
