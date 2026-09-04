//! Tier-1 contract checking — `docs/API_TRUST_MODEL.md` §7.5's proposed
//! extension to `smt.rs`, built out for real. §7.5 named exactly what
//! was missing before this file existed: "nothing anywhere accepts a
//! predicate *written by a human or a story extractor* as input" —
//! `smt.rs` only ever proves obligations it synthesizes itself while
//! walking a `let`/division/index. This is that missing obligation
//! channel: given a real `.nir` function and a Hoare predicate string
//! (a user story's `pre_logic`/`post_logic`, or a `workflow`'s
//! `routing_fn` contract — exactly the shape
//! `scratch/extracted_typed_v1.json` carries), either prove the
//! predicate holds for *every* input the function's declared parameter
//! types admit, or produce a concrete counterexample.
//!
//! **Scope, deliberately narrow — the same boundary §7.5 already named,
//! not loosened here:** integer parameters and an integer return value
//! only (no `f64`, no `bool` return, no `struct`/`enum`); no loops, no
//! calls, no division (truncation semantics would need separate,
//! careful modeling — the same reason `smt.rs` never asserts division's
//! *result* as an equality either); no interprocedural reasoning (a
//! predicate can only talk about the one function's own params/return).
//! Anything outside that shape is `Unsupported`, reported honestly, not
//! silently approximated — approximating an unmodelable sub-expression
//! with a fresh unconstrained value would be sound for a *proof*
//! (over-approximation only ever weakens what can be proven) but
//! **unsound for a counterexample** (a "violation" built partly out of a
//! meaningless free variable might not correspond to any real input/
//! output of the function at all) — so this walker aborts the moment it
//! can't model something, on both sides.
//!
//! **The `high_value_threshold` case, and why `extra_bindings`
//! exists.** `scratch/extracted_typed_v1.json`'s `WF-TRDPAY-001.
//! routing_fn.post_logic` is `"(result == 2) == (amount_cents >=
//! high_value_threshold)"` — but the real `required_eyes_for_amount`
//! (`examples/trade-finance/trade_finance.nir`) takes only
//! `amount_cents`; `high_value_threshold` isn't one of its parameters,
//! it's a PRD concept the code hardcodes as a literal. §7.1a already
//! named this exact gap: a story's predicate and the code's actual
//! parameterization can disagree about what's a variable and what's a
//! constant. Rather than silently treating `high_value_threshold` as
//! either "any value" (which would make the predicate unprovable for a
//! reason that has nothing to do with the code being wrong) or "some
//! value nobody chose," `check_fn_contract` requires the caller to
//! supply a concrete value for every such name via `extra_bindings` —
//! `UnboundIdentifier` is the honest result when they don't, naming
//! exactly the missing piece instead of a confusing SMT failure.

use std::collections::{HashMap, HashSet};

use z3::ast::{Bool, Int};
use z3::{SatResult, Solver};

use crate::ast::*;
use crate::parser::parse_standalone_expr;

#[derive(Debug, Clone, PartialEq)]
pub enum ContractCheckResult {
    /// The predicate holds on every path, for every input satisfying the
    /// function's own declared parameter-type bounds (and, for a
    /// `pre_logic` predicate, that predicate holds too — see
    /// `check_fn_pre_and_post_contract`).
    Proved,
    /// A concrete input (and, where computable, the return value it
    /// produces) that satisfies every `pre_logic` predicate but violates
    /// `violated_predicate` — real numbers, not a symbolic report; feed
    /// them straight into `nir_scenario!` or an integration test to
    /// reproduce it.
    Counterexample { violated_predicate: String, bindings: Vec<(String, i64)>, result: Option<i64> },
    /// A name in the predicate is neither `result`, nor `fn_name`'s own
    /// parameter, nor supplied in `extra_bindings` — §7.1a's "the spec
    /// references a quantity the code doesn't parameterize on" case.
    UnboundIdentifier(String),
    /// No function named `fn_name` exists in `program`.
    NoSuchFunction(String),
    /// `predicate_src` isn't a valid Nirdosha expression.
    PredicateParseError(String),
    /// The function or the predicate uses a shape this Tier-1 walker
    /// doesn't model (loops, calls, floats, non-integer params/return,
    /// division, an unresolvable bare-bool identifier, ...) — an honest
    /// "can't decide," never a silently wrong `Proved`/`Counterexample`.
    Unsupported(String),
}

/// Checks a real Hoare triple `{pre_logic} fn_name {post_logic}` — every
/// `pre_logic` entry is asserted as a *hypothesis* (an input not
/// satisfying it is simply not searched — this is what makes it a
/// precondition, not another universal claim), then every `post_logic`
/// entry must hold, with `result` bound to the function's actual return
/// value, on every path the function can take under that hypothesis.
/// `extra_bindings` supplies a concrete value for every identifier
/// either list mentions that isn't `fn_name`'s own parameter or `result`
/// — see the module doc's `high_value_threshold` example for why this is
/// a required, explicit input rather than an inferred default. Passing
/// an empty `pre_logic` checks `post_logic` over the function's full
/// declared-type domain, same as no precondition at all.
pub fn check_fn_contract(
    program: &Program,
    fn_name: &str,
    pre_logic: &[String],
    post_logic: &[String],
    extra_bindings: &HashMap<String, i64>,
) -> ContractCheckResult {
    let Some(f) = program.fns.iter().find(|f| f.name == fn_name) else {
        return ContractCheckResult::NoSuchFunction(fn_name.to_string());
    };
    let mut pre_exprs = Vec::new();
    for src in pre_logic {
        match parse_standalone_expr(src) {
            Ok(e) => pre_exprs.push((src.clone(), e)),
            Err(msg) => return ContractCheckResult::PredicateParseError(msg),
        }
    }
    let mut post_exprs = Vec::new();
    for src in post_logic {
        match parse_standalone_expr(src) {
            Ok(e) => post_exprs.push((src.clone(), e)),
            Err(msg) => return ContractCheckResult::PredicateParseError(msg),
        }
    }
    check_fn_contract_parsed(f, &pre_exprs, &post_exprs, extra_bindings, &HashMap::new())
}

