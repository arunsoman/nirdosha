//! Static type checker — runs before the interpreter ever sees the program.
//! This is the milestone flagged at the end of Phase 0: ownership analysis
//! and refinement types (goal.md rows 1, 4) both need a fully-typed AST to
//! work over, and until now nothing built one ahead of time — the old
//! interpreter checked types *as it executed*, which is Python's discipline,
//! not the one goal.md asks for.
//!
//! Design notes worth keeping visible, not just in commit history:
//!
//! - **Error recovery, not fail-fast.** A mismatch is recorded and checking
//!   continues with a poison type (`Ty::Error`) standing in for the bad
//!   expression, so one mistake doesn't hide the next five behind it. A
//!   compiler that stops at the first error is a worse interface for an
//!   agent's self-repair loop (goal.md row 9) than one that reports
//!   everything it can see in one pass.
//! - **Integer literals are flexible, declared variables are not.** `n - 1`
//!   type-checks against whatever `n`'s declared width is; two variables of
//!   *different* declared widths do not implicitly convert to each other —
//!   that's goal.md §3's "no implicit conversions" core-language rule,
//!   actually enforced here for the first time (the interpreter alone never
//!   enforced it, since every `Ty` collapses to the same `Value::Int(i64)`
//!   at runtime).
//! - **`if` used as a statement doesn't need its branches to agree in
//!   type; `if` used as a value does.** `if c { count = count + 1 }` with
//!   no `else`, appearing as a bare statement, is not an error — nothing
//!   reads its value. `let x: i32 = if c { 1 } else { 2 }` requires both
//!   branches to produce `i32`, and requires an `else` to exist at all.
//!   Conflating these two positions would make this checker reject
//!   `examples/loop.nir`, which is exactly why the distinction is
//!   load-bearing, not decorative.
//! - **`return` can appear inside a value-position `if`.** `let x: i32 =
//!   if c { return 5 } else { 10 }` is legal — the interpreter already
//!   runs it correctly (a `return` unwinds the whole function regardless
//!   of where it's nested; see `interpreter.rs`'s `Signal`). The checker
//!   has to thread the function's declared return type through *every*
//!   value position, not just statement position, or it would be either
//!   unsound (accepting a `return` of the wrong type) or, worse,
//!   inconsistent with what the interpreter actually does — rejecting
//!   programs that run correctly. `expected_ret` is threaded everywhere
//!   below for exactly this reason.
//! - **Definite-return analysis.** A function declared to return non-`unit`
//!   must, this pass proves, hit a `return` on every path — not "at
//!   runtime it happened to." That's a real static property, checked
//!   structurally over `if`/`else` (see `definitely_returns`), the same
//!   shape as Rust's or Java's version of the same check.
//! - **`Ty` values are threaded by reference (`&Ty`), not by value.**
//!   `Ty::Box` made `Ty` non-`Copy` (see ast.rs), and `expected_ret`/`want`
//!   get passed into nearly every function here — cloning at each hop
//!   would be needless allocation on every recursive call for no benefit,
//!   since nothing here ever needs to *own* an expected type, only read it.

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

