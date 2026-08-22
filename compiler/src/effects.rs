//! Effect inference — `goal.md`'s "Effects" synthesis layer (rows 4, 9),
//! `PROTOLANG_PORT.md`'s "Locked design 1". Computes every function's real
//! effect set (`ast::Effect`) by walking its body — the same "each pass
//! redoes its own minimal shadow walk over the AST" idiom `refine.rs`/
//! `smt.rs` already establish, not because this distrusts `typeck.rs`'s
//! result (a program reaching here already type-checked cleanly), but
//! because every `let`/param binding's type is already syntactically
//! explicit in this language (no local type inference to redo — see
//! `LANGUAGE.md`'s grammar), so there's nothing to *infer* about a local
//! binding's type, only to look up.
//!
//! Computed unconditionally for every function, whether or not it declared
//! an `effect(...)` annotation — `typeck::typecheck` is the one that
//! checks a declared annotation against this pass's result
//! (`TypeErrorKind::EffectNotDeclared`); this module only computes, never
//! rejects.
//!
//! ## Classification
//!
//! | Effect | Comes from |
//! |---|---|
//! | `pure` (the empty set) | everything not listed below |
//! | `rng` | `rand_seed`/`rand_f64`/`rand_gaussian` |
//! | `io` | `print`; `file`'s `open`/`send`/`recv`/`stop` |
//! | `concurrent` | `spawn`/`join`; `chan`'s `send`/`recv`; `sandbox`/`stop` |
//! | `network` | `connect`/`listen`/`accept`; `tcp`'s `send`/`recv`/`stop` |
//!
//! `spawn`/`sandbox` also inherit whatever the spawned function's own
//! effect set is (transitively) — the *caller* triggered that work, even
//! though it now runs on another thread/process.
//!
//! ## Recursion
//!
//! Computed by least-fixpoint iteration over the whole call graph, not a
//! single top-down walk — correct for direct and mutual recursion alike
//! (`LANGUAGE.md` §6: "direct and mutual recursion both work"). The
//! lattice here is finite (at most `ast::Effect`'s four tags) and every
//! iteration step only ever adds to a function's set, never removes it —
//! monotone, so the loop is guaranteed to reach a fixed point, in the
//! worst case one pass per tag.

use std::collections::{BTreeSet, HashMap};

use crate::ast::{is_builtin, Effect, ElseBranch, Expr, Program, Stmt, Ty, TypeRegistry};

pub type EffectSet = BTreeSet<Effect>;

/// One function's inferred effect set, plus whatever it declared (if
/// anything) — `typeck::typecheck` turns a mismatch here into a real
/// `TypeErrorKind::EffectNotDeclared`; this module just reports both
/// halves.
#[derive(Debug, Clone)]
pub struct FnEffects {
    pub inferred: EffectSet,
    pub declared: Option<EffectSet>,
}