/// Same Tier-1 walker as `check_fn_contract` above, fed a `.nir`
/// `validate <fn_name> { pre: <expr>  post: <expr> }` block's
/// already-parsed `Expr`s directly (`ast::ValidateDecl` —
/// `docs/ROADMAP.md` Track F, F3) instead of predicate strings parsed out of
/// an extraction JSON. No `extra_bindings` — a `validate` block can only
/// reference `fn_name`'s own real parameters (and `result`, in `post`);
/// the `extra_bindings` escape hatch stays scoped to the string-based
/// extraction pipeline above, which needs it for exactly the disclosed
/// "the spec references a quantity the code doesn't parameterize on"
/// case (this module's own doc comment) that inline `.nir` source
/// doesn't have a symmetric problem with — an author writing a contract
/// against their own fn can just... use the fn's real parameter names.
/// `{:?}` Debug-formats each expr for use as its own "predicate name" in
/// a `Counterexample`/error message, since there's no original source
/// string to quote post-parse (same precedent this file's own
/// `int_expr`/`bool_expr` already set for an `Unsupported` message). No
/// interprocedural summaries either — a single isolated call gets none
/// of the other `validate` blocks in the same program as context; see
/// `run_program_validates` for the whole-program version that builds
/// and uses them.
pub fn check_fn_contract_exprs(program: &Program, fn_name: &str, pre: &[Expr], post: &[Expr]) -> ContractCheckResult {
    check_fn_contract_exprs_with_summaries(program, fn_name, pre, post, &HashMap::new())
}

fn check_fn_contract_exprs_with_summaries(
    program: &Program,
    fn_name: &str,
    pre: &[Expr],
    post: &[Expr],
    summaries: &HashMap<String, Summary>,
) -> ContractCheckResult {
    let Some(f) = program.fns.iter().find(|f| f.name == fn_name) else {
        return ContractCheckResult::NoSuchFunction(fn_name.to_string());
    };
    let pre_exprs: Vec<(String, Expr)> = pre.iter().map(|e| (format!("{e:?}"), e.clone())).collect();
    let post_exprs: Vec<(String, Expr)> = post.iter().map(|e| (format!("{e:?}"), e.clone())).collect();
    check_fn_contract_parsed(f, &pre_exprs, &post_exprs, &HashMap::new(), summaries)
}

/// A callee's independently-*proven* Hoare contract, promoted to a fact
/// any caller's own proof may use about the result of calling it
/// (`docs/ROADMAP.md` Track F, F3's disclosed "no per-function pre/
/// postcondition inference exists yet" gap — closed for the bounded,
/// sound case this covers). Used as `pre(callee's args) =>
/// post(callee's args, result)`, an implication, never an unconditional
/// fact — so a call site whose arguments don't (or can't be shown to)
/// satisfy the callee's own precondition gets a vacuous, uninformative
/// axiom instead of a wrong one; see `Eval::int_expr`'s `Expr::Call` arm
/// for where this is actually asserted. Built once, up front, by
/// `run_program_validates` below, and only ever for a callee whose own
/// contract was independently `Proved` first — never for one that's
/// merely declared, `Unsupported`, or itself depends on an
/// as-yet-unproven callee. That's the one invariant that keeps this
/// sound rather than assuming away the exact class of bug this whole
/// file exists to catch: a summary can only be *used* once it's
/// genuinely *earned*.
struct Summary {
    params: Vec<Param>,
    ret: Ty,
    pre: Vec<Expr>,
    post: Vec<Expr>,
}

/// One `validate <fn_name> { ... }` block's static-check outcome —
/// `run_program_validates`' own return shape, consumed two different
/// ways by `check_program_contracts`/`unsupported_validate_notes` below.
pub struct ValidateOutcome {
    pub fn_name: String,
    pub result: ContractCheckResult,
}

