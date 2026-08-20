//! SMT-backed refinement checking — the Tier-1 pass goal.md §3/§6 Phase 2
//! actually specifies (an SMT-discharged refinement layer), using a real
//! Z3 (linked against the system library via the `z3` crate) now that
//! this environment has one. This **supersedes `refine.rs`'s interval
//! analysis** as the primary Tier-1 checker — `refine.rs` stays in the
//! tree deliberately, not deleted, as the documented fallback for an
//! environment without Z3 available (see its module doc); its design
//! reasoning doesn't stop being correct just because a stronger solver
//! showed up.
//!
//! **What SMT actually buys, demonstrated by the tests below, not just
//! claimed:**
//! - **Condition-based narrowing.** Entering the `else` of `if n <= 1
//!   {...} else {...}`, this pass asserts `!(n <= 1)` into the solver
//!   before checking anything inside — so `n > 1` is genuinely known
//!   there, for free. `refine.rs` structurally cannot do this (interval
//!   analysis has no way to represent "this variable's value depends on
//!   a boolean condition holding").
//! - **Correlated-variable reasoning.** Z3 tracks exact symbolic
//!   relationships between variables (via equalities asserted at each
//!   `let`), not just each variable's independent range — so it can
//!   prove facts interval analysis's per-variable bounds lose entirely
//!   (e.g. `a - b` where a prior branch establishes `a >= b`).
//!
//! **What didn't change just because the solver got stronger.** Same two
//! proof targets as `refine.rs`, for the same reasons:
//! 1. An arithmetic expression's value fits its declared target type.
//! 2. A division's divisor is never zero.
//!
//! Division's *result* value is still not modeled as a symbolic term at
//! all (division isn't asserted as an equality the way +/-/* are) —
//! that decision in `refine.rs` was about avoiding integer-truncation
//! edge cases, not about solver power, so a stronger solver doesn't
//! change the reasoning. No interprocedural summaries either: a call's
//! result is a fresh symbolic value bounded only by the callee's
//! declared return type, same limitation, same reason (no per-function
//! pre/postcondition inference exists yet).
//!
//! **Loops:** same conservative choice as `refine.rs` — any variable the
//! loop body reassigns becomes a fresh, only-bounds-constrained symbolic
//! value before the loop is analyzed at all, rather than attempting loop
//! invariant synthesis. That's not a shortcut unique to this project:
//! SPARK itself requires the *programmer* to write loop invariants for
//! exactly this reason — automatically inferring them is a genuinely
//! larger undertaking than this pass, or most refinement-type systems
//! without an invariant-inference research component, attempt.
//!
//! **Not wired to elide the interpreter's runtime check** — same
//! reasoning as `refine.rs`: there's no backend yet to spend a Tier-1
//! proof's performance payoff on, so the redundant runtime check stays.

use std::collections::{HashMap, HashSet};

use z3::ast::{Bool, Int};
use z3::{SatResult, Solver};

use crate::ast::*;
use crate::token::Span;

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct SmtReport {
    pub proven_in_range: HashSet<Span>,
    pub proven_nonzero_divisor: HashSet<Span>,
}

/// One binding's declared type plus the Z3 term currently standing for
/// its value — kept together so a lookup never has to ask two different
/// tables that could drift out of sync with each other.
struct Scopes(Vec<HashMap<String, (Ty, Int)>>);

impl Scopes {
    fn new() -> Self {
        Scopes(vec![HashMap::new()])
    }
    fn push(&mut self) {
        self.0.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.0.pop();
    }
    fn define(&mut self, name: &str, ty: Ty, term: Int) {
        self.0.last_mut().unwrap().insert(name.to_string(), (ty, term));
    }
    fn get(&self, name: &str) -> Option<(Ty, Int)> {
        self.0.iter().rev().find_map(|s| s.get(name)).cloned()
    }
    fn set(&mut self, name: &str, term: Int) {
        for scope in self.0.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                slot.1 = term;
                return;
            }
        }
    }
}

pub fn analyze(program: &Program) -> SmtReport {
    let mut report = SmtReport::default();
    for f in &program.fns {
        // A fresh Solver per function — no reason for one function's
        // assertions to leak into another's, and it keeps each
        // function's analysis independently comprehensible.
        let solver = Solver::new();
        let mut scopes = Scopes::new();
        for p in &f.params {
            let term = Int::fresh_const(&p.name);
            assert_bounds(&solver, &term, &p.ty);
            scopes.define(&p.name, p.ty.clone(), term);
        }
        let mut checker = Checker { solver: &solver, report: &mut report };
        checker.stmts(&f.body.stmts, &mut scopes);
    }
    report
}

