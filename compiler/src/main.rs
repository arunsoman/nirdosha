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
    // `--otel-console` (observability layer 1's local stdout tracer) is
    // scanned exactly the way `--format=json` already is — an
    // interpret-only, host-controlled switch that can appear anywhere on
    // the command line, not a subcommand or a per-command flag. `--otel`/
    // `--otel-endpoint=...` parse (so a caller who reaches for the
    // eventual real-OTLP flags gets a clear, non-zero-exit message, not
    // "unknown file `--otel`") but aren't implemented yet — real export is
    // layer 2 (see the observability design plan).
    let otel_console = raw.iter().any(|a| a == "--otel-console");
    let otel_unimplemented = raw.iter().any(|a| a == "--otel" || a.starts_with("--otel-endpoint"));
    if otel_unimplemented {
        eprintln!(
            "--otel/--otel-endpoint: real OTLP export isn't implemented yet (observability layer 2). \
             Use --otel-console for layer 1's local stdout tracer."
        );
        return ExitCode::FAILURE;
    }
    let mut args =
        raw.into_iter().filter(|a| a != "--format=json" && a != "--otel-console");
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
        "emit-ui" => cmd_emit_ui(args),
        "serve" => cmd_serve(args),
        // Not a user-facing subcommand — the *only* caller is a `sandbox`
        // handle's own `Expr::SpawnSandbox` (see interpreter.rs), which
        // execs this exact binary with this exact flag to become the
        // separate OS process a sandbox handle actually points at.
        // Deliberately absent from `print_usage()` for the same reason.
        "--sandbox-worker" => cmd_sandbox_worker(args),
        path => cmd_interpret(path, format_json, otel_console),
    }
}

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  nirdosha <file.nir> [--format=json] [--otel-console]");
    eprintln!("                                      interpret");
    eprintln!("                                      (--format=json: structured diagnostics on failure)");
    eprintln!("                                      (--otel-console: print a JSON trace span per");
    eprintln!("                                       effectful call to stdout -- layer 1, see");
    eprintln!("                                       the observability design plan; --otel/");
    eprintln!("                                       --otel-endpoint aren't implemented until layer 2)");
    eprintln!("  nirdosha build <file.nir> -o <out> [--opt0]");
    eprintln!("                                      compile to a native binary (LLVM, -O2 by default)");
    eprintln!("  nirdosha emit-llvm <file.nir>       print the generated LLVM IR");
    eprintln!("  nirdosha emit-ast <file.nir>        print the parsed AST as JSON (goal.md row 9)");
    eprintln!("  nirdosha emit-ui <file.nir> [-o out.html]");
    eprintln!("                                      derive a Material-styled web UI from struct/fn conventions");
    eprintln!("  nirdosha serve <file.nir> [--port 8080] [--jwks-file P --issuer S --audience S]");
    eprintln!("                             [--identity-base URL]");
    eprintln!("                                      run the program as a real HTTP service (UI at GET /,");
    eprintln!("                                      API at POST /api/<fn>) -- see src/serve.rs");
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
        | Value::File(_)
        | Value::Mq(_) => {
            println!("=> {v:?}")
        }
        Value::Vector(_) | Value::Matrix(..) => println!("=> {v:?}"),
        Value::Struct(..) | Value::Enum(..) | Value::Json(_) | Value::Db(_) | Value::Fn(_) => println!("=> {v:?}"),
    }
    ExitCode::SUCCESS
}

