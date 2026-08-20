//! Static move-checker — goal.md row 1's actual content ("no GC, no manual
//! `free()`"). Runs after `typeck.rs`, over the same AST.
//!
//! **Why this doesn't matter for *this* interpreter's safety, and why it's
//! built anyway.** `interpreter.rs` clones a `Value` on every variable
//! read (`Env::get`), so right now, aliasing a `box` can't actually corrupt
//! anything — two "owners" just end up with two independent Rust-owned
//! trees. A real (future, LLVM-compiled, arena/region-based) backend
//! wouldn't clone; it would hand out the same address twice, and a
//! use-after-move would be a real dangling pointer or double-free. This
//! pass proves, statically, that no well-typed Nirdosha program ever does
//! that — the proof a real backend needs already exists before there's a
//! real backend to need it, which is the honest way to read "row 1 is
//! partially done": the discipline is proved, not yet load-bearing.
//!
//! **The rule.** Every type is either *affine* (`Ty::is_affine` — currently
//! only `Ty::Box`) or freely copyable (everything else). Using an
//! affine-typed variable **by name** — as a `let` initializer, an
//! assignment RHS, a call argument, a `return` value — transfers ownership
//! out of that variable; any later use of the same variable (on the same
//! control-flow path) is a "use after move" error. Reading *through* a box
//! (`*b`) moves it only when what comes out is itself affine — `*b` for
//! `b: box i64` copies a scalar out and leaves `b` valid, but `*bb` for
//! `bb: box box i64` hands out the inner `box i64` by value, so *that*
//! does consume `bb` (see the `Expr::Deref` arm of `touch_expr` for the
//! type-directed check this needs — it was originally written as an
//! unconditional exemption and only caught during testing that nested
//! boxes made that unsound; the fix and the reasoning are both there).
//!
//! **Branches merge, conservatively.** After `if c { moves b } else {
//! doesn't }`, `b` has to be treated as moved either way — the checker
//! can't know at compile time which branch ran, so it assumes the worse
//! one, the same as Rust's own borrow checker does. See `merge_moved`.
//!
//! **Known limitation: no place-expression semantics, so `&box T` is
//! borrow-only, not read-through.** `*r` for `r: &box i64` denotes the
//! `box i64` itself — extracting it is correctly rejected as a move out
//! of a shared reference (`typeck.rs`'s `CannotMoveOutOfReference`),
//! since you don't own what's behind a `&`. But that means there is
//! currently *no way* to read the scalar inside a box reached only
//! through a reference — `**r` doesn't help, because the inner `*r` hits
//! the same rejection before the outer `*` ever runs. Real Rust avoids
//! this with place-expression semantics: `**r` reads straight through
//! both layers in one composed operation, never treating the
//! intermediate `Box` as a value that has to be moved out on its own.
//! Building that properly is real, additional work (tracking whether an
//! expression denotes a *place* vs. a *value*, the way a MIR-based
//! borrow checker does) that this increment doesn't attempt — `&box T`
//! is honestly borrow-and-pass-around-only for now, not read-through.
//! Reading a box's content still works fine through an *owned* box (the
//! existing `box`/`*` tests), just not through a `&` to one.
//!
//! **Loops get checked twice.** A `while` body might run more than once,
//! and a variable the body moves on iteration 1 is gone by iteration 2 —
//! checking the body only once, from the state *before* the loop, would
//! miss that entirely (this was caught while writing this module: the
//! first draft's single-pass version would have accepted `while cond { let
//! c = b; use(c); }` even though the second iteration re-reads an already-
//! moved `b`). The fix: check the body once, silently, to see what state
//! one iteration produces; merge that with the pre-loop state (the loop
//! might run 0 or more times); then check the body *again*, for real, from
//! that merged state. This is a sound approximation, not a full fixed
//! point — a body that only becomes movable after two-or-more prior
//! iterations could theoretically still slip through — documented here as
//! a known limitation rather than silently assumed away.

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum OwnershipErrorKind {
    /// The variable was already moved from earlier on this path.
    UseAfterMove { name: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct OwnershipError {
    pub kind: OwnershipErrorKind,
    pub span: Span,
}

impl std::fmt::Display for OwnershipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Span { line, col } = self.span;
        match &self.kind {
            OwnershipErrorKind::UseAfterMove { name } => {
                write!(f, "{line}:{col}: use of `{name}` after it was moved")
            }
        }
    }
}

/// `(declared type, currently moved-from)`, one entry per binding, scoped
/// the same way `typeck::Scopes` is. Kept as its own small structure
/// (rather than reusing typeck's) because this pass needs to *snapshot and
/// merge* whole scope stacks for branch/loop analysis — see module doc —
/// which is easiest to reason about with a type this pass owns outright.
#[derive(Clone)]
struct OwnScopes(Vec<HashMap<String, (Ty, bool)>>);

