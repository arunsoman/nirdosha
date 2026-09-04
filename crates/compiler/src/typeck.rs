//! Static type checker — runs before the interpreter ever sees the program.
//! This is the milestone flagged at the end of Phase 0: ownership analysis
//! and refinement types (docs/goal.md rows 1, 4) both need a fully-typed AST to
//! work over, and until now nothing built one ahead of time — the old
//! interpreter checked types *as it executed*, which is Python's discipline,
//! not the one docs/goal.md asks for.
//!
//! Design notes worth keeping visible, not just in commit history:
//!
//! - **Error recovery, not fail-fast.** A mismatch is recorded and checking
//!   continues with a poison type (`Ty::Error`) standing in for the bad
//!   expression, so one mistake doesn't hide the next five behind it. A
//!   compiler that stops at the first error is a worse interface for an
//!   agent's self-repair loop (docs/goal.md row 9) than one that reports
//!   everything it can see in one pass.
//! - **Integer literals are flexible, declared variables are not.** `n - 1`
//!   type-checks against whatever `n`'s declared width is; two variables of
//!   *different* declared widths do not implicitly convert to each other —
//!   that's docs/goal.md §3's "no implicit conversions" core-language rule,
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
    /// declared return type must be `unit`. As of docs/SANDBOXING.md's layer
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
    /// (`docs/TRANSACT.md`), so anything else is exactly as wrong as a
    /// non-`bool` `if` condition, just for this one named position.
    TransactVerifyMustReturnBool { found: Ty },
    /// A `transact` slot named a builtin instead of a user-defined
    /// function — see `infer_transact_slot`'s doc comment for why every
    /// slot is restricted to a user function (mirrors
    /// `CannotSpawnBuiltin`'s identical restriction and identical
    /// underlying reason: no declared-signature table exists for
    /// builtins to look a return type up from).
    CannotUseBuiltinInTransact { name: String },
    /// `transact`'s optional `precheck` slot must return `bool`, exactly
    /// the same rule and reason as `TransactVerifyMustReturnBool` — it
    /// decides whether the block aborts before anything durable is ever
    /// written, the same "checked like an `if` condition" treatment.
    TransactPrecheckMustReturnBool { found: Ty },
    /// `transact`'s `network` slot must reference the implicit `txn_id:
    /// str` binding somewhere in its own argument list — the idempotency
    /// key a crash-replayed resend relies on the downstream system to
    /// dedupe (`docs/TRANSACT.md`'s durability section: no local mechanism can
    /// make `network` itself exactly-once without that cooperation, so
    /// the language at least forces every `network` call to carry the
    /// key that makes it possible). Checked syntactically — `txn_id` must
    /// appear as a bare argument expression — not semantically; this
    /// can't prove the callee actually *uses* it, only that it received
    /// it, the same honesty limit `TransactVerifyMustReturnBool`'s
    /// "checked like an `if`" analogy already accepts elsewhere in this
    /// construct.
    TransactNetworkMustUseTxnId,
    /// `transact`'s `verify` slot's arguments must each be exactly the
    /// implicit `network` or `txn_id` binding -- never an outer-scope
    /// variable from the enclosing function. This is what makes crash
    /// replay from the narrow `"pending"` window (a crash during/right
    /// after `network`, before `verify` ever ran in the original
    /// process) *fully* reconstructable: `network`'s result and `txn_id`
    /// are always known to `Interpreter::replay_pending_transactions`,
    /// so `verify`'s exact original call can always be rebuilt from
    /// them, with no dependence on state that crashed away.  Matches
    /// `verify`'s own documented contract anyway ("inspect `network`'s
    /// outcome," per `docs/TRANSACT.md`) -- every worked example already
    /// satisfies this (`verify: check(network)`).
    TransactVerifyArgsMustBeImplicitBindings,
    /// A `transact` slot's argument, or `network`/`verify`'s own declared
    /// return type, isn't one of the four plain scalars
    /// (`Ty::is_transact_scalar`) the durability log can serialize and
    /// replay after a crash — a resource handle (`db`/`tcp`/`thread`/
    /// `sandbox`) or an aggregate/struct/enum value can't survive a
    /// process restart, so nothing that crosses `transact`'s durability
    /// boundary is allowed to be one.
    TransactValueNotDurable { where_: String, found: Ty },
    /// This function's declared `effect(...)` annotation (`ast::FnDecl::
    /// declared_effects`) didn't list `missing` — but `effects::
    /// infer_effects` found it in the body anyway (directly, or
    /// transitively through a call). A declared effect the body never
    /// uses is not an error (`docs/goal.md` §3's effect-subsumption
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
    /// (`docs/nirdosha_row11_amendment.md` §3.2).
    DuplicateConstructor(String),
    /// Two fields of the same `struct` share a name.
    DuplicateField { struct_name: String, field: String },
    /// A bare `ident` in type position (a `let`/param/return/field/
    /// payload type) didn't resolve to any declared `struct`/`enum`.
    UnknownType(String),
    /// A qualified (`Mod::Name`) reference resolved to a real,
    /// namespaced declaration, but that declaration isn't `pub` and the
    /// referencing site is outside its own `ns` (`docs/ROADMAP.md` Track F,
    /// F2 piece 2). Never fires for a bare (unqualified) reference —
    /// those can only ever resolve to a non-namespaced declaration in
    /// the first place (`ast::scope_key`'s doc comment), which is
    /// always effectively `pub`.
    PrivateItem(String),
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
    /// (`docs/nirdosha_row11_amendment.md` §3.4), so exhaustiveness means
    /// "every declared variant, exactly once."
    NonExhaustiveMatch { enum_name: String, missing: Vec<String> },
    /// A literal-pattern `match` (scrutinee `str`/`i64`/`bool`, not an
    /// enum) has no trailing `_` arm -- a literal domain isn't closed
    /// the way an enum's variant set is, so there's no way to prove
    /// every case is covered without one.
    NonExhaustiveLiteralMatch { found: Ty },
    /// `_` appeared somewhere other than the last arm of a literal
    /// `match` -- every arm after it would be unreachable.
    WildcardArmNotLast,
    /// A literal `match`'s arm pattern is a literal of the wrong type
    /// for the scrutinee (e.g. an `i64` pattern against a `str`
    /// scrutinee).
    LiteralPatternTypeMismatch { scrutinee_ty: Ty, pattern_ty: Ty },
    /// A `match` on an enum scrutinee had an arm written as a literal/
    /// `_` pattern instead of naming one of the enum's variants.
    MatchArmMustBeVariant { enum_name: String },
    /// A `match` on a `str`/`i64`/`bool` scrutinee had an arm written as
    /// a bare variant-style name instead of a literal/`_` pattern.
    MatchArmMustBeLiteral { scrutinee_ty: Ty },
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
    /// constructor's own arguments (`docs/nirdosha_row11_amendment.md` has no
    /// turbofish-style explicit-type-argument syntax at a call site at
    /// all — §3.1's "Nirdosha never uses `<...>` for type application" —
    /// so there is no third way to supply one). The same shape of gap
    /// `chan`'s own `ChannelNeedsExplicitType` already has, generalized
    /// from "no expected type at all" to "no expected type *and* the
    /// arguments alone don't determine every parameter."
    GenericConstructorNeedsExplicitType { name: String },
    /// `name` is a `requires`-gated function (`ast::FnDecl::requires`)
    /// referenced somewhere other than `acquire`'s own callee position —
    /// a direct call (`transfer_funds(500)`) or a bare value-reference
    /// (`let f = transfer_funds`). This *is* the enforcement: the only
    /// way to end up holding a callable value for a gated function is to
    /// go through `acquire` and present a matching proof (see
    /// `ast::Requirement`).
    PrivilegedFnNotAcquired { name: String, requirement: Requirement },
    /// `acquire`'s callee named a real function, but one with no
    /// `requires(...)` annotation at all — nothing to acquire (an
    /// ordinary function is already a first-class value, just by naming
    /// it directly).
    AcquireOfUngatedFn(String),
    /// A direct `RoleView(...)`/`ClaimView(...)` constructor call —
    /// these two prelude structs are `acquire`'s proof types (see
    /// `Requirement::proof_ty`), meant to be producible only by
    /// `check_role`/`extract_claim` after a real `oidc_validate_token`.
    /// Every other prelude/user struct is legitimately constructible via
    /// the ordinary `Name(args...)` call syntax (`infer_struct_
    /// construction`'s own doc comment) — these two are singled out
    /// because letting user code build one directly is a forged proof:
    /// `acquire gated_fn(RoleView("admin"))` would satisfy the type
    /// checker's `requirement.proof_ty()` check with zero relation to
    /// any validated identity, defeating `requires`/`acquire` entirely.
    UnforgeableProofConstruction(String),

    // ---- Row 12's UI DSL (`screen`/`dashboard`) ----------------------
    /// `screen <Name>` where `<Name>` is not a declared struct.
    UnknownScreenStruct(String),
    /// `validate <fn_name>` where `<fn_name>` is not a declared `fn`
    /// (`docs/ROADMAP.md` Track F, F3).
    ValidateFnNotFound(String),
    /// A `validate <fn_name> { ... }` entry whose key isn't `pre`/`post`
    /// — the only two contextual keys this block recognizes.
    ValidateUnknownKey { fn_name: String, key: String },
    /// `field <fname>` inside a `screen <Struct>` where `<Struct>` has no
    /// field named `<fname>`.
    UnknownScreenField { struct_name: String, field_name: String },
    /// `layout { action "<label>" }` (`docs/ROADMAP.md` Track F, F1) where
    /// `<label>` matches neither a custom `screen { action "..." -> ... }`
    /// nor one of the five inferred CRUD kinds (`"list"`/`"create"`/
    /// `"update"`/`"delete"`/`"get"`).
    UnknownLayoutAction { struct_name: String, label: String },
    /// `field <name> { render: "searchable_select" source: <name> }`
    /// where `<name>` names neither a declared struct nor a real
    /// function — the one shape `check_fn_ref`'s bare "must be a fn"
    /// rule doesn't fit, since `source` may legitimately name either.
    SearchableSelectSourceNotFound { struct_name: String, field_name: String, source: String },
    /// `layout { timeline { } }` with no `source: <fn>` entry at all —
    /// distinct from `ScreenFnNotAnIdent` (which fires when `source` is
    /// present but not a bare identifier); this fires when the key is
    /// missing outright.
    TimelineWidgetMissingSource { struct_name: String },
    /// A `list`/`create`/`update`/`delete` screen entry, or an `action`'s
    /// `->` target, that doesn't resolve to a real user-defined function.
    ScreenFnNotFound { key: String, fn_name: String },
    /// `list`/`create`/`update`/`delete`, or an `action`'s `->` target,
    /// whose value isn't a bare function name (`Expr::Ident`) — e.g. a
    /// string or integer literal was written where a function reference
    /// was expected.
    ScreenFnNotAnIdent(String),
    /// A `view`/`edit` field-visibility entry that isn't a `role(...)`/
    /// `claim(...)` call with string-literal arguments — the same shape
    /// `requires(...)` already enforces via `ast::Requirement`, reused
    /// here rather than inventing a second visibility grammar.
    InvalidVisibilityExpr { key: String },
    /// A `field <name> { pattern: ... }` whose value isn't a plain string
    /// literal, or a `field <name> { min/max: ... }` whose value isn't a
    /// plain int/float literal — the only shapes `serve.rs`'s runtime
    /// enforcement (and `ui_gen_template.html`'s client-side mirror) know
    /// how to read. Also reused for `state Name { label: ... }`
    /// (`docs/WORKFLOW.md`'s "state ownership" section) whose value isn't a
    /// plain string literal — same "must be a literal, not a computed
    /// expression" shape, different construct.
    InvalidFieldValidationExpr { key: String },
    /// `field <name> { pattern: "..." }` where `<name>`'s declared type
    /// isn't `str` (a regex has nothing to match against a number/bool/
    /// enum), or `field <name> { min/max: ... }` where `<name>`'s type
    /// isn't one of Nirdosha's numeric scalars — the same "shape must
    /// match what the field can actually carry" posture `check_screen`
    /// already applies to `view`/`edit`.
    FieldValidationTypeMismatch { struct_name: String, field_name: String, key: String, field_ty: String },
    /// A `field <name> { pattern: "..." }` string that doesn't compile as
    /// a valid regex (`regex` crate syntax) — caught here, at typeck
    /// time, rather than surfacing as a confusing 500 the first time an
    /// admin submits a form that happens to hit `create_`/`update_`.
    InvalidRegexPattern { struct_name: String, field_name: String, error: String },
    /// `field <name> { format: "..." }` where `"..."` isn't one of
    /// `ast::well_known_format_pattern`'s fixed set — that function is
    /// the single source of truth for valid names, so this only fires
    /// when it returns `None`.
    UnknownFieldFormat { format: String },
    /// `field <name> { pattern: ..., format: ... }` declared together on
    /// the same field — ambiguous which one actually constrains the
    /// value, so rejected rather than silently picking one.
    ConflictingPatternAndFormat { struct_name: String, field_name: String },
    /// A `dashboard` `tile`/`chart` target that doesn't resolve to a real
    /// user-defined function.
    UnknownDashboardFn { metric_kind: String, fn_name: String },

    // ---- Track E1's `workspace`/`panel` DSL ---------------------------
    /// `workspace <Name>` with no `subject: <Struct>` entry at all.
    WorkspaceMissingSubject(String),
    /// `subject: <expr>` where `<expr>` isn't a bare struct name
    /// (`Expr::Ident`) — the same "must name a function/type directly"
    /// shape `ScreenFnNotAnIdent` already enforces for `screen`'s own
    /// `list`/`create`/etc.
    WorkspaceSubjectNotAnIdent(String),
    /// `subject: <Name>` where `<Name>` isn't a declared struct.
    UnknownWorkspaceSubject { workspace: String, struct_name: String },
    /// `subject: <Struct>` where `<Struct>` has no `id: i64` field — the
    /// primary-key convention every `get_<S>`/`update_<S>`/`delete_<S>`
    /// (and, here, every panel's `source` fn) already assumes.
    WorkspaceSubjectMissingId { workspace: String, struct_name: String },
    /// `panel "<label>" { ... }` with no `source: <fn>` entry at all.
    PanelMissingSource { workspace: String, panel: String },
    /// `source: <expr>` where `<expr>` isn't a bare function name.
    PanelSourceNotAnIdent { workspace: String, panel: String },
    /// `source: <fn>` where `<fn>` doesn't take exactly one `i64`
    /// parameter and return `Result(json, _)` — the one shape
    /// `ui_gen_template.html`'s `renderWorkspace` (`callFn(source,
    /// {id})`, expecting a JSON array/object back) actually calls.
    PanelSourceWrongShape { workspace: String, panel: String, fn_name: String },

    // ---- Track E2's `render:` DSL (on `visual` and, reused, `panel`) ---
    /// `visual "..." -> fn { render: "..." }`, `panel "..." { render:
    /// "..." }`, or `field <name> { render: "..." }` (Track E2/E3) whose
    /// `render` value is a string literal, but not one of that
    /// particular context's own closed set. `context` is a pre-formatted
    /// description of where this fired (`"visual \"...\""`, `"panel
    /// \"...\" in workspace ..."`, `"field <name>\" on <struct>"`), and
    /// `allowed` a pre-formatted, already-quoted list of what would have
    /// been accepted (`"\"graph\", \"heatmap\", \"timeline\""` or just
    /// `"\"countdown\""`) — one shared variant/check across all three
    /// contexts rather than three near-identical ones. A non-string-
    /// literal `render` value is caught by the existing, more general
    /// `InvalidFieldValidationExpr { key: "render" }` instead — this
    /// variant only ever fires once that shape check already passed.
    UnknownRenderValue { context: String, render: String, allowed: String },

    // ---- Track E4's `action { show_result: true }` --------------------
    /// `action "..." -> fn { show_result: true }` (on a `screen` or,
    /// reused, a `workspace` `panel`) where `fn`'s return type isn't
    /// `Result(json, _)` — nothing for `ui_gen_template.html`'s result
    /// modal to actually show beyond what the row/panel refresh already
    /// implies. `context` is a pre-formatted description (`"action
    /// \"...\" on screen ..."` or `"...\" in workspace ..."`), the same
    /// two-caller-one-check shape `UnknownRenderValue` already has.
    ShowResultRequiresJsonResult { context: String, fn_name: String },
    /// A user `fn`'s parameter or return type is (or contains) `str` —
    /// the "enum favoring" rule: `str` may not cross a user function's
    /// call boundary, so categorical string data belongs in a real
    /// `enum` and free text belongs in a carrier struct (e.g. `Text`)
    /// instead. Builtins (`http_*`/`db_*`/`json_*`/...) and struct/enum
    /// constructors are unaffected — this only fires for `program.fns`
    /// entries (`check_fn`'s own loop), which is what makes those two
    /// exempt without any special-casing. `param_name` is `None` when
    /// the offending position is the return type rather than a
    /// parameter. The one structural exception is a parameter literally
    /// named `txn_id`: `transact`'s synthesized idempotency-key binding
    /// must stay a plain `str` scalar for WAL durability
    /// (`Ty::is_transact_scalar`), so `check_fn` skips this check for it.
    StrInFnSignature { fn_name: String, param_name: Option<String> },
    /// `__workflow_advance`/`__workflow_link_advance`'s `event` argument
    /// wasn't a value of some declared `enum` type — every
    /// `workflow_lower.rs`-synthesized `<Workflow>Event` is one, but this
    /// isn't pinned to one fixed `Ty` (it's a different enum per
    /// workflow), so `infer_builtin_call` checks the shape here instead.
    WorkflowEventArgMustBeEnum { fn_name: String, found: Ty },
    /// `__workflow_start`'s `data` argument, or `__workflow_link_advance`'s
    /// `token` argument, wasn't a value of some declared `struct` type —
    /// every `workflow_lower.rs`-synthesized `<Workflow>Data`/
    /// `<Workflow>LinkToken` is one; same "not pinned to one fixed `Ty`"
    /// reasoning as `WorkflowEventArgMustBeEnum`.
    WorkflowStructArgMustBeStruct { fn_name: String, arg: String, found: Ty },
    /// A `workflow` declares a non-`terminal` `state` with no outgoing
    /// `on` transition at all — a dead end no `advance_*` call could ever
    /// leave, almost certainly a missing `on ... -> ...` line rather than
    /// an intentional trap (a genuine dead end should be `terminal`).
    WorkflowStateHasNoTransitions { workflow: String, state: String },
    /// A `workflow`'s `on <Event> -> <Target>` names a `<Target>` that
    /// isn't one of that same workflow's own declared `state`s.
    WorkflowUnknownTargetState { workflow: String, state: String, target: String },
    /// Two `on` transitions out of the *same* `state` share an event
    /// name — `advance_<workflow>` couldn't dispatch unambiguously.
    WorkflowDuplicateEvent { workflow: String, state: String, event: String },
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
                 a `chan` argument instead, or use `stop`'s exit code; see docs/SANDBOXING.md)"
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
            TypeErrorKind::TransactPrecheckMustReturnBool { found } => write!(
                f,
                "{line}:{col}: `transact`'s `precheck` slot must return `bool`, found `{}`",
                found.name()
            ),
            TypeErrorKind::TransactNetworkMustUseTxnId => write!(
                f,
                "{line}:{col}: `transact`'s `network` slot must pass the implicit `txn_id` binding as one of its arguments"
            ),
            TypeErrorKind::TransactVerifyArgsMustBeImplicitBindings => write!(
                f,
                "{line}:{col}: `transact`'s `verify` slot's arguments must each be exactly `network` or `txn_id` -- \
                 no outer variable, so a crash before `verify` ever ran can still safely replay it"
            ),
            TypeErrorKind::TransactValueNotDurable { where_, found } => write!(
                f,
                "{line}:{col}: `transact`'s durability log can't record `{}` ({where_}) -- only i8/16/32/64, u8/16/32/64, usize, f64, bool, and str are allowed to cross a `transact` boundary",
                found.name()
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
            TypeErrorKind::PrivateItem(n) => {
                write!(f, "{line}:{col}: `{n}` is private to its own module — mark it `pub` to reference it from outside")
            }
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
                "{line}:{col}: `match` needs an enum, `str`, `i64`, or `bool` scrutinee, found `{}`",
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
            TypeErrorKind::NonExhaustiveLiteralMatch { found } => write!(
                f,
                "{line}:{col}: `match` on `{}` needs a trailing `_` arm -- `str`/`i64`/`bool` aren't closed the way an enum's variants are",
                found.name()
            ),
            TypeErrorKind::WildcardArmNotLast => {
                write!(f, "{line}:{col}: `_` must be the last arm of a `match`")
            }
            TypeErrorKind::LiteralPatternTypeMismatch { scrutinee_ty, pattern_ty } => write!(
                f,
                "{line}:{col}: `match` on `{}` has an arm pattern of type `{}`",
                scrutinee_ty.name(),
                pattern_ty.name()
            ),
            TypeErrorKind::MatchArmMustBeVariant { enum_name } => write!(
                f,
                "{line}:{col}: `match` on enum `{enum_name}` arms must name a variant, not a literal or `_` pattern"
            ),
            TypeErrorKind::MatchArmMustBeLiteral { scrutinee_ty } => write!(
                f,
                "{line}:{col}: `match` on `{}` arms must be a literal or `_`, not a variant-style name",
                scrutinee_ty.name()
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
            TypeErrorKind::PrivilegedFnNotAcquired { name, requirement } => write!(
                f,
                "{line}:{col}: `{name}` {} — acquire it first (`acquire {name}(proof)`) \
                 and call the resulting value instead",
                requirement.describe()
            ),
            TypeErrorKind::AcquireOfUngatedFn(name) => write!(
                f,
                "{line}:{col}: `{name}` has no `requires(...)` annotation — nothing to acquire; \
                 name it directly to get a first-class value (`let f = {name}`)"
            ),
            TypeErrorKind::UnforgeableProofConstruction(name) => write!(
                f,
                "{line}:{col}: `{name}` can't be constructed directly — it's a proof value only \
                 `check_role`/`extract_claim` may produce, against a real validated identity"
            ),
            TypeErrorKind::UnknownScreenStruct(name) => {
                write!(f, "{line}:{col}: `screen {name}` — no struct named `{name}` is declared")
            }
            TypeErrorKind::ValidateFnNotFound(name) => {
                write!(f, "{line}:{col}: `validate {name}` — no function named `{name}` is declared")
            }
            TypeErrorKind::ValidateUnknownKey { fn_name, key } => write!(
                f,
                "{line}:{col}: `validate {fn_name}` — `{key}` isn't a recognized key here, only `pre`/`post` are"
            ),
            TypeErrorKind::UnknownScreenField { struct_name, field_name } => write!(
                f,
                "{line}:{col}: `field {field_name}` — struct `{struct_name}` has no field named `{field_name}`"
            ),
            TypeErrorKind::UnknownLayoutAction { struct_name, label } => write!(
                f,
                "{line}:{col}: `layout {{ action \"{label}\" }}` — `screen {struct_name}` has no custom action \
                 labeled `{label}`, and it isn't one of `list`/`create`/`update`/`delete`/`get`"
            ),
            TypeErrorKind::SearchableSelectSourceNotFound { struct_name, field_name, source } => write!(
                f,
                "{line}:{col}: `field {field_name}` on `{struct_name}` — `source: {source}` names neither a \
                 declared struct nor a declared function"
            ),
            TypeErrorKind::TimelineWidgetMissingSource { struct_name } => write!(
                f,
                "{line}:{col}: `layout {{ timeline {{ ... }} }}` in `screen {struct_name}` needs a `source: <fn>` entry"
            ),
            TypeErrorKind::ScreenFnNotFound { key, fn_name } => write!(
                f,
                "{line}:{col}: `{key}: {fn_name}` — no function named `{fn_name}` is declared"
            ),
            TypeErrorKind::ScreenFnNotAnIdent(key) => write!(
                f,
                "{line}:{col}: `{key}` must name a function directly (e.g. `{key}: list_product`)"
            ),
            TypeErrorKind::InvalidVisibilityExpr { key } => write!(
                f,
                "{line}:{col}: `{key}` must be `role(\"...\")` or `claim(\"...\", \"...\")` \
                 with string-literal arguments"
            ),
            TypeErrorKind::InvalidFieldValidationExpr { key } if key == "pattern" => {
                write!(f, "{line}:{col}: `pattern` must be a string literal (a regex)")
            }
            TypeErrorKind::InvalidFieldValidationExpr { key } if key == "label" => {
                write!(f, "{line}:{col}: `label` must be a string literal")
            }
            TypeErrorKind::InvalidFieldValidationExpr { key } if key == "render" => {
                write!(f, "{line}:{col}: `render` must be a string literal")
            }
            TypeErrorKind::InvalidFieldValidationExpr { key } if key == "show_result" => {
                write!(f, "{line}:{col}: `show_result` must be a boolean literal (`true`/`false`)")
            }
            TypeErrorKind::InvalidFieldValidationExpr { key } => {
                write!(f, "{line}:{col}: `{key}` must be an int or float literal")
            }
            TypeErrorKind::FieldValidationTypeMismatch { struct_name, field_name, key, field_ty } if key == "pattern" => write!(
                f,
                "{line}:{col}: `field {field_name} {{ pattern: ... }}` on `{struct_name}` — \
                 `{field_name}` is `{field_ty}`, not `str`; `pattern` only applies to `str` fields"
            ),
            TypeErrorKind::FieldValidationTypeMismatch { struct_name, field_name, key, field_ty } if key == "render" => write!(
                f,
                "{line}:{col}: `field {field_name} {{ render: \"countdown\" }}` on `{struct_name}` — \
                 `{field_name}` is `{field_ty}`, not an integer; `render: \"countdown\"` only applies to an integer field"
            ),
            TypeErrorKind::FieldValidationTypeMismatch { struct_name, field_name, key, field_ty } => write!(
                f,
                "{line}:{col}: `field {field_name} {{ {key}: ... }}` on `{struct_name}` — \
                 `{field_name}` is `{field_ty}`, not numeric; `{key}` only applies to a numeric field"
            ),
            TypeErrorKind::InvalidRegexPattern { struct_name, field_name, error } => write!(
                f,
                "{line}:{col}: `field {field_name} {{ pattern: ... }}` on `{struct_name}` — \
                 not a valid regex: {error}"
            ),
            TypeErrorKind::UnknownFieldFormat { format } => write!(
                f,
                "{line}:{col}: `format: \"{format}\"` — not a recognized format; use one of \
                 \"email\", \"phone\", \"date\", \"url\", \"uuid\", or write your own `pattern: \"<regex>\"`"
            ),
            TypeErrorKind::ConflictingPatternAndFormat { struct_name, field_name } => write!(
                f,
                "{line}:{col}: `field {field_name}` on `{struct_name}` declares both `pattern` \
                 and `format` — pick one"
            ),
            TypeErrorKind::UnknownDashboardFn { metric_kind, fn_name } => write!(
                f,
                "{line}:{col}: `{metric_kind} ... -> {fn_name}` — no function named `{fn_name}` is declared"
            ),
            TypeErrorKind::WorkspaceMissingSubject(name) => write!(
                f,
                "{line}:{col}: `workspace {name}` has no `subject: <Struct>` entry — every workspace must name the struct it's scoped per-instance-of"
            ),
            TypeErrorKind::WorkspaceSubjectNotAnIdent(name) => write!(
                f,
                "{line}:{col}: `workspace {name}` — `subject` must name a struct directly (e.g. `subject: Case`)"
            ),
            TypeErrorKind::UnknownWorkspaceSubject { workspace, struct_name } => write!(
                f,
                "{line}:{col}: `workspace {workspace} {{ subject: {struct_name} }}` — no struct named `{struct_name}` is declared"
            ),
            TypeErrorKind::WorkspaceSubjectMissingId { workspace, struct_name } => write!(
                f,
                "{line}:{col}: `workspace {workspace} {{ subject: {struct_name} }}` — `{struct_name}` has no `id: i64` field, \
                 which every workspace's per-instance URL (`#/ws/{workspace}/<id>`) and every panel's `source` fn assume"
            ),
            TypeErrorKind::PanelMissingSource { workspace, panel } => write!(
                f,
                "{line}:{col}: `panel \"{panel}\"` in `workspace {workspace}` has no `source: <fn>` entry"
            ),
            TypeErrorKind::PanelSourceNotAnIdent { workspace, panel } => write!(
                f,
                "{line}:{col}: `panel \"{panel}\"` in `workspace {workspace}` — `source` must name a function directly"
            ),
            TypeErrorKind::PanelSourceWrongShape { workspace, panel, fn_name } => write!(
                f,
                "{line}:{col}: `panel \"{panel}\"` in `workspace {workspace}` — `source: {fn_name}` must take exactly one \
                 `i64` parameter and return `Result(json, _)`"
            ),
            TypeErrorKind::UnknownRenderValue { context, render, allowed } => write!(
                f,
                "{line}:{col}: {context} {{ render: \"{render}\" }} — not a recognized render kind; use one of {allowed}"
            ),
            TypeErrorKind::ShowResultRequiresJsonResult { context, fn_name } => write!(
                f,
                "{line}:{col}: {context} {{ show_result: true }} — `{fn_name}` must return `Result(json, _)` \
                 for there to be anything to show"
            ),
            TypeErrorKind::StrInFnSignature { fn_name, param_name: Some(param_name) } => write!(
                f,
                "{line}:{col}: `fn {fn_name}`'s parameter `{param_name}` is (or contains) `str` — \
                 `str` can't cross a function boundary; use an `enum` for categorical data or a \
                 carrier struct (e.g. `Text`) for free text"
            ),
            TypeErrorKind::StrInFnSignature { fn_name, param_name: None } => write!(
                f,
                "{line}:{col}: `fn {fn_name}`'s return type is (or contains) `str` — \
                 `str` can't cross a function boundary; use an `enum` for categorical data or a \
                 carrier struct (e.g. `Text`) for free text"
            ),
            TypeErrorKind::WorkflowEventArgMustBeEnum { fn_name, found } => write!(
                f,
                "{line}:{col}: `{fn_name}`'s `event` argument must be a workflow event enum value, found {found:?}"
            ),
            TypeErrorKind::WorkflowStructArgMustBeStruct { fn_name, arg, found } => write!(
                f,
                "{line}:{col}: `{fn_name}`'s `{arg}` argument must be a workflow-declared struct value, found {found:?}"
            ),
            TypeErrorKind::WorkflowStateHasNoTransitions { workflow, state } => write!(
                f,
                "{line}:{col}: `workflow {workflow}`'s state `{state}` is not `terminal` but declares no \
                 outgoing `on` transition — either add one or mark it `terminal`"
            ),
            TypeErrorKind::WorkflowUnknownTargetState { workflow, state, target } => write!(
                f,
                "{line}:{col}: `workflow {workflow}`'s state `{state}` has a transition to `{target}`, \
                 which is not a state declared in this workflow"
            ),
            TypeErrorKind::WorkflowDuplicateEvent { workflow, state, event } => write!(
                f,
                "{line}:{col}: `workflow {workflow}`'s state `{state}` declares the event `{event}` on \
                 more than one outgoing transition"
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
    /// Mirrors `ast::FnDecl::requires` — carried here too so every place
    /// that already looks a name up in `sigs` (direct calls, bare
    /// value-references, `acquire`) can enforce the gate without a
    /// second lookup table.
    requires: Option<Requirement>,
    /// Mirrors `ast::FnDecl::ns`/`exported` — carried here too, same
    /// reasoning as `requires` above: `infer_call`'s fn-call path needs
    /// `check_visibility`'s inputs right where it already has `sig` in
    /// hand, with no second lookup back into `program.fns` (`Checker`
    /// only ever borrows a `TypeRegistry`, not the whole `&Program`).
    ns: Option<String>,
    exported: bool,
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
    /// The `ns` of whatever struct/enum/fn declaration is currently
    /// being checked (the registration loop / `check_fn`'s own callers
    /// set this right before validating that one declaration's fields/
    /// payload/params/return/body) — `None` outside a real `module
    /// Ident { ... }` block, exactly as most of this program is
    /// (`docs/ROADMAP.md` Track F, F2). The *only* thing this is used for:
    /// deciding whether a qualified reference that resolved to a
    /// private (`exported: false`) namespaced item is a legal
    /// same-module self-reference or an illegal cross-module one — see
    /// `check_visibility`. Never affects *resolution* itself (which
    /// namespaced item, if any, a name refers to) — only bare vs.
    /// qualified spelling ever affects that, per `ast::scope_key`'s
    /// doc comment, so no "which module is currently being checked"
    /// state is needed for resolution, only for this one gate.
    current_ns: Option<String>,
    /// `docs/ROADMAP.md` Track G, G1 / `docs/ECOSYSTEM.md` §G1's Stage 1:
    /// name -> (declared param types, declared return type) for every
    /// builtin a plugin crate contributed (`plugin::signatures`), empty
    /// for every ordinary `typecheck`/`typecheck_optional_main` caller.
    /// Consulted everywhere `ast::is_builtin` already is (see
    /// `is_builtin_or_plugin`) so a plugin builtin is indistinguishable
    /// from a real one to every existing check — can't be spawned, can't
    /// be a `transact` slot, can't be shadowed by a `fn`/`struct`/enum
    /// variant — and by `infer_builtin_call`'s own early-return, which is
    /// the one place the actual signature gets used.
    plugins: HashMap<String, (Vec<Ty>, Ty)>,
}

/// Type-check a whole program. `Ok(())` means every function body is well
/// typed *and* proved to return on every path where its signature demands
/// a value — the interpreter should never be run on a program this
/// rejects. Requires a zero-arg `fn main()` — the entrypoint every
/// `run`/`build`/`emit-llvm` caller is about to execute.
pub fn typecheck(program: &Program) -> Result<(), Vec<TypeError>> {
    typecheck_impl(program, true, &HashMap::new())
}

/// Same as `typecheck`, plus a plugin crate's declared builtin
/// signatures (`docs/ROADMAP.md` Track G, G1 / `docs/ECOSYSTEM.md` §G1's
/// Stage 1) — `plugin::signatures(plugins)`'s shape, so every call to a
/// plugin builtin is checked exactly like a call to a real one:
/// resolvable, right arity, right argument/return types, and unusable in
/// a `spawn`/`transact` slot or as a `fn`/`struct`/variant name.
pub fn typecheck_with_plugins(program: &Program, plugins: &[crate::plugin::PluginBuiltin]) -> Result<(), Vec<TypeError>> {
    typecheck_impl(program, true, &crate::plugin::signatures(plugins))
}

/// Same checks as `typecheck`, but does not require a `fn main()`. For
/// callers that never execute an entrypoint — `nirdosha serve` (every
/// `fn` is reached individually via `POST /api/<fn>`, not through
/// `main`), `emit-ui` (reads `fn`/`struct`/`screen` declarations to
/// render UI, never runs anything), and `--sandbox-worker` (calls one
/// named fn directly, per that command's own doc comment: "there's no
/// `main` to run here"). A generated nirdosha-lane program is exactly
/// this shape by design (`nirdosha-default-pipeline-plan.md`: one
/// project, N `module { }` blocks of `fn`/`screen` constructs, no
/// `main`), so requiring one here would make every such program
/// permanently unservable.
pub fn typecheck_optional_main(program: &Program) -> Result<(), Vec<TypeError>> {
    typecheck_impl(program, false, &HashMap::new())
}

/// A non-fatal diagnostic — unlike `TypeError`, this never blocks
/// `typecheck`/compilation; a caller prints it (or not) and continues
/// either way. The one kind that exists today is `docs/ROADMAP.md` A10 /
/// `docs/API_TRUST_MODEL.md` §4's fix: `serve.rs::dispatch` is default-open
/// (any `fn` with no `requires(...)` and no `VerifiedIdentity` param is
/// callable by anyone, with no token at all), and nothing previously
/// surfaced that as anything other than silent. This doesn't change
/// `dispatch`'s runtime behavior — it makes the previously-silent case
/// visible at typecheck time instead, so an author has to see and
/// deliberately silence (`requires(public)`) an unintentionally-open
/// function rather than ship one by omission.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeWarning {
    pub kind: TypeWarningKind,
    pub span: Span,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum TypeWarningKind {
    /// `fn_name` has no `requires(...)`, no `requires(public)`, no
    /// `VerifiedIdentity` parameter, and no `db`/`mq` parameter (the last
    /// two would already 400 at `serve.rs::decode_value` regardless of
    /// this warning — excluded so the count matches what's actually
    /// reachable, same accounting `docs/API_TRUST_MODEL.md` §4 uses for its
    /// "79 of 246" figure). `nirdosha serve` will route it and any caller
    /// with no token at all can call it.
    UngatedFnReachableWithNoToken { fn_name: String },
    /// `docs/WORKFLOW.md`'s "state ownership" section: a non-terminal `state`
    /// with no `owner: role(...)/claim(...)` entry — any authenticated
    /// caller may fire one of its outgoing events (the same "default open
    /// unless you say otherwise" posture `UngatedFnReachableWithNoToken`
    /// already warns about for ordinary `fn`s, applied to a workflow
    /// state instead). Non-fatal for the same reason that one is: a
    /// state's owner genuinely is optional (a purely automatic, no-
    /// human-in-the-loop state, `WF-RIDE-003`'s `FareSettlement` shape),
    /// so this surfaces the *unintentional* case rather than forbidding
    /// the intentional one.
    WorkflowStateHasNoOwner { workflow: String, state: String },
}

impl std::fmt::Display for TypeWarning {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let Span { line, col } = self.span;
        match &self.kind {
            TypeWarningKind::UngatedFnReachableWithNoToken { fn_name } => write!(
                f,
                "{line}:{col}: warning: `{fn_name}` has no `requires(...)` and takes no `VerifiedIdentity` \
                 parameter — it will be callable by anyone with no token at all once served; add \
                 `requires(role: ...)`/`requires(claim: ..., ...)` to gate it, or `requires(public)` \
                 if that's intentional"
            ),
            TypeWarningKind::WorkflowStateHasNoOwner { workflow, state } => write!(
                f,
                "{line}:{col}: warning: `workflow {workflow}`'s state `{state}` has no `owner: role(...)` \
                 — any signed-in caller will be able to advance it once served; add an `owner` if that's \
                 not intended"
            ),
        }
    }
}

/// Walks every declared `fn` and reports `UngatedFnReachableWithNoToken`
/// for each one `serve.rs::dispatch` would route to an anonymous caller.
/// Deliberately a separate pass from `typecheck`/`typecheck_impl` above,
/// not folded into `Checker`'s error list — a warning must never fail a
/// build the way a `TypeError` does, and every existing caller of
/// `typecheck`/`typecheck_optional_main` already treats `Ok(())` as
/// "proceed"; adding a new fatal condition to that Result would be a
/// behavior change well beyond this fix's scope. Callers that want these
/// (`main.rs`'s CLI commands, `serve.rs::run`) call this alongside
/// `typecheck`, not instead of it.
pub fn ungated_fn_warnings(program: &Program) -> Vec<TypeWarning> {
    program
        .fns
        .iter()
        .filter(|f| is_reachable_with_no_token(f))
        .map(|f| TypeWarning { kind: TypeWarningKind::UngatedFnReachableWithNoToken { fn_name: f.name.clone() }, span: f.span })
        .collect()
}

fn is_reachable_with_no_token(f: &FnDecl) -> bool {
    if f.requires.is_some() || f.explicit_public {
        return false;
    }
    // `Option(VerifiedIdentity)` (`docs/WORKFLOW.md`'s "who submitted this"
    // section) is a deliberate, explicit "reachable with no token, and
    // I know it" declaration — the same reasoning `explicit_public`
    // above already gets exempted for, not an oversight the warning
    // should be surfacing.
    !f.params.iter().any(|p| is_verified_identity(&p.ty) || is_optional_verified_identity(&p.ty) || matches!(p.ty, Ty::Db | Ty::Mq))
}

fn is_verified_identity(ty: &Ty) -> bool {
    matches!(ty, Ty::Named(n, args) if n == "VerifiedIdentity" && args.is_empty())
}

/// `Option(VerifiedIdentity)` — the optional-identity shape `serve.rs::
/// dispatch` injects `Some(id)`/`None` for, never a 401 either way (see
/// that module's doc comment and `docs/WORKFLOW.md`'s "who submitted this"
/// section, the feature that motivated it).
fn is_optional_verified_identity(ty: &Ty) -> bool {
    matches!(ty, Ty::Named(n, args) if n == "Option" && args.len() == 1 && is_verified_identity(&args[0]))
}

/// Walks every declared `workflow`'s non-terminal `state`s and reports
/// `WorkflowStateHasNoOwner` for each one with no `owner` entry — the
/// workflow sibling of `ungated_fn_warnings` above, same "separate,
/// non-fatal pass" reasoning (a warning must never fail a build).
/// Terminal states are exempt: nothing is ever *decided* there (no
/// outgoing event to gate), only `on_entry`/`on_exit` actions run.
pub fn workflow_owner_warnings(program: &Program) -> Vec<TypeWarning> {
    program
        .workflows
        .iter()
        .flat_map(|w| {
            w.states.iter().filter(|s| !s.terminal && !s.entries.iter().any(|(k, _)| k == "owner")).map(|s| TypeWarning {
                kind: TypeWarningKind::WorkflowStateHasNoOwner { workflow: w.name.clone(), state: s.name.clone() },
                span: s.span,
            })
        })
        .collect()
}

/// Every `role(...)`/`claim(k, v)` string this program declares anywhere,
/// deduped and sorted — the demo-mode login screen's "what can I try"
/// catalog (`nirdosha serve` with no `--jwks-file`/`--issuer`/
/// `--audience`). Same "standalone `pub fn(program: &Program) -> ...`,
/// no `Checker` involved" shape as `ungated_fn_warnings`/
/// `workflow_owner_warnings` above.
///
/// Three sources, since role/claim strings appear in three unrelated
/// places in the grammar: a `fn`'s own `requires(...)` (`FnDecl::
/// requires`, typed as `Requirement`); a `screen <Struct> { field <name>
/// { view: role(...)/claim(...), edit: ... } }` field override
/// (`ScreenDecl.fields[].entries`, raw `Expr::Call` inside a `KvEntry` —
/// `ui_gen.rs::kv_gate` is the existing parser for this shape, but it's
/// private to that module; the ~10-line match is duplicated here rather
/// than exposing it, to avoid `typeck` taking a dependency on `ui_gen`,
/// the outer layer, for one small helper); and a `workflow`'s `state
/// Name { owner: role(...)/claim(...) }` (`WorkflowDecl.states[].
/// entries`, same shape again).
///
/// `role(...)` is any-of and can name more than one role in a single
/// call (`kv_gate`'s own doc comment) — every name is collected, not
/// just the first. Claims are collected as the exact `(key, value)`
/// pair a `requires(claim: ...)`/`claim(...)` demands, not just the
/// key, since that's the actually-actionable unit for someone trying to
/// self-assign a matching identity in the demo picker.
pub fn collect_role_claim_strings(program: &Program) -> (Vec<String>, Vec<(String, String)>) {
    let mut roles: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut claims: std::collections::BTreeSet<(String, String)> = std::collections::BTreeSet::new();

    // Local mirror of `ui_gen.rs::kv_gate` -- see this fn's own doc
    // comment for why it's duplicated rather than shared.
    fn role_claim_from_expr(v: &Expr, roles: &mut std::collections::BTreeSet<String>, claims: &mut std::collections::BTreeSet<(String, String)>) {
        match v {
            Expr::Call(name, args, _) if name == "role" => {
                for a in args {
                    if let Expr::Str(s, _) = a {
                        roles.insert(s.clone());
                    }
                }
            }
            Expr::Call(name, args, _) if name == "claim" && args.len() == 2 => {
                if let (Expr::Str(k, _), Expr::Str(val, _)) = (&args[0], &args[1]) {
                    claims.insert((k.clone(), val.clone()));
                }
            }
            _ => {}
        }
    }

    for f in &program.fns {
        match &f.requires {
            Some(Requirement::Role(r)) => {
                roles.insert(r.clone());
            }
            Some(Requirement::Claim(k, v)) => {
                claims.insert((k.clone(), v.clone()));
            }
            None => {}
        }
    }
    for screen in &program.screens {
        for field in &screen.fields {
            for (key, v) in &field.entries {
                if key == "view" || key == "edit" {
                    role_claim_from_expr(v, &mut roles, &mut claims);
                }
            }
        }
    }
    for workflow in &program.workflows {
        for state in &workflow.states {
            for (key, v) in &state.entries {
                if key == "owner" {
                    role_claim_from_expr(v, &mut roles, &mut claims);
                }
            }
        }
    }

    (roles.into_iter().collect(), claims.into_iter().collect())
}

fn typecheck_impl(
    program: &Program,
    require_main: bool,
    plugins: &HashMap<String, (Vec<Ty>, Ty)>,
) -> Result<(), Vec<TypeError>> {
    let registry = TypeRegistry::build(program);
    let mut c = Checker {
        sigs: HashMap::new(),
        errors: Vec::new(),
        registry,
        silent: false,
        current_ns: None,
        plugins: plugins.clone(),
    };

    // ---- Row 11: register struct/enum type names + their constructors --
    // Two independent namespaces, per `docs/nirdosha_row11_amendment.md` §3.1-
    // 3.2: `type_names` (struct/enum names, used in type position) and
    // `callable_names` (struct names *as constructors*, enum variant
    // names, function names, builtin names — anything `Expr::Call` can
    // name). A struct's name lives in both; an enum's own name lives only
    // in the first (only its variants are callable).
    //
    // Keyed by `ast::scope_key(decl.ns, &decl.name)` (`docs/ROADMAP.md` Track
    // F, F2), not the bare name — a top-level/prelude/legacy-`module`-
    // string declaration (`ns: None`) keys exactly as it always did
    // (`scope_key` is the identity function there), so every duplicate-
    // detection rule below is byte-for-byte unchanged for any program
    // that declares no real (`module Ident { }`) namespace. Two
    // declarations sharing a bare name but different `ns` now key
    // differently and no longer collide — the direct fix for the
    // documented `struct Pair` (vs. the prelude's own) and enum-variant
    // (`CurrencyCode::SAR`) collisions; see `scope_key`'s own doc
    // comment for why a namespaced item is then reachable only via its
    // qualified form, never bare, with zero new resolution-context
    // tracking needed anywhere.
    let mut type_names: HashMap<String, Span> = HashMap::new();
    let mut callable_names: HashMap<String, Span> = HashMap::new();

    for s in &program.structs {
        let key = scope_key(s.ns.as_deref(), &s.name);
        if type_names.insert(key.clone(), s.span).is_some() {
            c.error(TypeErrorKind::DuplicateType(s.name.clone()), s.span);
        }
        if (s.ns.is_none() && (is_builtin(&s.name) || c.plugins.contains_key(&s.name))) || callable_names.contains_key(&key) {
            c.error(TypeErrorKind::DuplicateConstructor(s.name.clone()), s.span);
        } else {
            callable_names.insert(key, s.span);
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
        let key = scope_key(e.ns.as_deref(), &e.name);
        if type_names.insert(key.clone(), e.span).is_some() {
            c.error(TypeErrorKind::DuplicateType(e.name.clone()), e.span);
        }
        c.check_duplicate_type_params(&e.type_params, e.span);
        let mut variant_names: std::collections::HashSet<&str> = std::collections::HashSet::new();
        for v in &e.variants {
            if !variant_names.insert(v.name.as_str()) {
                c.error(TypeErrorKind::DuplicateConstructor(v.name.clone()), v.span);
                continue;
            }
            // A namespaced enum's variants are reachable only via
            // explicit qualification (`Enum::Variant`/`Mod::Enum::
            // Variant` — `ast::TypeRegistry::find_variant`), never a
            // bare call, so they never actually share the flat
            // `callable_names` bare-call namespace with anything —
            // registering them there would only manufacture false
            // collisions between two unrelated modules' same-named
            // variants (exactly the bug this fixes), not catch a real
            // one.
            if e.ns.is_none() {
                if is_builtin(&v.name) || c.plugins.contains_key(&v.name) || callable_names.contains_key(v.name.as_str()) {
                    c.error(TypeErrorKind::DuplicateConstructor(v.name.clone()), v.span);
                } else {
                    callable_names.insert(v.name.clone(), v.span);
                }
            }
        }
    }

    // Every syntactically-declared type (struct fields, enum payloads) is
    // validated against the registry now, so a bogus `Ty::Named` never
    // silently reaches construction/field-access checking below — each
    // declaration's own `type_params` are in scope for its own fields/
    // payloads (layer 6, generics), nowhere else.
    for s in &program.structs {
        c.current_ns = s.ns.clone();
        for field in &s.fields {
            c.validate_ty(&field.ty, s.span, &s.type_params);
        }
    }
    for e in &program.enums {
        c.current_ns = e.ns.clone();
        for v in &e.variants {
            for t in &v.payload {
                c.validate_ty(t, v.span, &e.type_params);
            }
        }
    }
    c.current_ns = None;

    for f in &program.fns {
        let key = scope_key(f.ns.as_deref(), &f.name);
        if f.ns.is_none() && (is_builtin(&f.name) || c.plugins.contains_key(&f.name)) {
            c.error(TypeErrorKind::FnNameShadowsBuiltin(f.name.clone()), f.span);
            continue;
        }
        if c.sigs.contains_key(&key) {
            c.error(TypeErrorKind::DuplicateFn(f.name.clone()), f.span);
            continue;
        }
        if callable_names.contains_key(&key) {
            c.error(TypeErrorKind::DuplicateConstructor(f.name.clone()), f.span);
            continue;
        }
        c.current_ns = f.ns.clone();
        // Functions have no type-parameter list of their own
        // (`docs/nirdosha_row11_amendment.md` §2.2/§3.3 scopes generics to
        // struct/enum declarations only) — empty scope.
        for p in &f.params {
            c.validate_ty(&p.ty, f.span, &[]);
        }
        c.validate_ty(&f.ret, f.span, &[]);
        c.current_ns = None;
        c.sigs.insert(
            key,
            FnSig {
                params: f.params.iter().map(|p| p.ty.clone()).collect(),
                ret: f.ret.clone(),
                requires: f.requires.clone(),
                ns: f.ns.clone(),
                exported: f.exported,
            },
        );
    }

    match c.sigs.get("main") {
        None if require_main => c.error(TypeErrorKind::NoMainFn, Span { line: 0, col: 0 }),
        None => {}
        Some(sig) if !sig.params.is_empty() => {
            let span = program.fns.iter().find(|f| f.name == "main").unwrap().span;
            c.error(TypeErrorKind::MainMustTakeNoParams, span);
        }
        Some(_) => {}
    }

    // ---- Row 12: `screen`/`dashboard` DSL -----------------------------
    // Existence/shape checks only (this phase): struct/field/fn names
    // resolve, `view`/`edit` are well-formed `role(...)`/`claim(...)`
    // calls. Signature-shape enforcement for pagination/search/sort
    // params is a later phase — tracked in `crates/compiler/UI_DSL_TODO.md`.
    for screen in &program.screens {
        c.check_screen(screen);
    }
    if let Some(dash) = &program.dashboard {
        c.check_dashboard(dash);
    }
    for ws in &program.workspaces {
        c.check_workspace(ws);
    }
    for v in &program.validates {
        c.check_validate(v, program);
    }

    for f in &program.fns {
        c.check_fn(f);
    }

    // `WorkflowDecl`'s own structural rules (docs/WORKFLOW.md) — walked
    // independently of `workflow_lower.rs`'s desugared `FnDecl`s/
    // `EnumDecl`s (which get the exact same `check_fn`/enum-registration
    // treatment as any other declaration, above): this is the one pass
    // that still has the original `state`/`on_entry`/`on_exit` syntax to
    // point diagnostics at.
    for w in &program.workflows {
        c.check_workflow_decl(w);
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

    /// A `key: value` entry expected to name a function directly
    /// (`list`/`create`/`update`/`delete`, an action's `->` target, a
    /// dashboard tile/chart's `->` target) — `value` must be a bare
    /// `Expr::Ident` naming a real, user-defined function in `c.sigs`.
    /// Builtins are deliberately excluded: every one of these slots names
    /// a function the *program* defines to serve that screen/tile.
    fn check_fn_ref(&mut self, key: &str, value: &Expr) {
        match value {
            Expr::Ident(name, span) => {
                if !self.sigs.contains_key(name) {
                    self.error(
                        TypeErrorKind::ScreenFnNotFound { key: key.to_string(), fn_name: name.clone() },
                        *span,
                    );
                }
            }
            other => self.error(TypeErrorKind::ScreenFnNotAnIdent(key.to_string()), other.span()),
        }
    }

    /// `view: role("admin")` / `edit: claim("department", "cardiology")`
    /// — same shape `requires(...)` already accepts (`ast::Requirement`),
    /// checked structurally here since these arrive as ordinary `Expr`
    /// values (parsed by `parse_expr()`, not `parse_requires_annotation`).
    fn check_visibility_expr(&mut self, key: &str, value: &Expr) {
        let ok = match value {
            Expr::Call(name, args, _) if name == "role" => {
                !args.is_empty() && args.iter().all(|a| matches!(a, Expr::Str(..)))
            }
            Expr::Call(name, args, _) if name == "claim" => {
                args.len() == 2 && args.iter().all(|a| matches!(a, Expr::Str(..)))
            }
            _ => false,
        };
        if !ok {
            self.error(TypeErrorKind::InvalidVisibilityExpr { key: key.to_string() }, value.span());
        }
    }

    /// `field <name> { pattern: "..." }` — value must be a string literal,
    /// `<name>` must actually be a `str` field, and the literal itself
    /// must compile as a valid regex (`regex` crate syntax) — the same
    /// engine `serve.rs`'s runtime enforcement uses, so a pattern that
    /// typechecks is guaranteed to actually compile at request time too.
    /// `field_ty: None` means `fo.field_name` didn't resolve to a real
    /// field at all — already reported as `UnknownScreenField` by the
    /// caller, so this silently skips rather than piling on a second,
    /// confusing error about a field that doesn't exist.
    fn check_pattern_expr(&mut self, struct_name: &str, field_name: &str, field_ty: Option<&Ty>, value: &Expr) {
        let Expr::Str(pattern, _) = value else {
            self.error(TypeErrorKind::InvalidFieldValidationExpr { key: "pattern".to_string() }, value.span());
            return;
        };
        let Some(ty) = field_ty else { return };
        if *ty != Ty::Str {
            self.error(
                TypeErrorKind::FieldValidationTypeMismatch {
                    struct_name: struct_name.to_string(),
                    field_name: field_name.to_string(),
                    key: "pattern".to_string(),
                    field_ty: format!("{ty:?}"),
                },
                value.span(),
            );
            return;
        }
        if let Err(e) = regex::Regex::new(pattern) {
            self.error(
                TypeErrorKind::InvalidRegexPattern {
                    struct_name: struct_name.to_string(),
                    field_name: field_name.to_string(),
                    error: e.to_string(),
                },
                value.span(),
            );
        }
    }

    /// `field <name> { format: "..." }` — sugar over `pattern`
    /// (`ast::well_known_format_pattern`'s fixed vocabulary). Same
    /// string-literal-and-`str`-field shape checks as `check_pattern_expr`,
    /// plus validating the name itself is one of the known formats — no
    /// regex-compile check needed here, every entry in that table is a
    /// hardcoded, already-valid pattern.
    fn check_format_expr(&mut self, struct_name: &str, field_name: &str, field_ty: Option<&Ty>, value: &Expr) {
        let Expr::Str(format, _) = value else {
            self.error(TypeErrorKind::InvalidFieldValidationExpr { key: "format".to_string() }, value.span());
            return;
        };
        if crate::ast::well_known_format_pattern(format).is_none() {
            self.error(TypeErrorKind::UnknownFieldFormat { format: format.clone() }, value.span());
            return;
        }
        let Some(ty) = field_ty else { return };
        if *ty != Ty::Str {
            self.error(
                TypeErrorKind::FieldValidationTypeMismatch {
                    struct_name: struct_name.to_string(),
                    field_name: field_name.to_string(),
                    key: "format".to_string(),
                    field_ty: format!("{ty:?}"),
                },
                value.span(),
            );
        }
    }

    /// `field <name> { min: ... }` / `field <name> { max: ... }` — value
    /// must be an int or float literal, `<name>` must be one of
    /// Nirdosha's numeric scalar types (`Ty::is_numeric`). Same
    /// "already reported, don't pile on" skip for `field_ty: None` as
    /// `check_pattern_expr`.
    fn check_min_max_expr(&mut self, struct_name: &str, field_name: &str, key: &str, field_ty: Option<&Ty>, value: &Expr) {
        if !matches!(value, Expr::Int(..) | Expr::Float(..)) {
            self.error(TypeErrorKind::InvalidFieldValidationExpr { key: key.to_string() }, value.span());
            return;
        }
        let Some(ty) = field_ty else { return };
        if !ty.is_numeric() {
            self.error(
                TypeErrorKind::FieldValidationTypeMismatch {
                    struct_name: struct_name.to_string(),
                    field_name: field_name.to_string(),
                    key: key.to_string(),
                    field_ty: format!("{ty:?}"),
                },
                value.span(),
            );
        }
    }

    /// `field <name> { render: "..." }` (`docs/ROADMAP.md` Track E3, extended
    /// by Track F, F1 Phase A) — value must be a string literal from
    /// the fixed set `"countdown"` / `"badge"` / `"searchable_select"`
    /// (`docs/LANGUAGE.md`'s own original "candidate siblings, not designed
    /// yet" note named `"badge"` as a future extension of this exact
    /// key, not a one-off hack), reusing `check_render_expr`'s shape
    /// check. Each has its own field-type requirement — `"countdown"`
    /// only on an integer field (a unix-seconds deadline); `"badge"`
    /// only on an enum-typed field (colors the variant as a pill);
    /// `"searchable_select"` has no field-type requirement of its own
    /// (works on an `i64` id-shaped field or a struct-typed one alike)
    /// but requires a companion `source: <Struct|fn>` entry, checked
    /// separately by `check_screen`'s own field-entries loop since
    /// `source` is a sibling key, not part of `render`'s own value.
    fn check_field_render_expr(&mut self, struct_name: &str, field_name: &str, field_ty: Option<&Ty>, value: &Expr) {
        self.check_render_expr(
            format!("`field {field_name}` on `{struct_name}`"),
            value,
            |s| matches!(s, "countdown" | "badge" | "searchable_select"),
            "\"countdown\", \"badge\", \"searchable_select\"",
        );
        let Some(ty) = field_ty else { return };
        let Expr::Str(render, _) = value else {
            // Already reported (wrong literal shape) by `check_render_expr`
            // above — don't pile a second, confusing type-mismatch error
            // on top of a value that was never going to be valid regardless
            // of the field's type.
            return;
        };
        match render.as_str() {
            "countdown" if !ty.is_integer() => self.error(
                TypeErrorKind::FieldValidationTypeMismatch {
                    struct_name: struct_name.to_string(),
                    field_name: field_name.to_string(),
                    key: "render".to_string(),
                    field_ty: format!("{ty:?}"),
                },
                value.span(),
            ),
            "badge" => {
                let is_enum = matches!(ty, Ty::Named(n, _) if self.registry.is_enum(n));
                if !is_enum {
                    self.error(
                        TypeErrorKind::FieldValidationTypeMismatch {
                            struct_name: struct_name.to_string(),
                            field_name: field_name.to_string(),
                            key: "render".to_string(),
                            field_ty: format!("{ty:?}"),
                        },
                        value.span(),
                    );
                }
            }
            // "searchable_select": no field-type requirement, and
            // "countdown" on an integer field is already fine — nothing
            // further to check here in either case.
            _ => {}
        }
    }

    /// `field <name> { render: "searchable_select" source: <Struct|fn>
    /// }` (`docs/ROADMAP.md` Track F, F1 Phase A) — `source` must be a bare
    /// identifier naming either a declared struct (resolved to its own
    /// table for the scroll-paginated `/_nirdosha/table/<snake>` path,
    /// `ui_gen.rs`) or a declared function (the unpaginated `callFn`
    /// fallback) — the one shape `check_fn_ref`'s "must be a fn, full
    /// stop" rule doesn't fit, since either is legitimate here.
    fn check_searchable_select_source_expr(&mut self, struct_name: &str, field_name: &str, value: &Expr) {
        let Expr::Ident(name, span) = value else {
            self.error(TypeErrorKind::InvalidFieldValidationExpr { key: "source".to_string() }, value.span());
            return;
        };
        if self.registry.is_struct(name) || self.sigs.contains_key(name) {
            return;
        }
        self.error(
            TypeErrorKind::SearchableSelectSourceNotFound {
                struct_name: struct_name.to_string(),
                field_name: field_name.to_string(),
                source: name.clone(),
            },
            *span,
        );
    }

    /// `screen <Struct> { ... }` — existence/shape checks only; see the
    /// module-level note above `typecheck`'s `screens`/`dashboard` pass.
    fn check_screen(&mut self, screen: &ScreenDecl) {
        let Some(fields) = self.registry.struct_fields(&screen.struct_name) else {
            self.error(TypeErrorKind::UnknownScreenStruct(screen.struct_name.clone()), screen.span);
            return;
        };
        let field_names: std::collections::HashSet<&str> =
            fields.iter().map(|f| f.name.as_str()).collect();

        for (key, value) in &screen.entries {
            match key.as_str() {
                "list" | "create" | "update" | "delete" => self.check_fn_ref(key, value),
                // "paginate.page_size" / "paginate.total" / any other
                // slot: value shape isn't enforced yet (Phase 1 scope is
                // existence/shape for struct/field/fn/visibility only).
                _ => {}
            }
        }
        for fo in &screen.fields {
            if !field_names.contains(fo.field_name.as_str()) {
                self.error(
                    TypeErrorKind::UnknownScreenField {
                        struct_name: screen.struct_name.clone(),
                        field_name: fo.field_name.clone(),
                    },
                    fo.span,
                );
            }
            let field_ty = fields.iter().find(|f| f.name == fo.field_name).map(|f| &f.ty);
            if fo.entries.iter().any(|(k, _)| k == "pattern") && fo.entries.iter().any(|(k, _)| k == "format") {
                self.error(
                    TypeErrorKind::ConflictingPatternAndFormat {
                        struct_name: screen.struct_name.clone(),
                        field_name: fo.field_name.clone(),
                    },
                    fo.span,
                );
            }
            for (key, value) in &fo.entries {
                match key.as_str() {
                    "view" | "edit" => self.check_visibility_expr(key, value),
                    "pattern" => self.check_pattern_expr(&screen.struct_name, &fo.field_name, field_ty, value),
                    "format" => self.check_format_expr(&screen.struct_name, &fo.field_name, field_ty, value),
                    "min" | "max" => self.check_min_max_expr(&screen.struct_name, &fo.field_name, key, field_ty, value),
                    "render" => self.check_field_render_expr(&screen.struct_name, &fo.field_name, field_ty, value),
                    "source" => self.check_searchable_select_source_expr(&screen.struct_name, &fo.field_name, value),
                    _ => {}
                }
            }
        }
        for action in &screen.actions {
            self.check_fn_ref("action", &Expr::Ident(action.target_fn.clone(), action.span));
            self.check_action_show_result(format!("`action \"{}\"` on `screen {}`", action.label, screen.struct_name), action);
        }
        if let Some(layout) = &screen.layout {
            self.check_screen_layout(screen, layout);
        }
    }

    /// `layout { ... }` (`docs/ROADMAP.md` Track F, F1) — walks the tree
    /// recursively; a `Field`/`ActionRef` leaf must name something the
    /// screen actually declares, a `Widget` leaf's `kind` must be one of
    /// this pass's closed vocabulary (`"divider"`/`"card"`/`"timeline"`
    /// — Phase B grows this list, not this check's own shape). Runs
    /// *after* `screen.fields`/`screen.actions` have already been
    /// checked above, so `field_names`/action labels are recomputed
    /// fresh here rather than threaded through — cheap (a handful of
    /// fields/actions per screen), and keeps this fn a clean, separate
    /// entry point mirroring `check_dashboard`/`check_workspace`'s own
    /// "one fn per top-level DSL construct" shape.
    fn check_screen_layout(&mut self, screen: &ScreenDecl, node: &LayoutNode) {
        let field_names: std::collections::HashSet<&str> =
            self.registry.struct_fields(&screen.struct_name).map(|fs| fs.iter().map(|f| f.name.as_str()).collect()).unwrap_or_default();
        let action_labels: std::collections::HashSet<&str> =
            screen.actions.iter().map(|a| a.label.as_str()).collect();
        self.check_layout_node(screen, node, &field_names, &action_labels);
    }

    fn check_layout_node(
        &mut self,
        screen: &ScreenDecl,
        node: &LayoutNode,
        field_names: &std::collections::HashSet<&str>,
        action_labels: &std::collections::HashSet<&str>,
    ) {
        match node {
            LayoutNode::Row { children, .. } | LayoutNode::Column { children, .. } | LayoutNode::Grid { children, .. } => {
                for c in children {
                    self.check_layout_node(screen, c, field_names, action_labels);
                }
            }
            LayoutNode::Group { children, entries, .. } => {
                for c in children {
                    self.check_layout_node(screen, c, field_names, action_labels);
                }
                let _ = entries; // `title`/`collapsible`: no shape check needed yet (any string/bool).
            }
            LayoutNode::Tabs { tabs, .. } => {
                for (_, children) in tabs {
                    for c in children {
                        self.check_layout_node(screen, c, field_names, action_labels);
                    }
                }
            }
            LayoutNode::Field { name, span } => {
                if !field_names.contains(name.as_str()) {
                    self.error(
                        TypeErrorKind::UnknownScreenField { struct_name: screen.struct_name.clone(), field_name: name.clone() },
                        *span,
                    );
                }
            }
            LayoutNode::ActionRef { label, span } => {
                let is_crud_kind = matches!(label.as_str(), "list" | "create" | "update" | "delete" | "get");
                if !is_crud_kind && !action_labels.contains(label.as_str()) {
                    self.error(
                        TypeErrorKind::UnknownLayoutAction { struct_name: screen.struct_name.clone(), label: label.clone() },
                        *span,
                    );
                }
            }
            LayoutNode::Widget { kind, entries, span } => {
                if !matches!(kind.as_str(), "divider" | "card" | "timeline") {
                    self.error(
                        TypeErrorKind::UnknownRenderValue {
                            context: format!("`layout` in `screen {}`", screen.struct_name),
                            render: kind.clone(),
                            allowed: "\"divider\", \"card\", \"timeline\"".to_string(),
                        },
                        *span,
                    );
                    return;
                }
                if kind == "timeline" {
                    match entries.iter().find(|(k, _)| k == "source") {
                        Some((_, value)) => self.check_fn_ref("source", value),
                        None => self.error(
                            TypeErrorKind::TimelineWidgetMissingSource { struct_name: screen.struct_name.clone() },
                            *span,
                        ),
                    }
                }
            }
        }
    }

    /// `validate <fn_name> { pre: <expr>  post: <expr> ... }`
    /// (`docs/ROADMAP.md` Track F, F3) — `fn_name` resolves to a real `fn`,
    /// every key is `pre`/`post`, *and* (as of this pass) every `pre`/
    /// `post` expression is itself real-type-checked as a `bool`
    /// against a scope seeded with `fn_name`'s own real parameter names/
    /// types (plus `result: <fn_name's real return type>`, `post` only)
    /// — the exact same `Checker::check` entry point an ordinary `if`
    /// condition or `let` initializer already goes through, just seeded
    /// from a target fn's signature instead of the surrounding block's
    /// live scope (`validate_fragment`'s `FragmentEnv` established this
    /// same "seed `Scopes` from a caller-supplied name→`Ty` map, then
    /// reuse the ordinary checker" shape first). A non-bool predicate, or
    /// one referencing something that doesn't resolve, is now a real,
    /// span-located `TypeError` here — a build-time diagnostic instead
    /// of surfacing only as a runtime evaluation failure the first time
    /// the function is actually called (this pass's own previous,
    /// disclosed gap, `docs/ROADMAP.md` Track F, F3). Unlike `contract_check::
    /// check_program_contracts`'s own `UnboundIdentifier`/`Unsupported`
    /// reporting (which only ever runs *after* typecheck already passed,
    /// and only reaches Tier-1's narrower integer-only subset), this
    /// runs first and covers every `validate` block regardless of the
    /// target fn's shape — a struct-typed param, a `db`-touching body, a
    /// loop, all still get real static type errors for a malformed
    /// predicate now, even though none of those are ever statically
    /// *proven* by the Z3 pass.
    fn check_validate(&mut self, decl: &ValidateDecl, program: &Program) {
        if !self.sigs.contains_key(&decl.fn_name) {
            self.error(TypeErrorKind::ValidateFnNotFound(decl.fn_name.clone()), decl.span);
            return;
        }
        // `sigs` and `program.fns` are built from the same `Program` in
        // the same pass (`typecheck_impl`, right before either loop
        // runs) — a name present in `sigs` always has a matching
        // `FnDecl`; `else { return }` is defensive, not reachable in
        // practice, matching this file's own discipline elsewhere
        // (`check_validate`'s own earlier `NoSuchFunction` handling in
        // `contract_check.rs` takes the identical stance).
        let Some(f) = program.fns.iter().find(|f| f.name == decl.fn_name) else { return };
        for (key, value) in &decl.entries {
            match key.as_str() {
                "pre" | "post" => {
                    let mut scopes = Scopes::new();
                    for p in &f.params {
                        scopes.define(&p.name, p.ty.clone());
                    }
                    if key == "post" {
                        scopes.define("result", f.ret.clone());
                    }
                    self.check(value, &Ty::Bool, &Ty::Unit, &mut scopes);
                }
                other => self.error(
                    TypeErrorKind::ValidateUnknownKey { fn_name: decl.fn_name.clone(), key: other.to_string() },
                    value.span(),
                ),
            }
        }
    }

    /// `action "..." -> fn { show_result: true }` (`docs/ROADMAP.md` Track
    /// E4) — on a `screen`'s own action or, reused unchanged since both
    /// share the same `ActionDecl`/`PanelActionDecl` shape, a
    /// `workspace` `panel`'s action. `show_result`'s value must be a
    /// bool literal; when it's `true`, the target fn (already proven to
    /// resolve by `check_fn_ref`) must return `Result(json, _)` — there
    /// being nothing else `ui_gen_template.html`'s result modal could
    /// show. `show_result: false`, or the key's plain absence, needs no
    /// further check at all — the exact same "existence/shape only"
    /// posture every other `screen`-block consumer here already has.
    fn check_action_show_result(&mut self, context: String, action: &ActionDecl) {
        let Some((_, value)) = action.entries.iter().find(|(k, _)| k == "show_result") else { return };
        let Expr::Bool(show, _) = value else {
            self.error(TypeErrorKind::InvalidFieldValidationExpr { key: "show_result".to_string() }, value.span());
            return;
        };
        if !show {
            return;
        }
        // `self.sigs.get(...)` borrows only `self.sigs`, with a `'a`
        // lifetime independent of `&mut self` (`FnSig`'s own fields are
        // owned, not references into `self`) -- but computing a plain
        // `bool` first, the same "compute, then error" split
        // `check_workspace`'s panel `source` shape check already uses,
        // keeps this correct regardless and matches that precedent.
        let is_json_result = self.sigs.get(&action.target_fn).map(|sig| matches!(&sig.ret, Ty::Named(n, args) if n == "Result" && args.first() == Some(&Ty::Json)));
        if is_json_result == Some(false) {
            self.error(
                TypeErrorKind::ShowResultRequiresJsonResult { context, fn_name: action.target_fn.clone() },
                value.span(),
            );
        }
        // `None` (the fn doesn't resolve at all) is already reported by
        // `check_fn_ref` above -- don't pile a second, confusing error
        // about its return type on top of "this function doesn't exist."
    }

    /// `dashboard { tile "..." -> fn  chart "..." -> fn }` — each
    /// `MetricRef`'s target must resolve to a real function.
    fn check_dashboard(&mut self, dash: &DashboardDecl) {
        for t in &dash.tiles {
            self.check_metric_ref("tile", t);
        }
        for c in &dash.charts {
            self.check_metric_ref("chart", c);
        }
        for v in &dash.visuals {
            self.check_metric_ref("visual", v);
            for (key, value) in &v.entries {
                if key == "render" {
                    self.check_render_expr(
                        format!("`visual \"{}\"`", v.label),
                        value,
                        |s| matches!(s, "graph" | "heatmap" | "timeline"),
                        "\"graph\", \"heatmap\", \"timeline\"",
                    );
                }
            }
        }
    }

    /// `visual "..." -> fn { render: "..." }` or `panel "..." { render:
    /// "..." }`, or a `field <name> { render: "..." }` (`docs/ROADMAP.md`
    /// Track E2/E3) — value must be a string literal from `allowed`
    /// (checked as a `matches!` set the caller also names in `allowed`'s
    /// own display text, kept in sync by hand since a `&[&str]` can't
    /// itself produce a `matches!` pattern). Unlike `check_format_expr`,
    /// there's no backing struct field to cross-check a type against for
    /// `visual`/`panel` — a field-level `render` value's own field-type
    /// check is `check_field_render_expr`'s job instead, layered on top
    /// of this one. `context` is a pre-formatted, already-backtick-
    /// quoted description for the error message, since this one check
    /// serves three different callers with different surrounding syntax.
    fn check_render_expr(&mut self, context: String, value: &Expr, valid: fn(&str) -> bool, allowed: &str) {
        let Expr::Str(render, _) = value else {
            self.error(TypeErrorKind::InvalidFieldValidationExpr { key: "render".to_string() }, value.span());
            return;
        };
        if !valid(render) {
            self.error(
                TypeErrorKind::UnknownRenderValue { context, render: render.clone(), allowed: allowed.to_string() },
                value.span(),
            );
        }
    }

    /// `workspace <Name> { subject: <Struct> panel "..." { source: <fn>
    /// action ... } }` — mirrors `check_screen`, plus the one extra shape
    /// check `screen` doesn't need: a panel's `source` isn't just "any
    /// function," it's specifically a one-`id`-in/`Result(json, _)`-out
    /// function, since that's the exact call `ui_gen_template.html`'s
    /// `renderWorkspace` makes (`docs/ROADMAP.md` Track E1).
    fn check_workspace(&mut self, ws: &WorkspaceDecl) {
        match ws.entries.iter().find(|(k, _)| k == "subject") {
            None => self.error(TypeErrorKind::WorkspaceMissingSubject(ws.name.clone()), ws.span),
            Some((_, Expr::Ident(struct_name, span))) => match self.registry.struct_fields(struct_name) {
                None => self.error(
                    TypeErrorKind::UnknownWorkspaceSubject { workspace: ws.name.clone(), struct_name: struct_name.clone() },
                    *span,
                ),
                Some(fields) => {
                    if !fields.iter().any(|f| f.name == "id" && f.ty == Ty::I64) {
                        self.error(
                            TypeErrorKind::WorkspaceSubjectMissingId {
                                workspace: ws.name.clone(),
                                struct_name: struct_name.clone(),
                            },
                            *span,
                        );
                    }
                }
            },
            Some((_, other)) => self.error(TypeErrorKind::WorkspaceSubjectNotAnIdent(ws.name.clone()), other.span()),
        }

        for panel in &ws.panels {
            match panel.entries.iter().find(|(k, _)| k == "source") {
                None => self.error(
                    TypeErrorKind::PanelMissingSource { workspace: ws.name.clone(), panel: panel.title.clone() },
                    panel.span,
                ),
                Some((_, Expr::Ident(fn_name, span))) => {
                    // Borrow `self.sigs` only long enough to compute a
                    // plain `bool`, so the immutable borrow is released
                    // before `self.error(...)`'s mutable one below — the
                    // same "compute, then error" split `check_min_max_expr`
                    // and friends already use for the same reason.
                    let shape = self.sigs.get(fn_name).map(|sig| {
                        sig.params.len() == 1
                            && sig.params[0] == Ty::I64
                            && matches!(&sig.ret, Ty::Named(n, args) if n == "Result" && args.first() == Some(&Ty::Json))
                    });
                    match shape {
                        None => self.error(
                            TypeErrorKind::ScreenFnNotFound { key: "source".to_string(), fn_name: fn_name.clone() },
                            *span,
                        ),
                        Some(false) => self.error(
                            TypeErrorKind::PanelSourceWrongShape {
                                workspace: ws.name.clone(),
                                panel: panel.title.clone(),
                                fn_name: fn_name.clone(),
                            },
                            *span,
                        ),
                        Some(true) => {}
                    }
                }
                Some((_, other)) => self.error(
                    TypeErrorKind::PanelSourceNotAnIdent { workspace: ws.name.clone(), panel: panel.title.clone() },
                    other.span(),
                ),
            }
            for action in &panel.actions {
                self.check_fn_ref("action", &Expr::Ident(action.target_fn.clone(), action.span));
                self.check_action_show_result(
                    format!("`action \"{}\"` in `panel \"{}\"` on `workspace {}`", action.label, panel.title, ws.name),
                    action,
                );
            }
            // `panel "..." { render: "..." }` (Track E2) — same closed
            // vocabulary `visual`'s own `render` gets, reusing
            // `check_render_expr` rather than a second check.
            for (key, value) in &panel.entries {
                if key == "render" {
                    self.check_render_expr(
                        format!("`panel \"{}\"` in `workspace {}`", panel.title, ws.name),
                        value,
                        |s| matches!(s, "graph" | "heatmap" | "timeline"),
                        "\"graph\", \"heatmap\", \"timeline\"",
                    );
                }
            }
        }
    }

    fn check_metric_ref(&mut self, metric_kind: &str, m: &MetricRef) {
        if !self.sigs.contains_key(&m.target_fn) {
            self.error(
                TypeErrorKind::UnknownDashboardFn {
                    metric_kind: metric_kind.to_string(),
                    fn_name: m.target_fn.clone(),
                },
                m.span,
            );
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

    /// `owner_ns: Some(ns)` means the struct/enum/fn this name resolved
    /// to is a real namespaced declaration (only ever true when `name`
    /// was itself qualified — `ast::scope_key`'s doc comment) — `pub`/
    /// F2 piece 2. `None` (bare reference, or a resolved item outside
    /// any real namespace) is always visible, no check needed. A
    /// namespaced item that isn't `exported` is visible only from
    /// inside its own `ns` (`self.current_ns`) — a sibling declaration
    /// referencing it *still* has to spell it out qualified (`Mod::
    /// Name`), per `scope_key`'s "no bare access to a namespaced item,
    /// even from within its own module" rule, but that qualified
    /// self-reference is always legal regardless of `exported`.
    fn check_visibility(&mut self, name: &str, owner_ns: Option<&str>, exported: bool, span: Span) {
        if let Some(owner_ns) = owner_ns {
            if !exported && self.current_ns.as_deref() != Some(owner_ns) {
                self.error(TypeErrorKind::PrivateItem(name.to_string()), span);
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
                let (want_arity, owner) = if let Some(s) = self.registry.struct_decl(name) {
                    (Some(s.type_params.len()), Some((s.ns.as_deref(), s.exported)))
                } else if let Some(e) = self.registry.enum_decl(name) {
                    (Some(e.type_params.len()), Some((e.ns.as_deref(), e.exported)))
                } else {
                    (None, None)
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
                if let Some((owner_ns, exported)) = owner {
                    self.check_visibility(name, owner_ns, exported, span);
                }
            }
            Ty::Box(inner) | Ty::Ref(inner) | Ty::Thread(inner) | Ty::Channel(inner) => {
                self.validate_ty(inner, span, in_scope_params)
            }
            Ty::Vector(inner, _) | Ty::Matrix(inner, _, _) => self.validate_ty(inner, span, in_scope_params),
            Ty::Fn(params, ret) => {
                for p in params {
                    self.validate_ty(p, span, in_scope_params);
                }
                self.validate_ty(ret, span, in_scope_params);
            }
            _ => {}
        }
    }

    fn check_fn(&mut self, f: &FnDecl) {
        // Set for the whole body check, not just the registration
        // loop's own params/return-type validation — `check_visibility`
        // needs to know "am I inside `f`'s own module" for every
        // qualified reference `f`'s *body* makes too (a call, a `let`
        // annotation, a struct/variant construction), not only the ones
        // in its signature (`docs/ROADMAP.md` Track F, F2 piece 2).
        self.current_ns = f.ns.clone();
        // "Enum favoring": `str` may not be, or contain, a parameter or
        // return type of a user-defined function — see
        // `TypeErrorKind::StrInFnSignature`'s doc comment for the full
        // rule and its rationale. `txn_id` is the one structural
        // exception (`transact`'s synthesized idempotency key must stay
        // a plain `str` scalar for WAL durability, `Ty::is_transact_scalar`).
        for p in &f.params {
            if p.name != "txn_id" && p.ty.contains_str() {
                self.error(
                    TypeErrorKind::StrInFnSignature { fn_name: f.name.clone(), param_name: Some(p.name.clone()) },
                    f.span,
                );
            }
        }
        if f.ret.contains_str() {
            self.error(TypeErrorKind::StrInFnSignature { fn_name: f.name.clone(), param_name: None }, f.span);
        }

        let mut scopes = Scopes::new();
        for p in &f.params {
            scopes.define(&p.name, p.ty.clone());
        }
        self.check_stmts(&f.body.stmts, &f.ret, &mut scopes);

        if f.ret != Ty::Unit && !definitely_returns(&f.body.stmts) {
            self.error(TypeErrorKind::NotAllPathsReturn { fn_name: f.name.clone() }, f.span);
        }
        self.current_ns = None;
    }

    /// `WorkflowDecl`'s own rules (`docs/WORKFLOW.md`): every non-terminal
    /// `state` has a way out, every transition target exists, no
    /// ambiguous event dispatch — and, crucially, every `on_entry`/
    /// `on_exit` action call is *really* type-checked, not just
    /// name-resolved. `instance_id`/`data`/`link_<Event>` become real
    /// `Scopes` bindings (same mechanism `check_fn` above uses for a
    /// function's own parameters), then each action's call expression is
    /// synthesized and run through this checker's own `infer`
    /// (`infer_call`, specifically) exactly as if it were an ordinary
    /// call inside a function body — so `data.<field>` gets the real
    /// `NoSuchField` check, `link_<Event>` gets the real `UnknownVar`
    /// check, and (the actual gap this closes) every argument's type
    /// gets checked against the callee's real declared parameter types,
    /// arity included — a `send_email(conn, to, wrong_type_arg, vars)`
    /// is now exactly as much a compile error as it would be anywhere
    /// else in this language, not silently accepted because this was the
    /// one call site nothing looked at.
    fn check_workflow_decl(&mut self, w: &WorkflowDecl) {
        let state_names: std::collections::HashSet<&str> = w.states.iter().map(|s| s.name.as_str()).collect();
        let data_ty = Ty::Named(format!("{}Data", w.name), vec![]);
        let link_token_ty = Ty::Named(format!("{}LinkToken", w.name), vec![]);

        for s in &w.states {
            if !s.terminal && s.transitions.is_empty() {
                self.error(
                    TypeErrorKind::WorkflowStateHasNoTransitions { workflow: w.name.clone(), state: s.name.clone() },
                    s.span,
                );
            }

            let mut seen_events: std::collections::HashSet<&str> = std::collections::HashSet::new();
            let mut link_events: Vec<&str> = Vec::new();
            for t in &s.transitions {
                if !state_names.contains(t.target.as_str()) {
                    self.error(
                        TypeErrorKind::WorkflowUnknownTargetState {
                            workflow: w.name.clone(),
                            state: s.name.clone(),
                            target: t.target.clone(),
                        },
                        t.span,
                    );
                }
                if !seen_events.insert(t.event.as_str()) {
                    self.error(
                        TypeErrorKind::WorkflowDuplicateEvent {
                            workflow: w.name.clone(),
                            state: s.name.clone(),
                            event: t.event.clone(),
                        },
                        t.span,
                    );
                }
                if t.via_link {
                    link_events.push(t.event.as_str());
                }
            }

            // `state Name { owner: role(...)/claim(...), label: "..." }`
            // (`docs/WORKFLOW.md`'s "state ownership" section) — `owner` reuses
            // `screen`'s own `view`/`edit` shape check exactly (a role(...)/
            // claim(...) call with string-literal args); `label` must be a
            // plain string literal. Unknown keys are silently accepted,
            // same forward-compatible posture `check_screen` already has
            // for `screen.entries`.
            for (key, value) in &s.entries {
                match key.as_str() {
                    "owner" => self.check_visibility_expr(key, value),
                    "label" => {
                        if !matches!(value, Expr::Str(..)) {
                            self.error(TypeErrorKind::InvalidFieldValidationExpr { key: key.to_string() }, value.span());
                        }
                    }
                    _ => {}
                }
            }

            // `on_exit` only ever sees `instance_id`/`data` — the state
            // being left has no `link_<Event>` binding of its own to
            // offer (that only exists for the state being *entered*).
            let mut exit_scopes = Scopes::new();
            exit_scopes.define("instance_id", Ty::I64);
            exit_scopes.define("data", data_ty.clone());
            for action in &s.on_exit {
                let call = Expr::Call(action.name.clone(), action.args.clone(), action.span);
                self.infer(&call, &Ty::Unit, &mut exit_scopes);
            }

            let mut entry_scopes = Scopes::new();
            entry_scopes.define("instance_id", Ty::I64);
            entry_scopes.define("data", data_ty.clone());
            for event in &link_events {
                entry_scopes.define(&format!("link_{event}"), link_token_ty.clone());
            }
            for action in &s.on_entry {
                let call = Expr::Call(action.name.clone(), action.args.clone(), action.span);
                self.infer(&call, &Ty::Unit, &mut entry_scopes);
            }
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
                // Not a local binding — a bare top-level fn name is still
                // legal here (`let f: fn(..)->.. = transfer_funds`, or
                // passing one as a higher-order argument): first-class
                // functions are just ordinary values, so a plain function
                // name resolves the same way a local variable's name
                // would. A `requires`-gated function's name is the one
                // exception (`PrivilegedFnNotAcquired` — `acquire` is the
                // only way to get a value for one of those).
                None => match self.sigs.get(name.as_str()) {
                    Some(sig) if sig.requires.is_some() => {
                        let requirement = sig.requires.clone().unwrap();
                        self.error(TypeErrorKind::PrivilegedFnNotAcquired { name: name.clone(), requirement }, *span);
                        Ty::Error
                    }
                    Some(sig) => Ty::Fn(sig.params.clone(), Box::new(sig.ret.clone())),
                    None => {
                        self.error(TypeErrorKind::UnknownVar(name.clone()), *span);
                        Ty::Error
                    }
                },
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
            Expr::Acquire(name, proof, span) => self.infer_acquire(name, proof, expected_ret, scopes, *span),
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
            Expr::Transact { precheck, network, verify, commit, compensate, log, .. } => self.infer_transact(
                precheck.as_ref(),
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
                    // ...and closes a `db_connect(path)` handle — same
                    // one-time consuming close, reused a fourth time
                    // (`Ty::Db`'s doc comment).
                    Ty::Db => Ty::Unit,
                    // ...and closes an `mq_connect(host, port)` handle —
                    // same one-time consuming close, reused a fifth time
                    // (`Ty::Mq`'s doc comment).
                    Ty::Mq => Ty::Unit,
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
        if self.is_builtin_or_plugin(name) {
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
        if self.is_builtin_or_plugin(name) {
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

    /// `transact { ... }` (`docs/TRANSACT.md`) type-checks each slot exactly
    /// like an ordinary call to its own name (`infer_transact_slot`),
    /// then binds `network`/`verify`'s return types as scoped variables
    /// visible to every slot after them, matching `docs/TRANSACT.md`'s
    /// "implicit local bindings" rule exactly. Always produces `Ty::Bool`
    /// — `transact` is `true`/`false` by construction (`docs/TRANSACT.md`:
    /// "`true` if it committed, `false` if it compensated"), never
    /// anything else.
    fn infer_transact(
        &mut self,
        precheck: Option<&TransactSlot>,
        network: &TransactSlot,
        verify: &TransactSlot,
        commit: &TransactSlot,
        compensate: Option<&TransactSlot>,
        log: Option<&TransactSlot>,
        expected_ret: &Ty,
        scopes: &mut Scopes,
    ) -> Ty {
        scopes.push();

        // `txn_id` is bound before `precheck` even runs (interpreter.rs's
        // `Expr::Transact` arm generates it first) so it's in scope for
        // every slot, but only `network` is required to actually pass it
        // (checked below) -- the idempotency key a crash-replayed resend
        // needs.
        scopes.define("txn_id", Ty::Str);

        // `precheck`/`log` are never logged or replayed (`Expr::Transact`'s
        // doc comment) -- no durability-scalar constraint on either.
        if let Some(p) = precheck {
            let precheck_ty = self.infer_transact_slot(p, expected_ret, scopes);
            if precheck_ty != Ty::Bool && precheck_ty != Ty::Error {
                self.error(TypeErrorKind::TransactPrecheckMustReturnBool { found: precheck_ty }, p.span);
            }
        }

        // `network`'s args and its own declared return type both cross
        // the durability boundary: the return type becomes the `network`
        // binding, persisted into the log before `verify` runs.
        let network_ty = self.infer_transact_slot_durable(network, expected_ret, scopes, "network", true);
        if !network.args.iter().any(|a| matches!(a, Expr::Ident(name, _) if name == "txn_id")) {
            self.error(TypeErrorKind::TransactNetworkMustUseTxnId, network.span);
        }
        scopes.define("network", network_ty);

        // `verify`'s args cross the boundary too (a crash resuming from
        // `network_done` has to re-run `verify` with its original
        // arguments); its return type is separately forced to `bool`
        // just below, already scalar, so `check_return` is `false` here.
        let verify_ty = self.infer_transact_slot_durable(verify, expected_ret, scopes, "verify", false);
        if verify_ty != Ty::Bool && verify_ty != Ty::Error {
            self.error(TypeErrorKind::TransactVerifyMustReturnBool { found: verify_ty.clone() }, verify.span);
        }
        if !verify.args.iter().all(|a| matches!(a, Expr::Ident(name, _) if name == "network" || name == "txn_id")) {
            self.error(TypeErrorKind::TransactVerifyArgsMustBeImplicitBindings, verify.span);
        }
        scopes.define("verify", verify_ty);

        // `commit`/`compensate`'s own return type is deliberately left
        // unconstrained here (not durability-checked): it's allowed to be
        // a `Result<T, E>` -- the interpreter's `Expr::Transact` arm
        // treats an `Err` there as a failure to retry, the same way a
        // trap already is. Only their arguments need to be durable (they
        // get logged so a crash can retry just that slot).
        self.infer_transact_slot_durable(commit, expected_ret, scopes, "commit", false);
        if let Some(c) = compensate {
            self.infer_transact_slot_durable(c, expected_ret, scopes, "compensate", false);
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
    /// example in `docs/TRANSACT.md` needs a builtin in a slot; every slot
    /// names a real, user-authored business operation.
    fn infer_transact_slot(&mut self, slot: &TransactSlot, expected_ret: &Ty, scopes: &mut Scopes) -> Ty {
        if self.is_builtin_or_plugin(&slot.name) {
            self.error(TypeErrorKind::CannotUseBuiltinInTransact { name: slot.name.clone() }, slot.span);
            for a in &slot.args {
                self.infer(a, expected_ret, scopes);
            }
            return Ty::Error;
        }
        self.infer_call(&slot.name, &slot.args, expected_ret, scopes, slot.span, None)
    }

    /// `infer_transact_slot` plus the durability-log scalar constraint
    /// (`Ty::is_transact_scalar`) on the callee's declared parameter
    /// types (always) and its declared return type (only when
    /// `check_return`) -- see the three call sites in `infer_transact`
    /// for exactly which slots need which half. Reads the already-built
    /// `self.sigs` table rather than re-inferring each argument
    /// expression a second time; `is_builtin` slots already returned
    /// `Ty::Error` out of `infer_transact_slot` above and have no `sigs`
    /// entry, so the `sigs.get` below simply finds nothing for them --
    /// no double-reporting of `CannotUseBuiltinInTransact`.
    fn infer_transact_slot_durable(
        &mut self,
        slot: &TransactSlot,
        expected_ret: &Ty,
        scopes: &mut Scopes,
        where_: &'static str,
        check_return: bool,
    ) -> Ty {
        let ty = self.infer_transact_slot(slot, expected_ret, scopes);
        let params_and_ret = self.sigs.get(&slot.name).map(|sig| (sig.params.clone(), sig.ret.clone()));
        if let Some((params, ret)) = params_and_ret {
            for p in &params {
                if !p.is_transact_scalar() {
                    self.error(
                        TypeErrorKind::TransactValueNotDurable { where_: where_.to_string(), found: p.clone() },
                        slot.span,
                    );
                }
            }
            if check_return && ret != Ty::Error && !ret.is_transact_scalar() {
                self.error(TypeErrorKind::TransactValueNotDurable { where_: where_.to_string(), found: ret }, slot.span);
            }
        }
        ty
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
        // First-class function call-through-value: `name` shadows a local
        // binding whose type is `Ty::Fn(params, ret)` (either a plain
        // top-level fn named directly, or the result of a successful
        // `acquire`) — dispatched by checking `args` against the type's
        // own carried param list, exactly like an ordinary call, just
        // without a `sigs` lookup (the callee isn't known by name here,
        // only by the value flowing through `name`). Checked ahead of
        // every other resolution (builtin/struct/global fn) the same way
        // `Expr::Ident`'s own local-scope lookup always wins first.
        if let Some(Ty::Fn(params, ret)) = scopes.get(name) {
            if params.len() != args.len() {
                self.error(
                    TypeErrorKind::ArityMismatch { fn_name: name.to_string(), want: params.len(), got: args.len() },
                    span,
                );
            }
            for (arg, want) in args.iter().zip(params.iter()) {
                self.check(arg, want, expected_ret, scopes);
            }
            for extra in args.iter().skip(params.len()) {
                self.infer(extra, expected_ret, scopes);
            }
            return *ret;
        }
        if self.is_builtin_or_plugin(name) {
            return self.infer_builtin_call(name, args, expected_ret, scopes, span);
        }
        // Row 11: "construction is an ordinary call, not a new literal
        // form" (`docs/nirdosha_row11_amendment.md` §3.1) — a struct's own
        // name or an enum variant's name, called like a function, is how
        // a value gets built. `typecheck`'s registration pass already
        // proved these names can't collide with a function/builtin, so
        // checking them ahead of `self.sigs` is safe and unambiguous.
        if self.registry.is_struct(name) {
            // `RoleView`/`ClaimView` are `acquire`'s proof types (see
            // `Requirement::proof_ty`) — the *only* legitimate way to
            // hold one is `check_role`/`extract_claim` returning it
            // after a real `oidc_validate_token`. Every other struct in
            // the language, prelude or user-declared, is legitimately
            // constructible this way (`infer_struct_construction`'s own
            // doc comment) — these two are singled out because a direct
            // `RoleView("admin")` here would let `acquire gated_fn(...)`
            // typecheck against a forged proof with zero relation to any
            // validated identity, defeating `requires`/`acquire` outright.
            if name == "RoleView" || name == "ClaimView" {
                self.error(TypeErrorKind::UnforgeableProofConstruction(name.to_string()), span);
                for a in args {
                    self.infer(a, expected_ret, scopes);
                }
                return Ty::Error;
            }
            if let Some(s) = self.registry.struct_decl(name) {
                self.check_visibility(name, s.ns.as_deref(), s.exported, span);
            }
            return self.infer_struct_construction(name, args, expected, expected_ret, scopes, span);
        }
        if let Some((enum_name, variant)) = self.registry.find_variant(name) {
            let variant = variant.clone();
            if let Some(e) = self.registry.enum_decl(&enum_name) {
                self.check_visibility(name, e.ns.as_deref(), e.exported, span);
            }
            return self.infer_variant_construction(&enum_name, &variant, args, expected, expected_ret, scopes, span);
        }
        let Some(sig) = self.sigs.get(name) else {
            self.error(TypeErrorKind::UnknownFn(name.to_string()), span);
            for a in args {
                self.infer(a, expected_ret, scopes); // still check the args for their own errors
            }
            return Ty::Error;
        };
        // Everything needed out of `sig` is cloned up front, right here
        // — `sig` borrows `self.sigs` (unlike `self.registry`'s own
        // `'a`-lifetime-decoupled accessors above), so it has to stop
        // being read before the first `&mut self` call below
        // (`check_visibility`/`error`) or the borrow checker rejects it.
        let sig_ns = sig.ns.clone();
        let sig_exported = sig.exported;
        let requires = sig.requires.clone();
        let params = sig.params.clone();
        let ret = sig.ret.clone();
        self.check_visibility(name, sig_ns.as_deref(), sig_exported, span);
        // A `requires`-gated function has no direct-call path at all —
        // `acquire` (`infer_acquire`) is the only place that's allowed to
        // resolve this name against `sigs`. Still type-checks the
        // arguments (so a bad call reports its own errors too, not just
        // this one), same "don't cascade, but don't go silent either"
        // pattern `UnknownFn`'s branch above already follows.
        if let Some(requirement) = requires {
            self.error(TypeErrorKind::PrivilegedFnNotAcquired { name: name.to_string(), requirement }, span);
            for a in args {
                self.infer(a, expected_ret, scopes);
            }
            return Ty::Error;
        }
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

    /// `acquire name(proof)` (`Expr::Acquire`) — the only path that's
    /// allowed to turn a `requires`-gated function's name into a callable
    /// `Ty::Fn` value. `name` must be a real, gated top-level function
    /// (never a builtin/struct/local — mirrors `infer_spawn`'s identical
    /// "always a global fn" restriction); `proof`'s type must match what
    /// the requirement demands (`Requirement::proof_ty`). Always produces
    /// `Result(Ty::Fn(params, ret), str)`, the same prelude `Result` every
    /// other fallible identity builtin (`check_role`/`extract_claim`)
    /// already returns — `Err` means the proof didn't match, checked at
    /// runtime (`interpreter.rs`'s `Expr::Acquire` arm), not here: a
    /// `RoleView`'s `role` field is an ordinary runtime string, the same
    /// reason `check_role` itself is runtime-checked.
    fn infer_acquire(&mut self, name: &str, proof: &Expr, expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        if self.is_builtin_or_plugin(name) || self.registry.is_struct(name) || self.registry.find_variant(name).is_some() {
            self.error(TypeErrorKind::UnknownFn(name.to_string()), span);
            self.infer(proof, expected_ret, scopes);
            return Ty::Error;
        }
        let Some(sig) = self.sigs.get(name) else {
            self.error(TypeErrorKind::UnknownFn(name.to_string()), span);
            self.infer(proof, expected_ret, scopes);
            return Ty::Error;
        };
        let Some(requirement) = sig.requires.clone() else {
            self.error(TypeErrorKind::AcquireOfUngatedFn(name.to_string()), span);
            self.infer(proof, expected_ret, scopes);
            return Ty::Error;
        };
        let fn_ty = Ty::Fn(sig.params.clone(), Box::new(sig.ret.clone()));
        self.check(proof, &requirement.proof_ty(), expected_ret, scopes);
        Ty::Named("Result".to_string(), vec![fn_ty, Ty::Str])
    }

    /// `Point(1.0, 2.0)` — a struct constructor call, checked exactly
    /// like an ordinary function call's argument list (`infer_call`),
    /// positional-only against the struct's declared field types
    /// (`docs/nirdosha_row11_amendment.md` §3.1, §3.5's "extends the boundary
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
    /// no type of its own (`docs/nirdosha_row11_amendment.md` §3.2).
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
    /// (`docs/nirdosha_row11_amendment.md` §3.1: "Nirdosha never uses `<...>`
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
    /// (`docs/nirdosha_row11_amendment.md` §3.4: every declared variant,
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

        // Literal-pattern match: a `str`/`i64`/`bool` scrutinee is never
        // an enum, so it gets its own dedicated checking path (mandatory
        // trailing `_`, no variant/payload resolution at all) rather
        // than being folded into the enum path below.
        if matches!(st, Ty::Str | Ty::I64 | Ty::Bool) {
            return self.check_literal_match(&st, arms, span, want, expected_ret, scopes);
        }

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
            // A literal/`_` pattern arm inside what typechecked as an
            // enum match is a real error, not a variant lookup -- report
            // it and treat the arm as having no payload, same recovery
            // shape `UnknownVariant` below already uses.
            if arm.pattern.is_some() {
                if let Some(en) = &enum_name {
                    self.error(TypeErrorKind::MatchArmMustBeVariant { enum_name: en.clone() }, arm.span);
                }
            }
            let payload: Vec<Ty> = if arm.pattern.is_some() {
                Vec::new()
            } else {
                match &enum_name {
                    Some(en) => match self.registry.find_variant(&arm.variant) {
                        Some((owner, v)) if &owner == en => {
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
                }
            };

            // `seen`/the exhaustiveness check below both compare against
            // `ast::TypeRegistry::enum_variants`'s own bare `v.name`s —
            // `arm.variant` may itself be qualified (`"Mine::ReportType
            // ::SAR"`, `docs/ROADMAP.md` Track F, F2's qualified match-arm
            // grammar), so only the text after its last `::`, if any,
            // is ever recorded/compared.
            let bare_variant = arm.variant.rsplit_once("::").map_or(arm.variant.as_str(), |(_, v)| v).to_string();
            if arm.pattern.is_none() && !seen.insert(bare_variant) {
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

    /// `match` on a `str`/`i64`/`bool` scrutinee -- the non-enum sibling
    /// `check_match` delegates to above. Every arm's pattern must be a
    /// literal of the scrutinee's own type (or `_`), and exactly one `_`
    /// is required, last: a literal domain isn't closed the way an
    /// enum's variant set is, so there's no way to prove every case is
    /// covered without a trailing wildcard. The per-arm body-checking
    /// tail duplicates `check_match`'s (three short lines) rather than
    /// sharing it -- the enum path's version is entangled with payload
    /// bindings this path has none of, and the duplication is small.
    fn check_literal_match(
        &mut self,
        st: &Ty,
        arms: &[MatchArm],
        span: Span,
        want: MatchWant,
        expected_ret: &Ty,
        scopes: &mut Scopes,
    ) -> Ty {
        let mut seen: std::collections::HashSet<String> = std::collections::HashSet::new();
        let mut wildcard_seen = false;
        let mut result: Option<Ty> = None;

        for (i, arm) in arms.iter().enumerate() {
            match &arm.pattern {
                Some(LiteralPattern::Wildcard) => {
                    if wildcard_seen {
                        self.error(TypeErrorKind::DuplicateMatchArm { variant: "_".to_string() }, arm.span);
                    }
                    wildcard_seen = true;
                    if i != arms.len() - 1 {
                        self.error(TypeErrorKind::WildcardArmNotLast, arm.span);
                    }
                }
                Some(LiteralPattern::Str(_)) => {
                    if *st != Ty::Str {
                        self.error(
                            TypeErrorKind::LiteralPatternTypeMismatch { scrutinee_ty: st.clone(), pattern_ty: Ty::Str },
                            arm.span,
                        );
                    }
                    if !seen.insert(arm.variant.clone()) {
                        self.error(TypeErrorKind::DuplicateMatchArm { variant: arm.variant.clone() }, arm.span);
                    }
                }
                Some(LiteralPattern::Int(_)) => {
                    if *st != Ty::I64 {
                        self.error(
                            TypeErrorKind::LiteralPatternTypeMismatch { scrutinee_ty: st.clone(), pattern_ty: Ty::I64 },
                            arm.span,
                        );
                    }
                    if !seen.insert(arm.variant.clone()) {
                        self.error(TypeErrorKind::DuplicateMatchArm { variant: arm.variant.clone() }, arm.span);
                    }
                }
                Some(LiteralPattern::Bool(_)) => {
                    if *st != Ty::Bool {
                        self.error(
                            TypeErrorKind::LiteralPatternTypeMismatch { scrutinee_ty: st.clone(), pattern_ty: Ty::Bool },
                            arm.span,
                        );
                    }
                    if !seen.insert(arm.variant.clone()) {
                        self.error(TypeErrorKind::DuplicateMatchArm { variant: arm.variant.clone() }, arm.span);
                    }
                }
                None => {
                    self.error(TypeErrorKind::MatchArmMustBeLiteral { scrutinee_ty: st.clone() }, arm.span);
                }
            }

            scopes.push();
            let arm_ty = match want {
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

        if !wildcard_seen {
            self.error(TypeErrorKind::NonExhaustiveLiteralMatch { found: st.clone() }, span);
        }

        match want {
            MatchWant::Statement => Ty::Unit,
            MatchWant::Check(w) => w.clone(),
            MatchWant::Infer => result.unwrap_or(Ty::Unit),
        }
    }

    /// `docs/ROADMAP.md` Track G, G1: a plugin builtin (`self.plugins`) is
    /// a real builtin for every purpose `ast::is_builtin` alone used to
    /// gate — can't be spawned, can't be a `transact` slot, can't be
    /// shadowed, and resolves through `infer_builtin_call` exactly like
    /// one of `ast::BUILTIN_NAMES`. Every one of the (now five) call
    /// sites that used to ask `is_builtin(name)` alone asks this instead.
    fn is_builtin_or_plugin(&self, name: &str) -> bool {
        is_builtin(name) || self.plugins.contains_key(name)
    }

    /// Every builtin's shape rule, dispatched by name — `is_builtin`
    /// (ast.rs) is the shared membership check; the actual per-builtin
    /// logic lives here (and `interpreter.rs`'s `Expr::Call` arm has its
    /// own independent counterpart), not in a shared table, because a
    /// generic `fn(&[Ty]) -> Ty` signature can't see the *literal value*
    /// `zeros`/`ones`/`identity` need to fix their result's static shape
    /// — see `ast.rs::BUILTIN_NAMES`'s doc comment.
    fn infer_builtin_call(&mut self, name: &str, args: &[Expr], expected_ret: &Ty, scopes: &mut Scopes, span: Span) -> Ty {
        // `docs/ECOSYSTEM.md` §G1's Stage 1: a plugin builtin's signature is
        // declared data (`self.plugins`), not a hand-written match arm —
        // checked positionally against `params`, same as `self.check`
        // already does for e.g. the workflow builtins' fixed-shape
        // arguments below. Checked before `print`'s special case and the
        // giant `match` so a plugin can never accidentally collide with
        // either (`typecheck_with_plugins`'s registration-time guards
        // already prove no plugin name equals a real builtin's name).
        if let Some((params, ret)) = self.plugins.get(name).cloned() {
            if args.len() != params.len() {
                for a in args {
                    self.infer(a, expected_ret, scopes);
                }
                self.error(
                    TypeErrorKind::ArityMismatch { fn_name: name.to_string(), want: params.len(), got: args.len() },
                    span,
                );
                return Ty::Error;
            }
            for (a, want) in args.iter().zip(params.iter()) {
                self.check(a, want, expected_ret, scopes);
            }
            return ret;
        }
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
            ("sleep_ms", 1) => {
                self.check(&args[0], &Ty::I64, expected_ret, scopes);
                Ty::Unit
            }
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
            // JSON (`Ty::Json`'s doc comment) -- every fallible accessor
            // returns `Result(_, str)`, reusing the now-real prelude type
            // (layer 7) rather than a bespoke error shape.
            ("json_parse", 1) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Json)
            }
            ("json_get", 2) | ("json_array_get", 2) => {
                self.check(&args[0], &Ty::Json, expected_ret, scopes);
                let key_ty = if name == "json_get" { Ty::Str } else { Ty::I64 };
                self.check(&args[1], &key_ty, expected_ret, scopes);
                result_of(Ty::Json)
            }
            ("json_get_str", 2) => {
                self.check(&args[0], &Ty::Json, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Str)
            }
            ("json_get_i64", 2) => {
                self.check(&args[0], &Ty::Json, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                result_of(Ty::I64)
            }
            ("json_get_f64", 2) => {
                self.check(&args[0], &Ty::Json, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                result_of(Ty::F64)
            }
            ("json_get_bool", 2) => {
                self.check(&args[0], &Ty::Json, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Bool)
            }
            // `json_get_str`'s inverse — the one JSON-construction builtin
            // this language has (docs/WORKFLOW.md's `notify`/`send_email`
            // `vars` payloads need at least this much): sets `key` to a
            // `str` value on a JSON object, or starts a fresh one if `doc`
            // is `null` (the shape `json_parse("{}")` and `json_parse("null")`
            // both already produce). Any other JSON shape (an array, a
            // scalar) is a runtime `Err`, not a type error — the same
            // "some proven statically, some at runtime" split `json_get`'s
            // own key-not-found case already makes.
            ("json_set_str", 3) => {
                self.check(&args[0], &Ty::Json, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                self.check(&args[2], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Json)
            }
            ("json_array_len", 1) => {
                self.check(&args[0], &Ty::Json, expected_ret, scopes);
                result_of(Ty::I64)
            }
            // HTTP (plain, client-only -- `ast::BUILTIN_NAMES`'s doc
            // comment). `HttpResponse` is `ast::prelude_structs`'
            // non-generic struct, so `Ty::Named("HttpResponse", vec![])`
            // needs no substitution the way a real generic construction
            // would.
            ("http_get", 3) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // host
                self.check(&args[1], &Ty::I64, expected_ret, scopes); // port
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // path
                result_of(Ty::Named("HttpResponse".to_string(), vec![]))
            }
            ("http_post", 4) | ("https_post", 4) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // host
                self.check(&args[1], &Ty::I64, expected_ret, scopes); // port
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // path
                self.check(&args[3], &Ty::Str, expected_ret, scopes); // body
                result_of(Ty::Named("HttpResponse".to_string(), vec![]))
            }
            ("https_get", 3) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // host
                self.check(&args[1], &Ty::I64, expected_ret, scopes); // port
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // path
                result_of(Ty::Named("HttpResponse".to_string(), vec![]))
            }
            // Row 12: identity as a relying party.
            ("oidc_validate_token", 4) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // token
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // expected_issuer
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // expected_audience
                self.check(&args[3], &Ty::Str, expected_ret, scopes); // jwks_json
                result_of(Ty::Named("VerifiedIdentity".to_string(), vec![]))
            }
            ("check_role", 2) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Named("RoleView".to_string(), vec![]))
            }
            ("extract_claim", 2) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Named("ClaimView".to_string(), vec![]))
            }
            ("check_role_path", 3) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // dotted path to the roles array
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // role
                result_of(Ty::Named("RoleView".to_string(), vec![]))
            }
            ("extract_claim_path", 2) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // dotted path to the claim
                result_of(Ty::Named("ClaimView".to_string(), vec![]))
            }
            ("identity_expired", 2) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                self.check(&args[1], &Ty::I64, expected_ret, scopes);
                Ty::Bool
            }
            // Row 12 continued: session, refresh, revocation, API-key.
            ("create_application_session", 1) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                Ty::Named("ApplicationSession".to_string(), vec![])
            }
            ("session_cookie", 1) => {
                self.check(&args[0], &Ty::Named("ApplicationSession".to_string(), vec![]), expected_ret, scopes);
                Ty::Str
            }
            ("new_refresh_token", 1) => {
                self.check(&args[0], &Ty::I64, expected_ret, scopes);
                Ty::Named("RefreshTokenHandle".to_string(), vec![])
            }
            ("exchange_refresh_token", 3) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                self.check(&args[1], &Ty::Named("RefreshTokenHandle".to_string(), vec![]), expected_ret, scopes);
                self.check(&args[2], &Ty::I64, expected_ret, scopes);
                result_of(Ty::Named("VerifiedIdentity".to_string(), vec![]))
            }
            ("check_revocation", 1) => {
                self.check(&args[0], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes);
                Ty::Bool
            }
            ("validate_api_key", 2) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Named("VerifiedIdentity".to_string(), vec![]))
            }
            // Infallible -- any `str` hashes to something -- so no
            // `Result` wrap, same as `session_cookie`. The 2-arg form
            // hashes both parts in sequence rather than concatenating
            // first (there's no `+` on `str` to do that with) -- the
            // one thing a hash-chained audit log needs: `hash =
            // sha256_hex(prev_hash, payload)`.
            ("sha256_hex", 1) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes);
                Ty::Str
            }
            ("sha256_hex", 2) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                Ty::Str
            }
            ("constant_time_str_eq", 2) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes);
                Ty::Bool
            }
            // DB, layer 1 + layer 2 (`Ty::Db`'s doc comment) -- `path` is
            // really "connection string": a bare file path or `:memory:`
            // still means SQLite, `postgres://`/`postgresql://` selects
            // Postgres (`dbconn.rs`), but the checked type is `str` either
            // way, so nothing here changed when Postgres was added.
            ("db_connect", 1) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // connection string
                result_of(Ty::Db)
            }
            // `db_query`/`db_execute` accept up to 8 trailing bind-value
            // arguments (`?` placeholders in `sql`, SQLite-positional --
            // `dbconn.rs::rewrite_placeholders` rewrites them to Postgres's
            // `$1, $2, ...` when the handle underneath is a Postgres
            // connection, so this same `?` syntax works against either
            // backend) -- the *only* route to a parameterized query, since
            // `str` has no concatenation (docs/LANGUAGE.md §2): there's no way
            // to build a dynamic SQL string in Nirdosha source at all
            // otherwise. Each bind value's own type isn't constrained here
            // (`infer` only, not `check` against one fixed `Ty`) -- it can
            // be `str`/`i64`/`f64`/`bool`, whichever a caller's data
            // actually is; `interpreter.rs`'s `sql_bind_params` is the
            // real (runtime) gate on that, same "some proven away
            // statically, some at runtime" split every Tier-2 check here
            // already makes (`docs/LANGUAGE.md` §8).
            ("db_query", n) if (2..=10).contains(&n) => {
                self.check(&args[0], &Ty::Db, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // sql
                for a in &args[2..] {
                    self.infer(a, expected_ret, scopes);
                }
                result_of(Ty::Json)
            }
            ("db_execute", n) if (2..=10).contains(&n) => {
                self.check(&args[0], &Ty::Db, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // sql
                for a in &args[2..] {
                    self.infer(a, expected_ret, scopes);
                }
                result_of(Ty::I64)
            }
            // `dec128` (`docs/LANGUAGE.md` §5 "Decimal arithmetic", §6c/§6d) --
            // the only way in and out of a `dec128` value. `+`/`-`/`*`/
            // `/`/comparisons don't go through here at all -- they
            // dispatch through `infer_binary`'s ordinary numeric-scalar
            // path (`Ty::is_numeric()`), the ordinary operator table,
            // not a builtin call.
            ("dec_from_str", 1) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes);
                result_of(Ty::Dec128)
            }
            ("dec_from_i64", 2) => {
                self.check(&args[0], &Ty::I64, expected_ret, scopes); // unscaled value
                self.check(&args[1], &Ty::U32, expected_ret, scopes); // scale
                Ty::Dec128
            }
            ("dec_to_str", 1) => {
                self.check(&args[0], &Ty::Dec128, expected_ret, scopes);
                Ty::Str
            }
            ("dec_round", 2) => {
                self.check(&args[0], &Ty::Dec128, expected_ret, scopes);
                self.check(&args[1], &Ty::U32, expected_ret, scopes); // scale
                Ty::Dec128
            }
            ("dec_scale", 1) => {
                self.check(&args[0], &Ty::Dec128, expected_ret, scopes);
                Ty::U32
            }
            // MQ, layer 1 (`Ty::Mq`'s doc comment) -- Redis-backed, same
            // `Result(_, str)` convention as `db`.
            ("mq_connect", 2) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // host
                self.check(&args[1], &Ty::I64, expected_ret, scopes); // port
                result_of(Ty::Mq)
            }
            ("mq_publish", 3) => {
                self.check(&args[0], &Ty::Mq, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // queue
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // message
                result_of(Ty::Unit)
            }
            ("mq_consume", 3) => {
                self.check(&args[0], &Ty::Mq, expected_ret, scopes);
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // queue
                self.check(&args[2], &Ty::I64, expected_ret, scopes); // timeout_secs
                result_of(Ty::Str)
            }
            // Row 12's deliberate mock-only exception -- the inverse of
            // `oidc_validate_token`: signs a token instead of verifying
            // one. `mock_` is load-bearing, not decorative -- see
            // `interpreter.rs`'s `mock_issue_token` doc comment.
            // `issued_at` is an explicit argument, not a hidden wall-clock
            // read, so the builtin stays deterministic/pure (`effects.rs`
            // classifies it via the default `_ => {}` arm, same as
            // `json_*` -- no new effect tag needed).
            ("mock_issue_token", 7) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // subject
                self.check(&args[1], &Ty::Str, expected_ret, scopes); // issuer
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // audience
                self.check(&args[3], &Ty::I64, expected_ret, scopes); // issued_at
                self.check(&args[4], &Ty::I64, expected_ret, scopes); // ttl_secs
                self.check(&args[5], &Ty::Str, expected_ret, scopes); // claims_json
                self.check(&args[6], &Ty::Str, expected_ret, scopes); // jwks_json
                result_of(Ty::Str)
            }
            // docs/WORKFLOW.md: notification actions. `template`/`vars` are
            // exempt from the fn-boundary `str` ban like every other
            // builtin (docs/LANGUAGE.md §6b — this arm is never a `program.fns`
            // entry).
            ("send_email", 4) | ("send_sms", 4) | ("send_push", 4) => {
                self.check(&args[0], &Ty::Db, expected_ret, scopes); // conn
                self.check(&args[1], &Ty::Named("Recipient".to_string(), vec![]), expected_ret, scopes); // to
                self.check(&args[2], &Ty::Str, expected_ret, scopes); // template
                self.check(&args[3], &Ty::Json, expected_ret, scopes); // vars
                workflow_result_of(Ty::Bool)
            }
            ("notify", 5) => {
                self.check(&args[0], &Ty::Db, expected_ret, scopes); // conn
                self.check(&args[1], &Ty::Mq, expected_ret, scopes); // mq (presence/push bridge)
                self.check(&args[2], &Ty::Named("Recipient".to_string(), vec![]), expected_ret, scopes); // to
                self.check(&args[3], &Ty::Str, expected_ret, scopes); // template
                self.check(&args[4], &Ty::Json, expected_ret, scopes); // vars
                workflow_result_of(Ty::Bool)
            }
            // `workflow_lower.rs`'s shared internal dispatch builtins —
            // `event`/`token`'s type isn't pinned to one fixed `Ty` here
            // (a different compiler-synthesized enum/struct per
            // workflow); see `WorkflowEventArgMustBeEnum`'s doc comment.
            ("__workflow_start", 3) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // workflow_name
                // `identity: Option(VerifiedIdentity)` (`docs/WORKFLOW.md`'s
                // "who submitted this" section) — checked against the
                // concrete `Option(VerifiedIdentity)` type, not just any
                // `Option(_)`, the same precision `__workflow_advance`'s
                // own plain `VerifiedIdentity` check already has.
                self.check(
                    &args[1],
                    &Ty::Named("Option".to_string(), vec![Ty::Named("VerifiedIdentity".to_string(), vec![])]),
                    expected_ret,
                    scopes,
                );
                let data_ty = self.infer(&args[2], expected_ret, scopes);
                if data_ty != Ty::Error && !matches!(&data_ty, Ty::Named(n, _) if self.registry.is_struct(n)) {
                    self.error(
                        TypeErrorKind::WorkflowStructArgMustBeStruct {
                            fn_name: name.to_string(),
                            arg: "data".to_string(),
                            found: data_ty,
                        },
                        args[2].span(),
                    );
                }
                workflow_result_of(Ty::I64)
            }
            ("__workflow_advance", 5) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // workflow_name
                self.check(&args[1], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes); // identity
                self.check(&args[2], &Ty::I64, expected_ret, scopes); // instance_id
                let event_ty = self.infer(&args[3], expected_ret, scopes);
                if event_ty != Ty::Error && !matches!(&event_ty, Ty::Named(n, _) if self.registry.is_enum(n)) {
                    self.error(
                        TypeErrorKind::WorkflowEventArgMustBeEnum { fn_name: name.to_string(), found: event_ty },
                        args[3].span(),
                    );
                }
                self.check(&args[4], &Ty::Json, expected_ret, scopes); // payload
                workflow_result_of(Ty::Bool)
            }
            // `docs/WORKFLOW.md`'s "state ownership" section: backs
            // `list_<workflow>_pending_for_me`. `identity` names the
            // caller whose owned states are being queried — the query
            // itself (which instances/states satisfy it) is entirely a
            // `workflow_log.rs`/`WorkflowDecl` runtime lookup, nothing
            // left to typecheck about it beyond these two argument types.
            ("__workflow_pending_for_me", 2) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // workflow_name
                self.check(&args[1], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes); // identity
                workflow_result_of(Ty::Json)
            }
            // `docs/WORKFLOW.md`'s "who submitted this" section.
            ("__workflow_submitted_by_me", 2) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // workflow_name
                self.check(&args[1], &Ty::Named("VerifiedIdentity".to_string(), vec![]), expected_ret, scopes); // identity
                workflow_result_of(Ty::Json)
            }
            // `docs/WORKFLOW.md`'s "audit trail" section — `identity` isn't
            // one of these two arguments: `get_<workflow>_history`'s own
            // `identity: VerifiedIdentity` param exists only so
            // `serve.rs::dispatch` demands *a* signed-in caller (this
            // read has no per-viewer scoping to check against it, a
            // disclosed simplification — see that fn's own doc comment).
            ("__workflow_history", 2) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // workflow_name
                self.check(&args[1], &Ty::I64, expected_ret, scopes); // instance_id
                workflow_result_of(Ty::Json)
            }
            ("__workflow_link_advance", 5) => {
                self.check(&args[0], &Ty::Str, expected_ret, scopes); // workflow_name
                self.check(&args[1], &Ty::I64, expected_ret, scopes); // instance_id
                let event_ty = self.infer(&args[2], expected_ret, scopes);
                if event_ty != Ty::Error && !matches!(&event_ty, Ty::Named(n, _) if self.registry.is_enum(n)) {
                    self.error(
                        TypeErrorKind::WorkflowEventArgMustBeEnum { fn_name: name.to_string(), found: event_ty },
                        args[2].span(),
                    );
                }
                let token_ty = self.infer(&args[3], expected_ret, scopes);
                if token_ty != Ty::Error && !matches!(&token_ty, Ty::Named(n, _) if self.registry.is_struct(n)) {
                    self.error(
                        TypeErrorKind::WorkflowStructArgMustBeStruct {
                            fn_name: name.to_string(),
                            arg: "token".to_string(),
                            found: token_ty,
                        },
                        args[3].span(),
                    );
                }
                self.check(&args[4], &Ty::Json, expected_ret, scopes); // payload
                workflow_result_of(Ty::Bool)
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
            "json_set_str" => 3,
            "dot" | "cross" | "solve" | "json_get" | "json_get_str" | "json_get_i64" | "json_get_f64"
            | "json_get_bool" | "json_array_get" | "check_role" | "extract_claim" | "extract_claim_path"
            | "db_query" | "db_execute" | "validate_api_key" | "mq_connect" | "constant_time_str_eq"
            | "dec_from_i64" | "dec_round" => 2,
            "http_get" | "https_get" | "exchange_refresh_token" | "mq_publish" | "mq_consume" | "check_role_path" => 3,
            "http_post" | "https_post" | "oidc_validate_token" | "send_email" | "send_sms" | "send_push" => 4,
            "mock_issue_token" => 7,
            "notify" | "__workflow_link_advance" | "__workflow_advance" => 5,
            "__workflow_start" => 3,
            "__workflow_pending_for_me" | "__workflow_submitted_by_me" | "__workflow_history" => 2,
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
/// list (docs/SANDBOXING.md layers 1-2): a plain scalar (an integer type or
/// `bool`, layer 1's original rule), or now also `chan T` where `T`
/// itself is a plain scalar (layer 2's real cross-process transport —
/// see `interpreter.rs`'s `ChannelInner`/`spawn_sandbox`). Not `chan` of
/// anything else, and not `box`/`&`/`thread`/`sandbox` at all — those
/// have no wire format defined yet (docs/SANDBOXING.md layer 3).
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

/// docs/goal.md row 9's load-bearing piece (§4 of `typeck.rs`'s module doc):
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

    let mut checker = Checker {
        sigs: HashMap::new(),
        errors: Vec::new(),
        registry: TypeRegistry::empty(),
        silent: false,
        current_ns: None,
        plugins: HashMap::new(),
    };
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
