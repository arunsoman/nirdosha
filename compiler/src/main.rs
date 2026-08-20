use std::process::ExitCode;

use nirdosha::interpreter::Value;

fn main() -> ExitCode {
    let mut args = std::env::args().skip(1);
    let first = match args.next() {
        Some(a) => a,
        None => {
            print_usage();
            return ExitCode::FAILURE;
        }
    };

    match first.as_str() {
        "build" => cmd_build(args),
        "emit-llvm" => cmd_emit_llvm(args),
        path => cmd_interpret(path),
    }
}

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  nirdosha <file.nir>                interpret");
    eprintln!("  nirdosha build <file.nir> -o <out> [--opt0]");
    eprintln!("                                      compile to a native binary (LLVM, -O2 by default)");
    eprintln!("  nirdosha emit-llvm <file.nir>       print the generated LLVM IR");
}

fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("error reading {path}: {e}");
        ExitCode::FAILURE
    })
}

fn cmd_interpret(path: &str) -> ExitCode {
    let src = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    match nirdosha::run(&src) {
        Ok(Value::Int(n)) => {
            println!("=> {n}");
            ExitCode::SUCCESS
        }
        Ok(Value::Bool(b)) => {
            println!("=> {b}");
            ExitCode::SUCCESS
        }
        Ok(Value::Unit) => ExitCode::SUCCESS,
        Ok(v @ (Value::Boxed(_) | Value::Ref(_) | Value::Thread(_) | Value::Channel(_))) => {
            println!("=> {v:?}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

/// Lex -> parse -> typecheck -> ownership-check, shared by `build` and
/// `emit-llvm` — same static gates `nirdosha::run` applies before ever
/// interpreting, applied here before ever generating code. Codegen's own
/// `check_supported` (a third, narrower gate — signed-integer/bool/unit
/// only, no `box`/`&`/`*`) runs separately, inside `codegen::build`/
/// `emit_llvm_ir` themselves, since it's specific to this backend, not a
/// property of the language generally.
fn typecheck_and_own(src: &str) -> Result<nirdosha::ast::Program, String> {
    let toks = nirdosha::token::Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    let program = nirdosha::parser::Parser::new(toks)
        .parse_program()
        .map_err(|e| format!("parse error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    if let Err(errors) = nirdosha::typeck::typecheck(&program) {
        let joined = errors.iter().map(|e| format!("type error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    if let Err(errors) = nirdosha::ownership::check_ownership(&program) {
        let joined = errors.iter().map(|e| format!("ownership error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    Ok(program)
}

fn cmd_build(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut opt = nirdosha::codegen::OptLevel::O2;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => output = args.next(),
            // The generated IR is unoptimized either way (module doc) --
            // this only controls whether clang optimizes after. O2 is
            // the default: goal.md row 5 is about hardware speed, and
            // `nirdosha build` should actually deliver on that unless
            // asked not to (debugging a miscompile without an optimizer
            // in the way is the reason to ask).
            "--opt0" => opt = nirdosha::codegen::OptLevel::O0,
            other => input = Some(other.to_string()),
        }
    }
    let (Some(path), Some(out)) = (input, output) else {
        eprintln!("usage: nirdosha build <file.nir> -o <out> [--opt0]");
        return ExitCode::FAILURE;
    };
    let src = match read_source(&path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let program = match typecheck_and_own(&src) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    let smt_report = nirdosha::smt::analyze(&program);
    match nirdosha::codegen::build(&program, &smt_report, std::path::Path::new(&out), opt) {
        Ok(()) => {
            println!("wrote {out}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}

fn cmd_emit_llvm(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(path) = args.next() else {
        eprintln!("usage: nirdosha emit-llvm <file.nir>");
        return ExitCode::FAILURE;
    };
    let src = match read_source(&path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let program = match typecheck_and_own(&src) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    let smt_report = nirdosha::smt::analyze(&program);
    match nirdosha::codegen::emit_llvm_ir(&program, &smt_report) {
        Ok(ir) => {
            print!("{ir}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("{e}");
            ExitCode::FAILURE
        }
    }
}