impl OwnScopes {
    fn new() -> Self {
        OwnScopes(vec![HashMap::new()])
    }
    fn push(&mut self) {
        self.0.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.0.pop();
    }
    fn define(&mut self, name: &str, ty: Ty) {
        self.0.last_mut().unwrap().insert(name.to_string(), (ty, false));
    }
    fn lookup(&self, name: &str) -> Option<(Ty, bool)> {
        self.0.iter().rev().find_map(|s| s.get(name)).cloned()
    }
    /// Sets the moved-flag on the innermost scope that declares `name`.
    /// `typeck.rs` already proved every name here resolves, so silently
    /// no-op'ing on a miss (rather than erroring) is fine — it can only
    /// happen for a name this pass doesn't otherwise track (there are
    /// none, currently), not a real program mistake.
    fn set_moved(&mut self, name: &str, moved: bool) {
        for scope in self.0.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                slot.1 = moved;
                return;
            }
        }
    }
}

/// After checking two branches independently from the same starting
/// snapshot, a name is moved in the merged result if it was moved down
/// *either* branch. Both `a` and `b` are guaranteed the same shape (same
/// scope depth, same keys per scope) because both started from an
/// identical clone and every block pushes and pops exactly the scope it
/// opened — see the module doc's branch-merge note.
fn merge_moved(a: Vec<HashMap<String, (Ty, bool)>>, b: Vec<HashMap<String, (Ty, bool)>>) -> Vec<HashMap<String, (Ty, bool)>> {
    a.into_iter()
        .zip(b)
        .map(|(a_scope, b_scope)| {
            a_scope
                .into_iter()
                .map(|(name, (ty, a_moved))| {
                    let b_moved = b_scope.get(&name).map(|(_, m)| *m).unwrap_or(a_moved);
                    (name, (ty, a_moved || b_moved))
                })
                .collect()
        })
        .collect()
}

pub struct Checker {
    scopes: OwnScopes,
    errors: Vec<OwnershipError>,
    /// Set during the throwaway first pass over a `while` body (see module
    /// doc) — errors found there are discarded, only the resulting *state*
    /// is kept, so the same mistake doesn't get reported twice.
    silent: bool,
}

pub fn check_ownership(program: &Program) -> Result<(), Vec<OwnershipError>> {
    let mut c = Checker { scopes: OwnScopes::new(), errors: Vec::new(), silent: false };
    for f in &program.fns {
        c.scopes = OwnScopes::new();
        for p in &f.params {
            c.scopes.define(&p.name, p.ty.clone());
        }
        c.check_stmts(&f.body.stmts);
    }
    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}

impl Checker {
    fn error(&mut self, kind: OwnershipErrorKind, span: Span) {
        if !self.silent {
            self.errors.push(OwnershipError { kind, span });
        }
    }

    fn check_stmts(&mut self, stmts: &[Stmt]) {
        for stmt in stmts {
            self.check_stmt(stmt);
        }
    }

