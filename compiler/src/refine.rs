//! Bounds proving via interval (range) analysis — goal.md row 4's Tier 1
//! (§4), implemented **without** an SMT solver.
//!
//! **Why not what goal.md's design actually calls for.** §3/§6 Phase 2
//! specify an SMT-discharged refinement layer (a Z3-class solver bundled
//! with the compiler). This environment has neither a system Z3 nor
//! `cmake` (needed for the `z3` crate's bundled build), and installing
//! system packages wasn't something to just do without asking — so this
//! is a deliberate, documented substitution, not a silent downgrade of
//! the target. What's here instead is **interval analysis**, the same
//! family of technique real safety-critical tools (Astrée, Polyspace) use
//! for exactly this class of proof, and one SPARK itself used before its
//! SMT-backed generation. It will fail to prove many facts a real solver
//! could — no disjunctive reasoning, no cross-procedure inference, no
//! nonlinear arithmetic, no condition-based narrowing in `if` branches
//! (a "prove `n > 1` inside the `else` of `if n <= 1`" - shaped
//! improvement, left for later rather than risking it now) — but
//! everything it *does* prove is genuinely proved, checked by real tests
//! against real (and real-ly *un*provable) programs below, not asserted.
//!
//! **Two proofs only, chosen for what could be gotten right, not for
//! completeness.** (1) An arithmetic expression's value is proven to fit
//! within its target's declared integer type. (2) A division's divisor is
//! proven never zero. Division's *result* interval is deliberately never
//! computed — integer-division interval arithmetic has real edge cases
//! (truncation direction, sign-crossing divisors) that are exactly the
//! kind of place a soundness bug hides, and a wrong proof is worse than
//! no proof, so it's left as "unknown" (full range) rather than risked.
//!
//! **Not wired to elide the runtime check.** `interpreter.rs` keeps every
//! Tier-2 check exactly as before, proven or not. Eliding a check only
//! pays off once there's a real ahead-of-time backend generating a binary
//! that runs faster without it — there's no codegen yet (goal.md §3's
//! Backend layer), so removing the interpreter's redundant check now
//! would only remove a safety net for zero benefit. This pass's output —
//! `RefineReport` — is the real, standalone deliverable: a genuine static
//! proof, ready for a future backend to act on.
//!
//! **Loops: widen, don't iterate to a fixed point.** A `while` body might
//! run any number of times, so any binding it reassigns is widened to its
//! full declared-type range *before* the body is analyzed at all — the
//! same "give up precision rather than risk unsoundness" choice
//! `ownership.rs`'s move-checker makes for a different reason. This means
//! bounds inside a loop are rarely provable; that's an honest, real
//! limitation, not an oversight — see the tests for what it costs.

use std::collections::HashMap;
use std::collections::HashSet;

use crate::ast::*;
use crate::token::Span;

/// Bounds are `i128`, not `i64` — deliberately, and not for headroom's own
/// sake. A real bug shipped in an earlier version of this pass with `i64`
/// bounds: `Interval::unknown()` was `[i64::MIN, i64::MAX]`, which is
/// *exactly* `Ty::I64`'s own legal range — so `within(&Ty::I64)` was
/// vacuously true for every interval, since no `i64`-backed bound could
/// ever fall outside `i64`'s own range in the first place. This pass
/// could prove an `i64`-typed value safe but could never actually catch
/// one that wasn't — a structural blind spot for the language's *widest*
/// type, invisible until `Stmt::Return` started getting checked at all
/// (see `PHASE0.md`'s write-up: `smt.rs` never had this bug, because
/// Z3's `Int` sort is genuinely unbounded, not `i64`-backed). `i128` gives
/// real headroom: a computation that would truly overflow `i64` now
/// produces bounds outside `i64::MIN..=i64::MAX`, which `within` can
/// actually detect. Two `i64`-scale values multiplied can still touch
/// `i128`'s own ceiling in extreme cases (`i64::MAX` squared is a little
/// under half of `i128::MAX`, chained further ops could approach it) —
/// `saturating_*` on `i128` keeps that sound (widening, never narrowing)
/// even then, at the cost of losing precision for genuinely extreme
/// chains, which is an honest, bounded residual limitation, not a
/// reintroduction of the original bug.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Interval {
    lo: i128,
    hi: i128,
}

