//! Static move-checker — docs/goal.md row 1's actual content ("no GC, no manual
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

use serde::{Deserialize, Serialize};

use crate::ast::*;
use crate::token::Span;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum OwnershipErrorKind {
    /// The variable was already moved from earlier on this path.
    UseAfterMove { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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

/// Where a real (non-cloning, LLVM-compiled) backend needs to insert
/// runtime teardown for an affine-typed binding — computed as a side effect
/// of the exact same move-tracking traversal `Checker` already runs for
/// error-checking (see `compute_free_map`), not a second, independently
/// re-derived liveness analysis. Every entry answers "at this exact
/// program point, which affine-typed bindings are still owned (never moved
/// away) and therefore need freeing here" — codegen looks each one up by
/// the matching AST node's own span (or, for a whole function, its name)
/// and emits the right runtime call per name, using its own `Scopes` to
/// find that name's current pointer register.
#[derive(Debug, Clone, Default)]
pub struct FreeMap {
    /// Keyed by a `Stmt::Return`'s own span. A `return` unwinds every
    /// currently-open scope at once, so (unlike the other three maps)
    /// this holds bindings from *every* enclosing scope, not just the
    /// innermost.
    pub at_return: HashMap<Span, Vec<String>>,
    /// Keyed by a `Stmt::While`'s own span — bindings declared directly
    /// in that loop's own body scope, still owned at the end of one
    /// representative (non-exploratory) pass over the body. Codegen emits
    /// these once per iteration (the loop body's IR runs every time), not
    /// once total — see `codegen.rs`'s `entry_allocas` doc for why the
    /// binding's own stack slot is reused, not re-allocated, per
    /// iteration; only the *heap* allocation a `box` inside the loop
    /// produces is fresh each time, and only this map's entries know to
    /// free the *previous* iteration's one before it's overwritten.
    pub at_while_end: HashMap<Span, Vec<String>>,
    /// Keyed by an `Expr::If`'s own span plus which branch (`true` =
    /// `then`, `false` = `else`) — bindings declared directly in that one
    /// branch's own block. Two independent entries per `if`, because each
    /// branch is its own scope with its own independent moved-state; only
    /// whichever branch actually runs at runtime frees what it itself
    /// still owns.
    pub at_if_branch_end: HashMap<(Span, bool), Vec<String>>,
    /// Keyed by a `Stmt::Audited`'s own span — same idea as
    /// `at_while_end`/`at_if_branch_end`, for an `audited { ... }` block's
    /// own scope.
    pub at_audited_end: HashMap<Span, Vec<String>>,
    /// Keyed by a `match` expression's own span plus the arm index —
    /// bindings declared directly in that arm's payload-pattern scope and
    /// still owned at the arm's end. Each arm is its own scope with its
    /// own moved-state; only the arm that actually runs at runtime frees
    /// what it itself still owns there.
    pub at_match_arm_end: HashMap<(Span, usize), Vec<String>>,
    /// Keyed by function name (unique per `Program.fns`, per `typeck.rs`)
    /// — bindings (including unused parameters) still owned when a
    /// function's body finishes without having hit an explicit `return`
    /// on that path. Codegen only ever consults this at the exact point
    /// it was already about to emit an implicit `ret void`/`unreachable`
    /// (`function()`'s `if !self.terminated` fallback) — a function every
    /// path through which *does* end in an explicit `return` never
    /// reaches that point, so there's no risk of this double-freeing
    /// anything `at_return` already covered.
    pub at_fn_end: HashMap<String, Vec<String>>,
}

/// The affine, not-yet-moved bindings in exactly one scope frame — the
/// "free these when *this* scope closes" answer, used for every
/// `FreeMap` field except `at_return` (which needs every open frame at
/// once, since a `return` unwinds them all simultaneously — see
/// `all_still_owned_affine`).
fn still_owned_affine(scope: &HashMap<String, (Ty, bool)>, registry: &TypeRegistry<'_>) -> Vec<String> {
    scope
        .iter()
        .filter(|(_, (ty, moved))| registry.is_affine(ty) && !moved)
        .map(|(name, _)| name.clone())
        .collect()
}

fn all_still_owned_affine(scopes: &OwnScopes, registry: &TypeRegistry<'_>) -> Vec<String> {
    scopes.0.iter().flat_map(|s| still_owned_affine(s, registry)).collect()
}

pub struct Checker<'a> {
    scopes: OwnScopes,
    errors: Vec<OwnershipError>,
    /// Set during the throwaway first pass over a `while` body (see module
    /// doc) — errors found there are discarded, only the resulting *state*
    /// is kept, so the same mistake doesn't get reported twice. Also
    /// gates `FreeMap` recording, for the identical reason: a throwaway
    /// exploratory pass has no real free-site to record either.
    silent: bool,
    free_map: FreeMap,
    /// Row 11's declaration table — the only thing this pass needs it
    /// for is registry-aware affinity (`TypeRegistry::is_affine`, see its
    /// doc comment for why `Ty::is_affine()` alone can't answer this for
    /// a `Ty::Named`) and looking up a variant's payload types to bind a
    /// `match` arm's names with their real type (`check_match_arms`,
    /// below).
    registry: TypeRegistry<'a>,
    /// Every user function's declared return type, by name — lets
    /// `check_match_arms` resolve a `match`ed function call's own
    /// concrete type arguments precisely (`match some_call() { .. }`) the
    /// same way it already does for a plain bound identifier, instead of
    /// falling back to the conservative "assume affine" sentinel for the
    /// single most common non-`Ident` scrutinee shape. A bare name→`Ty`
    /// map, not real type inference — this pass still does none (module
    /// doc); it's the same information `typeck.rs`'s own `sigs` table
    /// already holds, just re-derived here rather than threaded through
    /// (the two passes share no side-channel today).
    fn_rets: HashMap<String, Ty>,
}

/// The fixed return-type *shape* of a generic-returning builtin, by name
/// — every JSON builtin (`Ty::Json`'s doc comment) always resolves to
/// the same `Result(_, str)` instantiation regardless of its arguments'
/// *values* (unlike `zeros`/`ones`/`identity`, whose shape depends on a
/// literal argument — those aren't generic-returning in the Row 11
/// sense at all, so they don't need an entry here). Exists so
/// `check_match_arms` can resolve `match json_parse(s) { .. }` precisely
/// without this pass growing a general builtin type-inference table —
/// keep this in sync with `typeck.rs::infer_builtin_call`'s own
/// `"json_*"` arms if either changes; there is currently no single
/// source of truth for "this builtin's declared return type" the way
/// `ast::BUILTIN_NAMES` is one for "is this name a builtin" at all.
fn builtin_return_ty(name: &str) -> Option<Ty> {
    match name {
        "json_parse" | "json_get" | "json_array_get" | "json_set_str" => Some(result_of(Ty::Json)),
        "json_get_str" => Some(result_of(Ty::Str)),
        "json_get_i64" | "json_array_len" => Some(result_of(Ty::I64)),
        "json_get_f64" => Some(result_of(Ty::F64)),
        "json_get_bool" => Some(result_of(Ty::Bool)),
        "http_get" | "http_post" | "https_get" | "https_post" => {
            Some(result_of(Ty::Named("HttpResponse".to_string(), vec![])))
        }
        // Row 12: identity builtins all return Result(_, str) over a prelude
        // struct so `match` exhaustiveness can be resolved without a general
        // builtin type-inference table.
        "oidc_validate_token" => Some(result_of(Ty::Named("VerifiedIdentity".to_string(), vec![]))),
        "check_role" | "check_role_path" => Some(result_of(Ty::Named("RoleView".to_string(), vec![]))),
        "extract_claim" | "extract_claim_path" => Some(result_of(Ty::Named("ClaimView".to_string(), vec![]))),
        "validate_api_key" => Some(result_of(Ty::Named("VerifiedIdentity".to_string(), vec![]))),
        "exchange_refresh_token" => Some(result_of(Ty::Named("VerifiedIdentity".to_string(), vec![]))),
        "db_connect" => Some(result_of(Ty::Db)),
        "db_query" => Some(result_of(Ty::Json)),
        "db_execute" => Some(result_of(Ty::I64)),
        "mq_connect" => Some(result_of(Ty::Mq)),
        "mq_connect_via" => Some(result_of(Ty::Mq)),
        "mq_publish" => Some(result_of(Ty::Unit)),
        "mq_consume" => Some(result_of(Ty::Str)),
        "mock_issue_token" => Some(result_of(Ty::Str)),
        _ => None,
    }
}

/// The shared traversal both `check_ownership` and `compute_free_map`
/// need — computing `FreeMap` piggybacks on the exact same move-tracking
/// walk that already exists for error-checking (one `Checker`, one pass
/// per function), not a second, independently re-derived analysis.
fn run_checker(program: &Program) -> Checker<'_> {
    let mut c = Checker {
        scopes: OwnScopes::new(),
        errors: Vec::new(),
        silent: false,
        free_map: FreeMap::default(),
        registry: TypeRegistry::build(program),
        fn_rets: program.fns.iter().map(|f| (f.name.clone(), f.ret.clone())).collect(),
    };
    for f in &program.fns {
        c.scopes = OwnScopes::new();
        for p in &f.params {
            c.scopes.define(&p.name, p.ty.clone());
        }
        c.check_stmts(&f.body.stmts);
        // Whatever's left in the function's own top-level scope once its
        // body finishes (params plus any top-level `let`s — every nested
        // block scope has already been popped by this point) is exactly
        // what an implicit fall-off-the-end return needs freed.
        let freed = still_owned_affine(c.scopes.0.last().unwrap(), &c.registry);
        c.free_map.at_fn_end.insert(f.name.clone(), freed);
    }
    c
}