#[derive(Debug, Clone, PartialEq)]
pub enum TypeErrorKind {
    UnknownVar(String),
    UnknownFn(String),
    DuplicateFn(String),
    NoMainFn,
    MainMustTakeNoParams,
    ArityMismatch { fn_name: String, want: usize, got: usize },
    TypeMismatch { expected: Ty, found: Ty },
    ExpectedBool { found: Ty },
    ExpectedNumeric { found: Ty },
    ExpectedBoxType { found: Ty },
    CannotMoveOutOfReference { content: Ty },
    LiteralOutOfRange { ty: Ty, value: i64 },
    IfWithoutElseUsedAsValue { expected: Ty },
    NotAllPathsReturn { fn_name: String },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeError {
    pub kind: TypeErrorKind,
    pub span: Span,
}

impl std::fmt::Display for TypeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Span { line, col } = self.span;
        match &self.kind {
            TypeErrorKind::UnknownVar(n) => write!(f, "{line}:{col}: unknown variable `{n}`"),
            TypeErrorKind::UnknownFn(n) => write!(f, "{line}:{col}: unknown function `{n}`"),
            TypeErrorKind::DuplicateFn(n) => {
                write!(f, "{line}:{col}: `{n}` is defined more than once")
            }
            TypeErrorKind::NoMainFn => write!(f, "{line}:{col}: no `fn main()` found"),
            TypeErrorKind::MainMustTakeNoParams => {
                write!(f, "{line}:{col}: `main` must take no parameters")
            }
            TypeErrorKind::ArityMismatch { fn_name, want, got } => write!(
                f,
                "{line}:{col}: `{fn_name}` expects {want} argument(s), got {got}"
            ),
            TypeErrorKind::TypeMismatch { expected, found } => write!(
                f,
                "{line}:{col}: expected `{}`, found `{}`",
                expected.name(),
                found.name()
            ),
            TypeErrorKind::ExpectedBool { found } => {
                write!(f, "{line}:{col}: expected `bool`, found `{}`", found.name())
            }
            TypeErrorKind::ExpectedNumeric { found } => write!(
                f,
                "{line}:{col}: expected a numeric type, found `{}`",
                found.name()
            ),
            TypeErrorKind::ExpectedBoxType { found } => write!(
                f,
                "{line}:{col}: `*` needs a `box` or `&` type, found `{}`",
                found.name()
            ),
            TypeErrorKind::CannotMoveOutOfReference { content } => write!(
                f,
                "{line}:{col}: cannot move `{}` out of a shared reference \
                 (only through an owned `box`)",
                content.name()
            ),
            TypeErrorKind::LiteralOutOfRange { ty, value } => write!(
                f,
                "{line}:{col}: literal `{value}` does not fit in `{}`",
                ty.name()
            ),
            TypeErrorKind::IfWithoutElseUsedAsValue { expected } => write!(
                f,
                "{line}:{col}: `if` with no `else` cannot produce a value of type `{}` \
                 (only `unit`, from the implicit no-else case)",
                expected.name()
            ),
            TypeErrorKind::NotAllPathsReturn { fn_name } => write!(
                f,
                "{line}:{col}: not every path through `{fn_name}` returns a value"
            ),
        }
    }
}

struct FnSig {
    params: Vec<Ty>,
    ret: Ty,
}

/// A lexical scope stack from declared name to declared `Ty` — the static
/// analogue of `interpreter::Env`, minus the values.
struct Scopes(Vec<HashMap<String, Ty>>);

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
    fn define(&mut self, name: &str, ty: Ty) {
        self.0.last_mut().unwrap().insert(name.to_string(), ty);
    }
    fn get(&self, name: &str) -> Option<Ty> {
        self.0.iter().rev().find_map(|s| s.get(name)).cloned()
    }
}

pub struct Checker {
    sigs: HashMap<String, FnSig>,
    errors: Vec<TypeError>,
}

/// Type-check a whole program. `Ok(())` means every function body is well
/// typed *and* proved to return on every path where its signature demands
/// a value — the interpreter should never be run on a program this
/// rejects.
pub fn typecheck(program: &Program) -> Result<(), Vec<TypeError>> {
    let mut c = Checker { sigs: HashMap::new(), errors: Vec::new() };

    for f in &program.fns {
        if c.sigs.contains_key(&f.name) {
            c.error(TypeErrorKind::DuplicateFn(f.name.clone()), f.span);
            continue;
        }
        c.sigs.insert(
            f.name.clone(),
            FnSig { params: f.params.iter().map(|p| p.ty.clone()).collect(), ret: f.ret.clone() },
        );
    }

    match c.sigs.get("main") {
        None => c.error(TypeErrorKind::NoMainFn, Span { line: 0, col: 0 }),
        Some(sig) if !sig.params.is_empty() => {
            let span = program.fns.iter().find(|f| f.name == "main").unwrap().span;
            c.error(TypeErrorKind::MainMustTakeNoParams, span);
        }
        Some(_) => {}
    }

    for f in &program.fns {
        c.check_fn(f);
    }

    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}

impl Checker {
    fn error(&mut self, kind: TypeErrorKind, span: Span) {
        self.errors.push(TypeError { kind, span });
    }

