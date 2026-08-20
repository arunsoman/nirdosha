//! Tree-walking interpreter. Deliberately no effect checking, no
//! SMT-discharged bounds — those are goal.md rows 4, 9, Phase 2 work. What
//! this *does* enforce now: declared integer widths are checked at every
//! `let`, `return`, and function boundary (row 4's Tier-2 "checked"
//! behavior as a runtime stand-in for the eventual compile-time proof —
//! see `Ty::in_range` in ast.rs), and every error is a structured
//! `RuntimeError` with a span and a machine-matchable `kind`, not a
//! formatted string a caller has to re-parse (row 9).
//!
//! Ownership (goal.md row 1) is enforced *statically*, by `ownership.rs`,
//! before this file ever runs — this interpreter doesn't re-check moves at
//! runtime. Its own memory safety for `Value::Boxed` is simply inherited
//! from Rust's ownership system (a real `Box<Value>`, dropped by Rust when
//! it goes out of scope); see `Value`'s doc comment for why that's an
//! honest thing to say, not a shortcut being hidden.
//!
//! Control flow: `if` is a genuine expression in the grammar (GRAMMAR.md),
//! and a block's value is its last expression-statement (Unit if the block
//! is empty or ends in `let`/`return`/`while`). `return` has to be able to
//! unwind out of a `let`'s initializer, an `if`'s condition, a binary
//! operand — anywhere an expression can appear — not just out of a
//! statement list. `Signal` is what carries that: every `eval_expr` site
//! propagates it with `?`, and only `call()` catches `Signal::Return`.

use std::collections::HashMap;

use crate::ast::*;
use crate::token::Span;

/// Not `Copy` — `Value::Boxed` owns a heap allocation (a real Rust `Box`),
/// and giving the enum a free bitwise-copy would let two `Value`s alias the
/// same allocation with neither aware of the other, exactly the hazard
/// `Ty::Box`'s affine-ness (ast.rs) and `ownership.rs`'s move-checker exist
/// to prevent. This interpreter doesn't implement allocation or
/// deallocation itself — it inherits Rust's — so its actual contribution
/// to the "no GC" claim (goal.md row 1) is the *static* proof
/// (`ownership.rs`) that a Nirdosha program never uses a value after
/// giving it up, which is the same proof a real (future, LLVM-compiled)
/// backend would need to free deterministically with no garbage collector
/// at all. Don't read "the interpreter runs `box` correctly" as "row 1 is
/// done" — it's the checker that's doing the row-1 work, not this file.
#[derive(Debug, Clone, PartialEq)]
pub enum Value {
    Int(i64),
    Bool(bool),
    Unit,
    Boxed(Box<Value>),
}

