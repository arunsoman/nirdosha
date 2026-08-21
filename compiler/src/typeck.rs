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

use serde::{Deserialize, Serialize};

use crate::ast::*;
use crate::token::Span;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TypeErrorKind {
    UnknownVar(String),
    UnknownFn(String),
    DuplicateFn(String),
    /// A user `fn` declared with the same name as a builtin
    /// (`ast::is_builtin`) — every *call* to that name always resolves
    /// to the builtin (`infer_call` checks `is_builtin` before ever
    /// consulting `sigs`), so a same-named user function would be
    /// silently uncallable dead code without this check. Caught at
    /// declaration time, not left to surface as a confusing builtin-
    /// shaped type error at the first call site.
    FnNameShadowsBuiltin(String),
    NoMainFn,
    MainMustTakeNoParams,
    ArityMismatch { fn_name: String, want: usize, got: usize },
    TypeMismatch { expected: Ty, found: Ty },
    ExpectedBool { found: Ty },
    ExpectedNumeric { found: Ty },
    ExpectedBoxType { found: Ty },
    CannotMoveOutOfReference { content: Ty },
    ExpectedThreadType { found: Ty },
    CannotSpawnBuiltin { name: String },
    /// `chan` (the channel-creating expression) appeared somewhere its
    /// payload type can't be pinned down — either with no expected type at
    /// all (`infer`), or against an expected type that isn't `chan T`
    /// (`check`, which reports a `TypeMismatch` instead in that second
    /// case, so this variant is really just the first one).
    ChannelNeedsExplicitType,
    ExpectedChannelType { found: Ty },
    /// `accept(listener)` needs a `Ty::TcpListener`.
    ExpectedTcpListenerType { found: Ty },
    /// `sandbox name(...)` requires `name` to declare `-> unit` — there's
    /// `name`'s own return value has no path back to the caller — its
    /// declared return type must be `unit`. As of SANDBOXING.md's layer
    /// 2, this doesn't mean "no way to get a result back at all": a
    /// `chan T` argument gives a sandboxed function a real, live
    /// communication channel with the caller (see `is_sandbox_safe`);
    /// what's still missing (layer 3) is a serialization story general
    /// enough to carry an arbitrary *return value* across automatically.
    SandboxFnMustReturnUnit { name: String },
    /// `sandbox name(...)` requires every one of `name`'s declared
    /// parameters to satisfy `is_sandbox_safe`: a plain scalar (an
    /// integer type or `bool`), or `chan T` where `T` is one — see that
    /// function's doc comment for why nothing else qualifies yet.
    SandboxArgMustBeScalar { found: Ty },
    ExpectedSandboxType { found: Ty },
    LiteralOutOfRange { ty: Ty, value: i64 },
    IfWithoutElseUsedAsValue { expected: Ty },
    NotAllPathsReturn { fn_name: String },
    /// `Expr::Index` appeared somewhere, but no indexable `Ty` exists yet
    /// — `Vector`/`Matrix` land in a later phase (see `Expr::Index`'s doc
    /// comment in ast.rs). Every occurrence is a static rejection until
    /// then, by construction: this variant has no "found an indexable
    /// type but the index was wrong" companion yet, because there is no
    /// indexable type to have gotten right.
    NotIndexable { found: Ty },
    /// `v[...]` or `m[...]` with the wrong number of index expressions —
    /// a `Vector` needs exactly one, a `Matrix` exactly two.
    WrongIndexArity { expected: usize, found: usize },
    /// `*`'s inner-dimension check for `Matrix * Vector`/`Matrix *
    /// Matrix` — unlike every other `TypeMismatch` in this file, the two
    /// operand types are legitimately *supposed* to differ (that's the
    /// whole point of a rectangular matrix product), so a plain
    /// `expected == found` framing doesn't fit; this carries both full
    /// shapes so `Display` can name exactly which dimensions disagree.
    ShapeMismatch { left: Ty, right: Ty },
    /// `Vector * Vector` specifically — a type error with its own
    /// message (not a generic mismatch) because there's a specific,
    /// better-typed alternative to point at: `dot()` (Phase 2) or an
    /// explicit transpose, the same way Julia requires one of those
    /// instead of overloading `*` to guess which the caller meant
    /// (inner product vs. outer product are both "vector times vector"
    /// and genuinely ambiguous without one).
    VectorTimesVectorNotSupported,
    /// A `[...]` literal whose first element's own type is already a
    /// `Matrix`, or a `Vector` of a `Vector`/`Matrix` — i.e. the literal
    /// would need three or more levels of nesting to type. Out of scope
    /// on purpose (see the unified plan's §5): `Vector`/`Matrix` are
    /// flat 1-D/2-D shapes only, no general tensor nesting.
    ArrayLiteralTooDeep { found: Ty },
    /// `trace`/`det`/`inv`/`solve`/`is_symmetric`/`is_diag` all require a
    /// square `Matrix` — the one shape failure explicitly named in the
    /// unified plan's §4.2.1.
    NotSquare { found: Ty },
    /// A dense linear algebra builtin's argument didn't fit its specific
    /// requirement (e.g. `det` needs `Matrix(f64, n, n)`, `cross` needs
    /// `Vector(_, 3)` exactly) in a way none of the more specific
    /// variants above already name. Carries the builtin's name and a
    /// short description of what was expected, not just a bare found
    /// type, since these requirements are genuinely per-builtin — still
    /// structured (an agent can match on `builtin`/`expected`/`found`
    /// independently), just not one dedicated variant per builtin, which
    /// would be a lot of near-identical enum cases for the same shape of
    /// problem.
    WrongBuiltinArgType { builtin: String, expected: String, found: Ty },
    /// `zeros`/`ones`/`identity`'s dimension argument(s) must be a plain
    /// integer literal (`zeros(3)`), not an arbitrary expression —
    /// "Sized by Default" (§2) means the *result's* shape has to be
    /// known at typecheck time, and this language has no general
    /// constant-folding to derive it from anything less direct than a
    /// literal.
    ExpectedLiteralDimension { builtin: String },
    /// `audited "<justification>" { ... }` with an empty (or
    /// whitespace-only) justification — the compiler's whole enforcement
    /// role for Tier-3 escape hatches (unified plan §4.3.4): syntax and
    /// non-emptiness only, not judging the justification's content.
    EmptyAuditedJustification,
    /// `validate_fragment`'s input wasn't valid JSON, or was valid JSON
    /// that doesn't deserialize into `Expr` — the fragment-validation
    /// entry point's own failure mode, distinct from every type error
    /// above (which all assume a well-formed `Expr` already exists).
    MalformedFragmentJson { message: String },
    /// `transact`'s `verify` slot must return `bool` — it's checked like
    /// an `if` condition to decide `commit` vs. `compensate`
    /// (`TRANSACT.md`), so anything else is exactly as wrong as a
    /// non-`bool` `if` condition, just for this one named position.
    TransactVerifyMustReturnBool { found: Ty },
    /// A `transact` slot named a builtin instead of a user-defined
    /// function — see `infer_transact_slot`'s doc comment for why every
    /// slot is restricted to a user function (mirrors
    /// `CannotSpawnBuiltin`'s identical restriction and identical
    /// underlying reason: no declared-signature table exists for
    /// builtins to look a return type up from).
    CannotUseBuiltinInTransact { name: String },
    /// This function's declared `effect(...)` annotation (`ast::FnDecl::
    /// declared_effects`) didn't list `missing` — but `effects::
    /// infer_effects` found it in the body anyway (directly, or
    /// transitively through a call). A declared effect the body never
    /// uses is not an error (`goal.md` §3's effect-subsumption
    /// generosity); this is the one direction that's checked.
    EffectNotDeclared { fn_name: String, missing: Effect },
    /// A `struct`/`enum` name collides with another struct/enum's name —
    /// checked at declaration time, the same "caught at declaration time,
    /// not left to surface as a confusing error at first use" discipline
    /// `FnNameShadowsBuiltin` already follows.
    DuplicateType(String),
    /// A struct's own name (as its constructor) or an enum variant's name
    /// collides with a function name, a builtin, or another constructor —
    /// every constructor lives in one flat callable namespace
    /// (`nirdosha_row11_amendment.md` §3.2).
    DuplicateConstructor(String),
    /// Two fields of the same `struct` share a name.
    DuplicateField { struct_name: String, field: String },
    /// A bare `ident` in type position (a `let`/param/return/field/
    /// payload type) didn't resolve to any declared `struct`/`enum`.
    UnknownType(String),
    /// `expr.field` where `expr`'s type isn't a declared `struct`.
    NotAStruct { found: Ty },
    /// `expr.field` on a real struct type, but `field` isn't one of its
    /// declared fields.
    NoSuchField { struct_name: String, field: String },
    /// A struct/variant constructor call (`Point(1.0, 2.0)`, `Some(5)`)
    /// with the wrong number of positional arguments — the constructor
    /// analogue of `ArityMismatch`.
    ConstructorArityMismatch { name: String, want: usize, got: usize },
    /// `match`'s scrutinee isn't a declared `enum`.
    NotAnEnum { found: Ty },
    /// A `match` arm's head identifier doesn't name any variant of the
    /// scrutinee's specific enum.
    UnknownVariant { enum_name: String, variant: String },
    /// A `match` arm bound the wrong number of names for its variant's
    /// payload arity.
    WrongVariantArity { variant: String, want: usize, got: usize },
    /// The same variant's name appeared as more than one arm's head.
    DuplicateMatchArm { variant: String },
    /// Not every variant of the scrutinee's enum was covered — v1 has no
    /// wildcard/binding-only catch-all pattern
    /// (`nirdosha_row11_amendment.md` §3.4), so exhaustiveness means
    /// "every declared variant, exactly once."
    NonExhaustiveMatch { enum_name: String, missing: Vec<String> },
    /// A `struct`/`enum` declares the same type-parameter name twice
    /// (`struct Pair(A, A) { .. }`) — layer 6, generics.
    DuplicateTypeParam(String),
    /// A `Ty::Named` use — a `let`/param/return/field/payload annotation,
    /// or a generic type applied to arguments in source — supplied the
    /// wrong number of type arguments for what `name` actually declares
    /// (`want` type parameters, `got` arguments supplied). Also used for
    /// the (nonsensical) case of applying arguments to a bare reference
    /// to the *enclosing* declaration's own type parameter, where `want`
    /// is always `0`.
    WrongTypeArity { name: String, want: usize, got: usize },
    /// A generic struct/enum constructor call (`Pair(1, "one")`, `Some(5)`)
    /// appeared somewhere its type arguments can't be pinned down —
    /// neither from an expected type at the call site (`check`, which
    /// substitutes directly and never reaches this) nor from the
    /// constructor's own arguments (`nirdosha_row11_amendment.md` has no
    /// turbofish-style explicit-type-argument syntax at a call site at
    /// all — §3.1's "Nirdosha never uses `<...>` for type application" —
    /// so there is no third way to supply one). The same shape of gap
    /// `chan`'s own `ChannelNeedsExplicitType` already has, generalized
    /// from "no expected type at all" to "no expected type *and* the
    /// arguments alone don't determine every parameter."
    GenericConstructorNeedsExplicitType { name: String },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
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
            TypeErrorKind::FnNameShadowsBuiltin(n) => write!(
                f,
                "{line}:{col}: `{n}` is a builtin name and cannot be used as a function name \
                 (every call to `{n}` would resolve to the builtin, not this function)"
            ),
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
            TypeErrorKind::ExpectedThreadType { found } => write!(
                f,
                "{line}:{col}: `join` needs a `thread` type, found `{}`",
                found.name()
            ),
            TypeErrorKind::CannotSpawnBuiltin { name } => {
                write!(f, "{line}:{col}: `{name}` is a builtin and can't be spawned")
            }
            TypeErrorKind::ChannelNeedsExplicitType => write!(
                f,
                "{line}:{col}: `chan` needs an explicit `chan T` type annotation \
                 (e.g. `let c: chan i64 = chan`)"
            ),
            TypeErrorKind::ExpectedChannelType { found } => write!(
                f,
                "{line}:{col}: `send`/`recv` need a `chan` or `tcp` type, found `{}`",
                found.name()
            ),
            TypeErrorKind::ExpectedTcpListenerType { found } => write!(
                f,
                "{line}:{col}: `accept` needs a `tcp_listener` type, found `{}`",
                found.name()
            ),
            TypeErrorKind::SandboxFnMustReturnUnit { name } => write!(
                f,
                "{line}:{col}: `{name}` must return `unit` to be run with `sandbox` \
                 (its own return value has no way back to the caller -- send a result over \
                 a `chan` argument instead, or use `stop`'s exit code; see SANDBOXING.md)"
            ),
            TypeErrorKind::SandboxArgMustBeScalar { found } => write!(
                f,
                "{line}:{col}: `sandbox` functions can only take plain scalar parameters \
                 (an integer type or `bool`) or a `chan` of one, found `{}`",
                found.name()
            ),
            TypeErrorKind::ExpectedSandboxType { found } => write!(
                f,
                "{line}:{col}: `stop` needs a `sandbox` or `tcp` type, found `{}`",
                found.name()
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
            TypeErrorKind::NotIndexable { found } => {
                write!(f, "{line}:{col}: `{}` cannot be indexed", found.name())
            }
            TypeErrorKind::WrongIndexArity { expected, found } => write!(
                f,
                "{line}:{col}: expected {expected} index expression(s), found {found}"
            ),
            TypeErrorKind::ShapeMismatch { left, right } => write!(
                f,
                "{line}:{col}: shape mismatch: `{}` and `{}` have incompatible inner dimensions",
                left.name(),
                right.name()
            ),
            TypeErrorKind::VectorTimesVectorNotSupported => write!(
                f,
                "{line}:{col}: `Vector * Vector` is not supported -- use `dot()` (Phase 2) or an \
                 explicit transpose instead"
            ),
            TypeErrorKind::ArrayLiteralTooDeep { found } => write!(
                f,
                "{line}:{col}: array literal nested too deeply (found `{}`) -- only flat vector \
                 (`[..]`) and matrix (`[[..], ..]`) literals are supported",
                found.name()
            ),
            TypeErrorKind::NotSquare { found } => {
                write!(f, "{line}:{col}: expected a square Matrix, found `{}`", found.name())
            }
            TypeErrorKind::WrongBuiltinArgType { builtin, expected, found } => write!(
                f,
                "{line}:{col}: `{builtin}` expects {expected}, found `{}`",
                found.name()
            ),
            TypeErrorKind::ExpectedLiteralDimension { builtin } => write!(
                f,
                "{line}:{col}: `{builtin}`'s dimension argument(s) must be a literal integer"
            ),
            TypeErrorKind::MalformedFragmentJson { message } => {
                write!(f, "{line}:{col}: malformed fragment JSON: {message}")
            }
            TypeErrorKind::EmptyAuditedJustification => write!(
                f,
                "{line}:{col}: `audited` requires a non-empty justification string"
            ),
            TypeErrorKind::TransactVerifyMustReturnBool { found } => write!(
                f,
                "{line}:{col}: `transact`'s `verify` slot must return `bool`, found `{}`",
                found.name()
            ),
            TypeErrorKind::CannotUseBuiltinInTransact { name } => write!(
                f,
                "{line}:{col}: `{name}` is a builtin and can't be used as a `transact` slot"
            ),
            TypeErrorKind::EffectNotDeclared { fn_name, missing } => write!(
                f,
                "{line}:{col}: `{fn_name}` performs effect `{}` but its `effect(...)` annotation doesn't declare it",
                missing.name()
            ),
            TypeErrorKind::DuplicateType(n) => {
                write!(f, "{line}:{col}: `{n}` is declared as a struct/enum more than once")
            }
            TypeErrorKind::DuplicateConstructor(n) => write!(
                f,
                "{line}:{col}: `{n}` is already used as a function/builtin/constructor name \
                 (struct constructors and enum variants share one namespace with functions)"
            ),
            TypeErrorKind::DuplicateField { struct_name, field } => write!(
                f,
                "{line}:{col}: `{struct_name}` declares field `{field}` more than once"
            ),
            TypeErrorKind::UnknownType(n) => write!(f, "{line}:{col}: unknown type `{n}`"),
            TypeErrorKind::NotAStruct { found } => write!(
                f,
                "{line}:{col}: `.` needs a struct type, found `{}`",
                found.name()
            ),
            TypeErrorKind::NoSuchField { struct_name, field } => write!(
                f,
                "{line}:{col}: `{struct_name}` has no field `{field}`"
            ),
            TypeErrorKind::ConstructorArityMismatch { name, want, got } => write!(
                f,
                "{line}:{col}: `{name}` expects {want} field(s)/payload value(s), got {got}"
            ),
            TypeErrorKind::NotAnEnum { found } => write!(
                f,
                "{line}:{col}: `match` needs an enum type, found `{}`",
                found.name()
            ),
            TypeErrorKind::UnknownVariant { enum_name, variant } => write!(
                f,
                "{line}:{col}: `{variant}` is not a variant of `{enum_name}`"
            ),
            TypeErrorKind::WrongVariantArity { variant, want, got } => write!(
                f,
                "{line}:{col}: `{variant}` binds {want} value(s), found {got} binding(s)"
            ),
            TypeErrorKind::DuplicateMatchArm { variant } => write!(
                f,
                "{line}:{col}: `{variant}` appears in more than one `match` arm"
            ),
            TypeErrorKind::NonExhaustiveMatch { enum_name, missing } => write!(
                f,
                "{line}:{col}: `match` on `{enum_name}` doesn't cover: {}",
                missing.join(", ")
            ),
            TypeErrorKind::DuplicateTypeParam(p) => {
                write!(f, "{line}:{col}: type parameter `{p}` is declared more than once")
            }
            TypeErrorKind::WrongTypeArity { name, want, got } => write!(
                f,
                "{line}:{col}: `{name}` expects {want} type argument(s), got {got}"
            ),
            TypeErrorKind::GenericConstructorNeedsExplicitType { name } => write!(
                f,
                "{line}:{col}: `{name}`'s type argument(s) can't be inferred here — \
                 an expected type is needed (e.g. an explicit `let` annotation)"
            ),
        }
    }
}