    fn check_fn(&mut self, f: &FnDecl) {
        let mut scopes = Scopes::new();
        for p in &f.params {
            scopes.define(&p.name, p.ty.clone());
        }
        self.check_stmts(&f.body.stmts, &f.ret, &mut scopes);

        if f.ret != Ty::Unit && !definitely_returns(&f.body.stmts) {
            self.error(TypeErrorKind::NotAllPathsReturn { fn_name: f.name.clone() }, f.span);
        }
    }

    // ---- statement-level checking (expected_ret only, no value context) --

    fn check_stmts(&mut self, stmts: &[Stmt], expected_ret: &Ty, scopes: &mut Scopes) {
        for stmt in stmts {
            self.check_stmt(stmt, expected_ret, scopes);
        }
    }

    fn check_block(&mut self, block: &Block, expected_ret: &Ty, scopes: &mut Scopes) {
        scopes.push();
        self.check_stmts(&block.stmts, expected_ret, scopes);
        scopes.pop();
    }

    fn check_stmt(&mut self, stmt: &Stmt, expected_ret: &Ty, scopes: &mut Scopes) {
        match stmt {
            Stmt::Let { name, ty, value, .. } => {
                self.check(value, ty, expected_ret, scopes);
                scopes.define(name, ty.clone());
            }
            Stmt::Return { value, span } => match value {
                Some(e) => self.check(e, expected_ret, expected_ret, scopes),
                None => {
                    if *expected_ret != Ty::Unit {
                        self.error(
                            TypeErrorKind::TypeMismatch { expected: expected_ret.clone(), found: Ty::Unit },
                            *span,
                        );
                    }
                }
            },
            Stmt::While { cond, body, .. } => {
                let ct = self.infer(cond, expected_ret, scopes);
                if ct != Ty::Bool && ct != Ty::Error {
                    self.error(TypeErrorKind::ExpectedBool { found: ct }, cond.span());
                }
                self.check_block(body, expected_ret, scopes);
            }
            Stmt::Expr(e) => self.check_stmt_expr(e, expected_ret, scopes),
        }
    }

    /// A bare expression-statement: its value, if any, is discarded, so an
    /// `if` here doesn't need its branches to agree — see the module-level
    /// doc comment for why that distinction matters. This path does *not*
    /// go through `check_if`/`want` at all; it's a separate, simpler walk.
    fn check_stmt_expr(&mut self, e: &Expr, expected_ret: &Ty, scopes: &mut Scopes) {
        if let Expr::If { cond, then_block, else_block, .. } = e {
            let ct = self.infer(cond, expected_ret, scopes);
            if ct != Ty::Bool && ct != Ty::Error {
                self.error(TypeErrorKind::ExpectedBool { found: ct }, cond.span());
            }
            self.check_block(then_block, expected_ret, scopes);
            if let Some(eb) = else_block {
                match eb.as_ref() {
                    ElseBranch::Block(b) => self.check_block(b, expected_ret, scopes),
                    ElseBranch::If(e2) => self.check_stmt_expr(e2, expected_ret, scopes),
                }
            }
        } else {
            self.infer(e, expected_ret, scopes);
        }
    }

    // ---- value-position checking (expected_ret *and* a value type) -------

    /// Check `e` against an expected value type `want`, with integer-literal
    /// flexibility (see module doc). This is the entry point every value
    /// position (`let`, `return`, assignment RHS, call argument) goes
    /// through, so "no implicit conversions" is enforced in one place.
    fn check(&mut self, e: &Expr, want: &Ty, expected_ret: &Ty, scopes: &mut Scopes) {
        if let Some(lit) = literal_value(e) {
            if want.is_integer() {
                if !want.in_range(lit) {
                    self.error(
                        TypeErrorKind::LiteralOutOfRange { ty: want.clone(), value: lit },
                        e.span(),
                    );
                }
            } else {
                self.error(
                    TypeErrorKind::TypeMismatch { expected: want.clone(), found: Ty::I64 },
                    e.span(),
                );
            }
            return;
        }
        if let Expr::If { cond, then_block, else_block, span } = e {
            self.check_if(cond, then_block, else_block.as_deref(), *span, Some(want), expected_ret, scopes);
            return;
        }
        let found = self.infer(e, expected_ret, scopes);
        if found != Ty::Error && found != *want {
            self.error(TypeErrorKind::TypeMismatch { expected: want.clone(), found }, e.span());
        }
    }

