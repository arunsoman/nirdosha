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
    // `--transact-log=<path>` (`transact`'s durability log, `TRANSACT.md`'s
    // Layers 3-4) -- same up-front, interpret-only scan as `--format=json`/
    // `--otel-console`, so it can appear anywhere on the command line.
    // `cmd_interpret`'s default when omitted is `<source-file>.transact.db`
    // -- a stable, program-derived path a restart's crash replay can
    // actually find again (`Interpreter::new`'s own default, a random
    // per-instance temp file, exists only for callers with no real source
    // file at all, e.g. `nirdosha::run(src: &str)`).
    let transact_log_path: Option<String> =
        raw.iter().find_map(|a| a.strip_prefix("--transact-log=").map(String::from));
    // `--workflow-log=<path>` (`workflow { ... }`'s durable store,
    // `WORKFLOW.md`) — same up-front, interpret-only scan as
    // `--transact-log=<path>`, for the same reason: `cmd_interpret`'s
    // default when omitted is `<source-file>.workflow.db`, a stable,
    // program-derived path.
    let workflow_log_path: Option<String> =
        raw.iter().find_map(|a| a.strip_prefix("--workflow-log=").map(String::from));
    let mut args = raw.into_iter().filter(|a| {
        a != "--format=json"
            && a != "--otel-console"
            && !a.starts_with("--transact-log=")
            && !a.starts_with("--workflow-log=")
    });
    let first = match args.next() {
        Some(a) => a,
        None => {
            print_usage();
            return ExitCode::FAILURE;
        }
    };

    match first.as_str() {
        "init" => cmd_init(args),
        "gen-crud" => cmd_gen_crud(args),
        "build" => cmd_build(args),
        "emit-llvm" => cmd_emit_llvm(args),
        "emit-ast" => cmd_emit_ast(args),
        "emit-ui" => cmd_emit_ui(args),
        "serve" => cmd_serve(args, transact_log_path, workflow_log_path),
        // Not a user-facing subcommand — the *only* caller is a `sandbox`
        // handle's own `Expr::SpawnSandbox` (see interpreter.rs), which
        // execs this exact binary with this exact flag to become the
        // separate OS process a sandbox handle actually points at.
        // Deliberately absent from `print_usage()` for the same reason.
        "--sandbox-worker" => cmd_sandbox_worker(args),
        path => cmd_interpret(path, format_json, otel_console, transact_log_path, workflow_log_path),
    }
}

