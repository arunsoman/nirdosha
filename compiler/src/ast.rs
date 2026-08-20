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
    /// A shared, read-only borrow — `&i64`, `&box i64`. Unlike `Ty::Box`,
    /// this is **not** affine: unlimited simultaneous `&T`s are always
    /// sound (many readers, no writers), the same as Rust. There is no
    /// `&mut` yet — exclusive/mutable borrows need real liveness tracking
    /// to enforce "aliasing xor mutability" and are deliberately deferred;
    /// see `ownership.rs`'s module doc.
    Ref(Box<Ty>),
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
            Ty::Ref(inner) => format!("&{}", inner.name()),
            Ty::Error => "<error>".to_string(),
        }
    }

    pub fn is_integer(&self) -> bool {
        !matches!(self, Ty::Bool | Ty::Unit | Ty::Error | Ty::Box(_) | Ty::Ref(_))
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
        // `Ty::Ref` is deliberately excluded: a shared borrow is always
        // freely copyable (unlimited simultaneous readers is sound), even
        // though it can point at affine content. That's what actually
        // makes borrowing useful — see `Ty::Ref`'s doc comment.
        matches!(self, Ty::Box(_))
    }

    /// goal.md row 4's runtime fallback (Tier 2, §4): `interpreter.rs`
    /// checks every `let`/assign/return/call boundary against this. Two
    /// *static* passes now also exist and prove some of these checks
    /// unnecessary ahead of time (Tier 1) — `refine.rs` (interval
    /// analysis) and `smt.rs` (real Z3, where available) — but this
    /// runtime check stays regardless of what either proves, since
    /// there's no backend yet to spend a Tier-1 proof's performance
    /// payoff on removing it. See both modules' doc comments.
    pub fn in_range(&self, v: i64) -> bool {
        if !self.is_integer() {
            return false;
        }
        let (lo, hi) = self.bounds();
        (lo..=hi).contains(&v)
    }

    /// A type's legal values as bare `(min, max)` bounds — the same
    /// information `in_range` uses, exposed directly for `refine.rs` and
    /// `smt.rs`, both of which need the endpoints themselves (to build an
    /// `Interval` or a pair of SMT assertions), not just a yes/no
    /// predicate. Defined once, here, so the two static passes and the
    /// runtime check can never silently disagree about what a type's
    /// range actually is — that kind of three-way drift is exactly the
    /// class of bug that would be invisible until it wasn't.
    pub fn bounds(&self) -> (i64, i64) {
        match self {
            Ty::I8 => (i8::MIN as i64, i8::MAX as i64),
            Ty::I16 => (i16::MIN as i64, i16::MAX as i64),
            Ty::I32 => (i32::MIN as i64, i32::MAX as i64),
            Ty::I64 => (i64::MIN, i64::MAX),
            Ty::U8 => (0, u8::MAX as i64),
            Ty::U16 => (0, u16::MAX as i64),
            Ty::U32 => (0, u32::MAX as i64),
            // u64's true max doesn't fit in i64 — every value this
            // language can actually hold is backed by i64 anyway (see
            // interpreter.rs's `Value::Int(i64)`), so `i64::MAX` is the
            // real ceiling regardless of the declared type.
            Ty::U64 | Ty::Usize => (0, i64::MAX),
            // Never legitimately queried (`is_integer()` is false for
            // all of these, and `in_range` above already short-circuits
            // on that) — full range is the harmless, safe default if
            // something calls this directly anyway.
            Ty::Bool | Ty::Unit | Ty::Box(_) | Ty::Ref(_) | Ty::Error => (i64::MIN, i64::MAX),
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
    /// `*expr` — read the value out of a box or through a reference. Does
    /// **not** move the box/reference itself when what comes out is
    /// freely copyable; see `ownership.rs`'s doc comment for the type-
    /// directed rule this actually needs (it's more subtle than "derefs
    /// never move" once nested boxes and references both exist).
    Deref(Box<Expr>, Span),
    /// `&expr` — borrow a binding without taking ownership of it. The
    /// operand is restricted to a plain `Expr::Ident` — parsed and
    /// enforced in `parser.rs`, the same way `Expr::Assign`'s left side
    /// is — because "borrow an arbitrary temporary expression" drags in
    /// temporary-lifetime rules this language doesn't have a story for
    /// yet.
    Ref(Box<Expr>, Span),
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
            | Expr::Deref(_, s)
            | Expr::Ref(_, s) => *s,
        }
    }
}

#[derive(Debug, Clone)]
pub struct Program {
    pub fns: Vec<FnDecl>,
}

/// Returns `Some(n)` if `e` is, syntactically, an integer literal or a
/// unary-negated one (`-5`) — including through the transparent
/// parenthesization the parser already collapses away. Anything else,
/// including a variable that merely *holds* a literal-looking value, is
/// `None`: literal flexibility is a syntactic property of the expression,
/// not a value-flow analysis.
///
/// Shared, not duplicated per-module: `typeck.rs` uses this to decide
/// which expressions get flexible-width treatment against a declared
/// target type (goal.md §3's "no implicit conversions" rule, with
/// literals as the deliberate exception), and `codegen.rs` has to agree
/// with that decision *exactly* — a literal typeck allowed to fit a
/// narrower type has to be emitted by codegen at that same narrower
/// width, not a second, independently-written recognizer that might
/// drift out of sync with what typeck actually decided.
pub fn literal_value(e: &Expr) -> Option<i64> {
    match e {
        Expr::Int(n, _) => Some(*n),
        Expr::Unary(UnOp::Neg, inner, _) => literal_value(inner).map(|n| -n),
        _ => None,
    }
}