/// Runs every `validate` block in `program`, splitting each
/// `ValidateDecl.entries` into its `pre`/`post` `Expr`s first (the
/// AST-level shape `ast::ValidateDecl` stores them in — the same
/// generic `KvEntry` list `screen`/`dashboard` already use, filtered
/// here rather than given dedicated struct fields) — and, unlike a
/// single isolated `check_fn_contract_exprs` call, doing it as a real
/// **bounded fixed-point pass across the whole program**: a `Call`
/// inside one `validate` block's target fn (previously an automatic
/// `Unsupported`, no exceptions) can now be resolved using another
/// already-`validate`d function's own *proven* summary as an axiom
/// (`Summary`, above) — the interprocedural reasoning this file's own
/// module doc has disclosed as missing since A12. Each pass re-checks
/// every block with whatever summaries the *previous* pass managed to
/// prove; any block that newly resolves to `Proved` is promoted into
/// `summaries` for the *next* pass. Bounded by `program.validates.len()`
/// iterations — each pass either promotes at least one new summary or
/// changes nothing at all, in which case every further pass can only
/// repeat the same result, so a fixed point is reached well before that
/// bound in practice; no separate cycle-detection needed; a fn whose own
/// contract depends on a callee's, whose contract depends back on the
/// first (mutual recursion) simply never gets promoted by either pass
/// and stays honestly `Unsupported` — not a wrong answer, the same
/// "decline rather than guess" discipline this whole file already holds
/// to. A function with more than one `validate` block uses the *first*
/// one (declaration order) that resolves to `Proved` as its summary,
/// deliberately not a union of all of them — simpler, still fully
/// sound (a strict subset of what's actually provable), and avoids a
/// second merging step for a shape (`validate max_of {...} validate
/// max_of {...}`) rare enough not to need it.
pub fn run_program_validates(program: &Program) -> Vec<ValidateOutcome> {
    let mut summaries: HashMap<String, Summary> = HashMap::new();
    let mut outcomes: Vec<ValidateOutcome> = Vec::new();
    for _ in 0..=program.validates.len() {
        let mut progressed = false;
        outcomes = program
            .validates
            .iter()
            .map(|v| {
                let pre: Vec<Expr> = v.entries.iter().filter(|(k, _)| k == "pre").map(|(_, e)| e.clone()).collect();
                let post: Vec<Expr> = v.entries.iter().filter(|(k, _)| k == "post").map(|(_, e)| e.clone()).collect();
                let result = check_fn_contract_exprs_with_summaries(program, &v.fn_name, &pre, &post, &summaries);
                if result == ContractCheckResult::Proved && !summaries.contains_key(&v.fn_name) {
                    if let Some(f) = program.fns.iter().find(|f| f.name == v.fn_name) {
                        summaries.insert(
                            v.fn_name.clone(),
                            Summary { params: f.params.clone(), ret: f.ret.clone(), pre, post },
                        );
                        progressed = true;
                    }
                }
                ValidateOutcome { fn_name: v.fn_name.clone(), result }
            })
            .collect();
        if !progressed {
            break;
        }
    }
    outcomes
}

/// The build-time "self-check and fail" gate (`docs/ROADMAP.md` Track F, F3
/// — called once from `main.rs::typecheck_and_own_impl`, right after
/// `typeck`/`ownership` both pass, so it runs for every command that
/// owns a typechecked program: `build`/`run`/`serve`/`emit-ui`/
/// `emit-llvm`/`typecheck`). Hard-fails only on a genuine, *proven*
/// defect — a real Z3 counterexample, an identifier the contract
/// references that doesn't resolve, or (defensively; `typeck::
/// check_validate` already catches this earlier) a `fn_name` that
/// doesn't exist. **Does not fail on `Unsupported`** — a contract this
/// Tier-1 walker can't statically model (the fn touches `db`/`json`/
/// `http`, calls another fn, or loops — true of nearly every real fn in
/// a real app) is neither proved nor disproved here; it's still
/// enforced, just at runtime instead (`interpreter.rs::call`) — see
/// `unsupported_validate_notes` for the matching non-fatal notice.
pub fn check_program_contracts(program: &Program) -> Result<(), Vec<String>> {
    let errors: Vec<String> = run_program_validates(program).iter().filter_map(contract_error_message).collect();
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors)
    }
}

/// `--format=json`'s structured sibling of `check_program_contracts`
/// above — same hard-fail criteria (a genuine, *proven* defect only;
/// `Unsupported` never appears here either), same underlying
/// `run_program_validates` computation, just paired with `span` (the
/// *`validate` block's own* span, `ast::ValidateDecl.span` — the fn
/// itself is unannotated; the contract is what actually failed) for
/// `lib.rs::Diagnostic::Contract`, `run_diagnostic_*`'s one JSON-shaped
/// consumer.
pub fn check_program_contracts_diagnostics(program: &Program) -> Vec<ContractDiagnostic> {
    program
        .validates
        .iter()
        .zip(run_program_validates(program).iter())
        .filter_map(|(decl, outcome)| Some(ContractDiagnostic { message: contract_error_message(outcome)?, span: decl.span }))
        .collect()
}

/// `None` for `Proved`/`Unsupported` (nothing to report — see
/// `check_program_contracts`'s own doc comment for why `Unsupported`
/// specifically is never an error here); `Some(message)` for every
/// other `ContractCheckResult`, shared verbatim by both public entry
/// points above so their wording never drifts apart.
fn contract_error_message(outcome: &ValidateOutcome) -> Option<String> {
    match &outcome.result {
        ContractCheckResult::Proved | ContractCheckResult::Unsupported(_) => None,
        ContractCheckResult::Counterexample { violated_predicate, bindings, result } => {
            let bindings_str = bindings.iter().map(|(n, v)| format!("{n} = {v}")).collect::<Vec<_>>().join(", ");
            Some(format!(
                "`validate {}`: `{violated_predicate}` is violated when {bindings_str} (fn returns {})",
                outcome.fn_name,
                result.map(|r| r.to_string()).unwrap_or_else(|| "<uncomputed>".to_string())
            ))
        }
        ContractCheckResult::UnboundIdentifier(name) => Some(format!(
            "`validate {}`: `{name}` is neither `result` nor one of `{}`'s own parameters",
            outcome.fn_name, outcome.fn_name
        )),
        ContractCheckResult::NoSuchFunction(name) => {
            Some(format!("`validate {name}`: no such function (should have been caught by typeck)"))
        }
        ContractCheckResult::PredicateParseError(msg) => {
            Some(format!("`validate {}`: {msg} (should be unreachable — already a parsed `.nir` expr)", outcome.fn_name))
        }
    }
}