pub fn check_ownership(program: &Program) -> Result<(), Vec<OwnershipError>> {
    let c = run_checker(program);
    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}

/// Assumes `program` already passed `check_ownership` — like
/// `codegen.rs`'s own `check_supported`, this trusts a prior pass rather
/// than re-validating (the ownership-error path and the free-map path
/// share the same traversal, but only one of them is a fallible gate).
pub fn compute_free_map(program: &Program) -> FreeMap {
    run_checker(program).free_map
}

impl<'a> Checker<'a> {
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

    /// Runs `block` in its own scope, exactly as before, and returns the
    /// box-typed bindings declared directly in that block's own top scope
    /// that are still owned once the block finishes — the "insert
    /// `nir_free` here for these" answer `FreeMap`'s callers want at this
    /// exact scope-closing point. Callers under a silent (throwaway)
    /// exploration ignore this — see `Checker::silent`'s doc.
    fn check_block(&mut self, block: &Block) -> Vec<String> {
        self.scopes.push();
        self.check_stmts(&block.stmts);
        let freed = still_owned_affine(self.scopes.0.last().unwrap(), &self.registry);
        self.scopes.pop();
        freed
    }

    fn check_stmt(&mut self, stmt: &Stmt) {
        match stmt {
            Stmt::Let { name, ty, value, .. } => {
                self.touch_expr(value, true);
                self.scopes.define(name, ty.clone());
            }
            Stmt::Return { value, span } => {
                if let Some(e) = value {
                    self.touch_expr(e, true);
                }
                // A `return` unwinds every currently-open scope at once —
                // unlike `check_block`'s single-frame snapshot, this needs
                // everything still owned across the whole scope stack.
                if !self.silent {
                    self.free_map.at_return.insert(*span, all_still_owned_affine(&self.scopes, &self.registry));
                }
            }
            Stmt::While { cond, body, span } => {
                self.touch_expr(cond, true);
                self.check_while(*span, body);
            }
            Stmt::Expr(e) => self.check_stmt_expr(e),
            Stmt::Audited { body, span, .. } => {
                self.scopes.push();
                self.check_stmts(body);
                let freed = still_owned_affine(self.scopes.0.last().unwrap(), &self.registry);
                self.scopes.pop();
                if !self.silent {
                    self.free_map.at_audited_end.insert(*span, freed);
                }
            }
        }
    }