/// How a `match` expression's result is used — sharper than `check_if`'s
/// plain `Option<&Ty>`, because `match`'s three real use sites need three
/// different treatments, not two: a bare statement's arms don't need to
/// agree with each other *at all* (`check_stmt_expr`), a value position
/// with a known expected type checks every arm against it
/// (`Checker::check`), and a value position with no expected type yet
/// (`Checker::infer` — e.g. a `match` used as a match's own scrutinee, or
/// a binary operand) has to infer a coherent type by requiring every arm
/// to agree with each other, the same "then/else must agree" rule
/// `check_if`'s own `(None, Some(else_ty))` arm already applies. Reusing
/// `if`'s plain two-case `Option<&Ty>` here would conflate the first and
/// third of these (see `check_match`'s `MatchWant::Statement`/`::Infer`
/// arms for why they're genuinely different, not just spelled
/// differently) — a real bug caught by a bare-statement `match` test
/// whose second arm disagreed in type with its first.
#[derive(Clone, Copy)]
enum MatchWant<'t> {
    Statement,
    Check(&'t Ty),
    Infer,
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

pub struct Checker<'a> {
    sigs: HashMap<String, FnSig>,
    errors: Vec<TypeError>,
    /// Row 11's declaration table (`ast::TypeRegistry`) — built once, up
    /// front, from the same `&'a Program` this whole pass borrows.
    registry: TypeRegistry<'a>,
    /// Set during a throwaway structural-inference pass over a generic
    /// constructor's own arguments (`resolve_type_args`'s fallback path)
    /// — diagnostics found there are discarded; the *real* pass that
    /// immediately follows in the caller (this time non-silent) is what
    /// actually reports them. Mirrors `ownership.rs`'s identically-named,
    /// identically-purposed field.
    silent: bool,
}