/// `check_program_contracts_diagnostics`'s one payload shape —
/// `lib.rs::Diagnostic::Contract`'s inner type, same `{message, span}`
/// flatness `LexError`/`ParseError` already use for a diagnostic with no
/// further structured `kind` breakdown (`lib.rs::Diagnostic`'s own doc
/// comment on why `Lex`/`Parse` stay flat).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ContractDiagnostic {
    pub message: String,
    pub span: crate::token::Span,
}

/// The non-fatal half of `check_program_contracts`: one human-readable
/// note per `validate` block this Tier-1 walker honestly couldn't
/// statically decide, so an author sees *why* — same "surface the
/// previously-silent case" precedent `typeck::TypeWarning`/
/// `print_ungated_fn_warnings` already set for an ungated `fn`. Callers
/// print these (or not) and continue either way; nothing here blocks a
/// build. Still enforced — just at runtime, unconditionally, by
/// `interpreter.rs::call`.
pub fn unsupported_validate_notes(program: &Program) -> Vec<String> {
    run_program_validates(program)
        .into_iter()
        .filter_map(|o| match o.result {
            ContractCheckResult::Unsupported(msg) => Some(format!(
                "note: `validate {}`'s contract could not be proven statically ({msg}) — enforced at runtime on every call instead",
                o.fn_name
            )),
            _ => None,
        })
        .collect()
}

fn check_fn_contract_parsed(
    f: &FnDecl,
    pre_exprs: &[(String, Expr)],
    post_exprs: &[(String, Expr)],
    extra_bindings: &HashMap<String, i64>,
    summaries: &HashMap<String, Summary>,
) -> ContractCheckResult {
    for p in &f.params {
        if !p.ty.is_integer() {
            return ContractCheckResult::Unsupported(format!(
                "parameter `{}` has type `{}` — Tier 1 only models integer parameters today",
                p.name,
                p.ty.name()
            ));
        }
    }
    if !f.ret.is_integer() {
        return ContractCheckResult::Unsupported(format!(
            "`{}` returns `{}` — Tier 1 only models an integer-returning function today",
            f.name,
            f.ret.name()
        ));
    }

    let param_names: HashSet<&str> = f.params.iter().map(|p| p.name.as_str()).collect();
    let mut free = HashSet::new();
    for (_, e) in pre_exprs.iter().chain(post_exprs.iter()) {
        collect_idents(e, &mut free);
    }
    for name in &free {
        if name == "result" || param_names.contains(name.as_str()) || extra_bindings.contains_key(name.as_str()) {
            continue;
        }
        return ContractCheckResult::UnboundIdentifier(name.clone());
    }

    let solver = Solver::new();
    let mut top = HashMap::new();
    for p in &f.params {
        let term = Int::fresh_const(&p.name);
        assert_bounds(&solver, &term, &p.ty);
        top.insert(p.name.clone(), term);
    }
    for (name, value) in extra_bindings {
        if param_names.contains(name.as_str()) {
            // A real parameter always wins over a same-named extra
            // binding — the function's own signature is ground truth.
            continue;
        }
        let term = Int::fresh_const(name);
        solver.assert(term.eq(Int::from_i64(*value)));
        top.insert(name.clone(), term);
    }

    let mut scopes = Scopes(vec![top]);
    let mut eval = Eval { solver: &solver, post_logic: &post_exprs, outcome: None, summaries };
    // Assert every precondition as a hypothesis *before* walking the
    // body — everything downstream (including every `return` point's
    // counterexample search) then only ever considers inputs where
    // `pre_logic` actually holds.
    for (src, e) in pre_exprs {
        match eval.bool_expr(e, &mut scopes) {
            Ok(b) => solver.assert(b),
            Err(msg) => return ContractCheckResult::Unsupported(format!("pre_logic `{src}`: {msg}")),
        }
    }
    if let Err(msg) = eval.stmts(&f.body.stmts, &mut scopes) {
        return ContractCheckResult::Unsupported(msg);
    }
    match eval.outcome {
        Some(outcome) => outcome,
        None => ContractCheckResult::Proved,
    }
}

fn assert_bounds(solver: &Solver, term: &Int, ty: &Ty) {
    let (lo, hi) = ty.bounds();
    solver.assert(term.ge(Int::from_i64(lo)));
    solver.assert(term.le(Int::from_i64(hi)));
}

