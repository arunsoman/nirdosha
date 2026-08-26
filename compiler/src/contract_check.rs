//! Tier-1 contract checking — `API_TRUST_MODEL.md` §7.5's proposed
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
            "`{fn_name}` returns `{}` — Tier 1 only models an integer-returning function today",
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
    let mut eval = Eval { solver: &solver, post_logic: &post_exprs, outcome: None };
    // Assert every precondition as a hypothesis *before* walking the
    // body — everything downstream (including every `return` point's
    // counterexample search) then only ever considers inputs where
    // `pre_logic` actually holds.
    for (src, e) in &pre_exprs {
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
}

type EvalResult<T> = Result<T, String>;

impl Eval<'_> {
    fn stmts(&mut self, stmts: &[Stmt], scopes: &mut Scopes) -> EvalResult<()> {
        for s in stmts {
            if self.outcome.is_some() {
                return Ok(());
            }
            self.stmt(s, scopes)?;
        }
        Ok(())
    }

    fn stmt(&mut self, s: &Stmt, scopes: &mut Scopes) -> EvalResult<()> {
        match s {
            Stmt::Let { name, ty, value, .. } => {
                if !ty.is_integer() {
                    return Err(format!("`let {name}: {}` — Tier 1 only models integer locals", ty.name()));
                }
                let term = self.int_expr(value, scopes)?;
                assert_bounds(self.solver, &term, ty);
                scopes.define(name, term);
                Ok(())
            }
            Stmt::Return { value: Some(e), .. } => {
                let term = self.int_expr(e, scopes)?;
                self.check_return(term, scopes)
            }
            Stmt::Return { value: None, .. } => Ok(()),
            Stmt::Expr(Expr::If { cond, then_block, else_block, .. }) => {
                let cond_term = self.bool_expr(cond, scopes)?;
                self.solver.push();
                self.solver.assert(cond_term.clone());
                self.block(then_block, scopes)?;
                self.solver.pop(1);

                self.solver.push();
                self.solver.assert(cond_term.not());
                match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => self.block(b, scopes)?,
                    Some(ElseBranch::If(e2)) => self.stmt(&Stmt::Expr(e2.clone()), scopes)?,
                    None => {}
                }
                self.solver.pop(1);
                Ok(())
            }
            Stmt::Expr(e) => {
                self.int_expr(e, scopes)?;
                Ok(())
            }
            Stmt::While { .. } => Err("Tier 1 doesn't model loops (no invariant synthesis, same conservative choice smt.rs's own Tier-1 pass makes)".to_string()),
            Stmt::Audited { body, .. } => {
                scopes.push();
                let r = self.stmts(body, scopes);
                scopes.pop();
                r
            }
        }
    }

    fn block(&mut self, b: &Block, scopes: &mut Scopes) -> EvalResult<()> {
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
            other => Err(format!("Tier 1 doesn't model `{other:?}` — only integer literals/identifiers, +-*, and if/else are supported")),
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