fn print_usage() {
    eprintln!("usage:");
    eprintln!("  nirdosha init <project-name> [--dest <path>] [--no-email] [--no-roles] [--sms] [--push] [--force]");
    eprintln!("                                      scaffold a self-contained project folder: a starter");
    eprintln!("                                      <project-name>.nir (with the standing Email/RoleMapping");
    eprintln!("                                      admin-panel fixtures, unless disabled), a bundled copy of");
    eprintln!("                                      this executable, and a run.sh/run.bat launcher");
    eprintln!("  nirdosha <file.nir> [--format=json] [--otel-console]");
    eprintln!("                                      interpret");
    eprintln!("                                      (--format=json: structured diagnostics on failure)");
    eprintln!("                                      (--otel-console: print a JSON trace span per");
    eprintln!("                                       effectful call to stdout -- layer 1, see");
    eprintln!("                                       the observability design plan; --otel/");
    eprintln!("                                       --otel-endpoint aren't implemented until layer 2)");
    eprintln!("  nirdosha gen-crud <plan.json> --db <db_connect literal> [-o out.nir]");
    eprintln!("                                      deterministic struct+CRUD .nir source from a JSON");
    eprintln!("                                      entity plan (struct_name/fields/crud_slots/screen_title/");
    eprintln!("                                      field_labels per entity, plus a flat kpis list) --");
    eprintln!("                                      real db_connect/db_execute/db_query bodies, no LLM");
    eprintln!("  nirdosha build <file.nir> -o <out> [--opt0]");
    eprintln!("                                      compile to a native binary (LLVM, -O2 by default)");
    eprintln!("  nirdosha emit-llvm <file.nir>       print the generated LLVM IR");
    eprintln!("  nirdosha emit-ast <file.nir>        print the parsed AST as JSON (goal.md row 9)");
    eprintln!("  nirdosha emit-ui <file.nir> [-o out.html]");
    eprintln!("                                      derive a Material-styled web UI from struct/fn conventions");
    eprintln!("  nirdosha serve <file.nir> [--host 127.0.0.1] [--port 8080] [--jwks-file P --issuer S --audience S]");
    eprintln!("                             [--identity-base URL] [--db PATH] [--otel-port PORT --otel-token TOKEN]");
    eprintln!("                                      run the program as a real HTTP service (UI at GET /,");
    eprintln!("                                      API at POST /api/<fn>) -- see src/serve.rs");
    eprintln!("                                      (--otel-port: a second, loopback-only APM port, dynamically");
    eprintln!("                                       enabled only while a token-authenticated client is connected");
    eprintln!("                                       -- observability layer 2a, requires --otel-token)");
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

fn cmd_interpret(
    path: &str,
    format_json: bool,
    otel_console: bool,
    transact_log_path: Option<String>,
    workflow_log_path: Option<String>,
) -> ExitCode {
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
    // `transact`'s durability log: `--transact-log=<path>` if given,
    // otherwise `<source-file>.transact.db` -- a stable, program-derived
    // default so a restart's crash replay finds the previous run's
    // pending rows without the caller having to remember a flag.
    let transact_log = std::path::PathBuf::from(transact_log_path.unwrap_or_else(|| format!("{path}.transact.db")));
    // `workflow { ... }`'s durable store: same default-path convention as
    // `transact_log` above (`WORKFLOW.md`).
    let workflow_log = std::path::PathBuf::from(workflow_log_path.unwrap_or_else(|| format!("{path}.workflow.db")));
    if format_json {
        return match nirdosha::run_diagnostic_with_tracer_transact_and_workflow_log(
            &src,
            tracer,
            Some(transact_log),
            Some(workflow_log),
        ) {
            Ok(v) => print_value(&v),
            Err(nirdosha::RunFailure::Diagnostics(diags)) => {
                let json = serde_json::to_string(&diags).expect("Diagnostic always serializes");
                eprintln!("{json}");
                ExitCode::FAILURE
            }
        };
    }
    match nirdosha::run_with_tracer_transact_and_workflow_log(&src, tracer, Some(transact_log), Some(workflow_log)) {
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
    typecheck_and_own_impl(src, true)
}

/// Same as `typecheck_and_own`, but does not require a `fn main()` — for
/// commands that never execute an entrypoint (`serve`, `emit-ui`,
/// `--sandbox-worker`; see `typeck::typecheck_optional_main`'s doc
/// comment for why each of those doesn't need one).
fn typecheck_and_own_optional_main(src: &str) -> Result<nirdosha::ast::Program, String> {
    typecheck_and_own_impl(src, false)
}

/// Prints `typeck::ungated_fn_warnings` to stderr — non-fatal, unlike a
/// `TypeError` (`ROADMAP.md` A10). Called only from `serve`/`emit-ui`,
/// the two commands where "reachable via `/api/<fn>`" is actually the
/// question being asked; `run`/`build`/`emit-llvm` never serve anything,
/// so warning about HTTP reachability there would be noise unrelated to
/// what those commands do.
fn print_ungated_fn_warnings(program: &nirdosha::ast::Program) {
    for w in nirdosha::typeck::ungated_fn_warnings(program) {
        eprintln!("{w}");
    }
    // `WORKFLOW.md`'s "state ownership" section: same non-fatal,
    // reachability-shaped warning, for a workflow `state` with no
    // `owner` rather than a plain `fn` with no `requires(...)`.
    for w in nirdosha::typeck::workflow_owner_warnings(program) {
        eprintln!("{w}");
    }
}

fn typecheck_and_own_impl(src: &str, require_main: bool) -> Result<nirdosha::ast::Program, String> {
    let toks = nirdosha::token::Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    let program = nirdosha::parser::Parser::new(toks)
        .parse_program()
        .map_err(|e| format!("parse error at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    let type_result =
        if require_main { nirdosha::typeck::typecheck(&program) } else { nirdosha::typeck::typecheck_optional_main(&program) };
    if let Err(errors) = type_result {
        let joined = errors.iter().map(|e| format!("type error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    if let Err(errors) = nirdosha::ownership::check_ownership(&program) {
        let joined = errors.iter().map(|e| format!("ownership error: {e}")).collect::<Vec<_>>().join("\n");
        return Err(joined);
    }
    Ok(program)
}

/// `--theme <path>` for `emit-ui`/`serve` — reads a JSON file matching
/// `ui_gen::Theme`'s shape (every field optional, see that struct's own
/// doc comment) and layers it over the baked-in MD3 tokens. `None` (no
/// flag given) keeps output byte-identical to before this flag existed.
fn load_theme(path: Option<&str>) -> Result<Option<nirdosha::ui_gen::Theme>, String> {
    let Some(path) = path else { return Ok(None) };
    let text = std::fs::read_to_string(path).map_err(|e| format!("error reading {path}: {e}"))?;
    serde_json::from_str(&text)
        .map(Some)
        .map_err(|e| format!("error parsing {path} as a theme JSON object: {e}"))
}

/// `nirdosha init <project-name> [--dest <path>] [--no-email] [--no-roles]
/// [--sms] [--push] [--force]` -- scaffolds `<dest>/<project-name>/`
/// (default `<dest>`: current directory) containing a starter
/// `<project-name>.nir` (`nirdosha::init::generate_source`), a bundled
/// copy of this very executable (`std::env::current_exe()`, copied so the
/// folder can be moved to another machine and run with no separate
/// `nirdosha` install -- same-OS/arch as wherever `init` ran, no cross-
/// compilation attempted), a `run.sh`/`run.bat` launcher for that copy
/// (whichever matches the host OS -- never both, since the other one
/// couldn't run against this binary anyway), and a placeholder
/// `jwks.json` so the launcher's placeholder `--jwks-file`/`--issuer`/
/// `--audience` flags start successfully with every `requires(role: ...)`
/// route still honestly 401ing until real IdP values replace them. This
/// is tooling-level, not a compiler concept: `typeck`/`codegen`/`serve`
/// still only ever know about the one `.nir` file this writes.
fn cmd_init(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut project_name: Option<String> = None;
    let mut dest = ".".to_string();
    let mut opts = nirdosha::init::InitOptions::default();
    let mut force = false;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--dest" => dest = args.next().unwrap_or(dest),
            "--no-email" => opts.email = false,
            "--no-roles" => opts.roles = false,
            "--sms" => opts.sms = true,
            "--push" => opts.push = true,
            "--force" => force = true,
            other => project_name = Some(other.to_string()),
        }
    }
    let Some(name) = project_name else {
        eprintln!(
            "usage: nirdosha init <project-name> [--dest <path>] [--no-email] [--no-roles] [--sms] [--push] [--force]"
        );
        return ExitCode::FAILURE;
    };
    let source = match nirdosha::init::generate_source(&name, &opts) {
        Ok(s) => s,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };

    let project_dir = std::path::Path::new(&dest).join(&name);
    if let Err(e) = std::fs::create_dir_all(&project_dir) {
        eprintln!("error creating {}: {e}", project_dir.display());
        return ExitCode::FAILURE;
    }

    let nir_path = project_dir.join(format!("{name}.nir"));
    let jwks_path = project_dir.join("jwks.json");
    let exe_dest = project_dir.join(format!("nirdosha{}", std::env::consts::EXE_SUFFIX));
    // The bundled binary only ever works on the host's own OS/arch, so
    // only the launcher that could actually run against it is written --
    // a `run.bat` next to a Linux ELF binary would just be a trap.
    let (launcher_name, launcher_body) = if cfg!(windows) {
        ("run.bat", nirdosha::init::render_launcher_windows(&name))
    } else {
        ("run.sh", nirdosha::init::render_launcher_unix(&name))
    };
    let launcher_path = project_dir.join(launcher_name);

    if !force {
        let conflicts: Vec<String> = [&nir_path, &exe_dest, &launcher_path, &jwks_path]
            .into_iter()
            .filter(|p| p.exists())
            .map(|p| p.display().to_string())
            .collect();
        if !conflicts.is_empty() {
            eprintln!("refusing to overwrite existing file(s): {} (pass --force to overwrite)", conflicts.join(", "));
            return ExitCode::FAILURE;
        }
    }

    let current_exe = match std::env::current_exe() {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error locating the running nirdosha executable to bundle: {e}");
            return ExitCode::FAILURE;
        }
    };
    if let Err(e) = std::fs::copy(&current_exe, &exe_dest) {
        eprintln!("error copying {} to {}: {e}", current_exe.display(), exe_dest.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&nir_path, &source) {
        eprintln!("error writing {}: {e}", nir_path.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&jwks_path, nirdosha::init::placeholder_jwks()) {
        eprintln!("error writing {}: {e}", jwks_path.display());
        return ExitCode::FAILURE;
    }
    if let Err(e) = std::fs::write(&launcher_path, &launcher_body) {
        eprintln!("error writing {}: {e}", launcher_path.display());
        return ExitCode::FAILURE;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        // `fs::write` doesn't set the exec bit -- `fs::copy` above
        // already preserved it on the bundled binary, but the launcher
        // script is brand new content and needs it set explicitly.
        if let Err(e) = std::fs::set_permissions(&launcher_path, std::fs::Permissions::from_mode(0o755)) {
            eprintln!("error making {} executable: {e}", launcher_path.display());
            return ExitCode::FAILURE;
        }
    }

    println!("wrote {}/", project_dir.display());
    if cfg!(windows) {
        println!("run it: cd {} && {launcher_name}", project_dir.display());
    } else {
        println!("run it: cd {} && ./{launcher_name}", project_dir.display());
    }
    ExitCode::SUCCESS
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
/// `nirdosha gen-crud <plan.json> --db <literal> [-o out.nir]` — see
/// `crud_gen`'s module doc for why this exists (replaces protobox's
/// placeholder-only Python `_stub_fns` with real, compiling persistence
/// bodies, deterministically, no LLM call).
fn cmd_gen_crud(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut plan_path: Option<String> = None;
    let mut db: Option<String> = None;
    let mut out: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--db" => db = args.next(),
            "-o" => out = args.next(),
            other => plan_path = Some(other.to_string()),
        }
    }
    let (Some(plan_path), Some(db)) = (plan_path, db) else {
        eprintln!("usage: nirdosha gen-crud <plan.json> --db <db_connect literal> [-o out.nir]");
        return ExitCode::FAILURE;
    };
    let text = match std::fs::read_to_string(&plan_path) {
        Ok(t) => t,
        Err(e) => {
            eprintln!("error reading {plan_path}: {e}");
            return ExitCode::FAILURE;
        }
    };
    let plan: nirdosha::crud_gen::ScreenPlan = match serde_json::from_str(&text) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("error parsing {plan_path} as a screen plan: {e}");
            return ExitCode::FAILURE;
        }
    };
    let source = match nirdosha::crud_gen::render_plan(&plan, &db, "") {
        Ok(s) => s,
        Err(e) => {
            eprintln!("{e}");
            return ExitCode::FAILURE;
        }
    };
    match out {
        Some(path) => {
            if let Err(e) = std::fs::write(&path, &source) {
                eprintln!("error writing {path}: {e}");
                return ExitCode::FAILURE;
            }
        }
        None => print!("{source}"),
    }
    ExitCode::SUCCESS
}

fn cmd_emit_ui(mut args: impl Iterator<Item = String>) -> ExitCode {
    let mut input: Option<String> = None;
    let mut output: Option<String> = None;
    let mut theme_path: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "-o" => output = args.next(),
            "--theme" => theme_path = args.next(),
            other => input = Some(other.to_string()),
        }
    }
    let Some(path) = input else {
        eprintln!("usage: nirdosha emit-ui <file.nir> [-o out.html] [--theme theme.json]");
        return ExitCode::FAILURE;
    };
    let src = match read_source(&path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let program = match typecheck_and_own_optional_main(&src) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    print_ungated_fn_warnings(&program);
    let theme = match load_theme(theme_path.as_deref()) {
        Ok(t) => t,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    let registry = nirdosha::ast::TypeRegistry::build(&program);
    let effects = nirdosha::effects::infer_effects(&program, &registry);
    let html = nirdosha::ui_gen::generate(&program, &effects, None, false, theme.as_ref());
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
/// --audience S] [--identity-base URL] [--db PATH] [--otel-port PORT
/// --otel-token TOKEN]` — runs the program as a real HTTP service via
/// `serve::run` (`tiny_http`; see `src/serve.rs`'s module doc for the
/// request-handling design and, importantly, the authz gate it adds on
/// top of `Interpreter::call_named`, which by itself does not enforce
/// `requires(role: ...)`). `--db PATH` additionally exposes the generic
/// `/_nirdosha/table/<snake>` pagination/sort/filter/search route
/// (`serve.rs`'s own doc comment on `dispatch_table_query`) against the
/// SQLite file at `PATH`; omitted, every table renders exactly as it
/// always has (one unpaginated fetch). `--jwks-file`/
/// `--issuer`/`--audience` are all-or-nothing: without them, any
/// `Authorization: Bearer` header is rejected with a clear 500 rather
/// than silently accepted or silently ignored, and any `requires`-gated
/// handler is simply unreachable (every caller gets 401) — an honest
/// failure mode, not a security hole disguised as "it just worked."
/// `--otel-port`/`--otel-token` are observability layer 2a
/// (`observability.rs`'s "Rollout layers 2-4" section): a second,
/// loopback-only listener a token-bearing APM client connects to, live-
/// gating every request's tracer for as long as one stays connected.
/// Unlike `--jwks-file`'s trio, `--otel-port` is meaningful alone at the
/// parser level, but validated here as effectively all-or-nothing too —
/// see the check right below.
fn cmd_serve(
    mut args: impl Iterator<Item = String>,
    transact_log_path: Option<String>,
    workflow_log_path: Option<String>,
) -> ExitCode {
    let mut input: Option<String> = None;
    let mut host: String = "127.0.0.1".to_string();
    let mut port: u16 = 8080;
    let mut jwks_file: Option<String> = None;
    let mut issuer: Option<String> = None;
    let mut audience: Option<String> = None;
    let mut identity_base: Option<String> = None;
    let mut db_path: Option<String> = None;
    let mut theme_path: Option<String> = None;
    let mut presence_token: Option<String> = None;
    let mut otel_port: Option<u16> = None;
    let mut otel_token: Option<String> = None;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--host" => host = args.next().unwrap_or(host),
            "--port" => port = args.next().and_then(|s| s.parse().ok()).unwrap_or(port),
            "--theme" => theme_path = args.next(),
            "--jwks-file" => jwks_file = args.next(),
            "--issuer" => issuer = args.next(),
            "--audience" => audience = args.next(),
            "--identity-base" => identity_base = args.next(),
            "--db" => db_path = args.next(),
            // `WORKFLOW.md`'s presence bridge: the service token an
            // external WS gateway presents to `_presence_connect`/
            // `_presence_disconnect` (machine-to-machine, distinct from a
            // normal user's `Authorization: Bearer` identity token).
            // Omitted entirely means those two routes always 404 —
            // `notify()` still works, just always taking the offline
            // path, the same "a feature you don't opt into costs nothing"
            // degradation `--db`-gated routes already follow.
            "--presence-token" => presence_token = args.next(),
            // Observability layer 2a (`observability.rs`'s "Rollout
            // layers 2-4" section): a second, loopback-only listener
            // dedicated to APM consumers, dynamically gated by whether
            // one is actually connected. Omitted entirely means no
            // listener, no `Tracer` at all — byte-for-byte the same
            // server this always was, the same degradation every other
            // opt-in flag here already follows.
            "--otel-port" => otel_port = args.next().and_then(|s| s.parse().ok()),
            "--otel-token" => otel_token = args.next(),
            other => input = Some(other.to_string()),
        }
    }
    let Some(path) = input else {
        eprintln!(
            "usage: nirdosha serve <file.nir> [--host 127.0.0.1] [--port 8080] [--jwks-file P --issuer S --audience S] [--identity-base URL] [--db PATH] [--theme theme.json] [--presence-token TOKEN] [--otel-port PORT --otel-token TOKEN]"
        );
        return ExitCode::FAILURE;
    };
    // All-or-nothing, same posture `--jwks-file`/`--issuer`/`--audience`
    // already take below: a bearer-token-gated internal channel that
    // silently ran with no token would be a security hole disguised as
    // "it just worked" (`observability.rs`'s layer 2a design explicitly
    // calls this out — mandatory, not optional, unlike `--presence-token`
    // which is allowed to be absent because absence just 404s the routes
    // it gates instead of exposing anything).
    if otel_port.is_some() && otel_token.is_none() {
        eprintln!("--otel-port requires --otel-token (an unauthenticated APM port would leak call timing/error-rate data to anyone who can reach it)");
        return ExitCode::FAILURE;
    }
    let src = match read_source(&path) {
        Ok(s) => s,
        Err(code) => return code,
    };
    let program = match typecheck_and_own_optional_main(&src) {
        Ok(p) => p,
        Err(msg) => {
            eprintln!("{msg}");
            return ExitCode::FAILURE;
        }
    };
    print_ungated_fn_warnings(&program);
    let theme = match load_theme(theme_path.as_deref()) {
        Ok(t) => t,
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
    // `transact`'s durability log: one stable path for the whole server's
    // lifetime, shared by every request's own `Interpreter`
    // (`serve::dispatch` builds a fresh one per request already, for
    // isolation) — never a fresh temp file per request, which would
    // scatter a durable transaction's rows across files nothing ever
    // replays together, and would leak one file per request forever.
    let transact_log = std::path::PathBuf::from(transact_log_path.unwrap_or_else(|| format!("{path}.transact.db")));
    // `workflow { ... }`'s durable store: same one-stable-path-for-the-
    // server's-lifetime reasoning as `transact_log` above.
    let workflow_log = std::path::PathBuf::from(workflow_log_path.unwrap_or_else(|| format!("{path}.workflow.db")));
    match nirdosha::serve::run(
        std::sync::Arc::new(program),
        &host,
        port,
        auth,
        identity_base.as_deref(),
        transact_log,
        workflow_log,
        presence_token,
        db_path,
        theme.as_ref(),
        theme_path.as_deref(),
        otel_port,
        otel_token,
    ) {
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
    let program = match typecheck_and_own_optional_main(&src) {
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
            // A `chan`-typed parameter's argv string is the parent's
            // bound channel address (see interpreter.rs's
            // `spawn_sandbox`/`connect_chan` -- a Unix socket path on
            // Unix, a `host:port` string on Windows), not a value to
            // parse -- connect to the same socket the parent bound,
            // giving this side of the sandboxed process a live channel
            // to the parent, not a re-parsed literal.
            Ty::Channel(_) => match nirdosha::interpreter::connect_chan(std::path::Path::new(&raw)) {
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