fn collect_idents(e: &Expr, out: &mut HashSet<String>) {
    match e {
        Expr::Ident(name, _) => {
            out.insert(name.clone());
        }
        Expr::Int(_, _) | Expr::Float(_, _) | Expr::Str(_, _) | Expr::Bool(_, _) | Expr::Chan(_) => {}
        Expr::Unary(_, inner, _)
        | Expr::Box(inner, _)
        | Expr::Deref(inner, _)
        | Expr::Ref(inner, _)
        | Expr::Join(inner, _)
        | Expr::Recv(inner, _)
        | Expr::StopSandbox(inner, _)
        | Expr::FieldAccess(inner, _, _) => collect_idents(inner, out),
        Expr::Binary(_, l, r, _) => {
            collect_idents(l, out);
            collect_idents(r, out);
        }
        Expr::Assign(name, rhs, _) => {
            out.insert(name.clone());
            collect_idents(rhs, out);
        }
        Expr::Call(_, args, _) | Expr::Spawn(_, args, _) | Expr::SpawnSandbox(_, args, _) => {
            for a in args {
                collect_idents(a, out);
            }
        }
        Expr::Acquire(name, proof, _) => {
            out.insert(name.clone());
            collect_idents(proof, out);
        }
        Expr::Send(a, b, _) | Expr::Connect(a, b, _) => {
            collect_idents(a, out);
            collect_idents(b, out);
        }
        Expr::Listen(a, _) | Expr::Open(a, _, _) | Expr::Accept(a, _) => collect_idents(a, out),
        Expr::Index(base, indices, _) => {
            collect_idents(base, out);
            for i in indices {
                collect_idents(i, out);
            }
        }
        Expr::ArrayLit(elements, _) => {
            for e in elements {
                collect_idents(e, out);
            }
        }
        Expr::If { cond, then_block, else_block, .. } => {
            collect_idents(cond, out);
            collect_idents_block(then_block, out);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => collect_idents_block(b, out),
                Some(ElseBranch::If(e)) => collect_idents(e, out),
                None => {}
            }
        }
        Expr::Match { scrutinee, arms, .. } => {
            collect_idents(scrutinee, out);
            for arm in arms {
                collect_idents(&arm.body, out);
            }
        }
        Expr::Transact { precheck, network, verify, commit, compensate, log, .. } => {
            for call in [precheck.as_ref(), Some(network), Some(verify), Some(commit), compensate.as_ref(), log.as_ref()]
                .into_iter()
                .flatten()
            {
                for a in &call.args {
                    collect_idents(a, out);
                }
            }
        }
    }
}

fn collect_idents_block(b: &Block, out: &mut HashSet<String>) {
    collect_idents_stmts(&b.stmts, out);
}

fn collect_idents_stmts(stmts: &[Stmt], out: &mut HashSet<String>) {
    for s in stmts {
        match s {
            Stmt::Let { value, .. } => collect_idents(value, out),
            Stmt::Return { value: Some(e), .. } => collect_idents(e, out),
            Stmt::Return { value: None, .. } => {}
            Stmt::While { cond, body, .. } => {
                collect_idents(cond, out);
                collect_idents_block(body, out);
            }
            Stmt::Expr(e) => collect_idents(e, out),
            Stmt::Audited { body, .. } => collect_idents_stmts(body, out),
        }
    }
}

/// Name -> symbolic term, block-scoped — same shape and reasoning as
/// `smt.rs::Scopes`, duplicated rather than shared (that file's own doc
/// comments already establish the precedent: two independently-evolving
/// analyses over superficially-similar walks are kept apart on purpose).
struct Scopes(Vec<HashMap<String, Int>>);

impl Scopes {
    fn push(&mut self) {
        self.0.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.0.pop();
    }
    fn get(&self, name: &str) -> Option<Int> {
        self.0.iter().rev().find_map(|s| s.get(name)).cloned()
    }
    fn define(&mut self, name: &str, term: Int) {
        self.0.last_mut().unwrap().insert(name.to_string(), term);
    }
}

struct Eval<'s> {
    solver: &'s Solver,
    /// `(source text, parsed)` for every `post_logic` entry — kept
    /// paired with its own source string so a counterexample can name
    /// exactly which clause it violates, not just "some post_logic
    /// failed."
    post_logic: &'s [(String, Expr)],
    /// Set at most once — the first violating return path found. `stmt`/
    /// `stmts` check this at entry and skip further work once it's `Some`,
    /// the cheapest possible short-circuit (a single counterexample
    /// already disproves "holds for every input").
    outcome: Option<ContractCheckResult>,
    /// Independently-*proven* summaries of other `validate`d functions
    /// (`run_program_validates`'s whole-program fixed point) — consulted
    /// by `int_expr`'s `Expr::Call` arm, the interprocedural reasoning
    /// `Summary`'s own doc comment describes. Empty for a single isolated
    /// `check_fn_contract`/`check_fn_contract_exprs` call, same as
    /// before this existed — a `Call` inside such a check is still an
    /// honest `Unsupported`.
    summaries: &'s HashMap<String, Summary>,
}

type EvalResult<T> = Result<T, String>;