    /// Mirrors `typeck::check_stmt_expr`: an `if` used as a bare statement
    /// still needs both branches checked and their moved-state merged
    /// (moving `b` in only one branch still has to poison later uses of
    /// `b`), it just doesn't need the *value*-position machinery
    /// (`want`/branch-type-agreement) typeck.rs has, since there's no
    /// value here to reason about.
    fn check_stmt_expr(&mut self, e: &Expr) {
        if let Expr::If { cond, then_block, else_block, span } = e {
            self.touch_expr(cond, true);
            self.check_if_branches(*span, then_block, else_block.as_deref());
        } else {
            self.touch_expr(e, true);
        }
    }

    fn check_if_branches(&mut self, span: Span, then_block: &Block, else_block: Option<&ElseBranch>) {
        let pre = self.scopes.clone();

        let then_freed = self.check_block(then_block);
        let after_then = std::mem::replace(&mut self.scopes, pre.clone()).0;
        if !self.silent {
            self.free_map.at_if_branch_end.insert((span, true), then_freed);
        }

        match else_block {
            Some(ElseBranch::Block(b)) => {
                let else_freed = self.check_block(b);
                if !self.silent {
                    self.free_map.at_if_branch_end.insert((span, false), else_freed);
                }
            }
            // An `else if` delegates to its own `Expr::If` — that nested
            // call records its own branches under its own span; there's
            // no separate "else" scope of this `if`'s own to record.
            Some(ElseBranch::If(e2)) => self.check_stmt_expr(e2),
            None => {}
        }
        let after_else = self.scopes.0.clone();

        self.scopes = OwnScopes(merge_moved(after_then, after_else));
    }