    /// Infer `e`'s type with no expected *value* type — used for binary/
    /// unary operands and other positions the grammar doesn't pin to one
    /// specific type. Still needs `expected_ret` in case a `return` is
    /// nested somewhere inside (see module doc).
    fn infer(&mut self, e: &Expr, expected_ret: &Ty, scopes: &mut Scopes) -> Ty {
        match e {
            Expr::Int(_, _) => Ty::I64, // untyped literal's default when nothing constrains it
            Expr::Bool(_, _) => Ty::Bool,
            Expr::Ident(name, span) => match scopes.get(name) {
                Some(t) => t,
                None => {
                    self.error(TypeErrorKind::UnknownVar(name.clone()), *span);
                    Ty::Error
                }
            },
            Expr::Unary(op, inner, span) => {
                if literal_value(e).is_some() {
                    return Ty::I64;
                }
                let it = self.infer(inner, expected_ret, scopes);
                match op {
                    UnOp::Neg => {
                        if it != Ty::Error && !it.is_integer() {
                            self.error(TypeErrorKind::ExpectedNumeric { found: it }, *span);
                            Ty::Error
                        } else {
                            it
                        }
                    }
                    UnOp::Not => {
                        if it != Ty::Error && it != Ty::Bool {
                            self.error(TypeErrorKind::ExpectedBool { found: it }, *span);
                            Ty::Error
                        } else {
                            Ty::Bool
                        }
                    }
                }
            }
            Expr::Binary(op, lhs, rhs, span) => self.infer_binary(*op, lhs, rhs, expected_ret, scopes, *span),
            Expr::Call(name, args, span) => self.infer_call(name, args, expected_ret, scopes, *span),
            Expr::If { cond, then_block, else_block, span } => {
                self.check_if(cond, then_block, else_block.as_deref(), *span, None, expected_ret, scopes)
            }
            Expr::Assign(name, rhs, span) => {
                let ty = match scopes.get(name) {
                    Some(t) => t,
                    None => {
                        self.error(TypeErrorKind::UnknownVar(name.clone()), *span);
                        return Ty::Error;
                    }
                };
                self.check(rhs, &ty, expected_ret, scopes);
                ty
            }
            Expr::Box(inner, _span) => {
                let it = self.infer(inner, expected_ret, scopes);
                if it == Ty::Error {
                    Ty::Error
                } else {
                    Ty::Box(Box::new(it))
                }
            }
            Expr::Deref(inner, span) => {
                let it = self.infer(inner, expected_ret, scopes);
                match it {
                    Ty::Error => Ty::Error,
                    Ty::Box(t) => *t,
                    Ty::Ref(t) => {
                        if t.is_affine() {
                            self.error(TypeErrorKind::CannotMoveOutOfReference { content: *t }, *span);
                            Ty::Error
                        } else {
                            *t
                        }
                    }
                    other => {
                        self.error(TypeErrorKind::ExpectedBoxType { found: other }, *span);
                        Ty::Error
                    }
                }
            }
            Expr::Ref(inner, span) => {
                debug_assert!(
                    matches!(inner.as_ref(), Expr::Ident(..)),
                    "parser only ever produces Expr::Ref with an Ident operand"
                );
                let it = self.infer(inner, expected_ret, scopes);
                let _ = span;
                if it == Ty::Error {
                    Ty::Error
                } else {
                    Ty::Ref(Box::new(it))
                }
            }
        }
    }

    fn infer_call(&mut self, name: &str, args: &[Expr], expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        if name == "print" {
            for a in args {
                self.infer(a, expected_ret, scopes); // any of the language's types render fine
            }
            return Ty::Unit;
        }
        let Some(sig) = self.sigs.get(name) else {
            self.error(TypeErrorKind::UnknownFn(name.to_string()), span);
            for a in args {
                self.infer(a, expected_ret, scopes); // still check the args for their own errors
            }
            return Ty::Error;
        };
        let params = sig.params.clone();
        let ret = sig.ret.clone();
        if params.len() != args.len() {
            self.error(
                TypeErrorKind::ArityMismatch { fn_name: name.to_string(), want: params.len(), got: args.len() },
                span,
            );
        }
        for (arg, want) in args.iter().zip(params.iter()) {
            self.check(arg, want, expected_ret, scopes);
        }
        // Args beyond the shorter of the two lists still get inferred, so a
        // wrong-arity call reports its own internal errors too, not just
        // the arity mismatch.
        for extra in args.iter().skip(params.len()) {
            self.infer(extra, expected_ret, scopes);
        }
        ret
    }

