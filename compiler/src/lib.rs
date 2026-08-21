pub mod ast;
pub mod codegen;
pub mod effects;
pub mod interpreter;
pub mod ownership;
pub mod parser;
pub mod refine;
pub mod smt;
pub mod token;
pub mod typeck;

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
    Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src))
        .run_main()
        .map_err(|e| format!("runtime error: {e}"))
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
    Interpreter::new(std::sync::Arc::new(program), std::sync::Arc::from(src))
        .run_main()
        .map_err(|e| RunFailure::Diagnostics(vec![Diagnostic::Runtime(e)]))
}