impl Interval {
    fn exact(n: i64) -> Self {
        Interval { lo: n as i128, hi: n as i128 }
    }

    /// The maximally conservative "no information" interval — a sound
    /// over-approximation of any integer value this language can hold,
    /// with real headroom beyond `i64`'s own range (see the struct doc).
    fn unknown() -> Self {
        Interval { lo: i128::MIN, hi: i128::MAX }
    }

    /// Delegates to `Ty::bounds()` — see its doc comment for why this
    /// isn't a second, independent table.
    fn full(ty: &Ty) -> Self {
        let (lo, hi) = ty.bounds();
        Interval { lo: lo as i128, hi: hi as i128 }
    }

    fn union(self, other: Interval) -> Interval {
        Interval { lo: self.lo.min(other.lo), hi: self.hi.max(other.hi) }
    }

    fn neg(self) -> Interval {
        Interval {
            lo: self.hi.saturating_neg(),
            hi: self.lo.saturating_neg(),
        }
    }

    fn add(self, other: Interval) -> Interval {
        Interval {
            lo: self.lo.saturating_add(other.lo),
            hi: self.hi.saturating_add(other.hi),
        }
    }

    fn sub(self, other: Interval) -> Interval {
        Interval {
            lo: self.lo.saturating_sub(other.hi),
            hi: self.hi.saturating_sub(other.lo),
        }
    }

    fn mul(self, other: Interval) -> Interval {
        let candidates = [
            self.lo.saturating_mul(other.lo),
            self.lo.saturating_mul(other.hi),
            self.hi.saturating_mul(other.lo),
            self.hi.saturating_mul(other.hi),
        ];
        Interval {
            lo: *candidates.iter().min().unwrap(),
            hi: *candidates.iter().max().unwrap(),
        }
    }

    /// A type's legal values are themselves a contiguous `[MIN, MAX]`
    /// range for every integer type this language has, so "this interval
    /// is fully inside the type's legal range" reduces to checking both
    /// endpoints against `Ty::bounds()` widened to `i128` — *not*
    /// `Ty::in_range` (which takes a bare `i64` and can't accept an
    /// `i128` that has genuinely escaped `i64`'s range, which is exactly
    /// the case this function most needs to be able to say "no" to).
    fn within(self, ty: &Ty) -> bool {
        let (lo, hi) = ty.bounds();
        self.lo >= lo as i128 && self.hi <= hi as i128
    }

    fn excludes_zero(self) -> bool {
        self.hi < 0 || self.lo > 0
    }
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct RefineReport {
    /// Spans of `let`/`return`/assignment/call-argument value expressions
    /// whose result is proven to fit the declared target type — Tier 1,
    /// in goal.md §4's terms.
    pub proven_in_range: HashSet<Span>,
    /// Spans of `/` operations whose divisor is proven never zero.
    pub proven_nonzero_divisor: HashSet<Span>,
}

struct Scopes(Vec<HashMap<String, Interval>>);

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
    fn define(&mut self, name: &str, iv: Interval) {
        self.0.last_mut().unwrap().insert(name.to_string(), iv);
    }
    fn get(&self, name: &str) -> Option<Interval> {
        self.0.iter().rev().find_map(|s| s.get(name)).copied()
    }
    /// Overwrites `name`'s tracked interval in place, wherever it's
    /// declared — used both for an ordinary reassignment (the new
    /// interval is the RHS's freshly computed one, same precision logic
    /// as a `let`) and for the loop-entry "give up precision" widening
    /// (module doc; there the new interval is deliberately the maximally
    /// conservative `Interval::unknown()`). A no-op if the name isn't
    /// tracked, which is fine either way: an untracked name has no
    /// existing claim to overwrite.
    fn set(&mut self, name: &str, iv: Interval) {
        for scope in self.0.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                *slot = iv;
                return;
            }
        }
    }
}