/// What happens to control flow after one statement, on whatever path
/// reaches it — `stmt`'s own return type (`docs/ROADMAP.md` Track F, F3:
/// found live, via `validate`'s first real exercise of this walker on
/// ordinary early-return-shaped code, not the single hand-picked
/// flagship demo `check_fn_contract` had before — `if cond { return x }
/// return y`-style statement-position early return, ubiquitous
/// throughout this codebase's own `.nir` examples, is a completely
/// different shape from `required_eyes_for_amount`'s single
/// value-position `if {...} else {...}`, which never needed this).
/// Before this type existed, a `Stmt::Return` inside an `if` branch was
/// checked correctly (`check_return` ran under that branch's own pushed
/// condition) but its "definitely returned" fact was then silently
/// dropped — `stmts`' loop just kept walking the *next* sibling
/// statement unconditionally, with no branch condition asserted at all,
/// so a genuinely unreachable trailing `return` got checked as if
/// reachable for *every* input, producing real, wrong `Counterexample`s
/// against genuinely-correct functions — precisely the "silently wrong
/// answer" this module's own doc comment says the walker must never
/// produce (over-approximating what's reachable is unsound for a
/// counterexample the exact same way over-approximating an unmodelable
/// expression already was). The same `Result<Value, Signal::Return(_)>`
/// propagation shape `interpreter.rs::exec_stmts`'s real, correct
/// control flow already uses — this walker just never modeled it.
enum Flow {
    /// An ordinary statement — falls through unconditionally.
    Continues,
    /// Every path through this statement hits `return` — nothing after
    /// it, in the same straight-line sequence, is reachable at all.
    Returns,
    /// An `if` where exactly one branch returns and the other falls
    /// through — code after it is reachable only under the surviving
    /// branch's own condition, which `stmts` must assert for the *rest*
    /// of the enclosing sequence (not just re-check for this one
    /// statement) — an absent `else` behaves exactly like a `then` that
    /// returns and an implicit empty `else` that doesn't.
    ContinuesUnder(Bool),
}