impl Value {
    fn ty_name(&self) -> &'static str {
        match self {
            Value::Int(_) => "int",
            Value::Bool(_) => "bool",
            Value::Unit => "unit",
            Value::Boxed(_) => "box",
        }
    }

    fn render(&self) -> String {
        match self {
            Value::Int(n) => n.to_string(),
            Value::Bool(b) => b.to_string(),
            Value::Unit => "()".to_string(),
            Value::Boxed(inner) => format!("box({})", inner.render()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum ErrorKind {
    UnknownFn(String),
    UnknownVar(String),
    ArityMismatch { fn_name: String, want: usize, got: usize },
    // `String`, not `&'static str`: `Ty::name()` has to render recursively
    // for `box box i64`-style types, so it can no longer be a compile-time
    // constant.
    TypeMismatch { expected: String, found: String },
    OutOfRange { ty: String, value: i64 },
    DivByZero,
    MissingReturn { fn_name: String },
}

#[derive(Debug, Clone)]
pub struct RuntimeError {
    pub kind: ErrorKind,
    pub span: Span,
}

impl std::fmt::Display for RuntimeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Span { line, col } = self.span;
        match &self.kind {
            ErrorKind::UnknownFn(n) => write!(f, "{line}:{col}: unknown function `{n}`"),
            ErrorKind::UnknownVar(n) => write!(f, "{line}:{col}: unknown variable `{n}`"),
            ErrorKind::ArityMismatch { fn_name, want, got } => write!(
                f,
                "{line}:{col}: `{fn_name}` expects {want} argument(s), got {got}"
            ),
            ErrorKind::TypeMismatch { expected, found } => {
                write!(f, "{line}:{col}: expected {expected}, found {found}")
            }
            ErrorKind::OutOfRange { ty, value } => write!(
                f,
                "{line}:{col}: value {value} does not fit in `{ty}` (Tier-2 checked op — \
                 see goal.md §4; not yet proved absent at compile time)"
            ),
            ErrorKind::DivByZero => write!(f, "{line}:{col}: division by zero"),
            ErrorKind::MissingReturn { fn_name } => {
                write!(f, "{line}:{col}: `{fn_name}` did not return a value")
            }
        }
    }
}

fn err<T>(kind: ErrorKind, span: Span) -> Result<T, RuntimeError> {
    Err(RuntimeError { kind, span })
}

fn mismatch(expected: impl Into<String>, found: impl Into<String>, span: Span) -> Signal {
    Signal::Err(RuntimeError {
        kind: ErrorKind::TypeMismatch { expected: expected.into(), found: found.into() },
        span,
    })
}

/// Everything that can interrupt normal left-to-right evaluation.
/// `Signal::Return` unwinds through expression *and* statement evaluation
/// alike, all the way to the nearest `call()`.
enum Signal {
    Err(RuntimeError),
    Return(Value),
}

impl From<RuntimeError> for Signal {
    fn from(e: RuntimeError) -> Self {
        Signal::Err(e)
    }
}

type SResult<T> = Result<T, Signal>;

/// Bindings carry their declared `Ty` alongside the current `Value` so
/// `Expr::Assign` can re-check the new value against the *original*
/// declaration (goal.md row 4's Tier-2 placeholder), not just against
/// whatever kind of value happened to be there a moment ago.
struct Env {
    scopes: Vec<HashMap<String, (Value, Ty)>>,
}

#[derive(Debug)]
enum SetErr {
    NotFound,
}

impl Env {
    fn new() -> Self {
        Env { scopes: vec![HashMap::new()] }
    }
    fn push(&mut self) {
        self.scopes.push(HashMap::new());
    }
    fn pop(&mut self) {
        self.scopes.pop();
    }
    fn define(&mut self, name: &str, v: Value, ty: Ty) {
        self.scopes.last_mut().unwrap().insert(name.to_string(), (v, ty));
    }
    fn get(&self, name: &str) -> Option<Value> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).map(|(v, _)| v.clone())
    }
    fn get_ty(&self, name: &str) -> Option<Ty> {
        self.scopes.iter().rev().find_map(|s| s.get(name)).map(|(_, t)| t.clone())
    }
    fn set(&mut self, name: &str, v: Value) -> Result<(), SetErr> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some(slot) = scope.get_mut(name) {
                slot.0 = v;
                return Ok(());
            }
        }
        Err(SetErr::NotFound)
    }
}

pub struct Interpreter<'p> {
    fns: HashMap<String, &'p FnDecl>,
}

impl<'p> Interpreter<'p> {
    pub fn new(program: &'p Program) -> Self {
        let fns = program.fns.iter().map(|f| (f.name.clone(), f)).collect();
        Interpreter { fns }
    }

    pub fn run_main(&self) -> Result<Value, RuntimeError> {
        let span = Span { line: 0, col: 0 };
        self.call("main", &[], span)
    }

    /// The one place `Signal::Return` gets caught and turned back into a
    /// plain value — every nested `if`/block/expression underneath just
    /// propagates it with `?`.
    fn call(&self, name: &str, arg_vals: &[Value], span: Span) -> Result<Value, RuntimeError> {
        let f = *self.fns.get(name).ok_or_else(|| RuntimeError {
            kind: ErrorKind::UnknownFn(name.to_string()),
            span,
        })?;
        if f.params.len() != arg_vals.len() {
            return err(
                ErrorKind::ArityMismatch {
                    fn_name: name.to_string(),
                    want: f.params.len(),
                    got: arg_vals.len(),
                },
                span,
            );
        }

        let mut env = Env::new();
        for (p, v) in f.params.iter().zip(arg_vals) {
            self.check_ty(v, &p.ty, span)?;
            env.define(&p.name, v.clone(), p.ty.clone());
        }

        match self.exec_block(&f.body, &mut env) {
            Ok(_) => {
                // Block ran to completion without `return` — its value
                // (the last expression-statement, or Unit) is only a valid
                // function result if the declared return type is Unit.
                if f.ret == Ty::Unit {
                    Ok(Value::Unit)
                } else {
                    err(ErrorKind::MissingReturn { fn_name: name.to_string() }, f.span)
                }
            }
            Err(Signal::Return(v)) => {
                self.check_ty(&v, &f.ret, span)?;
                Ok(v)
            }
            Err(Signal::Err(e)) => Err(e),
        }
    }