    /// `match`'s ownership treatment: every arm runs from the *same*
    /// pre-match state (only one arm actually executes at runtime), each
    /// arm's payload bindings are fresh (scoped to just that arm — an
    /// affine payload, like `Some(box_val)`'s `box_val`, is a genuinely
    /// new owned binding each time, not aliased across arms), and the
    /// merged post-state has to account for a move down *any* arm — the
    /// same "branches merge, conservatively" rule `check_if_branches`
    /// already establishes, generalized from two branches to N.
    ///
    /// **Generic payloads (layer 6):** a variant's declared payload type
    /// may be a bare reference to the enum's own type parameter (`Some(T)`)
    /// — substituted against the *scrutinee's* concrete type arguments
    /// before a binding's affinity can be judged correctly. This pass does
    /// no general type inference (it trusts `typeck.rs` to have already
    /// proved every type resolves — module doc), so it can only recover
    /// those concrete arguments for the shapes it can resolve without one:
    /// a plain bound identifier (`match o { .. }`), a call to a known user
    /// function (`match get_option() { .. }` — `fn_rets`), or a call to a
    /// generic-returning builtin with a fixed return shape (`match
    /// json_parse(s) { .. }` — `builtin_return_ty`). Anything else (an
    /// arbitrary nested expression) falls back to a conservative
    /// substitution that treats every one of the enum's type parameters as
    /// affine regardless — a real, always-affine `Ty` (`box unit`) used
    /// purely as a sentinel, the same "assume the worse one" direction
    /// `check_if_branches`'s own branch-merge conservatism already takes.
    /// This can only ever over-restrict (reject some valid reuse this pass
    /// can't prove safe), never under-restrict (accept an actual
    /// double-move).
    fn check_match_arms(&mut self, scrutinee: &Expr, arms: &[MatchArm], span: Span) {
        let pre = self.scopes.clone();

        let concrete_args: Option<Vec<Ty>> = match scrutinee {
            Expr::Ident(name, _) => match pre.lookup(name) {
                Some((Ty::Named(_, args), _)) => Some(args),
                _ => None,
            },
            Expr::Call(name, _, _) => match self.fn_rets.get(name).cloned().or_else(|| builtin_return_ty(name)) {
                Some(Ty::Named(_, args)) => Some(args),
                _ => None,
            },
            _ => None,
        };
        let sentinel = Ty::Box(Box::new(Ty::Unit));

        let mut merged: Option<Vec<HashMap<String, (Ty, bool)>>> = None;

        for (arm_idx, arm) in arms.iter().enumerate() {
            self.scopes = pre.clone();
            self.scopes.push();
            if let Some((owner, v)) = self.registry.find_variant(&arm.variant) {
                let type_params = self.registry.enum_type_params(&owner).unwrap_or(&[]);
                let subst: HashMap<&str, &Ty> = match &concrete_args {
                    Some(args) if args.len() == type_params.len() => zip_type_params(type_params, args),
                    _ => type_params.iter().map(|p| (p.as_str(), &sentinel)).collect(),
                };
                let payload: Vec<Ty> = v.payload.iter().map(|t| substitute_ty(t, &subst)).collect();
                for (name, ty) in arm.bindings.iter().zip(payload.iter()) {
                    self.scopes.define(name, ty.clone());
                }
            }
            self.touch_expr(&arm.body, true);
            if !self.silent {
                let freed = still_owned_affine(self.scopes.0.last().unwrap(), &self.registry);
                self.free_map.at_match_arm_end.insert((span, arm_idx), freed);
            }
            self.scopes.pop();

            let after = self.scopes.0.clone();
            merged = Some(match merged {
                None => after,
                Some(prev) => merge_moved(prev, after),
            });
        }

        self.scopes = OwnScopes(merged.unwrap_or(pre.0));
    }

