//! The AST. Each node type is a fixed, closed set — deliberately, since row 8
//! (goal.md) asks for a semantics where the meaning of a compound expression
//! is a function only of the meanings of its parts (`⟦compose(a,b)⟧ =
//! F(⟦a⟧,⟦b⟧)`), never a special case. Concretely: `interpreter.rs` has
//! exactly one `eval_expr` match arm per `Expr` variant here, and no variant
//! reaches back into a sibling's internals — composition, not entanglement.

use crate::token::Span;

/// Not `Copy` — deliberately. `Ty::Box` owns a nested `Ty`, and giving the
/// whole enum a free bitwise-copy would say every type is trivially
/// duplicable, which is exactly the property `Ty::Box` exists to *not*
/// have (see `is_affine`, and the real move-checker in `ownership.rs`).
/// `Clone` stays cheap for everything else since only `Box` recurses.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ty {
    I8,
    I16,
    I32,
    I64,
    U8,
    U16,
    U32,
    U64,
    Usize,
    Bool,
    Unit,
    /// A heap-allocated single-value cell — `box i64`, `box bool`, even
    /// `box box i64`. The minimal vehicle for goal.md row 1 (no GC, no
    /// manual `free()`): every other type in this language is freely
    /// copyable, so there was nothing for ownership to say anything about
    /// until this existed. See `ownership.rs` for what actually enforces
    /// single ownership; this type just makes it meaningful to ask.
    Box(Box<Ty>),
    /// Poison type: stands in for an expression that already produced a
    /// type error, so the checker (`typeck.rs`) doesn't cascade a dozen
    /// follow-on diagnostics from one root cause. Never produced by the
    /// parser or lexer — source code has no way to spell this — and never
    /// reaches the interpreter, since `run()` refuses to execute a program
    /// that failed type checking.
    Error,
}

impl Ty {
    pub fn from_name(name: &str) -> Option<Ty> {
        Some(match name {
            "i8" => Ty::I8,
            "i16" => Ty::I16,
            "i32" => Ty::I32,
            "i64" => Ty::I64,
            "u8" => Ty::U8,
            "u16" => Ty::U16,
            "u32" => Ty::U32,
            "u64" => Ty::U64,
            "usize" => Ty::Usize,
            "bool" => Ty::Bool,
            "unit" => Ty::Unit,
            _ => return None,
        })
    }

    /// Owned, not `&'static str`, because `Ty::Box` has to render its
    /// inner type recursively (`"box i64"`, `"box box bool"`, ...).
    pub fn name(&self) -> String {
        match self {
            Ty::I8 => "i8".to_string(),
            Ty::I16 => "i16".to_string(),
            Ty::I32 => "i32".to_string(),
            Ty::I64 => "i64".to_string(),
            Ty::U8 => "u8".to_string(),
            Ty::U16 => "u16".to_string(),
            Ty::U32 => "u32".to_string(),
            Ty::U64 => "u64".to_string(),
            Ty::Usize => "usize".to_string(),
            Ty::Bool => "bool".to_string(),
            Ty::Unit => "unit".to_string(),
            Ty::Box(inner) => format!("box {}", inner.name()),
            Ty::Error => "<error>".to_string(),
        }
    }

    pub fn is_integer(&self) -> bool {
        !matches!(self, Ty::Bool | Ty::Unit | Ty::Error | Ty::Box(_))
    }

    /// The property that makes ownership meaningful: an affine value has
    /// exactly one owner at a time, and using it (other than reading
    /// *through* it — see `Expr::Deref`) transfers that ownership rather
    /// than copying it. Every scalar type here is the opposite —
    /// unrestricted, Rust's `Copy` — because duplicating an `i32` has no
    /// observable cost or hazard; duplicating a `box i32` would mean two
    /// owners believing they alone are responsible for freeing the same
    /// allocation, which is exactly the class of bug row 1 exists to rule
    /// out statically.
    pub fn is_affine(&self) -> bool {
        matches!(self, Ty::Box(_))
    }

    /// Placeholder for goal.md row 4 (refinement types, Phase 2). Real bounds
    /// eventually get SMT-discharged at compile time (Tier 1) or demand a
    /// checked op at the call site (Tier 2) — see goal.md §4. Until Phase 2
    /// exists, this is the honest stand-in: a *runtime* bounds check, so
    /// overflow is caught (never silently wraps) but not yet proved absent.
    pub fn in_range(&self, v: i64) -> bool {
        match self {
            Ty::I8 => (i8::MIN as i64..=i8::MAX as i64).contains(&v),
            Ty::I16 => (i16::MIN as i64..=i16::MAX as i64).contains(&v),
            Ty::I32 => (i32::MIN as i64..=i32::MAX as i64).contains(&v),
            Ty::I64 => true,
            Ty::U8 => (0..=u8::MAX as i64).contains(&v),
            Ty::U16 => (0..=u16::MAX as i64).contains(&v),
            Ty::U32 => (0..=u32::MAX as i64).contains(&v),
            Ty::U64 | Ty::Usize => v >= 0,
            Ty::Bool | Ty::Unit | Ty::Error | Ty::Box(_) => false,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Param {
    pub name: String,
    pub ty: Ty,
}

#[derive(Debug, Clone)]
pub struct FnDecl {
    pub name: String,
    pub params: Vec<Param>,
    pub ret: Ty,
    pub body: Block,
    pub span: Span,
}

#[derive(Debug, Clone)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

#[derive(Debug, Clone)]
pub enum Stmt {
    Let { name: String, ty: Ty, value: Expr, span: Span },
    Return { value: Option<Expr>, span: Span },
    While { cond: Expr, body: Block, span: Span },
    Expr(Expr),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Eq,
    NotEq,
    Lt,
    Gt,
    LtEq,
    GtEq,
    And,
    Or,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

#[derive(Debug, Clone)]
pub enum Expr {
    Int(i64, Span),
    Bool(bool, Span),
    Ident(String, Span),
    Unary(UnOp, Box<Expr>, Span),
    Binary(BinOp, Box<Expr>, Box<Expr>, Span),
    Call(String, Vec<Expr>, Span),
    If { cond: Box<Expr>, then_block: Block, else_block: Option<Box<ElseBranch>>, span: Span },
    /// `x = expr` — reassigns an existing binding. Ownership-checked as of
    /// `ownership.rs`: reassigning clears the LHS's moved status (it's
    /// holding a fresh value now); the RHS, if it names an affine binding
    /// directly, is a move of that binding, same as any other value use.
    Assign(String, Box<Expr>, Span),
    /// `box expr` — heap-allocate `expr`'s value. The only expression form
    /// that produces an affine (`Ty::Box`) value.
    Box(Box<Expr>, Span),
    /// `*expr` — read the value out of a box. Does **not** move the box:
    /// see `ownership.rs`'s doc comment for why a deref-read is exempt
    /// from move-checking in this language (every boxable inner type here
    /// is currently a scalar, i.e. freely copyable once read out).
    Deref(Box<Expr>, Span),
}

#[derive(Debug, Clone)]
pub enum ElseBranch {
    Block(Block),
    If(Expr),
}

impl Expr {
    pub fn span(&self) -> Span {
        match self {
            Expr::Int(_, s)
            | Expr::Bool(_, s)
            | Expr::Ident(_, s)
            | Expr::Unary(_, _, s)
            | Expr::Binary(_, _, _, s)
            | Expr::Call(_, _, s)
            | Expr::If { span: s, .. }
            | Expr::Assign(_, _, s)
            | Expr::Box(_, s)
            | Expr::Deref(_, s) => *s,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Program {
    pub fns: Vec<FnDecl>,
}
