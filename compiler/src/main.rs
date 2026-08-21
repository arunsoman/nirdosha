use std::process::ExitCode;
use std::sync::Arc;

use nirdosha::ast::Ty;
use nirdosha::interpreter::{Interpreter, Value};

fn main() -> ExitCode {
    // `--format=json` is scanned out of the raw args *before* dispatch,
    // not treated as a subcommand or as `build`/`emit-llvm`-style
    // per-command flag — it's an interpret-only, agent-facing switch
    // (goal.md row 9: structured diagnostics), and scanning it up front
    // means it can appear anywhere on the command line (`nirdosha
    // --format=json f.nir` or `nirdosha f.nir --format=json`) without
    // `cmd_interpret`'s caller needing its own flag-parsing loop.
    let raw: Vec<String> = std::env::args().skip(1).collect();
    let format_json = raw.iter().any(|a| a == "--format=json");
    let mut args = raw.into_iter().filter(|a| a != "--format=json");
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
        "emit-ast" => cmd_emit_ast(args),
        // Not a user-facing subcommand — the *only* caller is a `sandbox`
        // handle's own `Expr::SpawnSandbox` (see interpreter.rs), which
        // execs this exact binary with this exact flag to become the
        // separate OS process a sandbox handle actually points at.
        // Deliberately absent from `print_usage()` for the same reason.
        "--sandbox-worker" => cmd_sandbox_worker(args),
        path => cmd_interpret(path, format_json),
    }
}

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  nirdosha <file.nir> [--format=json]  interpret");
    eprintln!("                                      (--format=json: structured diagnostics on failure)");
    eprintln!("  nirdosha build <file.nir> -o <out> [--opt0]");
    eprintln!("                                      compile to a native binary (LLVM, -O2 by default)");
    eprintln!("  nirdosha emit-llvm <file.nir>       print the generated LLVM IR");
    eprintln!("  nirdosha emit-ast <file.nir>        print the parsed AST as JSON (goal.md row 9)");
}

fn read_source(path: &str) -> Result<String, ExitCode> {
    std::fs::read_to_string(path).map_err(|e| {
        eprintln!("error reading {path}: {e}");
        ExitCode::FAILURE
    })
}

/// Renders a successful run's result the same way regardless of which
/// entry point (`run`/`run_diagnostic`) produced it — `--format=json`
/// only changes how *failures* are reported (goal.md row 9 is about
/// diagnostics; a successful run has nothing to structure).
fn print_value(v: &Value) -> ExitCode {
    match v {
        Value::Int(n) => println!("=> {n}"),
        Value::Float(n) => println!("=> {n}"),
        Value::Bool(b) => println!("=> {b}"),
        Value::Unit => {}
        Value::Str(s) => println!("=> {s:?}"),
        Value::Boxed(_)
        | Value::Ref(_)
        | Value::Thread(_)
        | Value::Channel(_)
        | Value::Sandbox(_)
        | Value::Tcp(_)
        | Value::TcpListener(_)
        | Value::File(_) => {
            println!("=> {v:?}")
        }
        Value::Vector(_) | Value::Matrix(..) => println!("=> {v:?}"),
        Value::Struct(..) | Value::Enum(..) | Value::Json(_) | Value::Db(_) => println!("=> {v:?}"),
    }
    ExitCode::SUCCESS
}