fn cmd_interpret(path: &str, format_json: bool, otel_console: bool) -> ExitCode {
    let src = match read_source(path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    // Observability layer 1: `--otel-console` builds one `Tracer` up
    // front and hands it down through `lib.rs`'s `_with_tracer` variants
    // — `None` here is exactly `run`/`run_diagnostic`'s own behavior, so
    // an ordinary interpret run (the overwhelming common case) pays
    // nothing extra.
    let tracer = if otel_console { Some(nirdosha::observability::Tracer::new_console()) } else { None };
    if format_json {
        return match nirdosha::run_diagnostic_with_tracer(&src, tracer) {
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
    match nirdosha::run_with_tracer(&src, tracer) {
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

/// `nirdosha emit-ui <file.nir> [-o out.html]` — derives a self-contained,
/// Material-styled HTML/JS app from the program's `struct` declarations
/// and `list_/create_/update_/delete_/get_<struct>` naming convention
/// (`ui_gen::generate`). Unlike `emit-ast`, this needs the *typed*
/// program (`typecheck_and_own`, same gate `build`/`emit-llvm` use) —
/// screen inference reads resolved struct fields and function
/// signatures, not raw syntax.
fn cmd_emit_ui(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => output = args.next(),
            other => input = Some(other.to_string()),
        }
    }
    let Some(path) = input else {
        eprintln!("usage: nirdosha emit-ui <file.nir> [-o out.html]");
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
    let registry = nirdosha::ast::TypeRegistry::build(&program);
    let effects = nirdosha::effects::infer_effects(&program, &registry);
    let html = nirdosha::ui_gen::generate(&program, &effects, None);
    match output {
        Some(out) => match std::fs::write(&out, html) {
            Ok(()) => {
                println!("wrote {out}");
                ExitCode::SUCCESS
            }
            Err(e) => {
                eprintln!("error writing {out}: {e}");
                ExitCode::FAILURE
            }
        },
        None => {
            println!("{html}");
            ExitCode::SUCCESS
        }
    }
}

/// `nirdosha serve <file.nir> [--port 8080] [--jwks-file P --issuer S
/// --audience S] [--identity-base URL]` — runs the program as a real
/// HTTP service via `serve::run` (`tiny_http`; see `src/serve.rs`'s
/// module doc for the request-handling design and, importantly, the
/// authz gate it adds on top of `Interpreter::call_named`, which by
/// itself does not enforce `requires(role: ...)`). `--jwks-file`/
/// `--issuer`/`--audience` are all-or-nothing: without them, any
/// `Authorization: Bearer` header is rejected with a clear 500 rather
/// than silently accepted or silently ignored, and any `requires`-gated
/// handler is simply unreachable (every caller gets 401) — an honest
/// failure mode, not a security hole disguised as "it just worked."
fn cmd_serve(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut input: Option<String> = None;
    let mut port: u16 = 8080;
    let mut jwks_file: Option<String> = None;
    let mut issuer: Option<String> = None;
    let mut audience: Option<String> = None;
    let mut identity_base: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--port" => port = args.next().and_then(|s| s.parse().ok()).unwrap_or(port),
            "--jwks-file" => jwks_file = args.next(),
            "--issuer" => issuer = args.next(),
            "--audience" => audience = args.next(),
            "--identity-base" => identity_base = args.next(),
            other => input = Some(other.to_string()),
        }
    }
    let Some(path) = input else {
        eprintln!("usage: nirdosha serve <file.nir> [--port 8080] [--jwks-file P --issuer S --audience S] [--identity-base URL]");
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
    let auth = match (jwks_file, issuer, audience) {
        (Some(path), Some(issuer), Some(audience)) => match std::fs::read_to_string(&path) {
            Ok(jwks_json) => Some(nirdosha::serve::AuthConfig { jwks_json, issuer, audience }),
            Err(e) => {
                eprintln!("error reading {path}: {e}");
                return ExitCode::FAILURE;
            }
        },
        (None, None, None) => None,
        _ => {
            eprintln!("--jwks-file/--issuer/--audience must be given together, or not at all");
            return ExitCode::FAILURE;
        }
    };
    match nirdosha::serve::run(std::sync::Arc::new(program), port, auth, identity_base.as_deref()) {
        Ok(()) => ExitCode::SUCCESS,
        Err(msg) => {
            eprintln!("{msg}");
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
    match interp.call_named_on_big_stack(&fn_name, &vals) {
        Ok(_) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sandbox worker: runtime error: {e}");
            ExitCode::FAILURE
        }
    }
}