    fn check_block(&mut self, block: &Block) {
        self.scopes.push();
        self.check_stmts(&block.stmts);
        self.scopes.pop();
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { name, ty, value, .. } => {
                self.touch_expr(value, true);
                self.scopes.define(name, ty.clone());
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    self.touch_expr(e, true);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.touch_expr(cond, true);
                self.check_while(body);
            }
            Stmt::Expr(e) => self.check_stmt_expr(e),
        }
    }

    /// Mirrors `typeck::check_stmt_expr`: an `if` used as a bare statement
    /// still needs both branches checked and their moved-state merged
    /// (moving `b` in only one branch still has to poison later uses of
    /// `b`), it just doesn't need the *value*-position machinery
    /// (`want`/branch-type-agreement) typeck.rs has, since there's no
    /// value here to reason about.
    fn check_stmt_expr(&mut self, e: &Expr) {
        if let Expr::If { cond, then_block, else_block, .. } = e {
            self.touch_expr(cond, true);
            self.check_if_branches(then_block, else_block.as_deref());
        } else {
            self.touch_expr(e, true);
        }
    }

    fn check_if_branches(&mut self, then_block: &Block, else_block: Option<&ElseBranch>) {
        let pre = self.scopes.clone();

        self.check_block(then_block);
        let after_then = std::mem::replace(&mut self.scopes, pre.clone()).0;

        match else_block {
            Some(ElseBranch::Block(b)) => self.check_block(b),
            Some(ElseBranch::If(e2)) => self.check_stmt_expr(e2),
            None => {}
        }
        let after_else = self.scopes.0.clone();

        self.scopes = OwnScopes(merge_moved(after_then, after_else));
    }

    /// See the module doc's "loops get checked twice" note for why this
    /// isn't just `self.check_block(body)`.
    fn check_while(&mut self, body: &Block) {
        let pre = self.scopes.clone();

        // Pass 1, silent: find out what moving-state one iteration
        // produces, without reporting anything from it — the "for real"
        // errors, if any, come from pass 2.
        self.silent = true;
        self.check_block(body);
        self.silent = false;
        let after_one_pass = self.scopes.0.clone();

        // The loop might run 0 or more times before whatever comes next —
        // enter pass 2 from the union of "never ran" and "ran once".
        self.scopes = OwnScopes(merge_moved(pre.0.clone(), after_one_pass));

        // Pass 2, for real: re-check the body from that merged state, so a
        // variable that's only already-moved on a *second* iteration is
        // actually caught here, and any errors get reported this time.
        self.check_block(body);
        let after_real_pass = self.scopes.0.clone();
        self.scopes = OwnScopes(merge_moved(pre.0, after_real_pass));
    }

    /// Walk `e` looking for moving uses of affine-typed identifiers.
    /// `consume = true` at the top level of most value positions (`let`
    /// initializer, assignment RHS, call argument, return value); `false`
    /// only immediately inside `Expr::Deref`, where reading *through* a
    /// box is exempt from move-checking (see module doc).
    fn touch_expr(&mut self, e: &Expr, consume: bool) {
        match e {
            Expr::Int(_, _) | Expr::Bool(_, _) => {}
            Expr::Ident(name, span) => self.touch_ident(name, *span, consume),
            Expr::Unary(_, inner, _) => self.touch_expr(inner, true),
            Expr::Binary(_, lhs, rhs, _) => {
                self.touch_expr(lhs, true);
                self.touch_expr(rhs, true);
            }
            Expr::Call(_, args, _) => {
                for a in args {
                    self.touch_expr(a, true);
                }
            }
            Expr::If { cond, then_block, else_block, .. } => {
                self.touch_expr(cond, true);
                // A value-position `if` (e.g. `let x = if c {..} else {..}`)
                // still needs the same branch-merge treatment as a
                // statement-position one — moves inside it are real moves
                // either way.
                self.check_if_branches(then_block, else_block.as_deref());
            }
            Expr::Assign(name, rhs, span) => {
                self.touch_expr(rhs, true);
                // Reassignment gives `name` a fresh value — it can't still
                // be "moved from" after this, regardless of what it was
                // before. `typeck.rs` already proved `name` exists.
                let _ = span;
                self.scopes.set_moved(name, false);
            }
            Expr::Box(inner, _) => self.touch_expr(inner, true),
            Expr::Ref(inner, _) => {
                // Borrowing is, definitionally, not moving — that's the
                // entire point of `&`. No liveness/exclusivity tracking
                // is needed here yet because there's no `&mut`: unlimited
                // simultaneous shared borrows are always sound, so the
                // only thing to check is that the referent isn't already
                // moved, which `touch_expr(inner, false)` already does via
                // `touch_ident`'s existing moved-check.
                self.touch_expr(inner, false);
            }
            Expr::Deref(inner, _) => {
                // Reading through a box is exempt from moving it — *only*
                // when what comes out is freely copyable. `*b` for `b: box
                // i64` hands out a plain `i64`: exempt, `b` stays valid.
                // `*bb` for `bb: box box i64` hands out the *inner* `box
                // i64` by value — itself affine — so extracting it has to
                // count as consuming `bb`: you can't soundly claim `bb`
                // still owns something it just gave away. Only an `Ident`
                // needs this distinction at all; anything else being
                // dereferenced is a temporary with nothing left to reuse.
                if let Expr::Ident(name, span) = inner.as_ref() {
                    let extracting_affine_content = self
                        .scopes
                        .lookup(name)
                        .map(|(ty, _)| matches!(ty, Ty::Box(inner_ty) if inner_ty.is_affine()))
                        .unwrap_or(false);
                    self.touch_ident(name, *span, extracting_affine_content);
                } else {
                    self.touch_expr(inner, false);
                }
            }
            Expr::Spawn(_, args, _) => {
                // The actual content of goal.md rows 2-3's race-freedom
                // claim: every argument moved into a spawned computation
                // is checked exactly like a normal call argument. An
                // affine (`box`-typed) argument is consumed here, the
                // same as any function call — so the spawning side can
                // never touch it again, and no two concurrent
                // computations can ever alias the same allocation. No new
                // logic beyond "touch like a call" is needed for this to
                // be true; it falls out of the existing move-checker.
                for a in args {
                    self.touch_expr(a, true);
                }
            }
            Expr::Join(inner, _) => {
                // `join` consumes the whole handle -- a spawned
                // computation can only be joined once, the same
                // single-owner discipline as `box`.
                self.touch_expr(inner, true);
            }
        }
    }

    fn touch_ident(&mut self, name: &str, span: Span, consume: bool) {
        let Some((ty, moved)) = self.scopes.lookup(name) else {
            return; // typeck.rs already reports unknown variables
        };
        if !ty.is_affine() {
            return; // freely copyable — nothing to track
        }
        if moved {
            self.error(OwnershipErrorKind::UseAfterMove { name: name.to_string() }, span);
            return;
        }
        if consume {
            self.scopes.set_moved(name, true);
        }
    }
}