fn cmd_interpret(path: &str, format_json: bool) -> ExitCode {
    let src = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    if format_json {
        return match nirdosha::run_diagnostic(&src) {
            Ok(v) => print_value(&v),
            // Lex/parse errors aren't part of `Diagnostic` yet (see
            // `lib.rs`'s doc comment) — they stay plain text even under
            // `--format=json`, an honest, documented gap rather than a
            // silently-inconsistent flag.
            Err(nirdosha::RunFailure::Lex(msg)) | Err(nirdosha::RunFailure::Parse(msg)) => {
                eprintln!("{msg}");
                ExitCode::FAILURE
            }
            Err(nirdosha::RunFailure::Diagnostics(diags)) => {
                let json = serde_json::to_string(&diags).expect("Diagnostic always serializes");
                eprintln!("{json}");
                ExitCode::FAILURE
            }
        };
    }
    match nirdosha::run(&src) {
        Ok(v) => print_value(&v),
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

/// goal.md row 9: hands back the parsed `Program` as JSON, the same
/// `Serialize`/`Deserialize`-derived shape `typeck.rs::validate_fragment`
/// expects a single `Expr` fragment in (see its doc comment) — an agent
/// or tool can round-trip a whole program's structure, or splice one
/// fragment back in for isolated re-validation. Deliberately parse-only,
/// not `typecheck_and_own`'s full pipeline: the AST of a program that
/// doesn't yet typecheck is still a legitimate thing to want to inspect
/// (e.g. debugging *why* generation went wrong), so this doesn't gate on
/// it the way `build`/`emit-llvm` do.
fn cmd_emit_ast(mut args: impl Iterator<Item = String>) -> ExitCode {
    let Some(path) = args.next() else {
        eprintln!("usage: nirdosha emit-ast <file.nir>");
        return ExitCode::FAILURE;
    };
    let src = match read_source(&path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let toks = match nirdosha::token::Lexer::new(&src).tokenize() {
        Ok(t) => t,
        Err(e) => {
            eprintln!("lex error at {}:{}: {}", e.span.line, e.span.col, e.message);
            return ExitCode::FAILURE;
        }
    };
    let program = match nirdosha::parser::Parser::new(toks).parse_program() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("parse error at {}:{}: {}", e.span.line, e.span.col, e.message);
            return ExitCode::FAILURE;
        }
    };
    match serde_json::to_string_pretty(&program) {
        Ok(json) => {
            println!("{json}");
            ExitCode::SUCCESS
        }
        Err(e) => {
            eprintln!("failed to serialize AST: {e}");
            ExitCode::FAILURE
        }
    }
}

/// The process a `sandbox name(args)` expression actually spawns (see
/// `interpreter.rs`'s `spawn_sandbox`): re-lex/parse/typecheck the source
/// file it was handed, look up `name`, parse the remaining argv strings
/// back into `Value`s using that function's own declared parameter types
/// (`typeck.rs`'s `SandboxArgMustBeScalar` already proved every one of
/// them is `Int`-or-`Bool`-shaped, so this is a lossless round trip, not
/// a best-effort parse), and call it directly — there's no `main` to run
/// here, just the one function the parent asked for.
fn cmd_sandbox_worker(mut args: impl Iterator<Item = String>) -> ExitCode {
    let (Some(path), Some(fn_name)) = (args.next(), args.next()) else {
        eprintln!("usage: nirdosha --sandbox-worker <file.nir> <fn-name> [args...]");
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
    let Some(f) = program.fns.iter().find(|f| f.name == fn_name) else {
        eprintln!("sandbox worker: no such function `{fn_name}`");
        return ExitCode::FAILURE;
    };
    let mut vals = Vec::with_capacity(f.params.len());
    for (p, raw) in f.params.iter().zip(args) {
        let v = match &p.ty {
            Ty::Bool => match raw.as_str() {
                "true" => Value::Bool(true),
                "false" => Value::Bool(false),
                _ => {
                    eprintln!("sandbox worker: bad bool argument `{raw}` for `{}`", p.name);
                    return ExitCode::FAILURE;
                }
            },
            // A `chan`-typed parameter's argv string is a Unix socket
            // path (see interpreter.rs's `spawn_sandbox`), not a value to
            // parse -- connect to the same socket the parent bound,
            // giving this side of the sandboxed process a live channel
            // to the parent, not a re-parsed literal.
            Ty::Channel(_) => match std::os::unix::net::UnixStream::connect(&raw) {
                Ok(stream) => Value::Channel(Arc::new(nirdosha::interpreter::ChannelInner::from_socket(stream))),
                Err(e) => {
                    eprintln!("sandbox worker: failed to connect channel `{}`: {e}", p.name);
                    return ExitCode::FAILURE;
                }
            },
            _ => match raw.parse::<i64>() {
                Ok(n) => Value::Int(n),
                Err(_) => {
                    eprintln!("sandbox worker: bad integer argument `{raw}` for `{}`", p.name);
                    return ExitCode::FAILURE;
                }
            },
        };
        vals.push(v);
    }

    let interp = Interpreter::new(Arc::new(program), Arc::from(src.as_str()));
    match interp.call_named(&fn_name, &vals) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sandbox worker: runtime error: {e}");
            ExitCode::FAILURE
        }
    }
}