pub fn infer_effects(program: &Program, registry: &TypeRegistry) -> HashMap<String, FnEffects> {
    let mut known: HashMap<String, EffectSet> =
        program.fns.iter().map(|f| (f.name.clone(), EffectSet::new())).collect();

    loop {
        let mut changed = false;
        for f in &program.fns {
            let mut scopes = Scopes::new();
            scopes.push();
            for p in &f.params {
                scopes.define(&p.name, p.ty.clone());
            }
            let mut acc = known[&f.name].clone();
            walk_stmts(&f.body.stmts, &mut scopes, &known, registry, &mut acc);
            if acc != known[&f.name] {
                known.insert(f.name.clone(), acc);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }

    program
        .fns
        .iter()
        .map(|f| (f.name.clone(), FnEffects { inferred: known[&f.name].clone(), declared: f.declared_effects.clone() }))
        .collect()
}

/// A flat name → declared-type stack, pushed/popped per block — same
/// shape as `refine.rs`'s own `Scopes`, minus the interval-tracking this
/// pass has no use for. Only ever needs `Ty`, never a value: `local_ty`
/// below is the only reader.
struct Scopes(Vec<HashMap<String, Ty>>);

impl Scopes {
    fn new() -> Self {
        Scopes(Vec::new())
    }
    fn push(&mut self) {
        self.0.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.0.pop();
    }
    fn define(&mut self, name: &str, ty: Ty) {
        self.0.last_mut().expect("push() called before define()").insert(name.to_string(), ty);
    }
    fn get(&self, name: &str) -> Option<Ty> {
        self.0.iter().rev().find_map(|s| s.get(name)).cloned()
    }
}

fn walk_stmts(stmts: &[Stmt], scopes: &mut Scopes, known: &HashMap<String, EffectSet>, registry: &TypeRegistry, acc: &mut EffectSet) {
    for s in stmts {
        walk_stmt(s, scopes, known, registry, acc);
    }
}

fn walk_block(stmts: &[Stmt], scopes: &mut Scopes, known: &HashMap<String, EffectSet>, registry: &TypeRegistry, acc: &mut EffectSet) {
    scopes.push();
    walk_stmts(stmts, scopes, known, registry, acc);
    scopes.pop();
}

fn walk_stmt(stmt: &Stmt, scopes: &mut Scopes, known: &HashMap<String, EffectSet>, registry: &TypeRegistry, acc: &mut EffectSet) {
    match stmt {
        Stmt::Let { name, ty, value, .. } => {
            walk_expr(value, scopes, known, registry, acc);
            scopes.define(name, ty.clone());
        }
        Stmt::Return { value: Some(e), .. } => walk_expr(e, scopes, known, registry, acc),
        Stmt::Return { value: None, .. } => {}
        Stmt::While { cond, body, .. } => {
            walk_expr(cond, scopes, known, registry, acc);
            walk_block(&body.stmts, scopes, known, registry, acc);
        }
        Stmt::Expr(e) => walk_expr(e, scopes, known, registry, acc),
        // `audited` only suppresses codegen's Tier-1/2 guard *emission* —
        // what the code inside actually does, and therefore its effects,
        // is completely unaffected.
        Stmt::Audited { body, .. } => {
            scopes.push();
            for s in body {
                walk_stmt(s, scopes, known, registry, acc);
            }
            scopes.pop();
        }
    }
}

/// The type-relevant-to-effects category of `e`'s value, recovered
/// locally — every binding's type is already syntactically declared in
/// this language (no inference needed, only lookup), the same shortcut
/// `codegen.rs`'s own `local_ty_of` already takes, for the same reason.
/// Defaults to `Ty::Error` for anything not needed here (a plain scalar,
/// an unresolved name, ...) — callers only ever compare this against the
/// handful of affine-handle types that decide `Send`/`Recv`/
/// `StopSandbox`'s effect, never rely on it for anything else.
fn local_ty(e: &Expr, scopes: &Scopes) -> Ty {
    match e {
        Expr::Ident(name, _) => scopes.get(name).unwrap_or(Ty::Error),
        Expr::Connect(_, _, _) => Ty::Tcp,
        Expr::Listen(_, _) => Ty::TcpListener,
        Expr::Accept(_, _) => Ty::Tcp,
        Expr::Open(_, _, _) => Ty::File,
        // The element type doesn't matter here — every reader only
        // matches on the outer `Ty::Channel` shape, never `*inner`.
        Expr::Chan(_) => Ty::Channel(Box::new(Ty::Error)),
        _ => Ty::Error,
    }
}

/// A builtin's own direct effect — `LANGUAGE.md` §5's whole dense-linear-
/// algebra and geometry/Kalman-filter set is pure (reads its arguments,
/// returns a value, nothing else); only `print` and the RNG trio touch
/// anything outside the computation itself.
pub(crate) fn builtin_effect(name: &str) -> EffectSet {
    let mut s = EffectSet::new();
    match name {
        "print" => {
            s.insert(Effect::Io);
        }
        "rand_seed" | "rand_f64" | "rand_gaussian" => {
            s.insert(Effect::Rng);
        }
        // A real wall-clock sleep -- `Io`, not `Rng` (nothing to do with
        // the seeded PRNG stream).
        "sleep_ms" => {
            s.insert(Effect::Io);
        }
        // Same effect `connect`/`listen`/`accept`/`tcp`'s `send`/`recv`/
        // `stop` already get -- `http_get`/`http_post` are a real TCP
        // connection under the hood.
        "http_get" | "http_post" | "https_get" | "https_post" => {
            s.insert(Effect::Network);
        }
        // `json_*` builtins are pure: they only ever read the already-
        // parsed tree an earlier `json_parse` call produced, never touch
        // the outside world themselves (the classification's `_ => {}`
        // default already gives them `EffectSet::new()`; named here only
        // so a reader doesn't have to wonder whether they were missed).
        "json_parse" | "json_get" | "json_get_str" | "json_get_i64" | "json_get_f64" | "json_get_bool"
        | "json_array_len" | "json_array_get" => {}
        // Same effect `file`'s `open`/`send`/`recv`/`stop` already get --
        // SQLite (layer 1's only backend -- `Ty::Db`'s doc comment) is a
        // local file, not a network service.
        "db_connect" | "db_query" | "db_execute" => {
            s.insert(Effect::Io);
        }
        // Unlike `db` (SQLite, a local file), a queue is a real network
        // service -- same tag `http_get`/`connect`/`tcp` above get, not
        // `Io` (`Ty::Mq`'s doc comment).
        "mq_connect" | "mq_publish" | "mq_consume" => {
            s.insert(Effect::Network);
        }
        _ => {}
    }
    s
}

fn walk_expr(e: &Expr, scopes: &mut Scopes, known: &HashMap<String, EffectSet>, registry: &TypeRegistry, acc: &mut EffectSet) {
    match e {
        Expr::Int(_, _) | Expr::Float(_, _) | Expr::Bool(_, _) | Expr::Str(_, _) | Expr::Ident(_, _) | Expr::Chan(_) => {}
        Expr::Unary(_, inner, _) | Expr::Box(inner, _) | Expr::Deref(inner, _) | Expr::Ref(inner, _) => {
            walk_expr(inner, scopes, known, registry, acc);
        }
        Expr::Binary(_, l, r, _) => {
            walk_expr(l, scopes, known, registry, acc);
            walk_expr(r, scopes, known, registry, acc);
        }
        Expr::Assign(_, rhs, _) => walk_expr(rhs, scopes, known, registry, acc),
        Expr::Call(name, args, _) => {
            for a in args {
                walk_expr(a, scopes, known, registry, acc);
            }
            if is_builtin(name) {
                acc.extend(builtin_effect(name));
            } else if let Some(callee) = known.get(name) {
                acc.extend(callee.iter().copied());
            } else if matches!(scopes.get(name), Some(Ty::Fn(..))) {
                // Call-through-value: `name` is a local `fn(...)->...`
                // binding (an ordinary higher-order parameter, or the
                // `Ok(f)` a successful `acquire` produced — both are
                // just a local name of type `Ty::Fn` by the time we get
                // here, indistinguishable without real points-to
                // tracking this pass doesn't do), not a name `known`
                // resolves directly. A fixed red-team finding: this used
                // to silently attribute *nothing*, so a plain
                // `fn caller(f: fn()->i64) -> i64 effect(pure) { f() }`
                // typechecked clean no matter what `f` actually pointed
                // to and did at runtime -- unsound for the same reason
                // ordinary `known.get(name)` resolution exists at all.
                // We don't track which function(s) a given local could
                // actually be bound to, so the sound conservative
                // answer is "it could be anything already in this
                // program" -- the union of every function's own
                // inferred effects, not just this call's literal name.
                // Coarser than real flow analysis, but correct: it
                // never under-reports the way silence did, and it's
                // still no worse than assuming all four tags outright
                // since it only ever includes effects some real function
                // in this program actually has.
                acc.extend(known.values().flat_map(|fx| fx.iter().copied()));
            }
        }
        Expr::If { cond, then_block, else_block, .. } => {
            walk_expr(cond, scopes, known, registry, acc);
            walk_block(&then_block.stmts, scopes, known, registry, acc);
            match else_block.as_deref() {
                Some(ElseBranch::Block(b)) => walk_block(&b.stmts, scopes, known, registry, acc),
                Some(ElseBranch::If(e2)) => walk_expr(e2, scopes, known, registry, acc),
                None => {}
            }
        }
        Expr::Spawn(name, args, _) => {
            acc.insert(Effect::Concurrent);
            for a in args {
                walk_expr(a, scopes, known, registry, acc);
            }
            if let Some(callee) = known.get(name) {
                acc.extend(callee.iter().copied());
            }
        }
        // `acquire` itself performs no effect — it only checks `proof`
        // against the requirement, it doesn't invoke the gated function.
        // The resulting value's *own* call site (`Expr::Call` on a
        // `Ty::Fn`-typed local, e.g. the `f` in `Ok(f) => f(...)`) used
        // to be a real, silently-unsound gap here — `known.get(name)`
        // can't find a local binding's name, so nothing about the
        // privileged function's actual effects (or, more generally, an
        // *ordinary* higher-order parameter's — a fixed red-team finding
        // showed this wasn't specific to `acquire` at all) was ever
        // attributed to a caller holding one. `Expr::Call`'s own arm
        // above now handles this directly (any call through a local
        // `Ty::Fn` binding conservatively unions every function's
        // inferred effects), so this arm only needs to walk `proof`.
        Expr::Acquire(_, proof, _) => walk_expr(proof, scopes, known, registry, acc),
        Expr::Join(inner, _) => {
            acc.insert(Effect::Concurrent);
            walk_expr(inner, scopes, known, registry, acc);
        }
        Expr::Send(target, value, _) => {
            match local_ty(target, scopes) {
                Ty::Channel(_) => {
                    acc.insert(Effect::Concurrent);
                }
                Ty::Tcp => {
                    acc.insert(Effect::Network);
                }
                Ty::File => {
                    acc.insert(Effect::Io);
                }
                _ => {}
            }
            walk_expr(target, scopes, known, registry, acc);
            walk_expr(value, scopes, known, registry, acc);
        }
        Expr::Recv(target, _) => {
            match local_ty(target, scopes) {
                Ty::Channel(_) => {
                    acc.insert(Effect::Concurrent);
                }
                Ty::Tcp => {
                    acc.insert(Effect::Network);
                }
                Ty::File => {
                    acc.insert(Effect::Io);
                }
                _ => {}
            }
            walk_expr(target, scopes, known, registry, acc);
        }
        Expr::SpawnSandbox(name, args, _) => {
            acc.insert(Effect::Concurrent);
            for a in args {
                walk_expr(a, scopes, known, registry, acc);
            }
            if let Some(callee) = known.get(name) {
                acc.extend(callee.iter().copied());
            }
        }
        Expr::StopSandbox(inner, _) => {
            match local_ty(inner, scopes) {
                Ty::Sandbox => {
                    acc.insert(Effect::Concurrent);
                }
                Ty::Tcp | Ty::TcpListener => {
                    acc.insert(Effect::Network);
                }
                Ty::File => {
                    acc.insert(Effect::Io);
                }
                _ => {}
            }
            walk_expr(inner, scopes, known, registry, acc);
        }
        Expr::Connect(host, port, _) => {
            acc.insert(Effect::Network);
            walk_expr(host, scopes, known, registry, acc);
            walk_expr(port, scopes, known, registry, acc);
        }
        Expr::Listen(port, _) => {
            acc.insert(Effect::Network);
            walk_expr(port, scopes, known, registry, acc);
        }
        Expr::Accept(listener, _) => {
            acc.insert(Effect::Network);
            walk_expr(listener, scopes, known, registry, acc);
        }
        Expr::Open(path, mode, _) => {
            acc.insert(Effect::Io);
            walk_expr(path, scopes, known, registry, acc);
            walk_expr(mode, scopes, known, registry, acc);
        }
        Expr::Index(base, indices, _) => {
            walk_expr(base, scopes, known, registry, acc);
            for idx in indices {
                walk_expr(idx, scopes, known, registry, acc);
            }
        }
        Expr::ArrayLit(elements, _) => {
            for e in elements {
                walk_expr(e, scopes, known, registry, acc);
            }
        }
        // Every slot is restricted to a named user-function call
        // (`TRANSACT.md`) — no builtin to classify, just each callee's own
        // known effects, same as an ordinary `Expr::Call`.
        Expr::Transact { precheck, network, verify, commit, compensate, log, .. } => {
            let slots = precheck
                .iter()
                .chain(std::iter::once(network))
                .chain(std::iter::once(verify))
                .chain(std::iter::once(commit))
                .chain(compensate.iter())
                .chain(log.iter());
            for slot in slots {
                for a in &slot.args {
                    walk_expr(a, scopes, known, registry, acc);
                }
                if let Some(callee) = known.get(&slot.name) {
                    acc.extend(callee.iter().copied());
                }
            }
        }
        // Reading a field has no effect of its own -- just recurse into
        // the base, same treatment `Expr::Deref` already gets.
        Expr::FieldAccess(base, _, _) => walk_expr(base, scopes, known, registry, acc),
        // A `match` itself has no effect -- the scrutinee and every arm's
        // body are walked for their own effects, in a fresh scope with
        // that arm's payload bindings defined at their *real* declared
        // type (`TypeRegistry::find_variant`), not skipped -- a binding
        // that holds a `chan`/`tcp`/`file` value and is immediately
        // `send`/`recv`/`stop`-ed inside the arm has to be seen by
        // `local_ty`'s `Ident` lookup the same way a `let`-bound name
        // already is, or that effect would silently go undetected.
        Expr::Match { scrutinee, arms, .. } => {
            walk_expr(scrutinee, scopes, known, registry, acc);
            for arm in arms {
                scopes.push();
                if let Some((_, variant)) = registry.find_variant(&arm.variant) {
                    for (name, ty) in arm.bindings.iter().zip(variant.payload.iter()) {
                        scopes.define(name, ty.clone());
                    }
                }
                walk_expr(&arm.body, scopes, known, registry, acc);
                scopes.pop();
            }
        }
    }
}