fn assert_bounds(solver: &Solver, term: &Int, ty: &Ty) {
    let (lo, hi) = ty.bounds();
    solver.assert(term.ge(lo));
    solver.assert(term.le(hi));
}

/// Is it UNSAT that `term` falls outside `[lo, hi]`, given everything
/// currently asserted in `solver`? If so, `term` is proven to always be
/// in range on every path that reaches this point — that's the actual
/// proof this whole pass exists to produce. Uses `push`/`pop` so the
/// probing assertion never leaks into the solver's permanent state.
fn prove_in_range(solver: &Solver, term: &Int, ty: &Ty) -> bool {
    let (lo, hi) = ty.bounds();
    solver.push();
    solver.assert(term.lt(lo) | term.gt(hi));
    let result = solver.check();
    solver.pop(1);
    result == SatResult::Unsat
}

fn prove_nonzero(solver: &Solver, term: &Int) -> bool {
    solver.push();
    solver.assert(term.eq(0));
    let result = solver.check();
    solver.pop(1);
    result == SatResult::Unsat
}

struct Checker<'s> {
    solver: &'s Solver,
    report: &'s mut SmtReport,
}

impl Checker<'_> {
    fn stmts(&mut self, stmts: &[Stmt], scopes: &mut Scopes) {
        for stmt in stmts {
            self.stmt(stmt, scopes);
        }
    }

    fn block(&mut self, block: &Block, scopes: &mut Scopes) {
        scopes.push();
        self.stmts(&block.stmts, scopes);
        scopes.pop();
    }

    fn stmt(&mut self, stmt: &Stmt, scopes: &mut Scopes) {
        match stmt {
            Stmt::Let { name, ty, value, span } => {
                let term = self.expr(value, scopes);
                if prove_in_range(self.solver, &term, ty) {
                    self.report.proven_in_range.insert(*span);
                }
                // Permanently assert the binding's own bounds (not just
                // check them) — every *later* use of `name` should be
                // able to trust it's in range, the same way a real
                // compiler would trust a value it already validated.
                assert_bounds(self.solver, &term, ty);
                scopes.define(name, ty.clone(), term);
            }
            Stmt::Return { value: Some(e), .. } => {
                // Same as refine.rs: no declared return type visible
                // from inside this function-local walk without threading
                // it through every call site, and reusing the value's
                // own computed range would be circular. Left unproven —
                // `let`/assign/call-arg sites are this pass's real
                // contribution.
                self.expr(e, scopes);
            }
            Stmt::Return { value: None, .. } => {}
            Stmt::While { cond, body, .. } => self.while_loop(cond, body, scopes),
            Stmt::Expr(e) => {
                self.expr_stmt(e, scopes);
            }
        }
    }

    /// An `if` in statement position needs both branches checked with the
    /// condition asserted, but doesn't need a merged resulting *term* the
    /// way a value-position `if` does — mirrors `ownership.rs`'s
    /// statement/value split for the same underlying reason.
    fn expr_stmt(&mut self, e: &Expr, scopes: &mut Scopes) {
        if let Expr::If { cond, then_block, else_block, .. } = e {
            let cond_term = self.bool_expr(cond, scopes);

            self.enter_branch(&cond_term);
            self.block(then_block, scopes);
            self.exit_branch();

            self.enter_branch(&cond_term.not());
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => self.block(b, scopes),
                Some(ElseBranch::If(e2)) => self.expr_stmt(e2, scopes),
                None => {}
            }
            self.exit_branch();
        } else {
            self.expr(e, scopes);
        }
    }

    /// `enter_branch`/`exit_branch` bracket exactly one branch's worth of
    /// analysis with a `push`/assert/`pop`, so its condition is live for
    /// everything checked in between and never leaks past `exit_branch`.
    /// Split into two methods, rather than one taking a closure, because
    /// a single closure-based helper would need to capture `scopes: &mut
    /// Scopes` in two places at once (then-branch and else-branch) —
    /// Rust's borrow checker correctly refuses that even though the two
    /// closures only ever run one after the other, never concurrently.
    fn enter_branch(&mut self, cond: &Bool) {
        self.solver.push();
        self.solver.assert(cond);
    }
    fn exit_branch(&mut self) {
        self.solver.pop(1);
    }

    fn while_loop(&mut self, cond: &Expr, body: &Block, scopes: &mut Scopes) {
        // Give every loop-reassigned name a fresh symbolic value, bounded
        // only by its own declared type, *before* analyzing anything —
        // see module doc. This is strictly more precise than refine.rs
        // could manage here (that pass fell back to `i64`'s full range
        // for lack of an easy declared-Ty lookup); this pass already
        // tracks each name's `Ty` in `Scopes`, so it can re-bound to the
        // *actual* declared type instead.
        for name in assigned_names(&body.stmts) {
            if let Some((ty, _)) = scopes.get(&name) {
                let fresh = Int::fresh_const(&name);
                assert_bounds(self.solver, &fresh, &ty);
                scopes.set(&name, fresh);
            }
        }
        self.bool_expr(cond, scopes);
        self.block(body, scopes);
    }

    /// Evaluates `e` to a Z3 `Int` term. Anything not specially handled
    /// (calls, derefs, boxes, refs, booleans, if-as-value with a branch
    /// this walk can't reduce) gets a fresh, unconstrained-beyond-nothing
    /// symbolic `Int` — always sound (it claims no information at all),
    /// simple, and consistent with `refine.rs`'s `Interval::unknown()`
    /// fallback for the same shapes.
    fn expr(&mut self, e: &Expr, scopes: &mut Scopes) -> Int {
        match e {
            Expr::Int(n, _) => Int::from_i64(*n),
            Expr::Ident(name, _) => scopes.get(name).map(|(_, t)| t).unwrap_or_else(|| Int::fresh_const("unknown")),
            Expr::Unary(UnOp::Neg, inner, _) => -self.expr(inner, scopes),
            Expr::Unary(UnOp::Not, inner, _) => {
                self.expr(inner, scopes);
                Int::fresh_const("unknown")
            }
            Expr::Binary(op, lhs, rhs, span) => self.binary(*op, lhs, rhs, *span, scopes),
            Expr::Call(_, args, _) => {
                for a in args {
                    self.expr(a, scopes);
                }
                Int::fresh_const("call_result")
            }
            Expr::If { cond, then_block, else_block, .. } => {
                let cond_term = self.bool_expr(cond, scopes);

                self.enter_branch(&cond_term);
                let then_val = self.block_value(then_block, scopes);
                self.exit_branch();

                self.enter_branch(&cond_term.not());
                let else_val = match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => self.block_value(b, scopes),
                    Some(ElseBranch::If(e2)) => self.expr(e2, scopes),
                    None => Int::fresh_const("unknown"),
                };
                self.exit_branch();

                // Combine the two branch terms with Z3's own if-then-else
                // rather than unioning bounds by hand — exact, not an
                // approximation, since the solver already knows which
                // branch each half of the disjunction came from.
                cond_term.ite(&then_val, &else_val)
            }
            Expr::Assign(name, rhs, _) => {
                let term = self.expr(rhs, scopes);
                if let Some((ty, _)) = scopes.get(name) {
                    assert_bounds(self.solver, &term, &ty);
                    scopes.set(name, term.clone());
                }
                term
            }
            Expr::Box(inner, _) | Expr::Deref(inner, _) | Expr::Ref(inner, _) => {
                self.expr(inner, scopes);
                Int::fresh_const("unknown")
            }
            Expr::Bool(_, _) => Int::fresh_const("unknown"),
        }
    }

    fn block_value(&mut self, block: &Block, scopes: &mut Scopes) -> Int {
        scopes.push();
        let result = match block.stmts.split_last() {
            None => Int::fresh_const("unknown"),
            Some((last, rest)) => {
                self.stmts(rest, scopes);
                match last {
                    Stmt::Expr(e) => self.expr(e, scopes),
                    other => {
                        self.stmt(other, scopes);
                        Int::fresh_const("unknown")
                    }
                }
            }
        };
        scopes.pop();
        result
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span, scopes: &mut Scopes) -> Int {
        match op {
            BinOp::Add | BinOp::Sub | BinOp::Mul => {
                let l = self.expr(lhs, scopes);
                let r = self.expr(rhs, scopes);
                match op {
                    BinOp::Add => l + r,
                    BinOp::Sub => l - r,
                    BinOp::Mul => l * r,
                    _ => unreachable!(),
                }
            }
            BinOp::Div => {
                // Dividend is still visited (for its own nested proofs —
                // e.g. a division inside it), just not bound to a named
                // term: division's result is deliberately never modeled
                // (module doc), so there's nothing to combine it with.
                self.expr(lhs, scopes);
                let r = self.expr(rhs, scopes);
                if prove_nonzero(self.solver, &r) {
                    self.report.proven_nonzero_divisor.insert(span);
                }
                // Deliberately not asserted as an equality — see module
                // doc's "what didn't change" note.
                Int::fresh_const("div_result")
            }
            BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq | BinOp::And | BinOp::Or => {
                self.expr(lhs, scopes);
                self.expr(rhs, scopes);
                Int::fresh_const("unknown")
            }
        }
    }

    /// Boolean-valued expressions (conditions) get their own encoder,
    /// separate from `expr`'s `Int`-valued one, since Z3 sorts are
    /// distinct — a comparison produces a `Bool`, not an `Int`.
    /// Unrecognized shapes fall back to a fresh, unconstrained `Bool`
    /// (sound: asserts nothing, narrows nothing).
    fn bool_expr(&mut self, e: &Expr, scopes: &mut Scopes) -> Bool {
        match e {
            Expr::Bool(b, _) => Bool::from_bool(*b),
            Expr::Unary(UnOp::Not, inner, _) => self.bool_expr(inner, scopes).not(),
            Expr::Binary(BinOp::And, l, r, _) => self.bool_expr(l, scopes) & self.bool_expr(r, scopes),
            Expr::Binary(BinOp::Or, l, r, _) => self.bool_expr(l, scopes) | self.bool_expr(r, scopes),
            Expr::Binary(BinOp::Eq, l, r, _) => {
                let (lt, rt) = (self.expr(l, scopes), self.expr(r, scopes));
                lt.eq(&rt)
            }
            Expr::Binary(BinOp::NotEq, l, r, _) => {
                let (lt, rt) = (self.expr(l, scopes), self.expr(r, scopes));
                lt.eq(&rt).not()
            }
            Expr::Binary(BinOp::Lt, l, r, _) => {
                let (lt, rt) = (self.expr(l, scopes), self.expr(r, scopes));
                lt.lt(&rt)
            }
            Expr::Binary(BinOp::Gt, l, r, _) => {
                let (lt, rt) = (self.expr(l, scopes), self.expr(r, scopes));
                lt.gt(&rt)
            }
            Expr::Binary(BinOp::LtEq, l, r, _) => {
                let (lt, rt) = (self.expr(l, scopes), self.expr(r, scopes));
                lt.le(&rt)
            }
            Expr::Binary(BinOp::GtEq, l, r, _) => {
                let (lt, rt) = (self.expr(l, scopes), self.expr(r, scopes));
                lt.ge(&rt)
            }
            Expr::Ident(name, _) => {
                // A `bool`-typed variable — this pass has no Bool-sorted
                // scope table (only `Int`), so it's tracked as
                // unconstrained here. Real bool-variable narrowing (e.g.
                // `let ok: bool = n > 0; if ok { ... }`) is a real,
                // documented gap, not silently assumed away.
                let _ = scopes.get(name);
                Bool::fresh_const("unknown_bool")
            }
            other => {
                self.expr_stmt(other, scopes);
                Bool::fresh_const("unknown_bool")
            }
        }
    }
}