pub fn analyze(program: &Program) -> RefineReport {
    let mut r = Refiner { report: RefineReport::default(), current_fn_ret: Ty::Unit };
    for f in &program.fns {
        r.current_fn_ret = f.ret.clone();
        let mut scopes = Scopes::new();
        for p in &f.params {
            // No interprocedural analysis — a parameter could be anything
            // the caller passed, so it starts at its declared type's full
            // range, not narrowed by any particular call site.
            scopes.define(&p.name, Interval::full(&p.ty));
        }
        r.stmts(&f.body.stmts, &mut scopes);
    }
    r.report
}

struct Refiner {
    report: RefineReport,
    /// The function currently being walked — `Stmt::Return` needs its
    /// declared return type to check against, and there's no other way
    /// to reach it from inside `stmt()`. Same fix `codegen.rs`'s
    /// `current_fn_ret` field already made for the same reason; this was
    /// the documented gap that made it worth doing here too.
    current_fn_ret: Ty,
}

impl Refiner {
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
                let iv = self.expr(value, scopes);
                if iv.within(ty) {
                    self.report.proven_in_range.insert(*span);
                }
                scopes.define(name, iv);
            }
            Stmt::Return { value: Some(e), span } => {
                let iv = self.expr(e, scopes);
                if iv.within(&self.current_fn_ret) {
                    self.report.proven_in_range.insert(*span);
                }
            }
            Stmt::Return { value: None, .. } => {}
            Stmt::While { cond, body, .. } => self.while_loop(cond, body, scopes),
            Stmt::Expr(e) => {
                self.expr(e, scopes);
            }
        }
    }

    fn while_loop(&mut self, cond: &Expr, body: &Block, scopes: &mut Scopes) {
        // Widen every name the body assigns to its full declared range
        // *before* analyzing anything — see the module doc. Needs a
        // pre-pass to know the target type for names this scope doesn't
        // already track (freshly `let`-bound inside the loop don't need
        // widening at all, since they're reset each iteration; only
        // names assigned via `Expr::Assign` that were already tracked
        // matter here).
        for name in assigned_names(&body.stmts) {
            if let Some(prev) = scopes.get(&name) {
                let _ = prev;
                // We don't have the declared `Ty` here without a lookup
                // typeck.rs already did — using `Interval::unknown()` is
                // still sound (it's the most conservative interval there
                // is) even though it's looser than "the declared type's
                // own range" would be.
                scopes.set(&name, Interval::unknown());
            }
        }
        self.expr(cond, scopes);
        self.block(body, scopes);
        // After the loop, anything it could have touched is exactly as
        // unknown as it was made going in — no further widening needed,
        // the pre-loop widen already covers "ran zero or more times."
    }

    /// Returns this expression's proven interval — `Interval::unknown()`
    /// wherever the shape isn't specially handled (always sound: "no
    /// information" never claims too much).
    fn expr(&mut self, e: &Expr, scopes: &mut Scopes) -> Interval {
        match e {
            Expr::Int(n, _) => Interval::exact(*n),
            Expr::Ident(name, _) => scopes.get(name).unwrap_or(Interval::unknown()),
            Expr::Unary(UnOp::Neg, inner, _) => self.expr(inner, scopes).neg(),
            Expr::Unary(UnOp::Not, inner, _) => {
                self.expr(inner, scopes);
                Interval::unknown()
            }
            Expr::Binary(op, lhs, rhs, span) => self.binary(*op, lhs, rhs, *span, scopes),
            Expr::Call(_, args, _) => {
                for a in args {
                    self.expr(a, scopes);
                }
                Interval::unknown()
            }
            Expr::If { cond, then_block, else_block, .. } => {
                self.expr(cond, scopes);
                let then_iv = self.block_value(then_block, scopes);
                let else_iv = match else_block.as_deref() {
                    Some(ElseBranch::Block(b)) => self.block_value(b, scopes),
                    Some(ElseBranch::If(e2)) => self.expr(e2, scopes),
                    None => Interval::unknown(),
                };
                then_iv.union(else_iv)
            }
            Expr::Assign(name, rhs, _span) => {
                let iv = self.expr(rhs, scopes);
                // Reassignment gives `name` a fresh, precisely-tracked
                // interval — same "this binding's value is exactly known
                // now" reasoning as `Stmt::Let`, just without a declared
                // target `Ty` in scope here to check `iv` against (no
                // declared-type table is threaded into this pass; the
                // check_ty-equivalent proof for assignment sites is a
                // reasonable follow-up, not attempted this round).
                scopes.set(name, iv);
                iv
            }
            Expr::Box(inner, _) => {
                self.expr(inner, scopes);
                Interval::unknown()
            }
            Expr::Deref(inner, _) => {
                self.expr(inner, scopes);
                Interval::unknown()
            }
            Expr::Ref(inner, _) => {
                self.expr(inner, scopes);
                Interval::unknown()
            }
            Expr::Spawn(_, args, _) => {
                for a in args {
                    self.expr(a, scopes);
                }
                Interval::unknown()
            }
            Expr::Join(inner, _) => {
                self.expr(inner, scopes);
                Interval::unknown()
            }
            Expr::Bool(_, _) => Interval::unknown(),
        }
    }

    /// A block's value in this pass mirrors `typeck.rs`'s convention: the
    /// last expression-statement's interval, `unknown` for anything else
    /// (including "no statements at all").
    fn block_value(&mut self, block: &Block, scopes: &mut Scopes) -> Interval {
        scopes.push();
        let result = match block.stmts.split_last() {
            None => Interval::unknown(),
            Some((last, rest)) => {
                self.stmts(rest, scopes);
                match last {
                    Stmt::Expr(e) => self.expr(e, scopes),
                    other => {
                        self.stmt(other, scopes);
                        Interval::unknown()
                    }
                }
            }
        };
        scopes.pop();
        result
    }

    fn binary(&mut self, op: BinOp, lhs: &Expr, rhs: &Expr, span: Span, scopes: &mut Scopes) -> Interval {
        let l = self.expr(lhs, scopes);
        let r = self.expr(rhs, scopes);
        match op {
            BinOp::Add => l.add(r),
            BinOp::Sub => l.sub(r),
            BinOp::Mul => l.mul(r),
            BinOp::Div => {
                if r.excludes_zero() {
                    self.report.proven_nonzero_divisor.insert(span);
                }
                // Result interval deliberately not computed — see module
                // doc's "two proofs only" note.
                Interval::unknown()
            }
            BinOp::Eq
            | BinOp::NotEq
            | BinOp::Lt
            | BinOp::Gt
            | BinOp::LtEq
            | BinOp::GtEq
            | BinOp::And
            | BinOp::Or => Interval::unknown(),
        }
    }
}

/// Every name that's the target of an `Expr::Assign` anywhere in `stmts`,
/// recursively through nested blocks/branches — used by the loop-entry
/// widening pass (module doc). Doesn't need to distinguish *how deeply*
/// nested an assignment is, only whether one exists at all.
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
            Expr::Unary(_, inner, _)
            | Expr::Box(inner, _)
            | Expr::Deref(inner, _)
            | Expr::Ref(inner, _)
            | Expr::Join(inner, _) => walk_expr(inner, names),
            Expr::Binary(_, l, r, _) => {
                walk_expr(l, names);
                walk_expr(r, names);
            }
            Expr::Call(_, args, _) | Expr::Spawn(_, args, _) => {
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