impl Eval<'_> {
    /// `Ok(true)` means every path through `stmts` definitely hit a
    /// `return` — a real reachability fact, not just "no error
    /// occurred," consumed by an enclosing `if`'s own `Flow` computation
    /// (`stmt`'s `Expr::If` arm, below) — see `Flow`'s own doc comment
    /// for why this exists and what it fixed. A `Flow::ContinuesUnder`
    /// reported by one statement is asserted onto the solver for every
    /// statement *after* it in this same call — correctly narrowing
    /// which inputs can even reach them — and popped once this whole
    /// sequence is done being walked; on an `Err` (`Unsupported`) exit
    /// the pushes are simply abandoned, harmless since the whole
    /// `Solver` this walker owns is discarded right after by its one
    /// caller, `check_fn_contract_parsed`.
    fn stmts(&mut self, stmts: &[Stmt], scopes: &mut Scopes) -> EvalResult<bool> {
        let mut extra_pushes = 0usize;
        for s in stmts {
            if self.outcome.is_some() {
                self.solver.pop(extra_pushes as u32);
                return Ok(false);
            }
            match self.stmt(s, scopes)? {
                Flow::Continues => {}
                Flow::Returns => {
                    self.solver.pop(extra_pushes as u32);
                    return Ok(true);
                }
                Flow::ContinuesUnder(cond) => {
                    self.solver.push();
                    self.solver.assert(cond);
                    extra_pushes += 1;
                }
            }
        }
        self.solver.pop(extra_pushes as u32);
        Ok(false)
    }

    fn stmt(&mut self, s: &Stmt, scopes: &mut Scopes) -> EvalResult<Flow> {
        match s {
            Stmt::Let { name, ty, value, .. } => {
                if !ty.is_integer() {
                    return Err(format!("`let {name}: {}` — Tier 1 only models integer locals", ty.name()));
                }
                let term = self.int_expr(value, scopes)?;
                assert_bounds(self.solver, &term, ty);
                scopes.define(name, term);
                Ok(Flow::Continues)
            }
            Stmt::Return { value: Some(e), .. } => {
                let term = self.int_expr(e, scopes)?;
                self.check_return(term, scopes)?;
                Ok(Flow::Returns)
            }
            Stmt::Return { value: None, .. } => Ok(Flow::Returns),
            Stmt::Expr(Expr::If { cond, then_block, else_block, .. }) => {
                let cond_term = self.bool_expr(cond, scopes)?;
                self.solver.push();
                self.solver.assert(cond_term.clone());
                let then_returns = self.block(then_block, scopes)?;
                self.solver.pop(1);

                self.solver.push();
                self.solver.assert(cond_term.not());
                let else_returns = match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => self.block(b, scopes)?,
                    Some(ElseBranch::If(e2)) => matches!(self.stmt(&Stmt::Expr(e2.clone()), scopes)?, Flow::Returns),
                    // No `else` at all: the "condition didn't hold" path
                    // always falls through to whatever comes after this
                    // `if` — never a return by itself.
                    None => false,
                };
                self.solver.pop(1);
                Ok(match (then_returns, else_returns) {
                    (true, true) => Flow::Returns,
                    (true, false) => Flow::ContinuesUnder(cond_term.not()),
                    (false, true) => Flow::ContinuesUnder(cond_term),
                    (false, false) => Flow::Continues,
                })
            }
            Stmt::Expr(e) => {
                self.int_expr(e, scopes)?;
                Ok(Flow::Continues)
            }
            Stmt::While { .. } => Err("Tier 1 doesn't model loops (no invariant synthesis, same conservative choice smt.rs's own Tier-1 pass makes)".to_string()),
            Stmt::Audited { body, .. } => {
                scopes.push();
                let r = self.stmts(body, scopes);
                scopes.pop();
                r.map(|returns| if returns { Flow::Returns } else { Flow::Continues })
            }
        }
    }

    fn block(&mut self, b: &Block, scopes: &mut Scopes) -> EvalResult<bool> {
        scopes.push();
        let r = self.stmts(&b.stmts, scopes);
        scopes.pop();
        r
    }

    /// The value a block produces when used in value position (the
    /// `then`/`else` half of a value-position `if`) — its last statement,
    /// if that statement is a bare expression; anything else has no
    /// value this walker can extract.
    fn block_value(&mut self, b: &Block, scopes: &mut Scopes) -> EvalResult<Int> {
        scopes.push();
        let r = match b.stmts.split_last() {
            Some((Stmt::Expr(e), rest)) => {
                self.stmts(rest, scopes)?;
                self.int_expr(e, scopes)
            }
            _ => Err("Tier 1 needs a value-position `if`'s branch to end in a bare expression".to_string()),
        };
        scopes.pop();
        r
    }

    /// Reached a `return`: bind `result` to `term` and check whether
    /// `self.predicate` can be violated given everything asserted on the
    /// path that reached this point (every enclosing branch condition —
    /// already live in `self.solver` via the `push`/`assert`/pop pairs in
    /// `stmt`'s `if` handling). Unsat negation == proved on this path;
    /// sat == a real counterexample, extracted from the model.
    fn check_return(&mut self, term: Int, scopes: &mut Scopes) -> EvalResult<()> {
        // Every `post_logic` clause is checked independently — a
        // counterexample names exactly the one clause it violates,
        // rather than an opaque "the conjunction failed." First
        // violation found wins (matches `outcome`'s own "first found"
        // short-circuit); the rest go unchecked on this path once one
        // clause already disproves "holds everywhere."
        for (src, predicate) in self.post_logic {
            if self.outcome.is_some() {
                return Ok(());
            }
            scopes.push();
            scopes.define("result", term.clone());
            let holds = self.bool_expr(predicate, scopes);
            scopes.pop();
            let holds = holds?;

            self.solver.push();
            self.solver.assert(holds.not());
            let sat = self.solver.check();
            if sat == SatResult::Sat {
                let model = self.solver.get_model().expect("SAT result has a model");
                let mut bindings = Vec::new();
                for scope in &scopes.0 {
                    for (name, t) in scope {
                        if name == "result" {
                            continue;
                        }
                        if let Some(v) = model.eval(t, true).and_then(|v| v.as_i64()) {
                            bindings.push((name.clone(), v));
                        }
                    }
                }
                let result = model.eval(&term, true).and_then(|v| v.as_i64());
                self.outcome = Some(ContractCheckResult::Counterexample { violated_predicate: src.clone(), bindings, result });
            }
            self.solver.pop(1);
        }
        Ok(())
    }

    fn int_expr(&mut self, e: &Expr, scopes: &mut Scopes) -> EvalResult<Int> {
        match e {
            Expr::Int(n, _) => Ok(Int::from_i64(*n)),
            Expr::Ident(name, _) => scopes.get(name).ok_or_else(|| format!("unbound identifier `{name}`")),
            Expr::Unary(UnOp::Neg, inner, _) => Ok(-self.int_expr(inner, scopes)?),
            Expr::Binary(BinOp::Add, l, r, _) => Ok(self.int_expr(l, scopes)? + self.int_expr(r, scopes)?),
            Expr::Binary(BinOp::Sub, l, r, _) => Ok(self.int_expr(l, scopes)? - self.int_expr(r, scopes)?),
            Expr::Binary(BinOp::Mul, l, r, _) => Ok(self.int_expr(l, scopes)? * self.int_expr(r, scopes)?),
            Expr::Binary(BinOp::Div, _, _, _) => {
                Err("Tier 1 doesn't model division's result (integer-truncation semantics, same conservative choice smt.rs makes)".to_string())
            }
            Expr::If { cond, then_block, else_block, .. } => {
                let cond_term = self.bool_expr(cond, scopes)?;
                self.solver.push();
                self.solver.assert(cond_term.clone());
                let then_val = self.block_value(then_block, scopes);
                self.solver.pop(1);
                let then_val = then_val?;

                self.solver.push();
                self.solver.assert(cond_term.not());
                let else_val = match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => self.block_value(b, scopes),
                    Some(ElseBranch::If(e2)) => self.int_expr(e2, scopes),
                    None => Err("Tier 1 needs a value-position `if` to have an `else`".to_string()),
                };
                self.solver.pop(1);
                Ok(cond_term.ite(&then_val, &else_val?))
            }
            // Interprocedural reasoning (`docs/ROADMAP.md` Track F, F3;
            // `Summary`'s own doc comment) — bounded and sound: only a
            // callee whose own `validate` contract was *already proven*
            // (by an earlier pass of `run_program_validates`) has a
            // `Summary` here at all; anything else (no `validate` block,
            // one that's `Unsupported`, or a mutual-recursion cycle that
            // never resolves) is still an honest `Unsupported`, exactly
            // as before this arm existed.
            Expr::Call(name, args, _) => {
                let Some(summary) = self.summaries.get(name.as_str()) else {
                    return Err(format!(
                        "Tier 1 doesn't model a call to `{name}` (no independently-proven `validate` contract on it to use as a summary)"
                    ));
                };
                if summary.params.len() != args.len() {
                    return Err(format!("Tier 1: call to `{name}` has the wrong argument count (should be unreachable, typeck already checked this)"));
                }
                // Each argument is evaluated in *this* call's own scope
                // (the caller's params/locals), then bound to the
                // callee's own parameter names in a fresh, separate
                // scope below — the callee's `pre`/`post` never see the
                // caller's names at all, only its own.
                let arg_terms: Vec<Int> =
                    args.iter().map(|a| self.int_expr(a, scopes)).collect::<Result<_, _>>()?;
                let result_term = Int::fresh_const(&format!("{name}_call"));
                assert_bounds(self.solver, &result_term, &summary.ret);
                let mut call_scope = Scopes(vec![HashMap::new()]);
                for (p, term) in summary.params.iter().zip(arg_terms.iter()) {
                    call_scope.define(&p.name, term.clone());
                }
                let mut pre_conjunction = Bool::from_bool(true);
                for pre_e in &summary.pre {
                    pre_conjunction = pre_conjunction & self.bool_expr(pre_e, &mut call_scope)?;
                }
                call_scope.define("result", result_term.clone());
                // `pre => post`, an implication, never `post` on its own
                // — a call site whose arguments don't satisfy the
                // callee's own precondition gets a vacuous axiom here
                // (true regardless of `result`), not a wrong one.
                for post_e in &summary.post {
                    let post_term = self.bool_expr(post_e, &mut call_scope)?;
                    self.solver.assert(pre_conjunction.clone().not() | post_term);
                }
                Ok(result_term)
            }
            other => Err(format!("Tier 1 doesn't model `{other:?}` — only integer literals/identifiers, +-*, if/else, and a call to an independently-proven `validate`d fn are supported")),
        }
    }

    /// Same "unrecognized shape is honestly `Unsupported`, never a
    /// silent free variable" discipline as `int_expr`. The one deliberate
    /// piece of extra machinery here, absent from `smt.rs::bool_expr`:
    /// `Eq`/`NotEq` recurse into `bool_expr` (not `int_expr`) when both
    /// operands are themselves boolean-shaped — needed for exactly the
    /// biconditional idiom `scratch/extracted_typed_v1.json`'s own
    /// `routing_fn.post_logic` uses, `(result == 2) == (amount_cents >=
    /// high_value_threshold)`: the outer `==` is Boolean equality
    /// (iff) between two comparisons, not integer equality between two
    /// numbers. `smt.rs`'s `bool_expr` predates having any predicate
    /// shaped like this (every obligation it synthesizes itself is a
    /// plain numeric comparison) and would silently mis-evaluate this
    /// one — not a bug there, just untested territory this file actually
    /// needs to get right.
    fn bool_expr(&mut self, e: &Expr, scopes: &mut Scopes) -> EvalResult<Bool> {
        match e {
            Expr::Bool(b, _) => Ok(Bool::from_bool(*b)),
            Expr::Unary(UnOp::Not, inner, _) => Ok(self.bool_expr(inner, scopes)?.not()),
            Expr::Binary(BinOp::And, l, r, _) => Ok(self.bool_expr(l, scopes)? & self.bool_expr(r, scopes)?),
            Expr::Binary(BinOp::Or, l, r, _) => Ok(self.bool_expr(l, scopes)? | self.bool_expr(r, scopes)?),
            Expr::Binary(BinOp::Eq, l, r, _) if is_bool_shaped(l) && is_bool_shaped(r) => {
                Ok(self.bool_expr(l, scopes)?.eq(self.bool_expr(r, scopes)?))
            }
            Expr::Binary(BinOp::NotEq, l, r, _) if is_bool_shaped(l) && is_bool_shaped(r) => {
                Ok(self.bool_expr(l, scopes)?.eq(self.bool_expr(r, scopes)?).not())
            }
            Expr::Binary(BinOp::Eq, l, r, _) => Ok(self.int_expr(l, scopes)?.eq(self.int_expr(r, scopes)?)),
            Expr::Binary(BinOp::NotEq, l, r, _) => Ok(self.int_expr(l, scopes)?.eq(self.int_expr(r, scopes)?).not()),
            Expr::Binary(BinOp::Lt, l, r, _) => Ok(self.int_expr(l, scopes)?.lt(self.int_expr(r, scopes)?)),
            Expr::Binary(BinOp::Gt, l, r, _) => Ok(self.int_expr(l, scopes)?.gt(self.int_expr(r, scopes)?)),
            Expr::Binary(BinOp::LtEq, l, r, _) => Ok(self.int_expr(l, scopes)?.le(self.int_expr(r, scopes)?)),
            Expr::Binary(BinOp::GtEq, l, r, _) => Ok(self.int_expr(l, scopes)?.ge(self.int_expr(r, scopes)?)),
            other => Err(format!(
                "Tier 1 doesn't model `{other:?}` as a boolean expression — only comparisons, `&&`/`||`/`!`, and a boolean-shaped `==`/`!=` are supported"
            )),
        }
    }
}

fn is_bool_shaped(e: &Expr) -> bool {
    matches!(
        e,
        Expr::Bool(_, _)
            | Expr::Unary(UnOp::Not, _, _)
            | Expr::Binary(BinOp::And, _, _, _)
            | Expr::Binary(BinOp::Or, _, _, _)
            | Expr::Binary(BinOp::Eq, _, _, _)
            | Expr::Binary(BinOp::NotEq, _, _, _)
            | Expr::Binary(BinOp::Lt, _, _, _)
            | Expr::Binary(BinOp::Gt, _, _, _)
            | Expr::Binary(BinOp::LtEq, _, _, _)
            | Expr::Binary(BinOp::GtEq, _, _, _)
    )
}