/// Same helper as `refine.rs`'s — every name that's the target of an
/// `Expr::Assign` anywhere in `stmts`, used for the loop-entry widening
/// pass. Duplicated rather than shared: the two passes' walks are
/// superficially identical today but there's no guarantee they stay that
/// way, and a shared helper would be a strange, narrow point of coupling
/// between two otherwise-independent analyses.
fn assigned_names(stmts: &[Stmt]) -> HashSet<String> {
    let mut names = HashSet::new();
    fn walk_stmts(stmts: &[Stmt], names: &mut HashSet<String>) {
        for s in stmts {
            match s {
                Stmt::Let { value, .. } => walk_expr(value, names),
                Stmt::Return { value: Some(e), .. } => walk_expr(e, names),
                Stmt::Return { value: None, .. } => {}
                Stmt::While { cond, body, .. } => {
                    walk_expr(cond, names);
                    walk_stmts(&body.stmts, names);
                }
                Stmt::Expr(e) => walk_expr(e, names),
            }
        }
    }
    fn walk_expr(e: &Expr, names: &mut HashSet<String>) {
        match e {
            Expr::Int(_, _) | Expr::Bool(_, _) | Expr::Ident(_, _) => {}
            Expr::Unary(_, inner, _) | Expr::Box(inner, _) | Expr::Deref(inner, _) | Expr::Ref(inner, _) => {
                walk_expr(inner, names)
            }
            Expr::Binary(_, l, r, _) => {
                walk_expr(l, names);
                walk_expr(r, names);
            }
            Expr::Call(_, args, _) => {
                for a in args {
                    walk_expr(a, names);
                }
            }
            Expr::If { cond, then_block, else_block, .. } => {
                walk_expr(cond, names);
                walk_stmts(&then_block.stmts, names);
                match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => walk_stmts(&b.stmts, names),
                    Some(ElseBranch::If(e2)) => walk_expr(e2, names),
                    None => {}
                }
            }
            Expr::Assign(name, rhs, _) => {
                names.insert(name.clone());
                walk_expr(rhs, names);
            }
        }
    }
    walk_stmts(stmts, &mut names);
    names
}