    fn check_ty(&self, v: &Value, ty: &Ty, span: Span) -> Result<(), RuntimeError> {
        match (v, ty) {
            (Value::Bool(_), Ty::Bool) => Ok(()),
            (Value::Unit, Ty::Unit) => Ok(()),
            (Value::Boxed(inner), Ty::Box(inner_ty)) => self.check_ty(inner, inner_ty, span),
            (Value::Int(n), _) if ty.is_integer() => {
                if ty.in_range(*n) {
                    Ok(())
                } else {
                    err(ErrorKind::OutOfRange { ty: ty.name(), value: *n }, span)
                }
            }
            (v, ty) => err(
                ErrorKind::TypeMismatch { expected: ty.name(), found: v.ty_name().to_string() },
                span,
            ),
        }
    }

    /// A block's value is its last expression-statement (Unit if none) —
    /// the same implicit-last-expression convention `if`'s branches rely on.
    fn exec_block(&self, block: &Block, env: &mut Env) -> SResult<Value> {
        env.push();
        let result = self.exec_stmts(&block.stmts, env);
        env.pop();
        result
    }

    fn exec_stmts(&self, stmts: &[Stmt], env: &mut Env) -> SResult<Value> {
        let mut last = Value::Unit;
        for stmt in stmts {
            match stmt {
                Stmt::Let { name, ty, value, span } => {
                    let v = self.eval_expr(value, env)?;
                    self.check_ty(&v, ty, *span)?;
                    env.define(name, v, ty.clone());
                    last = Value::Unit;
                }
                Stmt::Return { value, span } => {
                    let v = match value {
                        Some(e) => self.eval_expr(e, env)?,
                        None => Value::Unit,
                    };
                    let _ = span;
                    return Err(Signal::Return(v));
                }
                Stmt::While { cond, body, span } => {
                    let _ = span;
                    loop {
                        let c = self.eval_expr(cond, env)?;
                        match c {
                            Value::Bool(true) => {}
                            Value::Bool(false) => break,
                            v => return Err(mismatch("bool", v.ty_name(), cond.span())),
                        }
                        self.exec_block(body, env)?; // propagates Signal::Return via `?`
                    }
                    last = Value::Unit;
                }
                Stmt::Expr(e) => {
                    last = self.eval_expr(e, env)?;
                }
            }
        }
        Ok(last)
    }

