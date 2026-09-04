pub mod ast;
pub mod codegen;
pub mod contract_check;
pub mod crud_gen;
pub mod dbconn;
pub mod durability;
pub mod effects;
pub mod extraction_schema;
pub mod init;
pub mod instance_lock;
pub mod interpreter;
pub mod loader;
pub mod migrate;
pub mod observability;
pub mod ownership;
pub mod parser;
pub mod pool;
pub mod rqlite;
pub mod thread_pool;
pub mod refine;
pub mod serve;
pub mod smt;
pub mod token;
pub mod transact_log;
pub mod typeck;
pub mod ui_gen;
pub mod workflow_conformance;
pub mod workflow_lower;
pub mod workflow_log;

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
    transact_log_path: Option<durability::LogTarget>,
) -> Result<Value, String> {
    run_with_tracer_transact_and_workflow_log(src, tracer, transact_log_path, None)
}

/// Same as `run_with_tracer_and_transact_log`, plus a caller-chosen,
/// stable `workflow_log_path` (`Interpreter::with_workflow_log_path`) —
/// `main.rs`'s `run`/`serve` commands use this instead, for the same
/// "a restart can find the previous run's state again" reason
/// `transact_log_path` already gets.
pub fn run_with_tracer_transact_and_workflow_log(
    src: &str,
    tracer: Option<std::sync::Arc<observability::Tracer>>,
    transact_log_path: Option<durability::LogTarget>,
    workflow_log_path: Option<durability::LogTarget>,
) -> Result<Value, String> {
    let toks = Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    let program = Parser::new(toks)
        .parse_program()
        .map_err(|e| format!("parse error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    run_program_with_tracer_transact_and_workflow_log(program, src, tracer, transact_log_path, workflow_log_path)
}

/// Same pipeline as `run_with_tracer_transact_and_workflow_log`, but
/// takes an already-parsed `Program` instead of lexing/parsing `src`
/// itself — the piece that fn factors out. The one real caller: `main.rs
/// ::cmd_interpret`, for a program whose `use "path.nir"` directives
/// (`docs/ROADMAP.md` Track F, F2 piece 3) `loader::load_program` has
/// already resolved and merged — this crate's own `run*` family stays
/// on a bare `src: &str` with no file path to resolve a relative import
/// against (unaffected either way for the overwhelming common case: a
/// program with no `use` directives behaves identically through
/// either entry point, since `Program.imports` is simply empty and
/// nothing here reads it again after parsing).
pub fn run_program_with_tracer_transact_and_workflow_log(
    program: ast::Program,
    src: &str,
    tracer: Option<std::sync::Arc<observability::Tracer>>,
    transact_log_path: Option<durability::LogTarget>,
    workflow_log_path: Option<durability::LogTarget>,
) -> Result<Value, String> {
    if let Err(errors) = typeck::typecheck(&program) {
        let joined = errors.iter().map(|e| format!("type error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    if let Err(errors) = ownership::check_ownership(&program) {
        let joined = errors.iter().map(|e| format!("ownership error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    // `validate <fn_name> { pre: ... post: ... }`'s build-time
    // "self-check and fail" gate — see `contract_check::
    // check_program_contracts`'s own doc comment for what this does and
    // doesn't catch (a genuine, *proven* defect only; anything the
    // Tier-1 walker can't statically model falls through to
    // `interpreter.rs::call`'s runtime backstop instead). Mirrors
    // `main.rs::typecheck_and_own_impl`'s identical gate, placed here
    // too since `main.rs`'s own pipeline is only reached by `build`/
    // `serve`/`emit-ui`/`emit-llvm` — plain `nirdosha <file.nir>` (the
    // default interpret command) goes through this function instead, by
    // a completely separate path, and needs the exact same gate to not
    // silently skip it.
    if let Err(errors) = contract_check::check_program_contracts(&program) {
        return Err(errors.join("\n"));
    }
    let mut interp = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src));
    if let Some(t) = tracer {
        interp = interp.with_tracer(t);
    }
    if let Some(p) = transact_log_path {
        interp = interp.with_transact_log_path(p);
    }
    if let Some(p) = workflow_log_path {
        interp = interp.with_workflow_log_path(p);
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
    // Same textual pre-check gate, for `WORKFLOW.md`'s own replay pass.
    if src.contains("workflow") {
        match interp.replay_pending_workflow_actions() {
            Ok(outcomes) => {
                for o in &outcomes {
                    eprintln!("workflow replay: {o:?}");
                }
            }
            Err(e) => return Err(format!("workflow log error during crash replay: {e}")),
        }
    }
    interp.run_main_on_big_stack().map_err(|e| format!("runtime error: {e}"))
}

/// goal.md row 9's actual content: a caller that wants the structured
/// `span`/`kind` data behind each stage's error, not `run()`'s single
/// printable string, drives this instead. One shape (`Diagnostic`) across
/// every stage that can fail — lex, parse, typecheck, ownership,
/// interpretation — each already carrying `#[derive(Serialize,
/// Deserialize)]` all the way down to `Ty` (see `ast.rs::Ty`'s doc
/// comment for why `Deserialize` had to come this early). `Lex`/`Parse`
/// carry `LexError`/`ParseError` directly (`message` + `span`, no `kind`
/// enum the way the three later stages have — a flatter shape, not a
/// missing one) — previously excluded here and left as plain strings on
/// `RunFailure::Lex`/`Parse`; a self-repair loop hitting a syntax error
/// got exactly the prose `--format=json` exists to avoid, an honest gap
/// this closes rather than documents.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "stage", content = "diagnostic")]
pub enum Diagnostic {
    Lex(token::LexError),
    Parse(parser::ParseError),
    Type(typeck::TypeError),
    Ownership(ownership::OwnershipError),
    /// `validate <fn_name> { pre: ... post: ... }`'s build-time gate
    /// (`docs/ROADMAP.md` Track F, F3) — a genuine, *proven* Tier-1 defect
    /// (never `Unsupported`, which isn't an error at all — see
    /// `contract_check::check_program_contracts`'s own doc comment).
    Contract(contract_check::ContractDiagnostic),
    Runtime(interpreter::RuntimeError),
}

impl Diagnostic {
    pub fn span(&self) -> token::Span {
        match self {
            Diagnostic::Lex(e) => e.span,
            Diagnostic::Parse(e) => e.span,
            Diagnostic::Type(e) => e.span,
            Diagnostic::Ownership(e) => e.span,
            Diagnostic::Contract(e) => e.span,
            Diagnostic::Runtime(e) => e.span,
        }
    }
}

#[derive(Debug, Clone)]
pub enum RunFailure {
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
    transact_log_path: Option<durability::LogTarget>,
) -> Result<Value, RunFailure> {
    run_diagnostic_with_tracer_transact_and_workflow_log(src, tracer, transact_log_path, None)
}

/// Same as `run_diagnostic_with_tracer_and_transact_log`, plus
/// `run_with_tracer_transact_and_workflow_log`'s own `workflow_log_path`.
pub fn run_diagnostic_with_tracer_transact_and_workflow_log(
    src: &str,
    tracer: Option<std::sync::Arc<observability::Tracer>>,
    transact_log_path: Option<durability::LogTarget>,
    workflow_log_path: Option<durability::LogTarget>,
) -> Result<Value, RunFailure> {
    let toks = Lexer::new(src).tokenize().map_err(|e| RunFailure::Diagnostics(vec![Diagnostic::Lex(e)]))?;
    let program =
        Parser::new(toks).parse_program().map_err(|e| RunFailure::Diagnostics(vec![Diagnostic::Parse(e)]))?;
    if let Err(errors) = typeck::typecheck(&program) {
        return Err(RunFailure::Diagnostics(errors.into_iter().map(Diagnostic::Type).collect()));
    }
    if let Err(errors) = ownership::check_ownership(&program) {
        return Err(RunFailure::Diagnostics(errors.into_iter().map(Diagnostic::Ownership).collect()));
    }
    let contract_diagnostics = contract_check::check_program_contracts_diagnostics(&program);
    if !contract_diagnostics.is_empty() {
        return Err(RunFailure::Diagnostics(contract_diagnostics.into_iter().map(Diagnostic::Contract).collect()));
    }
    let mut interp = Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src));
    if let Some(t) = tracer {
        interp = interp.with_tracer(t);
    }
    if let Some(p) = transact_log_path {
        interp = interp.with_transact_log_path(p);
    }
    if let Some(p) = workflow_log_path {
        interp = interp.with_workflow_log_path(p);
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
    if src.contains("workflow") {
        match interp.replay_pending_workflow_actions() {
            Ok(outcomes) => {
                for o in &outcomes {
                    eprintln!("workflow replay: {o:?}");
                }
            }
            Err(e) => return Err(RunFailure::Diagnostics(vec![Diagnostic::Runtime(e)])),
        }
    }
    interp.run_main_on_big_stack().map_err(|e| RunFailure::Diagnostics(vec![Diagnostic::Runtime(e)]))
}