/// Type-check a whole program. `Ok(())` means every function body is well
/// typed *and* proved to return on every path where its signature demands
/// a value — the interpreter should never be run on a program this
/// rejects.
pub fn typecheck(program: &Program) -> Result<(), Vec<TypeError>> {
    let registry = TypeRegistry::build(program);
    let mut c = Checker { sigs: HashMap::new(), errors: Vec::new(), registry, silent: false };

    // ---- Row 11: register struct/enum type names + their constructors --
    // Two independent namespaces, per `nirdosha_row11_amendment.md` §3.1-
    // 3.2: `type_names` (struct/enum names, used in type position) and
    // `callable_names` (struct names *as constructors*, enum variant
    // names, function names, builtin names — anything `Expr::Call` can
    // name). A struct's name lives in both; an enum's own name lives only
    // in the first (only its variants are callable).
    let mut type_names: HashMap<&str, Span> = HashMap::new();
    let mut callable_names: HashMap<&str, Span> = HashMap::new();

    for s in &program.structs {
        if type_names.insert(s.name.as_str(), s.span).is_some() {
            c.error(TypeErrorKind::DuplicateType(s.name.clone()), s.span);
        }
        if is_builtin(&s.name) || callable_names.contains_key(s.name.as_str()) {
            c.error(TypeErrorKind::DuplicateConstructor(s.name.clone()), s.span);
        } else {
            callable_names.insert(s.name.as_str(), s.span);
        }
        c.check_duplicate_type_params(&s.type_params, s.span);
        let mut field_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for field in &s.fields {
            if !field_names.insert(field.name.as_str()) {
                c.error(
                    TypeErrorKind::DuplicateField { struct_name: s.name.clone(), field: field.name.clone() },
                    s.span,
                );
            }
        }
    }
    for e in &program.enums {
        if type_names.insert(e.name.as_str(), e.span).is_some() {
            c.error(TypeErrorKind::DuplicateType(e.name.clone()), e.span);
        }
        c.check_duplicate_type_params(&e.type_params, e.span);
        let mut variant_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for v in &e.variants {
            if !variant_names.insert(v.name.as_str()) {
                c.error(TypeErrorKind::DuplicateConstructor(v.name.clone()), v.span);
                continue;
            }
            if is_builtin(&v.name) || callable_names.contains_key(v.name.as_str()) {
                c.error(TypeErrorKind::DuplicateConstructor(v.name.clone()), v.span);
            } else {
                callable_names.insert(v.name.as_str(), v.span);
            }
        }
    }

    // Every syntactically-declared type (struct fields, enum payloads) is
    // validated against the registry now, so a bogus `Ty::Named` never
    // silently reaches construction/field-access checking below — each
    // declaration's own `type_params` are in scope for its own fields/
    // payloads (layer 6, generics), nowhere else.
    for s in &program.structs {
        for field in &s.fields {
            c.validate_ty(&field.ty, s.span, &s.type_params);
        }
    }
    for e in &program.enums {
        for v in &e.variants {
            for t in &v.payload {
                c.validate_ty(t, v.span, &e.type_params);
            }
        }
    }

    for f in &program.fns {
        if is_builtin(&f.name) {
            c.error(TypeErrorKind::FnNameShadowsBuiltin(f.name.clone()), f.span);
            continue;
        }
        if c.sigs.contains_key(&f.name) {
            c.error(TypeErrorKind::DuplicateFn(f.name.clone()), f.span);
            continue;
        }
        if callable_names.contains_key(f.name.as_str()) {
            c.error(TypeErrorKind::DuplicateConstructor(f.name.clone()), f.span);
            continue;
        }
        // Functions have no type-parameter list of their own
        // (`nirdosha_row11_amendment.md` §2.2/§3.3 scopes generics to
        // struct/enum declarations only) — empty scope.
        for p in &f.params {
            c.validate_ty(&p.ty, f.span, &[]);
        }
        c.validate_ty(&f.ret, f.span, &[]);
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

    // Effect enforcement runs only over an otherwise-clean program —
    // `effects::infer_effects` assumes every binding's declared type
    // actually resolves (a `let x: file = ...` with an unknown builtin
    // on its RHS, say, would already be a different error above), so
    // there's nothing sound to check yet if that assumption doesn't hold.
    if c.errors.is_empty() {
        let inferred = crate::effects::infer_effects(program, &c.registry);
        for f in &program.fns {
            let Some(declared) = &f.declared_effects else { continue };
            let Some(fx) = inferred.get(&f.name) else { continue };
            for missing in fx.inferred.difference(declared) {
                c.error(TypeErrorKind::EffectNotDeclared { fn_name: f.name.clone(), missing: *missing }, f.span);
            }
        }
    }

    if c.errors.is_empty() {
        Ok(())
    } else {
        Err(c.errors)
    }
}

impl<'a> Checker<'a> {
    fn error(&mut self, kind: TypeErrorKind, span: Span) {
        if !self.silent {
            self.errors.push(TypeError { kind, span });
        }
    }

    fn check_duplicate_type_params(&mut self, type_params: &[String], span: Span) {
        let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for p in type_params {
            if !seen.insert(p.as_str()) {
                self.error(TypeErrorKind::DuplicateTypeParam(p.clone()), span);
            }
        }
    }

    /// Recursively checks every `Ty::Named` leaf inside `ty` resolves —
    /// either to one of `in_scope_params` (the enclosing struct/enum
    /// declaration's own type-parameter names, empty everywhere else —
    /// see `typecheck`'s call sites) or to a real declared struct/enum
    /// with a matching type-argument count — the one thing `expect_type`
    /// (`parser.rs`) can't itself verify (see `Ty::Named`'s doc comment:
    /// the parser has no declaration table). Called on every
    /// *syntactically declared* type (fn params/return, struct fields,
    /// enum payloads, `let` annotations) — never on an *inferred* type,
    /// which can only ever carry a `Ty::Named` this pass already proved
    /// real (see `infer_struct_construction`/`infer_variant_construction`).
    fn validate_ty(&mut self, ty: &Ty, span: Span, in_scope_params: &[String]) {
        match ty {
            Ty::Named(name, args) => {
                for a in args {
                    self.validate_ty(a, span, in_scope_params);
                }
                // A bare reference to the enclosing declaration's own type
                // parameter (`A` inside `struct Pair(A, B) { .. }`) is
                // never itself further applied to arguments — nothing in
                // this grammar can write "A(B)" as a *use* of a type
                // parameter, only as a declaration of one.
                if in_scope_params.iter().any(|p| p == name) {
                    if !args.is_empty() {
                        self.error(
                            TypeErrorKind::WrongTypeArity { name: name.clone(), want: 0, got: args.len() },
                            span,
                        );
                    }
                    return;
                }
                let want_arity = if let Some(p) = self.registry.struct_type_params(name) {
                    Some(p.len())
                } else {
                    self.registry.enum_type_params(name).map(|p| p.len())
                };
                match want_arity {
                    None => self.error(TypeErrorKind::UnknownType(name.clone()), span),
                    Some(want) if want != args.len() => {
                        self.error(
                            TypeErrorKind::WrongTypeArity { name: name.clone(), want, got: args.len() },
                            span,
                        );
                    }
                    Some(_) => {}
                }
            }
            Ty::Box(inner) | Ty::Ref(inner) | Ty::Thread(inner) | Ty::Channel(inner) => {
                self.validate_ty(inner, span, in_scope_params)
            }
            Ty::Vector(inner, _) | Ty::Matrix(inner, _, _) => self.validate_ty(inner, span, in_scope_params),
            _ => {}
        }
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
            Stmt::Let { name, ty, value, span } => {
                self.validate_ty(ty, *span, &[]);
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
            Stmt::Audited { justification, body, span } => {
                if justification.trim().is_empty() {
                    self.error(TypeErrorKind::EmptyAuditedJustification, *span);
                }
                scopes.push();
                self.check_stmts(body, expected_ret, scopes);
                scopes.pop();
            }
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
        } else if let Expr::Match { scrutinee, arms, span } = e {
            self.check_match(scrutinee, arms, *span, MatchWant::Statement, expected_ret, scopes);
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
        if let Expr::Match { scrutinee, arms, span } = e {
            self.check_match(scrutinee, arms, *span, MatchWant::Check(want), expected_ret, scopes);
            return;
        }
        // A call in value position gets `want` threaded down as its
        // `expected` type — the *only* place a generic struct/variant
        // constructor's type arguments can come from other than
        // structural inference (Row 11 layer 6: `resolve_type_args`'s
        // doc comment). Builtin/user-function calls ignore `expected`
        // entirely (their return type comes from their own signature
        // regardless), so this is a strict superset of what plain
        // `infer(e, ...)` would have done for every other `Expr::Call`.
        if let Expr::Call(name, args, span) = e {
            let found = self.infer_call(name, args, expected_ret, scopes, *span, Some(want));
            if found != Ty::Error && found != *want {
                self.error(TypeErrorKind::TypeMismatch { expected: want.clone(), found }, e.span());
            }
            return;
        }
        // `chan` has no sub-expression to infer a payload type from — it's
        // only well-typed against an expected `chan T`. Handled here,
        // top-down, the same reason `Expr::If`'s value-position case is
        // handled here rather than in `infer` below.
        if let Expr::Chan(span) = e {
            if matches!(want, Ty::Channel(_)) {
                return;
            }
            self.error(
                TypeErrorKind::TypeMismatch { expected: want.clone(), found: Ty::Channel(Box::new(Ty::Error)) },
                *span,
            );
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
            Expr::Float(_, _) => Ty::F64,
            Expr::Str(_, _) => Ty::Str,
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
                        if it != Ty::Error && !it.is_numeric() {
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
            Expr::Call(name, args, span) => self.infer_call(name, args, expected_ret, scopes, *span, None),
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
                        if self.registry.is_affine(&t) {
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
            Expr::Spawn(name, args, span) => self.infer_spawn(name, args, expected_ret, scopes, *span),
            Expr::Transact { network, verify, commit, compensate, log, .. } => self.infer_transact(
                network,
                verify,
                commit,
                compensate.as_ref(),
                log.as_ref(),
                expected_ret,
                scopes,
            ),
            Expr::Join(inner, span) => {
                let it = self.infer(inner, expected_ret, scopes);
                match it {
                    Ty::Error => Ty::Error,
                    Ty::Thread(t) => *t,
                    other => {
                        self.error(TypeErrorKind::ExpectedThreadType { found: other }, *span);
                        Ty::Error
                    }
                }
            }
            // Reached only with no expected type at all (`check`, above,
            // intercepts the case where an expected `chan T` type *is*
            // known) — e.g. a bare `chan` statement, or `print(chan)`.
            Expr::Chan(span) => {
                self.error(TypeErrorKind::ChannelNeedsExplicitType, *span);
                Ty::Error
            }
            Expr::Send(chan, value, span) => {
                let ct = self.infer(chan, expected_ret, scopes);
                match ct {
                    Ty::Error => {
                        self.infer(value, expected_ret, scopes);
                        Ty::Error
                    }
                    Ty::Channel(inner) => {
                        self.check(value, &inner, expected_ret, scopes);
                        Ty::Unit
                    }
                    // `send`/`recv` double as a `tcp` connection's I/O —
                    // same keywords, reused rather than duplicated, the
                    // same way `stop` is. A TCP payload is always `str`
                    // (see `Ty::Tcp`'s doc comment): there's no per-
                    // connection payload type to check against the way a
                    // `chan T`'s `T` gives one.
                    Ty::Tcp => {
                        self.check(value, &Ty::Str, expected_ret, scopes);
                        Ty::Unit
                    }
                    // `send`/`recv` triple as a `file`'s own I/O too, same
                    // reuse `tcp` already gets rather than a dedicated
                    // `read`/`write` pair — a `file` payload is `str`
                    // only, for the same reason a `tcp` one is (see
                    // `Ty::File`'s doc comment).
                    Ty::File => {
                        self.check(value, &Ty::Str, expected_ret, scopes);
                        Ty::Unit
                    }
                    other => {
                        self.error(TypeErrorKind::ExpectedChannelType { found: other }, *span);
                        self.infer(value, expected_ret, scopes);
                        Ty::Error
                    }
                }
            }
            Expr::Recv(chan, span) => {
                let ct = self.infer(chan, expected_ret, scopes);
                match ct {
                    Ty::Error => Ty::Error,
                    Ty::Channel(inner) => *inner,
                    Ty::Tcp => Ty::Str,
                    Ty::File => Ty::Str,
                    other => {
                        self.error(TypeErrorKind::ExpectedChannelType { found: other }, *span);
                        Ty::Error
                    }
                }
            }
            Expr::SpawnSandbox(name, args, span) => {
                self.infer_sandbox_spawn(name, args, expected_ret, scopes, *span)
            }
            Expr::StopSandbox(inner, span) => {
                let it = self.infer(inner, expected_ret, scopes);
                match it {
                    Ty::Error => Ty::Error,
                    Ty::Sandbox => Ty::I64,
                    // `stop` doubles as a TCP connection's consuming
                    // close (see `Expr::StopSandbox`'s doc comment) — no
                    // exit code to report for that case, just `unit`.
                    Ty::Tcp => Ty::Unit,
                    // `stop` also closes a `listen(port)` handle — same
                    // one-time consuming close, no exit code either.
                    Ty::TcpListener => Ty::Unit,
                    // ...and closes an `open(path, mode)` handle — same
                    // one-time consuming close, reused a third time.
                    Ty::File => Ty::Unit,
                    other => {
                        self.error(TypeErrorKind::ExpectedSandboxType { found: other }, *span);
                        Ty::Error
                    }
                }
            }
            Expr::Connect(host, port, _span) => {
                self.check(host, &Ty::Str, expected_ret, scopes);
                self.check(port, &Ty::I64, expected_ret, scopes);
                Ty::Tcp
            }
            Expr::Listen(port, _span) => {
                self.check(port, &Ty::I64, expected_ret, scopes);
                Ty::TcpListener
            }
            Expr::Accept(listener, span) => {
                let it = self.infer(listener, expected_ret, scopes);
                match it {
                    Ty::Error => Ty::Error,
                    Ty::TcpListener => Ty::Tcp,
                    other => {
                        self.error(TypeErrorKind::ExpectedTcpListenerType { found: other }, *span);
                        Ty::Error
                    }
                }
            }
            Expr::Open(path, mode, _span) => {
                self.check(path, &Ty::Str, expected_ret, scopes);
                self.check(mode, &Ty::Str, expected_ret, scopes);
                Ty::File
            }
            Expr::Index(base, indices, span) => {
                let bt = self.infer(base, expected_ret, scopes);
                for idx in indices {
                    let it = self.infer(idx, expected_ret, scopes);
                    if it != Ty::Error && !it.is_integer() {
                        self.error(TypeErrorKind::ExpectedNumeric { found: it }, idx.span());
                    }
                }
                match &bt {
                    Ty::Error => Ty::Error,
                    Ty::Vector(elem, _) => {
                        if indices.len() != 1 {
                            self.error(
                                TypeErrorKind::WrongIndexArity { expected: 1, found: indices.len() },
                                *span,
                            );
                            return Ty::Error;
                        }
                        (**elem).clone()
                    }
                    Ty::Matrix(elem, _, _) => {
                        if indices.len() != 2 {
                            self.error(
                                TypeErrorKind::WrongIndexArity { expected: 2, found: indices.len() },
                                *span,
                            );
                            return Ty::Error;
                        }
                        (**elem).clone()
                    }
                    other => {
                        self.error(TypeErrorKind::NotIndexable { found: other.clone() }, *span);
                        Ty::Error
                    }
                }
            }
            Expr::ArrayLit(elements, span) => self.infer_array_lit(elements, expected_ret, scopes, *span),
            Expr::FieldAccess(base, field, span) => {
                let bt = self.infer(base, expected_ret, scopes);
                match &bt {
                    Ty::Error => Ty::Error,
                    Ty::Named(name, args) => match self.registry.struct_fields(name) {
                        Some(fields) => match fields.iter().find(|f| &f.name == field) {
                            // Substituted against this specific
                            // instantiation's own type arguments (layer
                            // 6, generics) — `Pair(i64, str).first`'s
                            // declared type is the bare parameter `A`;
                            // the value this access actually produces is
                            // `i64`, `Pair`'s own first argument here.
                            Some(f) => {
                                let type_params = self
                                    .registry
                                    .struct_type_params(name)
                                    .expect("just found this struct's own fields above");
                                let subst = zip_type_params(type_params, args);
                                substitute_ty(&f.ty, &subst)
                            }
                            None => {
                                self.error(
                                    TypeErrorKind::NoSuchField { struct_name: name.clone(), field: field.clone() },
                                    *span,
                                );
                                Ty::Error
                            }
                        },
                        None => {
                            self.error(TypeErrorKind::NotAStruct { found: bt.clone() }, *span);
                            Ty::Error
                        }
                    },
                    other => {
                        self.error(TypeErrorKind::NotAStruct { found: other.clone() }, *span);
                        Ty::Error
                    }
                }
            }
            // Reached only in inference position (`check`, above,
            // intercepts the value-position case, the same split
            // `Expr::If`'s two call sites already establish).
            Expr::Match { scrutinee, arms, span } => {
                self.check_match(scrutinee, arms, *span, MatchWant::Infer, expected_ret, scopes)
            }
        }
    }

    /// `sandbox name(args)` type-checks its arguments exactly like an
    /// ordinary call (reusing `infer_call`, same as `infer_spawn` does),
    /// plus two extra gates that have no analog for `spawn`: `name`'s
    /// declared return type must be `unit`, and every declared parameter
    /// must be `sandbox_safe` (see that function). Both gates check the
    /// callee's *declared signature*, not just the arguments actually
    /// passed here — a `box i64` parameter is rejected even if every
    /// caller happens to pass something scalar-looking, because the
    /// restriction is about what can cross a real process boundary at
    /// all, not about this one call site.
    fn infer_sandbox_spawn(
        &mut self,
        name: &str,
        args: &[Expr],
        expected_ret: &Ty,
        scopes: &mut Scopes,
        span: Span,
    ) -> Ty {
        if is_builtin(name) {
            self.error(TypeErrorKind::CannotSpawnBuiltin { name: name.to_string() }, span);
            for a in args {
                self.infer(a, expected_ret, scopes);
            }
            return Ty::Error;
        }
        if let Some(sig) = self.sigs.get(name) {
            let ret_ty = sig.ret.clone();
            let params = sig.params.clone();
            if ret_ty != Ty::Unit {
                self.error(TypeErrorKind::SandboxFnMustReturnUnit { name: name.to_string() }, span);
            }
            for p in params {
                if !is_sandbox_safe(&p) {
                    self.error(TypeErrorKind::SandboxArgMustBeScalar { found: p }, span);
                }
            }
        }
        let ret = self.infer_call(name, args, expected_ret, scopes, span, None);
        if ret == Ty::Error {
            Ty::Error
        } else {
            Ty::Sandbox
        }
    }

    /// `spawn name(args)` type-checks its arguments exactly like an
    /// ordinary call to `name` (reusing the same signature lookup and
    /// literal-flexibility rules `infer_call` already has — a spawned
    /// computation's parameters are no different from a called
    /// function's), and wraps the result in `Ty::Thread` instead of
    /// returning it directly. `print` is rejected explicitly, not
    /// delegated to `infer_call`: `print` isn't in the `sigs` table at
    /// all (it's special-cased ahead of the lookup, in `infer_call`
    /// itself), so delegating blindly would silently accept `spawn
    /// print(x)` — but `interpreter.rs`'s spawn machinery only knows how
    /// to run a *named function* from `self.fns`, not the builtin. Caught
    /// here, at the type level, rather than left for the interpreter to
    /// fail on.
    fn infer_spawn(&mut self, name: &str, args: &[Expr], expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        if is_builtin(name) {
            self.error(TypeErrorKind::CannotSpawnBuiltin { name: name.to_string() }, span);
            for a in args {
                self.infer(a, expected_ret, scopes); // still check the args for their own errors
            }
            return Ty::Error;
        }
        let ret = self.infer_call(name, args, expected_ret, scopes, span, None);
        if ret == Ty::Error {
            Ty::Error
        } else {
            Ty::Thread(Box::new(ret))
        }
    }

    /// `transact { ... }` (`TRANSACT.md`) type-checks each slot exactly
    /// like an ordinary call to its own name (`infer_transact_slot`),
    /// then binds `network`/`verify`'s return types as scoped variables
    /// visible to every slot after them, matching `TRANSACT.md`'s
    /// "implicit local bindings" rule exactly. Always produces `Ty::Bool`
    /// — `transact` is `true`/`false` by construction (`TRANSACT.md`:
    /// "`true` if it committed, `false` if it compensated"), never
    /// anything else.
    fn infer_transact(
        &mut self,
        network: &TransactSlot,
        verify: &TransactSlot,
        commit: &TransactSlot,
        compensate: Option<&TransactSlot>,
        log: Option<&TransactSlot>,
        expected_ret: &Ty,
        scopes: &mut Scopes,
    ) -> Ty {
        scopes.push();

        let network_ty = self.infer_transact_slot(network, expected_ret, scopes);
        scopes.define("network", network_ty);

        let verify_ty = self.infer_transact_slot(verify, expected_ret, scopes);
        if verify_ty != Ty::Bool && verify_ty != Ty::Error {
            self.error(TypeErrorKind::TransactVerifyMustReturnBool { found: verify_ty.clone() }, verify.span);
        }
        scopes.define("verify", verify_ty);

        self.infer_transact_slot(commit, expected_ret, scopes);
        if let Some(c) = compensate {
            self.infer_transact_slot(c, expected_ret, scopes);
        }
        if let Some(l) = log {
            self.infer_transact_slot(l, expected_ret, scopes);
        }

        scopes.pop();
        Ty::Bool
    }

    /// A `transact` slot's call, type-checked exactly like `infer_call`
    /// except its callee is restricted to a user-defined function —
    /// never a builtin. Mirrors `infer_spawn`'s identical restriction and
    /// identical underlying reason: the interpreter needs an exact
    /// declared return `Ty` to bind `network`/`verify` as implicit local
    /// variables (`interpreter.rs`'s `Expr::Transact` arm looks this up
    /// via `find_fn` the same way `self.call`'s own parameter binding
    /// does), and builtins have no declared-signature table to look that
    /// up from — see `ast::BUILTIN_NAMES`'s doc comment on why not. No
    /// example in `TRANSACT.md` needs a builtin in a slot; every slot
    /// names a real, user-authored business operation.
    fn infer_transact_slot(&mut self, slot: &TransactSlot, expected_ret: &Ty, scopes: &mut Scopes) -> Ty {
        if is_builtin(&slot.name) {
            self.error(TypeErrorKind::CannotUseBuiltinInTransact { name: slot.name.clone() }, slot.span);
            for a in &slot.args {
                self.infer(a, expected_ret, scopes);
            }
            return Ty::Error;
        }
        self.infer_call(&slot.name, &slot.args, expected_ret, scopes, slot.span, None)
    }

    /// `expected` is `Some(want)` only when called from a real value
    /// position with a known target type (`check`'s own `Expr::Call`
    /// handling) — every other caller here (`infer`'s own `Expr::Call`
    /// arm, `infer_spawn`, `infer_sandbox_spawn`, `infer_transact_slot`)
    /// passes `None`, the same "no specific expected type" position
    /// they've always been. Only struct/variant construction (layer 6,
    /// generics) ever consults it — builtins and user functions resolve
    /// their return type from their own signature regardless of context,
    /// unaffected either way.
    fn infer_call(
        &mut self,
        name: &str,
        args: &[Expr],
        expected_ret: &Ty,
        scopes: &mut Scopes,
        span: Span,
        expected: Option<&Ty>,
    ) -> Ty {
        if is_builtin(name) {
            return self.infer_builtin_call(name, args, expected_ret, scopes, span);
        }
        // Row 11: "construction is an ordinary call, not a new literal
        // form" (`nirdosha_row11_amendment.md` §3.1) — a struct's own
        // name or an enum variant's name, called like a function, is how
        // a value gets built. `typecheck`'s registration pass already
        // proved these names can't collide with a function/builtin, so
        // checking them ahead of `self.sigs` is safe and unambiguous.
        if self.registry.is_struct(name) {
            return self.infer_struct_construction(name, args, expected, expected_ret, scopes, span);
        }
        if let Some((enum_name, variant)) = self.registry.find_variant(name) {
            let enum_name = enum_name.to_string();
            let variant = variant.clone();
            return self.infer_variant_construction(&enum_name, &variant, args, expected, expected_ret, scopes, span);
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

    /// `Point(1.0, 2.0)` — a struct constructor call, checked exactly
    /// like an ordinary function call's argument list (`infer_call`),
    /// positional-only against the struct's declared field types
    /// (`nirdosha_row11_amendment.md` §3.1, §3.5's "extends the boundary
    /// set" — a field's own integer-literal bounds get exactly the same
    /// `check` treatment a `let`/param does), substituted for this
    /// specific instantiation first if the struct is generic (layer 6 —
    /// see `resolve_type_args`). Produces `Ty::Named(name, type_args)`.
    fn infer_struct_construction(
        &mut self,
        name: &str,
        args: &[Expr],
        expected: Option<&Ty>,
        expected_ret: &Ty,
        scopes: &mut Scopes,
        span: Span,
    ) -> Ty {
        let decl_fields = self.registry.struct_fields(name).expect("just proved this is a struct").to_vec();
        let type_params = self.registry.struct_type_params(name).expect("just proved this is a struct").to_vec();
        let decl_tys: Vec<Ty> = decl_fields.iter().map(|f| f.ty.clone()).collect();
        let type_args =
            self.resolve_type_args(name, &type_params, &decl_tys, args, expected, expected_ret, scopes, span);
        let subst = zip_type_params(&type_params, &type_args);
        let fields: Vec<Ty> = decl_tys.iter().map(|t| substitute_ty(t, &subst)).collect();

        if fields.len() != args.len() {
            self.error(
                TypeErrorKind::ConstructorArityMismatch { name: name.to_string(), want: fields.len(), got: args.len() },
                span,
            );
        }
        for (arg, field_ty) in args.iter().zip(fields.iter()) {
            self.check(arg, field_ty, expected_ret, scopes);
        }
        for extra in args.iter().skip(fields.len()) {
            self.infer(extra, expected_ret, scopes);
        }
        Ty::Named(name.to_string(), type_args)
    }

    /// `Some(5)` / `None()` — an enum variant constructor call, same
    /// positional-argument-list treatment as `infer_struct_construction`,
    /// against the variant's declared payload types, substituted the same
    /// way if the owning enum is generic. Produces `Ty::Named(enum_name,
    /// type_args)` — the *enum's* name, not the variant's; a variant has
    /// no type of its own (`nirdosha_row11_amendment.md` §3.2).
    #[allow(clippy::too_many_arguments)]
    fn infer_variant_construction(
        &mut self,
        enum_name: &str,
        variant: &Variant,
        args: &[Expr],
        expected: Option<&Ty>,
        expected_ret: &Ty,
        scopes: &mut Scopes,
        span: Span,
    ) -> Ty {
        let type_params = self.registry.enum_type_params(enum_name).expect("just proved this is an enum").to_vec();
        let type_args = self.resolve_type_args(
            enum_name,
            &type_params,
            &variant.payload,
            args,
            expected,
            expected_ret,
            scopes,
            span,
        );
        let subst = zip_type_params(&type_params, &type_args);
        let payload: Vec<Ty> = variant.payload.iter().map(|t| substitute_ty(t, &subst)).collect();

        if payload.len() != args.len() {
            self.error(
                TypeErrorKind::ConstructorArityMismatch {
                    name: variant.name.clone(),
                    want: payload.len(),
                    got: args.len(),
                },
                span,
            );
        }
        for (arg, want) in args.iter().zip(payload.iter()) {
            self.check(arg, want, expected_ret, scopes);
        }
        for extra in args.iter().skip(payload.len()) {
            self.infer(extra, expected_ret, scopes);
        }
        Ty::Named(enum_name.to_string(), type_args)
    }

    /// Resolves the concrete type arguments for constructing `name` (a
    /// struct's own name, or the *owning enum's* name for a variant) —
    /// Row 11 layer 6. Two sources, tried in order, since there is no
    /// explicit-type-argument call syntax at all
    /// (`nirdosha_row11_amendment.md` §3.1: "Nirdosha never uses `<...>`
    /// for type application"):
    ///
    /// 1. `expected` — if it's `Ty::Named(name, args)` with the right
    ///    arity, those are the args, full stop. The common case: a
    ///    `let`/`return`/call-argument boundary already pins the exact
    ///    instantiation, the same way `Some(5)` needs no annotation
    ///    "passed where an `Option(i64)` is expected" (§3.2).
    /// 2. Structural inference from the arguments themselves — infers
    ///    each argument's own type (silently: `self.silent`, mirroring
    ///    `ownership.rs`'s identically-purposed field, so this doesn't
    ///    double-report an argument's own internal errors before the
    ///    real `self.check` pass that follows in the caller), then walks
    ///    each declared field/payload type opposite it (`bind_type_params`),
    ///    binding any type parameter found bare. A parameter that never
    ///    appears bare in any field/payload type (`Result(T, E)`'s `T`
    ///    when constructing `Err(msg)` alone) can't be recovered this way.
    ///
    /// Reports `GenericConstructorNeedsExplicitType` and fills any
    /// still-unresolved parameter with `Ty::Error` if neither source
    /// resolves every one — the same error-recovery shape (report once,
    /// keep checking with a poison type) every other failure in this file
    /// already uses.
    #[allow(clippy::too_many_arguments)]
    fn resolve_type_args(
        &mut self,
        name: &str,
        type_params: &[String],
        decl_tys: &[Ty],
        args: &[Expr],
        expected: Option<&Ty>,
        expected_ret: &Ty,
        scopes: &mut Scopes,
        span: Span,
    ) -> Vec<Ty> {
        if type_params.is_empty() {
            return Vec::new();
        }
        if let Some(Ty::Named(want_name, want_args)) = expected
            && want_name == name
            && want_args.len() == type_params.len()
        {
            return want_args.clone();
        }
        let mut subst: HashMap<String, Ty> = HashMap::new();
        let was_silent = self.silent;
        self.silent = true;
        for (decl_ty, arg) in decl_tys.iter().zip(args.iter()) {
            let arg_ty = self.infer(arg, expected_ret, scopes);
            if arg_ty != Ty::Error {
                bind_type_params(decl_ty, &arg_ty, type_params, &mut subst);
            }
        }
        self.silent = was_silent;
        match type_params.iter().map(|p| subst.get(p).cloned()).collect::<Option<Vec<_>>>() {
            Some(resolved) => resolved,
            None => {
                self.error(TypeErrorKind::GenericConstructorNeedsExplicitType { name: name.to_string() }, span);
                type_params.iter().map(|_| Ty::Error).collect()
            }
        }
    }

    /// `match scrutinee { variant(bindings) => body, ... }`. Exhaustiveness
    /// (`nirdosha_row11_amendment.md` §3.4: every declared variant,
    /// exactly once, no wildcard in v1) is checked unconditionally,
    /// regardless of `want` — it's a property of the `match` itself, not
    /// of how its value is used.
    fn check_match(
        &mut self,
        scrutinee: &Expr,
        arms: &[MatchArm],
        span: Span,
        want: MatchWant,
        expected_ret: &Ty,
        scopes: &mut Scopes,
    ) -> Ty {
        let st = self.infer(scrutinee, expected_ret, scopes);
        // `enum_name`/`type_args` are the scrutinee's own already-concrete
        // instantiation (layer 6, generics) — a `match` scrutinee is
        // always a fully-inferred *value*, never a fresh construction, so
        // there's no `resolve_type_args`-style ambiguity here: `st` is
        // simply `Ty::Named(enum_name, type_args)` already, straight from
        // whatever produced this value.
        let (enum_name, type_args) = match &st {
            Ty::Error => (None, Vec::new()),
            Ty::Named(name, args) if self.registry.is_enum(name) => (Some(name.clone()), args.clone()),
            other => {
                self.error(TypeErrorKind::NotAnEnum { found: other.clone() }, scrutinee.span());
                (None, Vec::new())
            }
        };
        let enum_type_params =
            enum_name.as_deref().and_then(|en| self.registry.enum_type_params(en)).unwrap_or(&[]).to_vec();
        let enum_subst = zip_type_params(&enum_type_params, &type_args);

        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut result: Option<Ty> = None;

        for arm in arms {
            let payload: Vec<Ty> = match &enum_name {
                Some(en) => match self.registry.find_variant(&arm.variant) {
                    Some((owner, v)) if owner == en => {
                        v.payload.iter().map(|t| substitute_ty(t, &enum_subst)).collect()
                    }
                    _ => {
                        self.error(
                            TypeErrorKind::UnknownVariant { enum_name: en.clone(), variant: arm.variant.clone() },
                            arm.span,
                        );
                        Vec::new()
                    }
                },
                None => Vec::new(),
            };

            if !seen.insert(arm.variant.clone()) {
                self.error(TypeErrorKind::DuplicateMatchArm { variant: arm.variant.clone() }, arm.span);
            }
            if payload.len() != arm.bindings.len() {
                self.error(
                    TypeErrorKind::WrongVariantArity {
                        variant: arm.variant.clone(),
                        want: payload.len(),
                        got: arm.bindings.len(),
                    },
                    arm.span,
                );
            }

            scopes.push();
            for (name, ty) in arm.bindings.iter().zip(payload.iter()) {
                scopes.define(name, ty.clone());
            }
            let arm_ty = match want {
                // Bare statement -- nothing reads this arm's value, so it's
                // just inferred (for its own internal diagnostics) and
                // discarded, the same "doesn't need its branches to agree"
                // treatment `check_stmt_expr` already gives a statement-
                // position `if` (module doc).
                MatchWant::Statement => {
                    self.infer(&arm.body, expected_ret, scopes);
                    Ty::Unit
                }
                MatchWant::Check(w) => {
                    self.check(&arm.body, w, expected_ret, scopes);
                    w.clone()
                }
                MatchWant::Infer => self.infer(&arm.body, expected_ret, scopes),
            };
            scopes.pop();

            if matches!(want, MatchWant::Infer) {
                result = Some(match result {
                    None => arm_ty,
                    Some(prev) => {
                        if prev != Ty::Error && arm_ty != Ty::Error && prev != arm_ty {
                            self.error(TypeErrorKind::TypeMismatch { expected: prev.clone(), found: arm_ty }, arm.span);
                            Ty::Error
                        } else if prev == Ty::Error {
                            prev
                        } else {
                            arm_ty
                        }
                    }
                });
            }
        }

        if let Some(en) = &enum_name
            && let Some(variants) = self.registry.enum_variants(en)
        {
            let missing: Vec<String> =
                variants.iter().map(|v| v.name.clone()).filter(|n| !seen.contains(n)).collect();
            if !missing.is_empty() {
                self.error(TypeErrorKind::NonExhaustiveMatch { enum_name: en.clone(), missing }, span);
            }
        }

        match want {
            MatchWant::Statement => Ty::Unit,
            MatchWant::Check(w) => w.clone(),
            MatchWant::Infer => result.unwrap_or(Ty::Unit),
        }
    }

    /// Every builtin's shape rule, dispatched by name — `is_builtin`
    /// (ast.rs) is the shared membership check; the actual per-builtin
    /// logic lives here (and `interpreter.rs`'s `Expr::Call` arm has its
    /// own independent counterpart), not in a shared table, because a
    /// generic `fn(&[Ty]) -> Ty` signature can't see the *literal value*
    /// `zeros`/`ones`/`identity` need to fix their result's static shape
    /// — see `ast.rs::BUILTIN_NAMES`'s doc comment.
    fn infer_builtin_call(&mut self, name: &str, args: &[Expr], expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        // `print` accepts any number of arguments of any type -- every
        // argument is still inferred, for its own diagnostics, but
        // nothing here constrains what it can be.
        if name == "print" {
            for a in args {
                self.infer(a, expected_ret, scopes);
            }
            return Ty::Unit;
        }

        match (name, args.len()) {
            ("transpose", 1) => match self.infer(&args[0], expected_ret, scopes) {
                Ty::Matrix(elem, r, c) => Ty::Matrix(elem, c, r),
                found => self.wrong_arg(name, "a Matrix", found, span),
            },
            ("dot", 2) | ("cross", 2) => {
                let lt = self.infer(&args[0], expected_ret, scopes);
                let rt = self.infer(&args[1], expected_ret, scopes);
                let (Ty::Vector(l_elem, ln), Ty::Vector(r_elem, rn)) = (lt.clone(), rt.clone()) else {
                    if lt != Ty::Error {
                        self.wrong_arg(name, "a Vector", lt, span);
                    }
                    if rt != Ty::Error {
                        self.wrong_arg(name, "a Vector", rt, span);
                    }
                    return Ty::Error;
                };
                if l_elem != r_elem {
                    self.error(TypeErrorKind::TypeMismatch { expected: *l_elem, found: *r_elem }, span);
                    return Ty::Error;
                }
                if !l_elem.is_numeric() {
                    self.error(TypeErrorKind::ExpectedNumeric { found: *l_elem }, span);
                    return Ty::Error;
                }
                if name == "cross" {
                    if ln != 3 || rn != 3 {
                        self.wrong_arg(name, "a Vector(_, 3)", if ln != 3 { lt } else { rt }, span);
                        return Ty::Error;
                    }
                    return Ty::Vector(l_elem, 3);
                }
                if ln != rn {
                    self.error(TypeErrorKind::ShapeMismatch { left: lt, right: rt }, span);
                    return Ty::Error;
                }
                *l_elem
            }
            ("zeros", 1) | ("ones", 1) => match self.literal_dimension(&args[0], name, span) {
                Some(n) => Ty::Vector(Box::new(Ty::F64), n),
                None => Ty::Error,
            },
            ("zeros", 2) | ("ones", 2) => {
                let r = self.literal_dimension(&args[0], name, span);
                let c = self.literal_dimension(&args[1], name, span);
                match (r, c) {
                    (Some(r), Some(c)) => Ty::Matrix(Box::new(Ty::F64), r, c),
                    _ => Ty::Error,
                }
            }
            ("identity", 1) => match self.literal_dimension(&args[0], name, span) {
                Some(n) => Ty::Matrix(Box::new(Ty::F64), n, n),
                None => Ty::Error,
            },
            ("sum", 1) => match self.infer(&args[0], expected_ret, scopes) {
                Ty::Vector(elem, _) | Ty::Matrix(elem, _, _) if elem.is_numeric() => *elem,
                found => self.wrong_arg(name, "a Vector or Matrix", found, span),
            },
            ("len", 1) => match self.infer(&args[0], expected_ret, scopes) {
                Ty::Vector(_, _) => Ty::I64,
                found => self.wrong_arg(name, "a Vector", found, span),
            },
            ("norm", 1) | ("norm1", 1) | ("norm_inf", 1) => {
                match self.expect_f64_vector(&args[0], name, expected_ret, scopes, span) {
                    Some(_) => Ty::F64,
                    None => Ty::Error,
                }
            }
            ("frobenius_norm", 1) => match self.expect_f64_matrix(&args[0], name, expected_ret, scopes, span) {
                Some(_) => Ty::F64,
                None => Ty::Error,
            },
            ("trace", 1) => match self.infer(&args[0], expected_ret, scopes) {
                Ty::Matrix(elem, r, c) if elem.is_numeric() => {
                    if r != c {
                        self.error(TypeErrorKind::NotSquare { found: Ty::Matrix(elem, r, c) }, span);
                        return Ty::Error;
                    }
                    *elem
                }
                found => self.wrong_arg(name, "a square Matrix", found, span),
            },
            ("det", 1) | ("inv", 1) => match self.expect_square_f64_matrix(&args[0], name, expected_ret, scopes, span) {
                Some(_) if name == "det" => Ty::F64,
                Some(n) => Ty::Matrix(Box::new(Ty::F64), n, n),
                None => Ty::Error,
            },
            ("solve", 2) => {
                let n = self.expect_square_f64_matrix(&args[0], name, expected_ret, scopes, span);
                let m = self.expect_f64_vector(&args[1], name, expected_ret, scopes, span);
                match (n, m) {
                    (Some(n), Some(m)) if n == m => Ty::Vector(Box::new(Ty::F64), n),
                    (Some(n), Some(m)) => {
                        self.error(
                            TypeErrorKind::ShapeMismatch {
                                left: Ty::Matrix(Box::new(Ty::F64), n, n),
                                right: Ty::Vector(Box::new(Ty::F64), m),
                            },
                            span,
                        );
                        Ty::Error
                    }
                    _ => Ty::Error,
                }
            }
            ("rank", 1) => match self.expect_f64_matrix(&args[0], name, expected_ret, scopes, span) {
                Some(_) => Ty::I64,
                None => Ty::Error,
            },
            ("is_symmetric", 1) | ("is_diag", 1) => {
                match self.expect_square_f64_matrix(&args[0], name, expected_ret, scopes, span) {
                    Some(_) => Ty::Bool,
                    None => Ty::Error,
                }
            }
            ("is_square", 1) => match self.infer(&args[0], expected_ret, scopes) {
                Ty::Matrix(..) => Ty::Bool,
                found => self.wrong_arg(name, "a Matrix", found, span),
            },
            // ---- Phase 3: deterministic simulation primitives --------
            ("rand_seed", 1) => match self.infer(&args[0], expected_ret, scopes) {
                t if t.is_integer() => Ty::Unit,
                found => self.wrong_arg(name, "an integer", found, span),
            },
            ("rand_f64", 0) => Ty::F64,
            ("rand_gaussian", 2) => {
                self.check(&args[0], &Ty::F64, expected_ret, scopes);
                self.check(&args[1], &Ty::F64, expected_ret, scopes);
                Ty::F64
            }
            ("distance", 2) => {
                let a = self.expect_f64_vector(&args[0], name, expected_ret, scopes, span);
                let b = self.expect_f64_vector(&args[1], name, expected_ret, scopes, span);
                match (a, b) {
                    (Some(a), Some(b)) if a == b => Ty::F64,
                    (Some(a), Some(b)) => {
                        self.error(
                            TypeErrorKind::ShapeMismatch {
                                left: Ty::Vector(Box::new(Ty::F64), a),
                                right: Ty::Vector(Box::new(Ty::F64), b),
                            },
                            span,
                        );
                        Ty::Error
                    }
                    _ => Ty::Error,
                }
            }
            // Takes the same `Vector(f64, 3)` lat/lon/alt representation
            // every other geometry builtin here does (altitude ignored)
            // -- not a separate `Vector(f64, 2)`, so callers don't need
            // a throwaway lat/lon-only vector just for this one builtin.
            ("bearing", 2) => {
                self.check(&args[0], &Ty::Vector(Box::new(Ty::F64), 3), expected_ret, scopes);
                self.check(&args[1], &Ty::Vector(Box::new(Ty::F64), 3), expected_ret, scopes);
                Ty::F64
            }
            ("lla_to_ecef", 1) | ("ecef_to_lla", 1) => {
                self.check(&args[0], &Ty::Vector(Box::new(Ty::F64), 3), expected_ret, scopes);
                Ty::Vector(Box::new(Ty::F64), 3)
            }
            ("ecef_to_enu", 2) | ("enu_to_ecef", 2) => {
                self.check(&args[0], &Ty::Vector(Box::new(Ty::F64), 3), expected_ret, scopes);
                self.check(&args[1], &Ty::Vector(Box::new(Ty::F64), 3), expected_ret, scopes);
                Ty::Vector(Box::new(Ty::F64), 3)
            }
            // Linear Kalman filter. Split into `_state`/`_cov` pairs, not
            // the plan's single `kf_predict`/`kf_update` call each -- this
            // language has no tuple/struct type to return "the new (x, P)
            // pair" as one value (see the unified plan's §5: generics,
            // which a real product type needs, are explicitly out of
            // scope this phase). Both halves of a pair take the *same*
            // arguments and are meant to be called together at each
            // simulation step; splitting them is an honest adaptation to
            // a real language constraint, not a design preference.
            ("kf_predict_state", 4) | ("kf_predict_cov", 4) => {
                let n1 = self.expect_f64_vector(&args[0], name, expected_ret, scopes, span);
                let n2 = self.expect_square_f64_matrix(&args[1], name, expected_ret, scopes, span);
                let n3 = self.expect_square_f64_matrix(&args[2], name, expected_ret, scopes, span);
                let n4 = self.expect_square_f64_matrix(&args[3], name, expected_ret, scopes, span);
                match (n1, n2, n3, n4) {
                    (Some(n1), Some(n2), Some(n3), Some(n4)) if n1 == n2 && n2 == n3 && n3 == n4 => {
                        if name == "kf_predict_state" {
                            Ty::Vector(Box::new(Ty::F64), n1)
                        } else {
                            Ty::Matrix(Box::new(Ty::F64), n1, n1)
                        }
                    }
                    (Some(n1), ..) => {
                        self.error(
                            TypeErrorKind::WrongBuiltinArgType {
                                builtin: name.to_string(),
                                expected: "x/P/F/Q of matching dimension n".to_string(),
                                found: Ty::Vector(Box::new(Ty::F64), n1),
                            },
                            span,
                        );
                        Ty::Error
                    }
                    _ => Ty::Error,
                }
            }
            ("kf_update_state", 5) | ("kf_update_cov", 5) => {
                let n = self.expect_f64_vector(&args[0], name, expected_ret, scopes, span);
                let n2 = self.expect_square_f64_matrix(&args[1], name, expected_ret, scopes, span);
                let m = self.expect_f64_vector(&args[2], name, expected_ret, scopes, span);
                let (h_rows, h_cols) = match self.expect_f64_matrix(&args[3], name, expected_ret, scopes, span) {
                    Some(rc) => rc,
                    None => (usize::MAX, usize::MAX),
                };
                let r_n = self.expect_square_f64_matrix(&args[4], name, expected_ret, scopes, span);
                match (n, n2, m, r_n) {
                    (Some(n), Some(n2), Some(m), Some(r_n))
                        if n == n2 && h_rows == m && h_cols == n && r_n == m =>
                    {
                        if name == "kf_update_state" {
                            Ty::Vector(Box::new(Ty::F64), n)
                        } else {
                            Ty::Matrix(Box::new(Ty::F64), n, n)
                        }
                    }
                    (Some(n), ..) => {
                        self.error(
                            TypeErrorKind::WrongBuiltinArgType {
                                builtin: name.to_string(),
                                expected: "x/P/z/H/R of matching dimensions (n, n, m, m x n, m)".to_string(),
                                found: Ty::Vector(Box::new(Ty::F64), n),
                            },
                            span,
                        );
                        Ty::Error
                    }
                    _ => Ty::Error,
                }
            }
            _ => {
                for a in args {
                    self.infer(a, expected_ret, scopes);
                }
                self.error(
                    TypeErrorKind::ArityMismatch { fn_name: name.to_string(), want: self.builtin_arity_hint(name), got: args.len() },
                    span,
                );
                Ty::Error
            }
        }
    }

    /// A rough "how many arguments did you mean" for the arity-mismatch
    /// message above — most builtins take exactly one, a few take two;
    /// this is display-only (the actual accepted counts are the match
    /// arms above, which may accept more than one arity, e.g. `zeros`).
    fn builtin_arity_hint(&self, name: &str) -> usize {
        match name {
            "dot" | "cross" | "solve" => 2,
            _ => 1,
        }
    }

    fn wrong_arg(&mut self, builtin: &str, expected: &str, found: Ty, span: Span) -> Ty {
        if found != Ty::Error {
            self.error(
                TypeErrorKind::WrongBuiltinArgType { builtin: builtin.to_string(), expected: expected.to_string(), found },
                span,
            );
        }
        Ty::Error
    }

    /// `zeros`/`ones`/`identity`'s dimension arguments: must be a plain
    /// integer literal (`literal_value`, ast.rs — already the same
    /// recognizer `typeck.rs` uses everywhere else for "is this a bare
    /// literal"), non-negative, and small enough to be a real `usize`.
    fn literal_dimension(&mut self, arg: &Expr, builtin: &str, span: Span) -> Option<usize> {
        match literal_value(arg).and_then(|n| usize::try_from(n).ok()) {
            Some(n) => Some(n),
            None => {
                self.error(TypeErrorKind::ExpectedLiteralDimension { builtin: builtin.to_string() }, span);
                None
            }
        }
    }

    fn expect_f64_matrix(&mut self, arg: &Expr, builtin: &str, expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Option<(usize, usize)> {
        match self.infer(arg, expected_ret, scopes) {
            Ty::Matrix(elem, r, c) if *elem == Ty::F64 => Some((r, c)),
            found => {
                self.wrong_arg(builtin, "a Matrix(f64, _, _)", found, span);
                None
            }
        }
    }

    fn expect_square_f64_matrix(&mut self, arg: &Expr, builtin: &str, expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Option<usize> {
        let (r, c) = self.expect_f64_matrix(arg, builtin, expected_ret, scopes, span)?;
        if r != c {
            self.error(TypeErrorKind::NotSquare { found: Ty::Matrix(Box::new(Ty::F64), r, c) }, span);
            return None;
        }
        Some(r)
    }

    fn expect_f64_vector(&mut self, arg: &Expr, builtin: &str, expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Option<usize> {
        match self.infer(arg, expected_ret, scopes) {
            Ty::Vector(elem, n) if *elem == Ty::F64 => Some(n),
            found => {
                self.wrong_arg(builtin, "a Vector(f64, _)", found, span);
                None
            }
        }
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
            // Both arms below used to check `t == Ty::Bool` specifically
            // -- the only non-numeric type that existed when they were
            // written. That under-restricted every *other* non-numeric
            // type (a real, found-by-testing gap: `"a" < "b"` and
            // `"a" + "b"` both typechecked cleanly and only failed at
            // *runtime*, with a generic `TypeMismatch`, instead of being
            // rejected statically the way `true < false` already was).
            // `!t.is_integer()` is the correct, general condition -- it
            // covers `Bool` and every other non-numeric type uniformly,
            // not just the one this project happened to have first.
            BinOp::Lt | BinOp::Gt | BinOp::LtEq | BinOp::GtEq => {
                let t = self.unify_operands(lhs, rhs, expected_ret, scopes, span);
                if t != Ty::Error && !t.is_numeric() {
                    self.error(TypeErrorKind::ExpectedNumeric { found: t }, span);
                }
                Ty::Bool
            }
            // Elementwise -- `Vector`/`Matrix` operands are allowed here
            // (as long as the shapes match exactly, which `unify_operands`
            // already enforces via plain `Ty` equality — a `Vector(f64,
            // 3)` and a `Vector(f64, 4)` are different types the same way
            // two different integer widths are), unlike `Div` below.
            BinOp::Add | BinOp::Sub => {
                let t = self.unify_operands(lhs, rhs, expected_ret, scopes, span);
                if t != Ty::Error && !is_elementwise_operand(&t) {
                    self.error(TypeErrorKind::ExpectedNumeric { found: t }, span);
                    return Ty::Error;
                }
                t
            }
            // No `Vector`/`Matrix` division exists this phase (dense
            // linear algebra's `A \ b`-style solve is Phase 2's `solve`
            // builtin, not this operator) -- stays scalar-only.
            BinOp::Div => {
                let t = self.unify_operands(lhs, rhs, expected_ret, scopes, span);
                if t != Ty::Error && !t.is_numeric() {
                    self.error(TypeErrorKind::ExpectedNumeric { found: t }, span);
                    return Ty::Error;
                }
                t
            }
            // Linear-algebra product: scalar×matrix, matrix×vector,
            // matrix×matrix (inner dims match) — genuinely heterogeneous
            // operand shapes, so it gets its own function rather than
            // `unify_operands`'s same-type-or-literal-flexible model.
            BinOp::Mul => self.infer_mul(lhs, rhs, expected_ret, scopes, span),
            // Hadamard (elementwise) multiply/divide — exact same shape,
            // the same rule `+`/`-` follow, just spelled with its own
            // operator because plain `*`/`/` already mean something else
            // for `Vector`/`Matrix` operands.
            BinOp::ElemMul | BinOp::ElemDiv => self.infer_hadamard(lhs, rhs, expected_ret, scopes, span),
        }
    }

    /// `*`'s full shape table (unified plan §4.1.3): scalar×matrix (either
    /// order, scalar type must match the matrix's element type exactly —
    /// no implicit conversion), matrix×vector and matrix×matrix (inner
    /// dimensions must match — `ShapeMismatch` otherwise). `Vector *
    /// Vector` gets its own specific rejection (`VectorTimesVectorNotSupported`)
    /// rather than falling through to a generic mismatch, since there's a
    /// concrete better alternative to point at.
    ///
    /// A bare int literal is never itself Vector/Matrix-shaped, so the
    /// only path either operand of a *literal* multiplication can take is
    /// plain scalar arithmetic — delegating that case to `unify_operands`
    /// keeps this function from having to reimplement literal-width
    /// flexibility (`n * 2` for `n: i8`, say) on top of everything else
    /// it already does.
    fn infer_mul(&mut self, lhs: &Expr, rhs: &Expr, expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        if literal_value(lhs).is_some() || literal_value(rhs).is_some() {
            let t = self.unify_operands(lhs, rhs, expected_ret, scopes, span);
            if t != Ty::Error && !t.is_numeric() {
                self.error(TypeErrorKind::ExpectedNumeric { found: t }, span);
                return Ty::Error;
            }
            return t;
        }
        let lt = self.infer(lhs, expected_ret, scopes);
        let rt = self.infer(rhs, expected_ret, scopes);
        if lt == Ty::Error || rt == Ty::Error {
            return Ty::Error;
        }
        let is_array = |t: &Ty| matches!(t, Ty::Vector(..) | Ty::Matrix(..));
        match (&lt, &rt) {
            (s, Ty::Matrix(elem, r, c)) if !is_array(s) && s == elem.as_ref() => {
                Ty::Matrix(elem.clone(), *r, *c)
            }
            (Ty::Matrix(elem, r, c), s) if !is_array(s) && s == elem.as_ref() => {
                Ty::Matrix(elem.clone(), *r, *c)
            }
            (Ty::Matrix(m_elem, r, c), Ty::Vector(v_elem, n)) => {
                if m_elem != v_elem {
                    self.error(TypeErrorKind::TypeMismatch { expected: (**m_elem).clone(), found: (**v_elem).clone() }, span);
                    return Ty::Error;
                }
                if c != n {
                    self.error(TypeErrorKind::ShapeMismatch { left: lt.clone(), right: rt.clone() }, span);
                    return Ty::Error;
                }
                Ty::Vector(m_elem.clone(), *r)
            }
            (Ty::Matrix(l_elem, r1, c1), Ty::Matrix(r_elem, r2, c2)) => {
                if l_elem != r_elem {
                    self.error(TypeErrorKind::TypeMismatch { expected: (**l_elem).clone(), found: (**r_elem).clone() }, span);
                    return Ty::Error;
                }
                if c1 != r2 {
                    self.error(TypeErrorKind::ShapeMismatch { left: lt.clone(), right: rt.clone() }, span);
                    return Ty::Error;
                }
                Ty::Matrix(l_elem.clone(), *r1, *c2)
            }
            (Ty::Vector(..), Ty::Vector(..)) => {
                self.error(TypeErrorKind::VectorTimesVectorNotSupported, span);
                Ty::Error
            }
            (l, r) if l.is_numeric() && r.is_numeric() && l == r => l.clone(),
            _ => {
                self.error(TypeErrorKind::TypeMismatch { expected: lt.clone(), found: rt.clone() }, span);
                Ty::Error
            }
        }
    }

    /// `.*`/`./` — exact same shape required (a plain `Ty` equality
    /// check, same as `+`/`-`), each side's element type numeric. Two
    /// matching scalars are trivially "the same shape," so this also
    /// covers scalar `.*`/`./`, harmlessly.
    fn infer_hadamard(&mut self, lhs: &Expr, rhs: &Expr, expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        let lt = self.infer(lhs, expected_ret, scopes);
        let rt = self.infer(rhs, expected_ret, scopes);
        if lt == Ty::Error || rt == Ty::Error {
            return Ty::Error;
        }
        if !is_elementwise_operand(&lt) {
            self.error(TypeErrorKind::ExpectedNumeric { found: lt }, lhs.span());
            return Ty::Error;
        }
        if !is_elementwise_operand(&rt) {
            self.error(TypeErrorKind::ExpectedNumeric { found: rt }, rhs.span());
            return Ty::Error;
        }
        if lt != rt {
            self.error(TypeErrorKind::TypeMismatch { expected: lt, found: rt }, span);
            return Ty::Error;
        }
        lt
    }

    /// `[e1, e2, ...]` — infers the first element's type `t0`, checks
    /// every other element against it (literal-flexible, same as any
    /// other value position — `[1, n]` for `n: i32` widens the literal),
    /// then classifies: `t0` a plain scalar → `Vector(t0, len)`; `t0`
    /// itself a `Vector` of a plain scalar → this is a matrix literal,
    /// `Matrix(inner, len, t0's length)`; anything else (`t0` is a
    /// `Matrix`, or a `Vector` of a `Vector`/`Matrix`) → `ArrayLiteralTooDeep`,
    /// this type system only goes to 2 dimensions.
    fn infer_array_lit(&mut self, elements: &[Expr], expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        // The parser never produces an empty `ArrayLit` (`[]` is a parse
        // error) — see `Expr::ArrayLit`'s doc comment.
        let t0 = self.infer(&elements[0], expected_ret, scopes);
        let result = if t0 == Ty::Error {
            Ty::Error
        } else {
            match &t0 {
                Ty::Vector(inner, n) if !matches!(inner.as_ref(), Ty::Vector(..) | Ty::Matrix(..)) => {
                    Ty::Matrix(inner.clone(), elements.len(), *n)
                }
                Ty::Vector(..) | Ty::Matrix(..) => {
                    self.error(TypeErrorKind::ArrayLiteralTooDeep { found: t0.clone() }, span);
                    Ty::Error
                }
                _ => Ty::Vector(Box::new(t0.clone()), elements.len()),
            }
        };
        for e in &elements[1..] {
            self.check(e, &t0, expected_ret, scopes);
        }
        result
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
                if !rt.is_numeric() {
                    self.error(TypeErrorKind::ExpectedNumeric { found: rt }, lhs.span());
                    return Ty::Error;
                }
                if !rt.is_integer() {
                    // Numeric but not integer -- `F64`. A bare int literal
                    // doesn't implicitly widen to float the way it widens
                    // across integer widths (only a float *literal*
                    // types as `F64` -- see `Expr::Float`'s doc comment),
                    // so this is a real mismatch, not a range check.
                    self.error(TypeErrorKind::TypeMismatch { expected: rt, found: Ty::I64 }, lhs.span());
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
                if !lt.is_numeric() {
                    self.error(TypeErrorKind::ExpectedNumeric { found: lt }, rhs.span());
                    return Ty::Error;
                }
                if !lt.is_integer() {
                    // See the mirror-image branch above.
                    self.error(TypeErrorKind::TypeMismatch { expected: lt, found: Ty::I64 }, rhs.span());
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
/// Row 11 layer 6's structural type-parameter binder — `resolve_type_args`'s
/// fallback path. Walks `decl_ty` (a struct/enum's own declared field/
/// payload type, possibly containing bare references to `type_params`)
/// opposite `concrete_ty` (that same position's actual, already-inferred
/// argument type), binding any `type_params` member found bare in
/// `decl_ty` to its counterpart in `concrete_ty`. A parameter already
/// bound keeps its first binding — a *conflicting* second binding
/// (`Pair(A, A)`-shaped field reuse with disagreeing argument types)
/// isn't specially diagnosed here; the caller's own `self.check` against
/// the resulting substitution catches the disagreement as an ordinary
/// `TypeMismatch` on whichever argument doesn't fit.
fn bind_type_params(decl_ty: &Ty, concrete_ty: &Ty, type_params: &[String], subst: &mut HashMap<String, Ty>) {
    match (decl_ty, concrete_ty) {
        (Ty::Named(name, args), _) if args.is_empty() && type_params.iter().any(|p| p == name) => {
            subst.entry(name.clone()).or_insert_with(|| concrete_ty.clone());
        }
        (Ty::Box(a), Ty::Box(b))
        | (Ty::Ref(a), Ty::Ref(b))
        | (Ty::Thread(a), Ty::Thread(b))
        | (Ty::Channel(a), Ty::Channel(b)) => bind_type_params(a, b, type_params, subst),
        (Ty::Vector(a, _), Ty::Vector(b, _)) | (Ty::Matrix(a, _, _), Ty::Matrix(b, _, _)) => {
            bind_type_params(a, b, type_params, subst)
        }
        (Ty::Named(dn, dargs), Ty::Named(cn, cargs)) if dn == cn && dargs.len() == cargs.len() => {
            for (da, ca) in dargs.iter().zip(cargs.iter()) {
                bind_type_params(da, ca, type_params, subst);
            }
        }
        _ => {}
    }
}

fn definitely_returns(stmts: &[Stmt]) -> bool {
    for stmt in stmts {
        match stmt {
            Stmt::Return { .. } => return true,
            Stmt::Expr(e) if if_definitely_returns(e) => return true,
            // Unlike `While` (which might run zero times, so its body
            // never counts), `audited`'s body is straight-line code that
            // always executes exactly once when reached — structurally
            // no different from inlining its statements directly here.
            Stmt::Audited { body, .. } if definitely_returns(body) => return true,
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

/// True for a plain numeric scalar, or a `Vector`/`Matrix` whose element
/// type is numeric — the operand shape `+`/`-` (elementwise) and `.*`/
/// `./` (Hadamard) accept, per the unified plan's §4.1.3 operator table.
fn is_elementwise_operand(ty: &Ty) -> bool {
    match ty {
        Ty::Vector(elem, _) | Ty::Matrix(elem, _, _) => elem.is_numeric(),
        other => other.is_numeric(),
    }
}

/// What's allowed to cross into a `sandbox`-spawned function's parameter
/// list (SANDBOXING.md layers 1-2): a plain scalar (an integer type or
/// `bool`, layer 1's original rule), or now also `chan T` where `T`
/// itself is a plain scalar (layer 2's real cross-process transport —
/// see `interpreter.rs`'s `ChannelInner`/`spawn_sandbox`). Not `chan` of
/// anything else, and not `box`/`&`/`thread`/`sandbox` at all — those
/// have no wire format defined yet (SANDBOXING.md layer 3).
fn is_sandbox_safe(ty: &Ty) -> bool {
    match ty {
        Ty::Channel(inner) => inner.is_integer() || **inner == Ty::Bool,
        other => other.is_integer() || *other == Ty::Bool,
    }
}

/// The "caller-supplied variable environment" `validate_fragment` type-
/// checks a fragment against — a flat name→`Ty` map representing
/// whatever's already in scope at the splice point (e.g. an agent
/// generating a replacement for `<expr>` inside `let x: i64 = <expr>`,
/// where the surrounding function already has `a`, `b` in scope, passes
/// an environment mapping those two names to their declared types).
/// Deliberately flat, not the real `Scopes`' nested stack — a fragment
/// being validated in isolation has exactly one scope, the caller's
/// flattened view of everything visible at that one point; there's no
/// nested-block structure to preserve across the validation boundary.
#[derive(Default)]
pub struct FragmentEnv(HashMap<String, Ty>);

impl FragmentEnv {
    pub fn new() -> Self {
        FragmentEnv(HashMap::new())
    }

    pub fn with(mut self, name: impl Into<String>, ty: Ty) -> Self {
        self.0.insert(name.into(), ty);
        self
    }
}

/// goal.md row 9's load-bearing piece (§4 of `typeck.rs`'s module doc):
/// "agents emit typed AST/IR fragments the compiler validates before
/// splicing, not raw text." `json` is a JSON-serialized `Expr` (the same
/// shape `--emit-ast=json` — main.rs — produces for a whole program,
/// here for one expression), deserialized and then type-checked exactly
/// the way any other value-position expression would be (`Checker::check`
/// — the same entry point `let`/`return`/call-argument positions already
/// go through), seeded with `env`'s bindings instead of a real function's
/// parameters.
///
/// **Scope boundary, stated explicitly:** this checks *types* only, not
/// ownership — `ownership.rs`'s move-checker reasons over a whole
/// function's control flow (branch/loop merging), which a fragment
/// validated in isolation, with no caller-supplied move-state, has no
/// sound way to reconstruct. A fragment that would move an affine
/// binding already consumed elsewhere in the real program is *not*
/// caught here; that's the caller's responsibility once the fragment is
/// actually spliced in and the whole function is re-checked for real.
///
/// A fragment containing `return` type-checks against `Ty::Unit` as its
/// enclosing function's return type — a fragment validated in isolation
/// has no real enclosing function to ask, and this covers the realistic
/// case (splicing a small, `return`-free subexpression) honestly rather
/// than guessing.
pub fn validate_fragment(json: &str, expected_ty: &Ty, env: &FragmentEnv) -> Result<Expr, Vec<crate::Diagnostic>> {
    let expr: Expr = serde_json::from_str(json).map_err(|e| {
        vec![crate::Diagnostic::Type(TypeError {
            kind: TypeErrorKind::MalformedFragmentJson { message: e.to_string() },
            span: Span { line: 0, col: 0 },
        })]
    })?;

    let mut checker = Checker { sigs: HashMap::new(), errors: Vec::new(), registry: TypeRegistry::empty(), silent: false };
    let mut scopes = Scopes::new();
    for (name, ty) in &env.0 {
        scopes.define(name, ty.clone());
    }
    checker.check(&expr, expected_ty, &Ty::Unit, &mut scopes);

    if checker.errors.is_empty() {
        Ok(expr)
    } else {
        Err(checker.errors.into_iter().map(crate::Diagnostic::Type).collect())
    }
}
