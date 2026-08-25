# Structural index — large `compiler/src/` files

Line numbers are approximate, taken from the file state as of the date
in each file's header — they will drift as files are edited. If a
number looks wrong, `grep -n '<the name>' compiler/src/<file>.rs` to
find the current location; the names in this index are the durable
part.

## codegen.rs (5086 lines, as of 2026-08-23)

- `1` — module doc: LLVM codegen strategy overview (goal.md row 5, "native, hardware-speed codegen")
- `79` `struct CodegenError` — the one error type every codegen fallibility collapses into
- `89` `fn unsupported<T>` — the "reject, don't mis-compile" helper every not-yet-compiled construct calls
- `208` `fn affine_codegen_supported` — whether an affine-containing `Ty::Named` (Phase 4b) can compile
- `244` `fn llvm_ty` — maps a `Ty` to its LLVM type string (free-function form, used pre-`Codegen` construction)
- `386` `fn mangle_ty` — generic instantiation name mangling (`%Result$i64$str`-style)
- `430` `fn conservative_word_count` — over-allocated word count for a tagged-union enum payload
- `589` `pub fn check_supported` — the real gate: walks the whole program, rejects everything `codegen.rs` doesn't yet compile with a named reason (grep this for the ground-truth "what's compiled vs interpreter-only" list, don't trust docs)
- `601`/`608`/`629` `fn check_stmts`/`check_stmt`/`check_expr` — `check_supported`'s recursive walk
- `799` `struct Scopes` / `impl Scopes` — codegen's own variable-name→(Ty, LLVM-pointer) environment
- `833` `struct Codegen<'a>` — the main codegen driver: LLVM context/module/builder-equivalent state, register/label/global counters
- `928` `pub fn emit_llvm_ir` — top-level entry point: program → full LLVM IR text
- `1085` `fn bind_type_params_owned` — resolves a generic decl's type params against a concrete instantiation
- `1106` `impl Codegen<'_>` — the big impl block (through ~5010), methods below are inside it
- `1143` `fn llvm_ty` (method) — pure delegation to the free function, but through `&mut self`
- `1184` `fn declare_named_type` — emits a `struct`/`enum`'s LLVM named type declaration (struct: real fields; enum: hand-rolled `{i64 tag, [N x i64] payload}`)
- `1247` `fn ctor_ty` — resolves a struct/enum constructor call's target type
- `1323`/`1352`/`1385` `fn construct`/`construct_struct`/`construct_variant` — lowering `Expr::Call` as a struct/enum constructor
- `1484` `fn function` — lowers one `FnDecl` to an LLVM function definition
- `1581`/`1591` `fn stmts`/`fn stmt` — statement-sequence and single-statement lowering
- `1719` `fn local_ty_of` — recovers an already-typechecked expr's `Ty` without a full inference pass (trusts typeck already ran)
- `1868` `fn builtin_result_ty` — a builtin call's result type, needed before its args are known (for aggregate dest allocation)
- `1946` `fn widen_to_i64` — the one real signed/unsigned instruction choice (`zext` vs `sext`) this backend needs
- `2001` `fn guard_in_range` — Tier-1/2 bounds check emission, elided where SMT already proved it safe
- `2028` `fn emit_affine_free` — real `nir_free` call driven by `ownership.rs`'s `FreeMap`
- `2154` `fn guard_index_in_bounds` — `Vector`/`Matrix` dynamic-index bounds check
- `2174` `fn while_loop`
- `2213` `fn expr` — **the** scalar-value lowering entry point: `Expr` → one LLVM register/literal operand
- `2582` `fn call` — scalar-returning call dispatch (builtins + user fns)
- `2870` `fn call_builtin_scalar` — the huge per-builtin-name match for scalar-shaped builtins
- `3116` `fn call_builtin_agg` — same, for aggregate-shaped (`Vector`/`Matrix`-returning) builtins
- `3336`–`3529` geometry/linalg helper cluster: `lla_to_ecef_vals`, `ecef_to_lla_vals`, `enu_rotation_vals`, `mat_mul_ptr_vals`, `mat_mul_a_bt_vals`, `mat_vec_mul_ptr_vals`, `vec_add_vals`, `load_all_f64_vals`, `store_all_f64_vals` — the fully-unrolled-at-compile-time linalg codegen
- `3547` `fn expr_ptr` — **the** aggregate-value lowering entry point: `Expr` → pointer to a stack-allocated value (the `struct`/`enum`/`Vector`/`Matrix` counterpart to `expr`)
- `3654` `fn array_lit`
- `3691` `fn call_ptr` — aggregate-returning call dispatch (mirrors `call`)
- `3710` `fn binary` — the main `BinOp` lowering entry point, dispatches to the specialized helpers below for non-scalar cases
- `3910`–`3966` low-level emit helpers: `str_parts`, `icmp`, `fcmp`, `agg_elem_ptr`, `agg_load_elem`, `agg_store_elem`
- `4043` `fn agg_eq` — `struct`/`enum`/`Vector`/`Matrix` structural `==`/`!=`
- `4082` `fn str_eq` — `str` `==`/`!=` via a native-runtime `nir_str_eq` call, not hand-emitted IR
- `4107` `fn agg_binary` / `4120` `fn agg_elementwise` / `4174` `fn agg_mul` / `4247` `fn agg_scale` — `Vector`/`Matrix` `+`/`-`/`.*`/`./`/`*` lowering, fully unrolled
- `4261` `fn short_circuit` — `&&`/`||` as real branches, not eager and/or
- `4307` `fn if_expr` — `if`/`else` as a value-producing expression (the block-value-then-merge pattern `match_expr` reuses)
- `4479` `fn match_expr` — top-level `match` lowering, delegates to `match_enum`/`match_literal`
- `4540` `fn match_enum` — real LLVM `switch` on the tag word, one case per declaration-order variant
- `4665` `fn match_literal` — `str`/`i64`/`bool` literal-pattern match; `str` as a sequential `nir_str_eq`-then-branch chain
- `4834` `fn block_side_effects` — runs a block for side effects only (unit-valued `if`/`match` arms)
- `4857` `fn block_value_to_slot`
- `4902` `fn emit_c_main` — synthesizes the real C-ABI `main` wrapping the program's own `main` (return-value-as-exit-code / str-prints-and-exits convention)
- `5011` `pub enum OptLevel` — `-O0`/`-O2` (the whole point of the doc comment above it: `-O2`'s aggressive optimization is what actually stresses a subtly-wrong `unreachable`)
- `5033` `static RUNTIME_KERNELS_LIB` — the statically-embedded `runtime_kernels.rs` static-lib bytes (`include_bytes!`), so a compiled binary has no runtime dependency on this compiler's own installation
- `5033` `pub fn build` — the actual `clang`/linker invocation producing a native binary from emitted IR

## interpreter.rs (~5000 lines, as of 2026-08-23)

- `81` `struct MqConn` / `impl Debug/Deref/DerefMut` — thin wrapper around a `redis::Connection`
- `103` `pub enum Value` — every runtime value shape (the interpreter's dynamic type)
- `277` `struct ChannelInner` / `283` `enum TransportState` / `289` `impl ChannelInner` (`send`/`recv`) / `384` `impl Drop` — `chan`'s runtime backing, in-process or cross-process (sandbox) transport
- `412`–`504` raw I/O helpers: `write_value`/`read_value` (cross-process channel wire format), `write_tcp`/`read_tcp`, `write_file`/`read_file`
- `525` `struct SandboxChild` / `536` `impl` (`pid`, `stop`) / `565` `impl Drop` — real child-process handle with a genuine `Drop`-driven kill, not just Rust memory reclamation
- `576` `impl PartialEq for Value` — manual (not derived: `Thread`'s `JoinHandle` has no `PartialEq`); `Enum`/`Struct` compare name+fields structurally here — this is what `codegen.rs::agg_eq` and `eval_binary`'s `Value::Struct`/`Value::Enum` arm both delegate to
- `607` `fn literal_pattern_matches` — `match`'s literal-pattern arm test, backed by the `PartialEq` above
- `617` `impl Value` (`ty_name`, `render`) — runtime type name for error messages; `render` is `print`'s actual formatting (bool→`1`/`0`, not `true`/`false` — matches the compiled backend's disclosed cosmetic difference)
- `692` `pub enum ErrorKind` — every structured runtime-error shape (serializes for `--format=json`)
- `785` `pub struct RuntimeError`
- `859`–`1997` free-function builtin implementations, roughly one cluster per builtin family: JWT/JWKS (`jwks_key`, `hmac_sha256_base64url`, `mock_issue_token` at `1107`, `identity_claims`, `verified_identity_value`), sessions (`application_session_value`, `refresh_token_value`), hashing (`sha256_hex`/`sha256_hex_chain`), DB (`db_query` free fn at `1412`, `sql_bind_params`), HTTP/HTTPS (`build_http_request`, `parse_http_response`, `send_and_receive`, `http_request`/`https_request`), dense linalg (`matrix_det`/`matrix_inv`/`matrix_solve`/`matrix_rank`), geometry (`lla_to_ecef`/`ecef_to_lla`/`enu_rotation`/bearing), Kalman filter (`kf_predict`/`kf_update`)
- `1997` `fn eval_builtin` — **the** builtin dispatch: name → implementation, the interpreter's counterpart to `codegen.rs`'s `call_builtin_scalar`/`call_builtin_agg`
- `2671` `enum Signal` / `2676` `impl From<RuntimeError>` — the control-flow signal type (`Return`/`Err`/ordinary value) `eval_expr` threads through
- `2688` `struct Env` / `2697` `impl Env` — the interpreter's variable environment (scope stack)
- `2732` `pub struct Interpreter` — the whole interpreter's state; cheap to reconstruct per-thread (`Arc::clone` the program/source)
- `2896` `struct RngState` / `impl` — SplitMix64 + Box-Muller, the seeded-determinism RNG (goal.md's determinism row)
- `2938` `pub enum ReplayOutcome` — `transact` crash-replay's result shape
- `2954` `impl Interpreter` — the big impl block (through ~4900), methods below are inside it
- `2997` `pub fn run_main` / `3007` `run_main_on_big_stack` — top-level entry points
- `3019` `pub fn call_named` — call an arbitrary function by name (what `serve.rs`'s `POST /api/<fn>` and the CLI's `--call` path both use)
- `3055` `pub fn replay_pending_transactions` — crash-replay entry point, run once at `serve` startup before new requests
- `3190`/`3200`/`3209` `fn find_fn`/`find_struct`/`find_variant` — AST lookup by name
- `3290` `fn eval_transact_slot` / `3300` `eval_transact_slot_args` / `3327` `run_transact_write_slot` / `3372` `call_network_with_retry` — `transact`'s slot execution (TRANSACT.md Layer 2's retry-with-backoff lives at `3372`)
- `3465` `fn call` — ordinary (non-transact) function-call dispatch, the interpreter's counterpart to `codegen.rs::call`/`call_ptr`
- `3552` `fn effect_of_fn` — lazy per-fn effect lookup, only computed when tracing is on
- `3583` `fn check_ty` — runtime type-tag verification
- `3695`/`3702` `fn exec_block`/`exec_stmts` — a block's value is its last expression-statement, same convention `if`'s branches use
- `3755` `fn eval_expr` — **the** expression evaluator, the interpreter's counterpart to `codegen.rs::expr`/`expr_ptr` combined (one function, not split — no aggregate/scalar distinction needed without LLVM register types)
- `4740` `fn spawn_sandbox` — cross-process sandboxed function call, lossless value round-trip over the channel wire format
- `4812` `fn eval_binary` — `BinOp` evaluation; the `Value::Struct`/`Value::Enum` arm here (added 2026-08-23 alongside the str-ban work) is what makes `struct`/`enum` `==`/`!=` actually work at runtime, not just typecheck

## typeck.rs (~3461 lines, as of 2026-08-23)

- `61` `pub enum TypeErrorKind` — every structured type error shape (serializes for `--format=json`); every new static check this project adds gets a variant here, e.g. `StrInFnSignature`
- `376` `pub struct TypeError` / `impl Display` — wraps a `TypeErrorKind` with its `Span`, and formats it human-readably
- `720` `struct FnSig` / `732` `struct Scopes` — signature table and lexical-scope variable-type stack
- `752` `pub struct Checker<'a>` — the whole typecheck pass's state (registry, signatures, accumulated errors)
- `771` `pub fn typecheck` — top-level entry point; registers every struct/enum/fn name (two namespaces: type names vs. callable names — see Row 11 §3.1/3.2), then checks every `fn` body. A program the interpreter runs was always fully typed and proved-returning first.
- `921` `fn error` — push one `TypeErrorKind` at a `Span`, the one place every check funnels through
- `968`/`1009`/`1018` `fn check_screen`/`check_dashboard`/`check_metric_ref` — Row 12 UI-DSL shape checks (existence/shape only, not full signature enforcement — see `compiler/UI_DSL_TODO.md`)
- (near `check_screen`) `fn check_pattern_expr`/`check_format_expr`/`check_min_max_expr` — `field <name> { pattern/format/min/max: ... }` shape + field-type-applicability checks (2026-08-24), incl. compiling `pattern`'s regex via the `regex` crate at typeck time
- `1050` `fn validate_ty` — well-formedness of a type expression itself (arity, unknown names) — recurses through every type-former the same way `ast.rs::Ty::contains_str` does
- `1100` `fn check_fn` — per-function entry point: the "enum favoring" `str`-in-signature scan lives here (2026-08-23), then body-checks, then `NotAllPathsReturn`
- `1132`/`1138`/`1144` `fn check_stmts`/`check_block`/`check_stmt` — statement-level checking (no value context)
- `1185` `fn check_stmt_expr`
- `1211` `fn check` — **the** "does this expr have exactly this type" entry point; every value position (`let`, `return`, assignment RHS, call arg) goes through here, so "no implicit conversions" is enforced in exactly one place
- `1275` `fn infer` — the no-expected-type counterpart to `check`, for unary operands and other positions the grammar doesn't pin down
- `1616`/`1664` `fn infer_sandbox_spawn`/`infer_spawn`
- `1688`/`1768`/`1789` `fn infer_transact`/`infer_transact_slot`/`infer_transact_slot_durable` — `transact`'s slot-shape rules (`txn_id`'s exemption from the str-ban lives in `check_fn`, not here — this only validates the `transact` expression itself)
- `1824` `fn infer_call` — ordinary function-call type inference (builtins dispatch separately, see `infer_builtin_call`)
- `1943` `fn infer_acquire` — privileged first-class function (`acquire`/`requires`) checking, §6a
- `1972`/`2011` `fn infer_struct_construction`/`infer_variant_construction`
- `2082` `fn resolve_type_args` — generic instantiation resolution
- `2126` `fn check_match` — exhaustiveness (every enum variant covered exactly once, no wildcard in v1) is checked unconditionally here, regardless of how the match's value is used
- `2275` `fn check_literal_match` — the `str`/`i64`/`bool` literal-pattern sibling of `check_match`
- `2386` `fn infer_builtin_call` — the huge per-builtin-name signature table (`json_get_str`, `db_query`, `http_post`, `oidc_validate_token`, ... — this is where a new builtin's typeck signature gets added)
- `2866`–`2931` builtin-checking helpers: `builtin_arity_hint`, `wrong_arg`, `literal_dimension`, `expect_f64_matrix`/`expect_square_f64_matrix`/`expect_f64_vector`
- `2931` `fn infer_binary` — the `BinOp` type-inference dispatch (`Eq`/`NotEq` permit any matching type generically here — including `struct`/`enum` — which is why `interpreter.rs::eval_binary` needed a matching runtime arm)
- `3018`/`3078` `fn infer_mul`/`infer_hadamard` — linear-algebra `*` and elementwise `.*`/`./`
- `3107` `fn infer_array_lit`
- `3143` `fn unify_operands` — the shared "do these two operand types agree" logic every binary operator's inference goes through; returns `Ty::Error` on mismatch so callers can suppress follow-on diagnostics
- `3214` `fn check_if` — every branch (including a missing `else`, unless `ty` is `unit`) must produce the same type
- `3274` `fn check_block_value` — a block used in value position (the mechanism `if`/`match`/`transact` all share)
- `3315` `fn bind_type_params` / `3336` `fn definitely_returns` / `3352` `fn if_definitely_returns` / `3372` `fn is_elementwise_operand` / `3386` `fn is_sandbox_safe` — free-function helpers used across the checker
- `3404` `pub struct FragmentEnv` / `3441` `pub fn validate_fragment` — the agent-facing "typecheck one expression fragment in a given variable-type context" entry point (goal.md row 9 / `nirdosha-agent-api.md`'s A3 endpoint's underlying capability)

## ast.rs (~1681 lines, as of 2026-08-23)
- `25` `pub enum Ty` — every static type this language has; the grammar's `type` production made concrete
- `243` `impl Ty` — methods below are inside it
- `244` `fn from_name` / `271` `fn name` — string ↔ `Ty` for primitive type names
- `316`–`390` classification predicates: `is_unsigned`, `is_integer`, `is_numeric`, `is_aggregate` (lives in one SSA register vs. needs a stack slot — `codegen.rs`'s `expr` vs. `expr_ptr` split), `is_transact_scalar` (the 4 types `transact`'s durability log can serialize)
- `403` `fn contains_str` — the "enum favoring" str-ban's recursive scan (added 2026-08-23), walks every type-former the same way `validate_ty`/`substitute_ty` do
- `434` `fn is_affine` — the property that makes ownership meaningful; deliberately blind to `Ty::Named`'s real affinity (see `TypeRegistry::is_affine` below for the struct/enum-aware version)
- `459`/`475` `fn in_range`/`bounds` — an integer type's legal value range, used by both the interval-analysis proof and codegen's guard emission
- `518`/`530` `pub struct Param`/`Field`
- `550` `pub struct StructDecl`
- `572`/`586` `pub struct Variant`/`EnumDecl`
- `611`/`645` `pub fn prelude_enums`/`prelude_structs` — `Option(T)`/`Result(T,E)` and every Row-12 identity struct (`VerifiedIdentity`, `RoleView`, ...), injected into every program at parse time
- `746` `pub enum Effect` / `760` `impl` (`name`) — the `effect(...)` annotation vocabulary (`pure`/`io`/`network`/`concurrent`/`rng`)
- `778` `pub struct TransactSlot`
- `785` `pub struct FnDecl` — a function declaration's full shape (params, ret, body, `effect(...)`, `requires(...)`)
- `827` `pub enum Requirement` / `839` `impl` (`proof_ty`, `describe`) — `requires(role/claim: ...)`'s parsed shape
- `863` `pub struct Block`
- `868` `pub enum Stmt`
- `889`/`916` `pub enum BinOp`/`UnOp`
- `922` `pub enum Expr` — every expression form the parser produces
- `1136` `pub struct MatchArm` / `1165` `enum LiteralPattern` / `1173` `enum ElseBranch`
- `1178` `impl Expr` (`span`) — every `Expr` variant's source span, for error reporting
- `1216` `pub struct Program` — the whole-program AST root (`fns`/`structs`/`enums`/`screens`/`dashboard`)
- `1244`–`1305` Row 12 UI-DSL AST: `FieldOverride`, `ActionDecl`, `ScreenDecl`, `MetricRef`, `DashboardDecl`
- (just above `FieldOverride`) `pub fn well_known_format_pattern` — `field <name> { format: "..." }`'s fixed vocabulary (`email`/`phone`/`date`/`url`/`uuid`) → regex, the single source of truth `typeck.rs`/`ui_gen.rs` both consume (2026-08-24)
- `1305` `pub struct TypeRegistry<'a>` — the struct/enum declaration lookup table every later pass (typeck, ownership, effects, codegen, ui_gen) builds once and queries repeatedly
- `1327`–`1396` `impl TypeRegistry` methods: `build`, `struct_decl`/`enum_decl`, `struct_fields`/`enum_variants`, `struct_type_params`/`enum_type_params`, `is_struct`/`is_enum`, `find_variant`, `is_affine` (the struct/enum-aware version, delegates to `Ty::is_affine` for everything else)
- `1435` `pub fn result_of` — `Result(ok, str)` shorthand every builtin signature uses (builtins are exempt from the str-ban — see `LANGUAGE.md` §6b)
- `1439`/`1455` `pub fn zip_type_params`/`substitute_ty` — Row 11 layer 6's generic-substitution mechanism, used identically by typeck/ownership/codegen
- `1490` `pub fn literal_value` — extracts a literal integer from an `Expr`, for `zeros`/`ones`/`identity`'s compile-time-only dimension argument
- `1522` `pub const BUILTIN_NAMES` — the full builtin name list (grep here for "does builtin X exist")
- `1679` `pub fn is_builtin`

## parser.rs (1415 lines, as of 2026-08-23)
- `13` `pub struct ParseError`
- `18` `pub struct Parser` — hand-written recursive-descent, strictly one token of lookahead, no backtracking (GRAMMAR.md's Row 7 claim)
- `57` `impl Parser` — the whole parser, methods below are inside it
- `64`/`74` `fn enter_nesting`/`exit_nesting` — recursion-depth guard (stack-overflow protection for deeply nested expressions)
- `78`/`82`/`86`/`94` `fn peek`/`span`/`bump`/`expect` — the token-stream primitives everything else is built from
- `105`/`124` `fn expect_ident`/`expect_usize_literal`
- `139` `fn expect_type` — `type ::= "&" type | "box" type | i8 | ... | bool | unit | ident "(" type,* ")"`
- `264` `pub fn parse_program` — top-level entry point: `program ::= item*`
- `315` `fn parse_module_decl` — `module "Name" { ... }` nav-grouping sugar (single-level only)
- `369`–`499` Row 12 UI-DSL parsing: `parse_kv_entry`, `parse_screen_decl`, `parse_field_override`, `parse_action_decl`, `parse_dashboard_decl`
- `499` `fn parse_type_param_list` — `struct Pair(A, B)`'s generic parameter list
- `521`/`551` `fn parse_struct_decl`/`parse_enum_decl`
- `593` `fn parse_fn_decl`
- `630`/`686` `fn parse_effect_annotation`/`parse_requires_annotation` — `effect(...)`/`requires(role/claim: ...)`
- `717` `fn expect_str_lit`
- `731`/`742` `fn parse_block`/`parse_stmt`
- `753`/`772`/`783`/`794` `fn parse_audited_stmt`/`parse_let_stmt`/`parse_return_stmt`/`parse_while_stmt`
- `803` `fn parse_expr` — expression parsing entry point, feeds into the precedence-climbing chain below
- `826` `fn parse_match_expr`
- `911`–`997` `transact { ... }` parsing: `parse_transact_expr`, `parse_optional_int_modifier` (retry/timeout counts), `parse_transact_slot`, `parse_optional_transact_slot`
- `997` `fn parse_assignment`
- `1017` `fn parse_if_expr`
- **Precedence-climbing chain** (each calls the next, lowest to highest precedence — this *is* how binary-operator precedence works in this LL(1) grammar, not left-recursion): `1037 parse_logic_or` → `1048 parse_logic_and` → `1059 parse_equality` → `1075 parse_comparison` → `1093 parse_additive` → `1109 parse_multiplicative` → `1127 parse_unary`
- `1134` `fn parse_unary_inner` — the largest single production: unary ops, `box`/`&`/`*`, `spawn`/`join`/`chan`/`send`/`recv`, `sandbox`/`stop`, `connect`/`listen`/`accept`, `acquire`, literals
- `1290` `fn parse_call` — call-args and the chained-call rejection (`f()()` is a parse error)
- `1333` `fn parse_postfix` — `.field` access
- `1361` `fn parse_primary` — identifiers, parenthesized exprs, literals

## ui_gen.rs (963 lines, as of 2026-08-23)
- `80`/`127`/`180`/`192` `struct FieldSpec`/`Action`/`Metric`/`Screen` — the derived-UI shape, serialized as the JSON manifest the client-side renderer reads
- `212` `fn to_snake_case`
- `227` `fn find_fn`
- `237` `fn to_display_label`
- `254` `fn ty_label`
- `277`/`281` `fn resolve_struct`/`resolve_enum`
- `292` `fn is_date_like_field_name` — naming-convention heuristic for calendar-picker vs. plain text input
- `309` `fn build_field` — **the** field→form-control mapping: `Option(T)` unwraps, a zero-payload-only enum renders as a dropdown, the `Text {value: str}` free-text carrier (2026-08-23, str-ban work) renders as a plain input instead of a nested group, a struct reference expands one level deep, everything else falls back to `readonly`
- `413`/`417` `fn fn_requires_login`/`fn_role_gate`
- `425` `fn effect_badges`
- `441`/`469` `fn build_action`/`build_custom_action` — CRUD-convention and declared-`screen`-action derivation
- `487`/`503` `fn kv_str`/`kv_gate` — `screen` DSL's `key: value` entry helpers (`role(...)`/`claim(...)` extraction)
- (near `kv_str`) `fn kv_num` — numeric sibling of `kv_str`, for `min`/`max` (2026-08-24)
- `521` `fn find_screen_decl`
- `527` `fn to_title_case`
- `541`/`548`/`559` `fn is_numeric_scalar`/`is_stat_return_ty`/`is_chart_return_ty` — `stat_`/`chart_` naming-convention dashboard-metric detection
- `570`/`588`/`592` `fn build_metrics`/`build_stats`/`build_charts`
- `600` `pub struct GatedField` / `608`/`629`/`651`/`692` field-visibility gate resolution: `gates_from_screen_decl`, `field_gates_for_struct`, `field_gates_for_fn`, `update_gates_for_fn` — this is what `serve.rs`'s server-side redaction/edit-blocking actually consults
- (near `update_gates_for_fn`) `pub struct ValidatedField` / `fn resolve_pattern`/`validations_from_screen_decl` / `pub fn field_validations_for_fn` — field-format-constraint resolution (`pattern`/`format`/`min`/`max`), 2026-08-24; unlike `update_gates_for_fn`, matches either a struct's `create` OR `update` slot — this is what `serve.rs::check_field_validations` consults
- `727` `fn apply_field_overrides`
- `749` `fn build_screens` — assembles the full per-struct `Screen` list (the main derivation pass)
- `818`/`829`/`840` `fn field_json`/`metrics_json`/`manifest_json` — JSON-manifest serialization
- `900` `pub fn generate` — top-level entry point: program → complete self-contained HTML string (embeds `manifest_json` into `ui_gen_template.html`)
- (near `generate`) `pub struct Theme` / `ThemeFonts`/`ThemeRadius`/`ThemeDensity`/`ThemeMotion`/`ThemeLayout`/`ThemeTypeScale` — 2026-08-25 redesign, a 1:1 mirror of protobox's `resolve_design_tokens()` JSON shape (LANGUAGE.md SS11b); `fn theme_override_css`/`theme_html_class`/`theme_bootstrap_script` — the three `__NIRDOSHA_*__` placeholders `generate` splices in; `const RAMP_STEPS` / `struct RampRoleStep` + the `PRIMARY`/`ON_PRIMARY`/`SURFACE`/etc. consts — the semantic-role → ramp-step mapping deriving `--md-*` from the raw `brand`/`neutral` ramps

## serve.rs (957 lines, as of 2026-08-23)
- `54` `pub struct AuthConfig` — JWKS/issuer/audience server-side config
- `60`/`68` `fn cors_headers`/`header`
- `78` `pub fn run` — top-level entry point: binds the HTTP server, wires `--db` migration (`migrate.rs::plan_and_apply`) + crash-replay before serving
- (near `run`) `struct ThemeCache` / `fn theme_ttl`/`refresh_theme_html_if_stale` — live `--theme` reload, 2026-08-25 (LANGUAGE.md SS11b): re-reads `theme.json` from disk and regenerates the served HTML on `GET /` at most once per TTL (env-overridable via `NIRDOSHA_TEST_THEME_TTL_MS` for `tests/theme_reload.rs`), tolerating a missing/malformed file by keeping the last-good page instead of erroring
- `113` (inline in `run`) — `migrations_dir` derivation (`--db` path's parent + `/migrations`) and the `migrate.rs`/`replay_pending_transactions` call sequence — see `ROADMAP.md` Track B1/B2 before changing this
- `262` `fn to_snake_case`
- (near `build_table_catalog`) `struct RoleMappingCache` / `fn load_role_mapping`/`refresh_role_mapping_if_stale`/`role_mapping_ttl`/`identity_has_mapped_role` — the identity role-mapping cache (`ROADMAP.md` Track A6, 2026-08-24): app_role -> idp_role synonyms, loaded eagerly at `run` startup + refreshed on a TTL (env-overridable for `tests/role_mapping.rs`), consulted by every `requires(role:...)`/`view`/`edit` check via `identity_has_mapped_role` in place of a bare `interpreter::identity_has_role`
- `290` `fn build_table_catalog`
- `301`/`317` `fn json_to_sql_value`/`sql_value_to_json`
- `342` `fn dispatch_table_query` — the paginated/searchable table route (`serverTableApi`); a `list_<struct>` doing a join or computed column is invisible to this route, client falls back to the plain unpaginated call
- `474` `fn resolve_identity` — bearer-token → validated identity, the real server-side `requires(...)` enforcement `Expr::Acquire` alone doesn't provide
- `514` `fn identity_satisfies_gate`
- `540` `fn redact_gated_fields` — field-level view-gate enforcement (reads `ui_gen::GatedField`)
- `579` `fn dispatch` — the actual request router, deliberately plain-data-in/`(status, json body)`-out with no `tiny_http` types, so `tests/serve.rs` can exercise it without a real socket
- `741` `fn check_edit_gates` — field-level edit-gate enforcement
- (near `check_edit_gates`) `fn check_field_validations` — field-format-constraint enforcement (`pattern`/`format`/`min`/`max`), 2026-08-24; unlike `check_edit_gates`, needs no `--db` (checks only the incoming value, never a stored one) and runs for both `create_`/`update_`
- `794` `fn value_matches_stored`
- `806` `fn describe_requirement`
- `813` `fn is_verified_identity`
- `817` `fn json_err`
- `821`/`825` `fn resolve_struct`/`resolve_enum`
- `836` `fn decode_enum_value` — the one enum shape that round-trips through a JSON request body: zero-payload-only
- `860` `fn decode_value` — JSON request body → `Value`, generic over any struct (this is why `struct Text { value: str }` needed zero `serve.rs` changes)
- `908` `fn encode_value` — the inverse: `Value` → JSON response body

## ownership.rs (848 lines, as of 2026-08-23)
- `76`/`82` `pub enum OwnershipErrorKind` / `struct OwnershipError`
- `104` `struct OwnScopes` / `106` `impl` — the move-tracking scope stack (name → (type, moved-flag))
- `143` `fn merge_moved` — branch-uniform merge: an affine value moved on only *some* `if`/`match` branches is rejected everywhere after that point (the checker isn't branch-uniform — cleanup must happen exactly once, on every path, uniformly)
- `169` `pub struct FreeMap` — the ownership pass's real deliverable for codegen: which `nir_free` call to emit, at which AST node's span, for every affine binding
- `219`/`227` `fn still_owned_affine`/`all_still_owned_affine`
- `231` `pub struct Checker<'a>`
- `273` `fn builtin_return_ty` — needs the same builtin-signature shape `typeck.rs::infer_builtin_call` has, but without re-deriving it through real type inference
- `306` `fn run_checker`
- `331` `pub fn check_ownership` — top-level entry point
- `344` `pub fn compute_free_map` — the other top-level entry point, what `codegen.rs` actually calls to know what to free and when
- `349` `fn error`
- `355`/`367`/`375`/`415` `fn check_stmts`/`check_block`/`check_stmt`/`check_stmt_expr`
- `424` `fn check_if_branches` / `479` `check_match_arms` / `530` `check_while` — the branch-uniform-affine-cleanup enforcement points
- `562` `fn touch_expr` — the core move-checking visitor; `consume=true` at value positions, `false` only inside `Expr::Deref` (reading *through* a box is exempt)
- `833` `fn touch_ident` — where a name actually gets marked moved

## runtime_kernels.rs (825 lines, as of 2026-08-23)
- `1` — module doc: this file compiles as an isolated `rustc --crate-type staticlib` (no `--extern`), linked into every `codegen.rs`-produced binary; the same "native call costs what inlined IR costs" reasoning as `@printf`/libm
- `29`–`214` pure-Rust `f64`-slice math (called from Rust, not `extern "C"` — the `nir_det`/etc. wrappers below call into these): `matrix_det`, `matrix_inv`, `matrix_solve`, `matrix_rank`, `mat_mul_f64`, `mat_vec_mul_f64`, `mat_transpose_f64`, `vec_add_f64`, `vec_sub_f64`
- `218` `fn kf_update` — Kalman filter update step, shared by the two `nir_kf_update_*` C-ABI wrappers
- `279`/`329` `fn sha256_compress`/`sha256` — the from-scratch SHA-256 (no `sha2` crate access here — see module doc), bit-verified against the standard's own test vectors
- `375`/`388` `fn hex_encode`/`constant_time_eq` — pure-Rust helpers behind `nir_sha256_hex`/`nir_constant_time_str_eq`
- **The C-ABI surface** (`extern "C" fn`, what `codegen.rs` actually emits `call`s to by name) — `410 nir_det`, `419 nir_inv`, `435 nir_solve`, `451 nir_rank`, `464 nir_kf_update_state`, `494 nir_kf_update_cov`, `527 nir_str_eq`, `562 nir_tcp_connect`, `576 nir_tcp_listen`, `590 nir_tcp_accept`, `603 nir_tcp_send`, `620 nir_tcp_recv`, `637 nir_tcp_stop`, `665 nir_sha256_hex`, `677 nir_constant_time_str_eq`, `725 nir_rand_seed`, `743 nir_rand_f64`, `755 nir_rand_gaussian`, `791 nir_alloc`, `814 nir_free`
- `706`/`715` `fn splitmix64_next`/`rand_next_f64` — the process-wide RNG stream backing `nir_rand_*` (necessarily process-wide, not per-"interpreter instance" — LANGUAGE.md §9)

## smt.rs (697 lines, as of 2026-08-23)
- `61` `pub struct SmtReport` — the whole program's Z3-discharged proof result set (which bounds/div-by-zero/index checks got proven statically, elidable at codegen)
- `78` `struct Scopes` / `80` `impl` — symbolic-`Int`-valued variable environment (mirrors `typeck.rs`/`codegen.rs`'s own `Scopes`, but maps names to Z3 terms, not `Ty`s)
- `106` `pub fn analyze` — top-level entry point, real Z3-backed SMT checking (row 4's interval-analysis precursor was upgraded to this)
- `125`/`136`/`145`/`156` `fn assert_bounds`/`prove_in_range`/`prove_nonzero`/`prove_index_in_bounds` — the actual theorem-proving calls
- `169` `fn ty_dims`
- `177` `struct Checker<'s>` / `187` `impl` — methods below are inside it
- `188`/`194`/`200` `fn stmts`/`block`/`stmt`
- `239` `fn expr_stmt`
- `267`/`271` `fn enter_branch`/`exit_branch` — path-condition scoping for `if`/`match` branches
- `275` `fn while_loop`
- `300` `fn expr` — the main symbolic-evaluation walk; anything this can't reduce gets a fresh, unconstrained `Int` (always sound, claims no information — same fallback shape as `refine.rs`'s `Interval::unknown()`)
- `481` `fn block_value`
- `500` `fn binary`
- `540` `fn bool_expr`
- `593`/`595`/`610` `fn assigned_names`/`walk_stmts`/`walk_expr` — free-variable/reassignment discovery for loop-invariant handling

## refine.rs (675 lines, as of 2026-08-23)
- `1` — module doc: **Tier 1** bounds proving via interval (range) analysis, *not* SMT — a deliberate, documented substitution for when a real solver isn't available (no system Z3/`cmake` in this environment), not a silent downgrade. `smt.rs` is the real Z3-backed Tier 2 upgrade; the two are complementary, not one superseding the other — `codegen.rs::guard_in_range`/`guard_index_in_bounds` elide a check if *either* tier already proved it.
- `74`/`79` `struct Interval`/`impl` — `exact`/`unknown`/`full`/`union`/`neg`/`add`/`sub`/`mul`/`within`/`excludes_zero`: the abstract-interval arithmetic itself
- `154` `pub struct RefineReport` — Tier 1's proof result set, same shape/role as `smt.rs::SmtReport`
- `178` `fn ty_dims`
- `186`/`188` `struct Scopes`/`impl` — interval-valued variable environment (mirrors `smt.rs`'s `Scopes` but with `Interval` instead of a Z3 `Int` term)
- `232` `pub fn analyze` — top-level entry point
- `248`/`258` `struct Refiner`/`impl` — methods below are inside it
- `259`/`265`/`271` `fn stmts`/`block`/`stmt`
- `301` `fn while_loop`
- `330` `fn expr` — the main interval-evaluation walk (structurally parallel to `smt.rs::Checker::expr`)
- `519` `fn block_value`
- `538` `fn binary`
- `571`/`573`/`588` `fn assigned_names`/`walk_stmts`/`walk_expr`

## main.rs (517 lines, as of 2026-08-23)
- `7` `fn main` — CLI entry point: pulls `--format=json`/`--otel-console`/`--transact-log=` off anywhere in argv, then dispatches on the first remaining arg to a subcommand (`build`/`emit-llvm`/`emit-ast`/`emit-ui`/`serve`/`--sandbox-worker`), or `cmd_interpret` if the first arg is just a path
- `71` `fn print_usage`
- `92` `fn read_source`
- `103` `fn print_value` — renders a successful run's result uniformly regardless of entry point (`run`/`run_diagnostic`); `--format=json` only changes how *failures* are reported
- `127` `fn cmd_interpret` — the default (no subcommand) path: parse → typecheck → own-check → `Interpreter::run_main`
- `177` `fn typecheck_and_own` — the shared parse+typecheck+ownership pipeline every subcommand but `emit-ast` gates on; `codegen.rs::check_supported` is a separate, narrower, backend-specific gate run later inside `build`/`emit-llvm` themselves
- `195` `fn cmd_build` — `nirdosha build <file> -o <out> [--opt0]`: full pipeline → `codegen::build` → native binary
- `240` `fn cmd_emit_llvm` — same pipeline, prints IR text instead of linking
- `278` `fn cmd_emit_ast` — parses only (not the full `typecheck_and_own` gate) — a program that doesn't yet typecheck is still legitimate to inspect here, unlike `build`/`emit-llvm`/`emit-ui`
- `320` `fn cmd_emit_ui` — needs the *typed* program (screen inference reads resolved struct fields/fn signatures), same gate as `build`/`emit-llvm`
- `381` `fn cmd_serve` — binds `serve.rs::run`; a bearer token with no server-side `AuthConfig` is a clear 500, not silently accepted or ignored
- `455` `fn cmd_sandbox_worker` — not a user-facing subcommand; the only caller is a `sandbox` handle's own `Expr::SpawnSandbox`, which execs this same binary with `--sandbox-worker` to become the child process
