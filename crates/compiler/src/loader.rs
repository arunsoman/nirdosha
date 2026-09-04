//! Multi-file `use` resolution (`docs/ROADMAP.md` Track F, F2 piece 3;
//! `docs/NEXT_GEN.md` §F2). Every other pass in this compiler (`typeck`,
//! `interpreter`, `ui_gen`, `codegen`) still only ever sees one flat
//! `Program`, exactly as before F2 — this module is what produces that
//! one `Program` from however many files a `use "path.nir"` graph
//! actually spans, before any of them run.
//!
//! Deliberately separate from `parser::parse_program` (one file/token-
//! stream only) and from `lib.rs`'s `run*`/`run_diagnostic*` family
//! (which take a bare `src: &str` with no file path to resolve a
//! relative import against, and are unchanged by F2 for exactly that
//! reason — a program with no `use` directives behaves identically
//! either way, since `Program.imports` is simply empty). Only
//! `main.rs`'s CLI, which always has a real file path on disk, and
//! `lib.rs::run_program_with_tracer_transact_and_workflow_log` (which
//! takes an already-loaded `Program`), go through this.

use crate::ast::{FnDecl, Program};
use crate::{parser, token};
use std::collections::HashSet;
use std::path::{Path, PathBuf};

/// Reads `entry_path`, parses it, and recursively resolves every
/// leading `use "..."` it declares. Returns the fully-merged `Program`
/// plus the *entry file's own* source text (an imported file's source
/// is never returned — nothing downstream needs it: neither `TypeError`
/// nor `ParseError` ever re-quotes a source snippet, only a `line:col`,
/// which is already meaningful relative to whichever file actually
/// produced it — see `resolve_imports`'s doc comment for why that's
/// enough for correct diagnostics with no multi-file source map).
pub fn load_program(entry_path: &str) -> Result<(Program, String), String> {
    let src = std::fs::read_to_string(entry_path).map_err(|e| format!("error reading {entry_path}: {e}"))?;
    let mut program = parse_one(&src, entry_path)?;
    let mut visited = HashSet::new();
    if let Ok(canon) = std::fs::canonicalize(entry_path) {
        visited.insert(canon);
    }
    resolve_imports(&mut program, entry_path, &mut visited)?;
    Ok((program, src))
}

fn parse_one(src: &str, path: &str) -> Result<Program, String> {
    let toks = token::Lexer::new(src)
        .tokenize()
        .map_err(|e| format!("lex error in {path} at {}:{}: {}", e.span.line, e.span.col, e.message))?;
    parser::Parser::new(toks)
        .parse_program()
        .map_err(|e| format!("parse error in {path} at {}:{}: {}", e.span.line, e.span.col, e.message))
}

/// Each `use "path.nir"` in `program` (already parsed, not yet
/// resolved) is loaded relative to `importing_path`'s own directory,
/// recursively resolves *its own* imports the same way, then
/// **typechecks standalone** — own source, own diagnostics, exactly as
/// if it were the entry file itself — before anything is merged. An
/// imported file must be independently valid on its own, full stop.
/// This is also what keeps every error's `line:col` correct with no
/// multi-file source map anywhere in the compiler: a `TypeError`/
/// `ParseError` raised while typechecking file X always already
/// carries X's own `line:col` (`typeck.rs`'s `Span`s are computed
/// purely from X's own token stream) — this fn only ever adds an "in
/// <path>:" prefix for context, never tries to re-render a source
/// excerpt (nothing in this compiler's error `Display` impls do that
/// at all, checked directly — `TypeError`/`ParseError` both print
/// exactly `"{line}:{col}: {message}"`, no source line quoted).
///
/// Only `pub`, real-namespace (`ns: Some(_)`) declarations are merged
/// into `program`'s own flat `fns`/`structs`/`enums` — a non-namespaced
/// (top-level) declaration in an imported file is never visible to the
/// importer, deliberately: only explicitly-exported, explicitly-
/// namespaced items ever cross a file boundary, so merging can never
/// reopen the flat-namespace collision risk `ns` exists to close (see
/// `ast::scope_key`'s doc comment).
fn resolve_imports(program: &mut Program, importing_path: &str, visited: &mut HashSet<PathBuf>) -> Result<(), String> {
    let base_dir = Path::new(importing_path).parent().unwrap_or_else(|| Path::new("."));
    let imports = std::mem::take(&mut program.imports);
    for imp in &imports {
        let target = base_dir.join(&imp.path);
        let canon = std::fs::canonicalize(&target)
            .map_err(|e| format!("error resolving `use \"{}\"` from {importing_path}: {e}", imp.path))?;
        if !visited.insert(canon.clone()) {
            return Err(format!("import cycle detected: `use \"{}\"` from {importing_path}", imp.path));
        }
        let target_str = target.to_string_lossy().to_string();
        let target_src = std::fs::read_to_string(&target).map_err(|e| format!("error reading {target_str}: {e}"))?;
        let mut imported = parse_one(&target_src, &target_str)?;
        // Nested imports resolve relative to *that* file's own
        // directory, recursively, before it's typechecked standalone —
        // same rule as the entry file's own imports, one shared
        // `visited` set threading cycle detection through the whole
        // graph regardless of depth.
        resolve_imports(&mut imported, &target_str, visited)?;
        if let Err(errors) = crate::typeck::typecheck_optional_main(&imported) {
            let joined = errors.iter().map(|e| format!("type error: {e}")).collect::<Vec<_>>().join("\n");
            return Err(format!("in {target_str}:\n{joined}"));
        }
        merge_exported(program, imported, &target_str)?;
    }
    Ok(())
}

/// Pulls every `pub`, real-namespace fn/struct/enum out of `imported`
/// into `program`. A namespace id (`ns`) already present in `program`
/// (from its own source, or from an earlier merged import) colliding
/// with one `imported` also declares is a real `Err`, not a silent
/// overwrite — two different files each declaring `module Audit { }`
/// is exactly as ambiguous as one file declaring it twice, and
/// `typeck.rs`'s own registration pass (keyed by `ast::scope_key`)
/// would only catch this *after* the silent overwrite already lost
/// one side's declarations, so it's caught explicitly, here, first.
fn merge_exported(program: &mut Program, imported: Program, from_path: &str) -> Result<(), String> {
    let mut existing_ns: HashSet<String> = HashSet::new();
    existing_ns.extend(program.fns.iter().filter_map(fn_ns));
    existing_ns.extend(program.structs.iter().filter_map(|s| s.ns.clone()));
    existing_ns.extend(program.enums.iter().filter_map(|e| e.ns.clone()));

    let mut incoming_ns: HashSet<String> = HashSet::new();
    incoming_ns.extend(imported.fns.iter().filter(|f| f.exported).filter_map(fn_ns));
    incoming_ns.extend(imported.structs.iter().filter(|s| s.exported).filter_map(|s| s.ns.clone()));
    incoming_ns.extend(imported.enums.iter().filter(|e| e.exported).filter_map(|e| e.ns.clone()));

    if let Some(dup) = incoming_ns.intersection(&existing_ns).next() {
        return Err(format!(
            "module `{dup}` (imported from {from_path}) collides with an already-declared module of the same name"
        ));
    }

    program.fns.extend(imported.fns.into_iter().filter(|f| f.ns.is_some() && f.exported));
    program.structs.extend(imported.structs.into_iter().filter(|s| s.ns.is_some() && s.exported));
    program.enums.extend(imported.enums.into_iter().filter(|e| e.ns.is_some() && e.exported));
    Ok(())
}

fn fn_ns(f: &FnDecl) -> Option<String> {
    f.ns.clone()
}