    fn eval_expr(&self, expr: &Expr, env: &mut Env) -> SResult<Value> {
        match expr {
            Expr::Int(n, _) => Ok(Value::Int(*n)),
            Expr::Bool(b, _) => Ok(Value::Bool(*b)),
            Expr::Ident(name, span) => env.get(name).ok_or_else(|| {
                Signal::Err(RuntimeError { kind: ErrorKind::UnknownVar(name.clone()), span: *span })
            }),
            Expr::Unary(op, inner, span) => {
                let v = self.eval_expr(inner, env)?;
                match (op, v) {
                    (UnOp::Neg, Value::Int(n)) => Ok(Value::Int(-n)),
                    (UnOp::Not, Value::Bool(b)) => Ok(Value::Bool(!b)),
                    (UnOp::Neg, v) => Err(mismatch("int", v.ty_name(), *span)),
                    (UnOp::Not, v) => Err(mismatch("bool", v.ty_name(), *span)),
                }
            }
            Expr::Binary(op, lhs, rhs, span) => self.eval_binary(*op, lhs, rhs, env, *span),
            Expr::Call(name, arg_exprs, span) => {
                if name == "print" {
                    let mut vals = Vec::with_capacity(arg_exprs.len());
                    for a in arg_exprs {
                        vals.push(self.eval_expr(a, env)?);
                    }
                    let rendered: Vec<String> = vals.iter().map(Value::render).collect();
                    println!("{}", rendered.join(" "));
                    return Ok(Value::Unit);
                }
                let mut vals = Vec::with_capacity(arg_exprs.len());
                for a in arg_exprs {
                    vals.push(self.eval_expr(a, env)?);
                }
                self.call(name, &vals, *span).map_err(Signal::Err)
            }
            Expr::If { cond, then_block, else_block, span } => {
                let c = self.eval_expr(cond, env)?;
                let take_then = match c {
                    Value::Bool(b) => b,
                    v => return Err(mismatch("bool", v.ty_name(), *span)),
                };
                if take_then {
                    self.exec_block(then_block, env)
                } else {
                    match else_block {
                        Some(branch) => match branch.as_ref() {
                            ElseBranch::Block(b) => self.exec_block(b, env),
                            ElseBranch::If(e) => self.eval_expr(e, env),
                        },
                        None => Ok(Value::Unit),
                    }
                }
            }
            Expr::Assign(name, rhs, span) => {
                let v = self.eval_expr(rhs, env)?;
                let ty = env.get_ty(name).ok_or_else(|| {
                    Signal::Err(RuntimeError { kind: ErrorKind::UnknownVar(name.clone()), span: *span })
                })?;
                self.check_ty(&v, &ty, *span)?;
                // `get_ty` above already proved the binding exists, so the
                // only way `set` fails is a logic error in this file, not
                // a user-reachable state — hence the `expect`.
                env.set(name, v.clone()).expect("checked above: binding exists");
                Ok(v)
            }
            Expr::Box(inner, _span) => {
                let v = self.eval_expr(inner, env)?;
                Ok(Value::Boxed(Box::new(v)))
            }
            Expr::Deref(inner, span) => {
                let v = self.eval_expr(inner, env)?;
                match v {
                    // Reading through the box copies the scalar out; the
                    // box itself is untouched (`ownership.rs` guarantees
                    // this doesn't need to move it — every inner type here
                    // is a scalar, hence freely copyable once read out).
                    Value::Boxed(inner) => Ok(*inner),
                    v => Err(mismatch("box", v.ty_name(), *span)),
                }
            }
        }
    }

    fn eval_binary(
        &self,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        env: &mut Env,
        span: Span,
    ) -> SResult<Value> {
        // Short-circuit && / || before evaluating the right side at all —
        // required for the operators to mean what their symbols claim.
        if op == BinOp::And || op == BinOp::Or {
            let l = self.eval_expr(lhs, env)?;
            let lb = match l {
                Value::Bool(b) => b,
                v => return Err(mismatch("bool", v.ty_name(), span)),
            };
            if op == BinOp::And && !lb {
                return Ok(Value::Bool(false));
            }
            if op == BinOp::Or && lb {
                return Ok(Value::Bool(true));
            }
            let r = self.eval_expr(rhs, env)?;
            return match r {
                Value::Bool(b) => Ok(Value::Bool(b)),
                v => Err(mismatch("bool", v.ty_name(), span)),
            };
        }

        let l = self.eval_expr(lhs, env)?;
        let r = self.eval_expr(rhs, env)?;
        let result: Result<Value, RuntimeError> = match (l, r) {
            (Value::Int(a), Value::Int(b)) => match op {
                BinOp::Add => Ok(Value::Int(a + b)),
                BinOp::Sub => Ok(Value::Int(a - b)),
                BinOp::Mul => Ok(Value::Int(a * b)),
                BinOp::Div => {
                    if b == 0 {
                        err(ErrorKind::DivByZero, span)
                    } else {
                        Ok(Value::Int(a / b))
                    }
                }
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                BinOp::Lt => Ok(Value::Bool(a < b)),
                BinOp::Gt => Ok(Value::Bool(a > b)),
                BinOp::LtEq => Ok(Value::Bool(a <= b)),
                BinOp::GtEq => Ok(Value::Bool(a >= b)),
                BinOp::And | BinOp::Or => unreachable!("handled above"),
            },
            (Value::Bool(a), Value::Bool(b)) => match op {
                BinOp::Eq => Ok(Value::Bool(a == b)),
                BinOp::NotEq => Ok(Value::Bool(a != b)),
                _ => err(
                    ErrorKind::TypeMismatch { expected: "int".to_string(), found: "bool".to_string() },
                    span,
                ),
            },
            (l, r) => err(
                ErrorKind::TypeMismatch {
                    expected: l.ty_name().to_string(),
                    found: r.ty_name().to_string(),
                },
                span,
            ),
        };
        result.map_err(Signal::Err)
    }
}