    /// See the module doc's "loops get checked twice" note for why this
    /// isn't just `self.check_block(body)`.
    fn check_while(&mut self, span: Span, body: &Block) {
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
        // This is also the pass whose result is real enough to record —
        // codegen frees these once per loop iteration (the body's IR runs
        // every time), reusing the same stack slot across iterations.
        let freed = self.check_block(body);
        self.free_map.at_while_end.insert(span, freed);
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
            Expr::Int(_, _) | Expr::Float(_, _) | Expr::Bool(_, _) | Expr::Str(_, _) => {}
            Expr::ArrayLit(elements, _) => {
                for e in elements {
                    self.touch_expr(e, true);
                }
            }
            Expr::Ident(name, span) => self.touch_ident(name, *span, consume),
            Expr::Unary(_, inner, _) => self.touch_expr(inner, true),
            Expr::Binary(_, lhs, rhs, _) => {
                self.touch_expr(lhs, true);
                self.touch_expr(rhs, true);
            }
            Expr::Call(name, args, _) => {
                for (i, a) in args.iter().enumerate() {
                    // `db_query`/`db_execute`'s connection argument is
                    // read, not consumed -- the same "read, don't move"
                    // treatment `Expr::Accept`'s listener operand and
                    // `Expr::Send`'s channel operand already get for
                    // their own dedicated `Expr` nodes. `db`-typed
                    // builtins are ordinary `Expr::Call`s instead
                    // (`Ty::Json`'s doc comment: "a new builtin, not a
                    // new grammar form" is Row 11's newer pattern), so
                    // this is the one place that treatment needs a
                    // per-builtin exception by name to stay usable more
                    // than once — a real connection is meant to run many
                    // queries, the same way a `tcp`/`file` handle is
                    // meant to `send`/`recv` many times before its one
                    // `stop` (`Ty::Db`'s doc comment).
                    let consume =
                        !(i == 0 && matches!(name.as_str(), "db_query" | "db_execute" | "mq_publish" | "mq_consume"));
                    self.touch_expr(a, consume);
                }
            }
            Expr::Transact { precheck, network, verify, commit, compensate, log, .. } => {
                // `docs/TRANSACT.md`'s own Layer 1 scope decision: "ownership
                // — slots are ordinary calls — nothing new for
                // ownership.rs to reason about, same as docs/SANDBOXING.md's
                // observation that spawn/chan cost zero new checker
                // machinery." Each slot's *arguments* are touched exactly
                // like an ordinary call's. Honest, narrow gap this
                // leaves: the implicit `network`/`verify` bindings later
                // slots can reference by name (`typeck.rs`/
                // `interpreter.rs` both define them) are deliberately
                // *not* registered in `self.scopes` here, so a
                // `network`/`verify` value of an affine type (e.g. a
                // slot returning `box i64`) referenced by name in two
                // later slots would not be caught as a double-move by
                // this pass — not a memory-safety hole (the
                // interpreter's `Env::get` always hands out a real,
                // independent `Value` clone, never a raw alias), just an
                // enforcement gap left for a later layer if it matters
                // in practice.
                if let Some(p) = precheck {
                    for a in &p.args {
                        self.touch_expr(a, true);
                    }
                }
                for a in &network.args {
                    self.touch_expr(a, true);
                }
                for a in &verify.args {
                    self.touch_expr(a, true);
                }
                for a in &commit.args {
                    self.touch_expr(a, true);
                }
                if let Some(c) = compensate {
                    for a in &c.args {
                        self.touch_expr(a, true);
                    }
                }
                if let Some(l) = log {
                    for a in &l.args {
                        self.touch_expr(a, true);
                    }
                }
            }
            Expr::If { cond, then_block, else_block, span } => {
                self.touch_expr(cond, true);
                // A value-position `if` (e.g. `let x = if c {..} else {..}`)
                // still needs the same branch-merge treatment as a
                // statement-position one — moves inside it are real moves
                // either way.
                self.check_if_branches(*span, then_block, else_block.as_deref());
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
                        .map(|(ty, _)| matches!(ty, Ty::Box(inner_ty) if self.registry.is_affine(&inner_ty)))
                        .unwrap_or(false);
                    self.touch_ident(name, *span, extracting_affine_content);
                } else {
                    self.touch_expr(inner, false);
                }
            }
            Expr::Spawn(_, args, _) => {
                // The actual content of docs/goal.md rows 2-3's race-freedom
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
            Expr::Acquire(_, proof, _) => {
                // `name` (the gated function) isn't a local binding to
                // touch -- it's always a global fn (`typeck.rs::
                // infer_acquire` already enforces that). `proof` is read,
                // not consumed: `RoleView`/`ClaimView` aren't affine (same
                // "read, don't move" treatment `check_role`/`extract_claim`'s
                // own identity argument already gets as an ordinary call).
                self.touch_expr(proof, false);
            }
            Expr::Join(inner, _) => {
                // `join` consumes the whole handle -- a spawned
                // computation can only be joined once, the same
                // single-owner discipline as `box`.
                self.touch_expr(inner, true);
            }
            Expr::Chan(_) => {
                // A fresh value with no sub-expression -- nothing to touch.
            }
            Expr::Send(chan, value, _) => {
                // The channel handle itself is freely reusable (not
                // affine -- see `Ty::Channel`'s doc comment), same
                // treatment as `&`'s referent: `touch_expr(chan, false)`.
                // The *payload* is the actual ownership transfer -- an
                // affine value handed to `send` is consumed exactly like
                // a call argument, which is what makes it sound for it to
                // cross to another concurrent computation.
                self.touch_expr(chan, false);
                self.touch_expr(value, true);
            }
            Expr::Recv(chan, _) => {
                self.touch_expr(chan, false);
            }
            Expr::SpawnSandbox(_, args, _) => {
                // Same reuse as `Expr::Spawn` -- typeck.rs already
                // restricts these args to non-affine scalars, so this is
                // a no-op in practice today, but the treatment matches
                // every other call-shaped form on principle, not because
                // it's currently load-bearing.
                for a in args {
                    self.touch_expr(a, true);
                }
            }
            Expr::StopSandbox(inner, _) => {
                // `stop` consumes the whole handle -- a sandboxed process
                // (or, reusing the same keyword, a TCP connection) can
                // only be stopped once, the same single-owner discipline
                // as `join`.
                self.touch_expr(inner, true);
            }
            Expr::Connect(host, port, _) => {
                // Neither operand is affine (`str`/`i64`), so this is a
                // no-op in practice, same as `Expr::SpawnSandbox`'s args
                // above -- touched anyway on principle, not because it's
                // currently load-bearing.
                self.touch_expr(host, true);
                self.touch_expr(port, true);
            }
            Expr::Listen(port, _) => {
                self.touch_expr(port, true);
            }
            Expr::Accept(listener, _) => {
                // The listener handle itself isn't consumed -- `accept`
                // can be called on it repeatedly (see `Expr::Accept`'s
                // doc comment), the same "read, don't move" treatment
                // `send`'s channel operand already gets.
                self.touch_expr(listener, false);
            }
            Expr::Open(path, mode, _) => {
                // Neither operand is affine (`str`), so this is a no-op
                // in practice, same as `Expr::Connect`'s operands above --
                // touched anyway on principle.
                self.touch_expr(path, true);
                self.touch_expr(mode, true);
            }
            Expr::Index(base, indices, _) => {
                // Neither the base nor an index is affine yet -- no
                // indexable type exists (see `Expr::Index`'s doc comment
                // in ast.rs) -- but every sub-expression still gets
                // touched on principle, the same "no currently load-
                // bearing reason, but consistent with every other form"
                // treatment `Expr::Connect` already gets.
                self.touch_expr(base, true);
                for idx in indices {
                    self.touch_expr(idx, true);
                }
            }
            Expr::FieldAccess(base, field, _) => {
                // Extracting a field moves the whole base struct only
                // when the extracted field's own type is affine -- same
                // "look one level through" rule `Expr::Deref` already
                // applies to `box` (`docs/nirdosha_row11_amendment.md` §3.5),
                // generalized from "the one field a box has" to "one of
                // several named fields."
                if let Expr::Ident(name, span) = base.as_ref() {
                    // Substituted against the base's own concrete type
                    // arguments (layer 6, generics) before checking
                    // affinity — a `Wrapper(box i64)`'s field `v: T` is
                    // affine for *this* binding even though `T` alone,
                    // unsubstituted, isn't a real type to ask about at all.
                    let extracting_affine_field = self
                        .scopes
                        .lookup(name)
                        .and_then(|(ty, _)| match ty {
                            Ty::Named(struct_name, args) => self.registry.struct_fields(&struct_name).and_then(|fields| {
                                let type_params = self.registry.struct_type_params(&struct_name).unwrap_or(&[]);
                                let subst = zip_type_params(type_params, &args);
                                fields
                                    .iter()
                                    .find(|f| &f.name == field)
                                    .map(|f| self.registry.is_affine(&substitute_ty(&f.ty, &subst)))
                            }),
                            _ => None,
                        })
                        .unwrap_or(false);
                    self.touch_ident(name, *span, extracting_affine_field);
                } else {
                    self.touch_expr(base, false);
                }
            }
            Expr::Match { scrutinee, arms, span } => {
                // The scrutinee is consumed as a whole -- matching
                // destructures it into fresh payload bindings, the same
                // "moves as a whole" rule struct/enum affinity already
                // gets (§3.5).
                self.touch_expr(scrutinee, true);
                self.check_match_arms(scrutinee, arms, *span);
            }
        }
    }

    fn touch_ident(&mut self, name: &str, span: Span, consume: bool) {
        let Some((ty, moved)) = self.scopes.lookup(name) else {
            return; // typeck.rs already reports unknown variables
        };
        if !self.registry.is_affine(&ty) {
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