    fn infer_binary(
        &mut self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        expected_ret: &Ty,
        scopes: &mut Scopes,
        span: Span,
    ) -> Ty {
        match op {
            BinOp::And | BinOp::Or => {
                self.check_bool_operand(lhs, expected_ret, scopes);
                self.check_bool_operand(rhs, expected_ret, scopes);
                Ty::Bool
            }
            BinOp::Eq | BinOp::NotEq => {
                self.unify_operands(lhs, rhs, expected_ret, scopes, span);
                Ty::Bool
            }
            BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                let t = self.unify_operands(lhs, rhs, expected_ret, scopes, span);
                if t == Ty::Bool {
                    self.error(TypeErrorKind::ExpectedNumeric { found: Ty::Bool }, span);
                }
                Ty::Bool
            }
            BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => {
                let t = self.unify_operands(lhs, rhs, expected_ret, scopes, span);
                if t == Ty::Bool {
                    self.error(TypeErrorKind::ExpectedNumeric { found: Ty::Bool }, span);
                    return Ty::Error;
                }
                t
            }
        }
    }

    fn check_bool_operand(&mut self, e: &Expr, expected_ret: &Ty, scopes: &mut Scopes) {
        let t = self.infer(e, expected_ret, scopes);
        if t != Ty::Error && t != Ty::Bool {
            self.error(TypeErrorKind::ExpectedBool { found: t }, e.span());
        }
    }

    /// The core of "literals are flexible, declared bindings are not": if
    /// exactly one side is a bare integer literal, it takes on the other
    /// side's type (range-checked); if both sides have a fixed, known type,
    /// they must match exactly. Returns `Ty::Error` if anything went wrong,
    /// so callers can suppress follow-on diagnostics.
    fn unify_operands(&mut self, lhs: &Expr, rhs: &Expr, expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        let l_lit = literal_value(lhs);
        let r_lit = literal_value(rhs);
        match (l_lit, r_lit) {
            (Some(_), Some(_)) => Ty::I64,
            (Some(lv), None) => {
                let rt = self.infer(rhs, expected_ret, scopes);
                if rt == Ty::Error {
                    return Ty::Error;
                }
                if !rt.is_integer() {
                    self.error(TypeErrorKind::ExpectedNumeric { found: rt }, lhs.span());
                    return Ty::Error;
                }
                if !rt.in_range(lv) {
                    self.error(TypeErrorKind::LiteralOutOfRange { ty: rt, value: lv }, lhs.span());
                    return Ty::Error;
                }
                rt
            }
            (None, Some(rv)) => {
                let lt = self.infer(lhs, expected_ret, scopes);
                if lt == Ty::Error {
                    return Ty::Error;
                }
                if !lt.is_integer() {
                    self.error(TypeErrorKind::ExpectedNumeric { found: lt }, rhs.span());
                    return Ty::Error;
                }
                if !lt.in_range(rv) {
                    self.error(TypeErrorKind::LiteralOutOfRange { ty: lt, value: rv }, rhs.span());
                    return Ty::Error;
                }
                lt
            }
            (None, None) => {
                let lt = self.infer(lhs, expected_ret, scopes);
                let rt = self.infer(rhs, expected_ret, scopes);
                if lt == Ty::Error || rt == Ty::Error {
                    return Ty::Error;
                }
                if lt != rt {
                    self.error(TypeErrorKind::TypeMismatch { expected: lt, found: rt }, span);
                    return Ty::Error;
                }
                lt
            }
        }
    }

    /// Shared by `infer` (`want = None`) and `check` (`want = Some(ty)`)
    /// for `if`-as-expression. `want = None` means nobody reads the
    /// result — branches don't need to agree, and a missing `else` isn't
    /// an error. `want = Some(ty)` means both branches (and a present
    /// `else`) must produce `ty`, and a missing `else` *is* an error
    /// unless `ty` is `unit`.
    #[allow(clippy::too_many_arguments)]
    fn check_if(
        &mut self,
        cond: &Expr,
        then_block: &Block,
        else_block: Option<&ElseBranch>,
        span: Span,
        want: Option<&Ty>,
        expected_ret: &Ty,
        scopes: &mut Scopes,
    ) -> Ty {
        let ct = self.infer(cond, expected_ret, scopes);
        if ct != Ty::Bool && ct != Ty::Error {
            self.error(TypeErrorKind::ExpectedBool { found: ct }, cond.span());
        }

        let then_ty = self.check_block_value(then_block, want, expected_ret, scopes);

        let else_ty = match else_block {
            Some(ElseBranch::Block(b)) => Some(self.check_block_value(b, want, expected_ret, scopes)),
            Some(ElseBranch::If(e2)) => {
                let Expr::If { cond: c2, then_block: t2, else_block: eb2, span: s2 } = e2 else {
                    unreachable!("parser only ever produces Expr::If for an else-if chain")
                };
                Some(self.check_if(c2, t2, eb2.as_deref(), *s2, want, expected_ret, scopes))
            }
            None => None,
        };

        match (want, else_ty) {
            (Some(w), None) => {
                if *w != Ty::Unit {
                    self.error(TypeErrorKind::IfWithoutElseUsedAsValue { expected: w.clone() }, span);
                    Ty::Error
                } else {
                    Ty::Unit
                }
            }
            (Some(w), Some(_)) => w.clone(), // both branches already individually checked against `w`
            (None, None) => Ty::Unit,
            (None, Some(else_ty)) => {
                if then_ty != Ty::Error && else_ty != Ty::Error && then_ty != else_ty {
                    self.error(
                        TypeErrorKind::TypeMismatch { expected: then_ty.clone(), found: else_ty },
                        span,
                    );
                    Ty::Error
                } else if then_ty == Ty::Error || else_ty == Ty::Error {
                    Ty::Error
                } else {
                    then_ty
                }
            }
        }
    }

    /// A block used in value position: every statement but the last is
    /// checked normally; the last, if it's an expression-statement, is
    /// checked against `want` (or just inferred, if `want` is `None`) —
    /// that's the block's "trailing expression" value. A block ending in
    /// `let`/`return`/`while`, or an empty block, has value `unit`.
    fn check_block_value(&mut self, block: &Block, want: Option<&Ty>, expected_ret: &Ty, scopes: &mut Scopes) -> Ty {
        scopes.push();
        let result = match block.stmts.split_last() {
            None => Ty::Unit,
            Some((last, rest)) => {
                self.check_stmts(rest, expected_ret, scopes);
                match last {
                    Stmt::Expr(e) => match want {
                        Some(w) => {
                            self.check(e, w, expected_ret, scopes);
                            w.clone()
                        }
                        None => self.infer(e, expected_ret, scopes),
                    },
                    other => {
                        self.check_stmt(other, expected_ret, scopes);
                        Ty::Unit
                    }
                }
            }
        };
        scopes.pop();
        result
    }
}

/// Structural "does every path through this statement list hit a
/// `return`" analysis. An `if` counts only when it has an `else` and both
/// branches definitely return — an `if` with no `else` never counts, since
/// the no-else path falls through.
fn definitely_returns(stmts: &[Stmt]) -> bool {
    for stmt in stmts {
        match stmt {
            Stmt::Return { .. } => return true,
            Stmt::Expr(e) if if_definitely_returns(e) => return true,
            _ => {}
        }
    }
    false
}

fn if_definitely_returns(e: &Expr) -> bool {
    match e {
        Expr::If { then_block, else_block, .. } => {
            let then_ret = definitely_returns(&then_block.stmts);
            let else_ret = match else_block {
                Some(eb) => match eb.as_ref() {
                    ElseBranch::Block(b) => definitely_returns(&b.stmts),
                    ElseBranch::If(e2) => if_definitely_returns(e2),
                },
                None => false,
            };
            then_ret && else_ret
        }
        _ => false,
    }
}
