pub mod ast;
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
    Interpreter::new(&program)
        .run_main()
        .map_err(|e| format!("runtime error: {e}"))
}
