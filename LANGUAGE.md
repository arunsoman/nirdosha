# Nirdosha — language and feature reference

A practical reference to what Nirdosha actually supports today, mined
directly from the implementation (`compiler/src/`) rather than from the
design docs (`goal.md`, `Nirdosha_Unified_Plan.md`), which describe
intent and aspiration as much as delivered fact. Where something is
interpreter-only vs. compiled to native code, that's called out
explicitly — it matters for anyone benchmarking or reasoning about
performance.

---

## 1. Execution modes

```sh
nirdosha <file.nir> [--format=json]   # interpret (tree-walking)
nirdosha build <file.nir> -o <out> [--opt0]   # compile to a native binary (LLVM, -O2 by default)
nirdosha emit-llvm <file.nir>         # print the generated LLVM IR
nirdosha emit-ast <file.nir>          # print the parsed AST as JSON
```

- **Interpret** — always works for every construct in this document.
- **Build/emit-llvm** — only supports the subset covered in §10
  ("What's compiled"). Everything else is rejected at compile time with
  a specific reason (`codegen::check_supported`), never silently
  mis-compiled.
- `--format=json` — on failure, prints a structured `Diagnostic` (one
  shape across type/ownership/runtime errors) instead of a plain-text
  message. `emit-ast` always prints JSON — the same `Serialize`-derived
  shape a fragment-validation caller (`typeck::validate_fragment`) can
  deserialize back.

---

## 2. Types

| Type | Spelling | Affine? | Notes |
|---|---|---|---|
| Signed integers | `i8` `i16` `i32` `i64` | no | Range-checked at every `let`/return/assign boundary (runtime; some proven away statically — see §8). |
| Unsigned integers | `u8` `u16` `u32` `u64` `usize` | no | Same range-checking. Compiled (§10) — same widths as their signed counterparts; `codegen.rs` needs the signed-vs-unsigned instruction choice in exactly one place, not throughout (every unsigned type's range is capped at `[0, i64::MAX]`, and this backend computes all arithmetic at `i64` width regardless of source width). |
| Float | `f64` | no | IEEE 754 double. One width — no `f32`, no literal-widening story. Saturates (`inf`/`NaN`), never traps. |
| Boolean | `bool` | no | |
| Unit | `unit` | no | No literal syntax — only reachable as a function's implicit return. |
| String | `str` | no | UTF-8, `Arc<str>`-backed. Literal + escapes (`\"` `\\` `\n` `\t` `\r`) only — no concatenation, slicing, or indexing. May not be, or be part of, a user `fn`'s parameter or return type — see §6b. |
| Heap cell | `box T` | **yes** | Single-owner heap allocation. `*expr` dereferences. |
| Shared reference | `&T` | no | Read-only borrow of a plain identifier only (`&x`, not `&(x+1)`). No `&mut`. |
| Thread handle | `thread T` | **yes** | A spawned computation's real-OS-thread handle; `join` consumes it once. |
| Channel | `chan T` | no | Unbounded MPMC queue; the handle is freely copyable, the *payload* moves through `send`. |
| Sandbox handle | `sandbox` | **yes** | A real, separate OS process; `stop` consumes it once. |
| TCP connection | `tcp` | **yes** | A real TCP socket (client or accepted server side); `stop` closes it once. |
| TCP listener | `tcp_listener` | **yes** | A real bound+listening TCP socket; `accept` doesn't consume it, `stop` does. |
| File handle | `file` | **yes** | A real local file (`open(path, mode)`); `send`/`recv`/`stop` reused verbatim from `tcp` — see `PROTOLANG_PORT.md`. |
| Verified identity | `VerifiedIdentity` | no | Row 12: result of validating an external IdP token. Freely copyable. Fields: `subject`, `issuer`, `audience`, `expires_at`, `issued_at`, `claims_json`. |
| Role proof | `RoleView` | no | Row 12: proof that `check_role(identity, role)` succeeded. Field: `role`. |
| Claim proof | `ClaimView` | no | Row 12: proof that `extract_claim(identity, name)` succeeded. Field: `value`. |
| Vector | `Vector(T, N)` | no | Fixed-length dense 1-D array, `N` a compile-time literal. `Vector(f64, 3) ≠ Vector(f64, 4)` — different types. |
| Matrix | `Matrix(T, R, C)` | no | Fixed-shape dense 2-D array, row-major, `R`/`C` compile-time literals. |

`Vector`/`Matrix` are generic over element type `T`, but every dense
linear-algebra builtin (§5) requires `T = f64` specifically — integer- or
bool-element vectors/matrices only support literal construction,
indexing, and elementwise `+`/`-`/`.*`/`./`/`==`/`!=`.

---

## 3. Literals

```
42                  // i64 by default; flexes to a narrower declared width if it fits
3.14                // f64 — decimal only, no scientific notation
true  false
"hello\nworld"      // \" \\ \n \t \r only
[1.0, 2.0, 3.0]                       // Vector(f64, 3)
[[1.0, 2.0], [3.0, 4.0]]              // Matrix(f64, 2, 2), row-major
```

Integer literals are the *only* thing with width flexibility — `let n:
i8 = 100` needs no cast, but `let a: i64 = 1; let b: i32 = 2; a + b` is a
type error (no implicit conversions, ever, between two already-typed
values). A float literal is always exactly `f64`; there is no
int↔float conversion operator at all.

---

## 4. Operators

| Op | Meaning | Operand shapes |
|---|---|---|
| `+` `-` | Add/subtract | scalar↔scalar (same type); `Vector`/`Matrix` elementwise (exact same shape) |
| `*` | Multiply | scalar↔scalar; scalar×`Matrix` (either order, scalar type = element type); `Matrix`×`Vector` (inner dims match); `Matrix`×`Matrix` (inner dims match) |
| `/` | Divide | scalar↔scalar only. Int: traps on zero (Tier-2 runtime check, provable away — §8). Float: saturates to `inf`/`NaN`, never traps. |
| `.*` `./` | Hadamard (elementwise) multiply/divide | scalar or `Vector`/`Matrix`, exact same shape |
| `==` `!=` | Equality | any matching type, including `Vector`/`Matrix` (structural) |
| `<` `>` `<=` `>=` | Ordering | numeric scalars only |
| `&&` `\|\|` | Short-circuit bool | `bool` only |
| `!` | Not | `bool` |
| `-` (unary) | Negate | numeric scalar |
| `*` (unary) | Deref | `box T` or `&T` |
| `&` (unary) | Borrow | a plain identifier |

`Vector * Vector` is a **type error** by design (ambiguous — inner vs.
outer product) — use `dot()`.

No `%` (modulo), no bitwise operators, no `f.(x)`-style broadcasting.

---

## 5. Builtins

Every builtin is native Rust, registered by name (`ast::BUILTIN_NAMES`) —
not expressible in Nirdosha source itself (no generics, no `for` loops
to write them with). All require `f64` elements unless noted.

**I/O**
- `print(x)` — any number of args, any type, when interpreted. When
  *compiled*, every scalar shape works (integer/`f64`/`str`/`bool`/
  `unit` — literal, variable, or computed result, all identically); the
  one remaining gap is a whole `Vector`/`Matrix` argument (§10).

**Dense linear algebra** (Phase 2)
- `transpose(m: Matrix) -> Matrix` — any element type.
- `dot(a: Vector, b: Vector) -> T` — same length, numeric element.
- `cross(a: Vector(T,3), b: Vector(T,3)) -> Vector(T,3)` — 3-vectors only.
- `zeros(n)` / `zeros(r, c)` → `Vector(f64,n)` / `Matrix(f64,r,c)` — `n`/`r`/`c` **must be literal integers** (the result's shape has to be known at typecheck time).
- `ones(n)` / `ones(r, c)` — same shape rule, filled with `1.0`.
- `identity(n)` → `Matrix(f64, n, n)`.
- `sum(v_or_m) -> T` — any numeric element type.
- `len(v: Vector) -> i64`.
- `norm(v: Vector(f64,_)) -> f64` — 2-norm. `norm1` — sum of `|x|`. `norm_inf` — max `|x|`. `frobenius_norm(m: Matrix(f64,_,_))`.
- `trace(m: Matrix(T,n,n)) -> T` — square only (`NotSquare` otherwise), numeric element.
- `det(m: Matrix(f64,n,n)) -> f64` — Gaussian elimination, partial pivoting.
- `inv(m: Matrix(f64,n,n)) -> Matrix(f64,n,n)` — Gauss-Jordan; runtime error (`SingularMatrix`) if singular.
- `solve(a: Matrix(f64,n,n), b: Vector(f64,n)) -> Vector(f64,n)` — `A \ b`; `SingularMatrix` if singular.
- `rank(m: Matrix(f64,_,_)) -> i64` — row-echelon reduction, any shape.
- `is_symmetric(m)` / `is_diag(m)` — square `Matrix(f64,n,n)` only. `is_square(m)` — any `Matrix`, any element type.

**Deterministic simulation** (Phase 3)
- `rand_seed(seed: <int>)` — resets the RNG stream (SplitMix64; see §9 — interpreted and compiled each keep their own, per §9/§10). Required before any draw.
- `rand_f64() -> f64` — uniform `[0, 1)`.
- `rand_gaussian(mean: f64, stddev: f64) -> f64` — Box-Muller.
- `distance(a: Vector(f64,3), b: Vector(f64,3)) -> f64` — Euclidean.
- `bearing(from: Vector(f64,3), to: Vector(f64,3)) -> f64` — initial great-circle bearing, degrees `[0,360)`; takes lat/lon/alt vectors, altitude ignored.
- `lla_to_ecef(v: Vector(f64,3)) -> Vector(f64,3)` / `ecef_to_lla` — WGS84.
- `ecef_to_enu(ecef, ref_lla) -> Vector(f64,3)` / `enu_to_ecef` — local East-North-Up relative to a reference point.
- `kf_predict_state(x, P, F, Q) -> Vector` / `kf_predict_cov(x, P, F, Q) -> Matrix` — linear Kalman filter predict step (split in two — no tuple/struct return type exists).
- `kf_update_state(x, P, z, H, R) -> Vector` / `kf_update_cov(...) -> Matrix` — update step.

**Identity / relying party** (Row 12)
- `oidc_validate_token(token: str, expected_issuer: str, expected_audience: str, jwks_json: str) -> Result(VerifiedIdentity, str)` — validates a mock OIDC/JWT ID token against the supplied JWKS JSON (HMAC-SHA256). Checks issuer, audience, and signature. Returns a `VerifiedIdentity` on success. The runtime never mints tokens; it only consumes externally-issued ones.
- `check_role(identity: VerifiedIdentity, role: str) -> Result(RoleView, str)` — succeeds if `identity.claims_json` contains a `roles` array with the requested role.
- `extract_claim(identity: VerifiedIdentity, name: str) -> Result(ClaimView, str)` — extracts a string claim from `identity.claims_json`.
- `check_role_path(identity: VerifiedIdentity, path: str, role: str) -> Result(RoleView, str)` — `check_role`'s dotted-path sibling, for IdPs that nest the roles array under a path instead of a flat top-level `"roles"` field (e.g. Keycloak's `"realm_access.roles"`). `check_role`/`extract_claim` are unchanged and still the right call for a flat claim — including one whose own name contains a literal dot (Auth0-style namespaced claims like `"https://myapp.example.com/roles"`), which is a flat key, not a nested path.
- `extract_claim_path(identity: VerifiedIdentity, path: str) -> Result(ClaimView, str)` — `extract_claim`'s dotted-path sibling, same nested-vs-flat distinction as `check_role_path` above.
- `identity_expired(identity: VerifiedIdentity, now: i64) -> bool` — true if `now > identity.expires_at`.

---

## 6. Control flow, functions, ownership

```
fn name(param: Ty, ...) -> RetTy effect(...)? requires(...)? { ... }   // RetTy omitted => unit
let x: Ty = expr
x = expr                                    // reassignment, not a new binding
return expr?
while cond { ... }
if cond { ... } else { ... }                // also usable as an expression: let x = if c {1} else {2}
audited "non-empty justification" { ... }   // suppresses codegen's Tier-1/2 guards inside; interpreter unaffected
```

- **No `for` loops**, no closures/lambdas. **First-class functions do
  exist** (interpreter-only — see §10 and "Privileged first-class
  functions" below): a plain function name, used where a `fn(T1, T2) ->
  R`-typed value is expected, evaluates to that function as a value —
  `let f: fn(i64) -> i64 = double`, `f(21)`, or passing one as an
  ordinary higher-order argument. Still no closures — a first-class
  function value is just a target name, nothing captured (this language
  has no enclosing-scope capture at all).
- **Recursion works** — functions are looked up by name in a table, so
  direct and mutual recursion both work (see `fib` in `bench/corpus.json`).
- **Ownership**: `box`/`thread`/`sandbox`/`tcp`/`tcp_listener`/`file` are
  *affine* — using the binding by name moves it; a later use on the same
  path is a static "use after move" error, checked by a real move-checker
  (`ownership.rs`), not just at runtime. `&` borrows without moving.
- **Effects** (`PROTOLANG_PORT.md`'s "Locked design 1", `effects.rs`): a
  `fn` may optionally declare `effect(pure)` or `effect(t1, t2, ...)` where
  each `t` is one of `rng`/`io`/`concurrent`/`network` — a Koka-style
  *set*, not a total order. Omitted entirely (the common case): fully
  inferred, nothing checked, zero notational cost. Declared: the real
  effect set (computed by fixpoint iteration over the call graph, so
  mutual recursion works) must be a *subset* of what's declared —
  declaring more than the body uses is fine, an undeclared-but-performed
  effect is `TypeErrorKind::EffectNotDeclared`. `pure` denotes the empty
  set and can't be combined with other names. Typeck-only; no codegen
  changes — the whole compiled subset (§10) is `pure` except `print`
  (`io`).
- **No tuples** — `struct`/`enum`/`match` and generics exist (Row 11,
  `nirdosha_row11_amendment.md`), so returning a record or sum type is
  possible. The Kalman-filter builtins above remain split for historical
  reasons, not because the language lacks product types.

### 6a. Privileged first-class functions

```
fn transfer_funds(amount: i64) -> i64 requires(role: "admin") { ... }
fn read_chart(id: Text) -> Text requires(claim: "department", "cardiology") { ... }

acquire transfer_funds(proof)   // proof: RoleView/ClaimView -> Result(fn(...)->..., str)
```

A `requires(role: "<name>")` or `requires(claim: "<name>", "<value>")`
annotation gates a function's *value*, not just its behavior: `fn`'s name
has no direct-call path at all once gated — `transfer_funds(500)` and
`let f = transfer_funds` are both **static** `TypeErrorKind::
PrivilegedFnNotAcquired` errors, not runtime ones. `acquire fn_name(proof)`
is the only way to obtain a callable value, and it demands a real proof:
a `RoleView` (from `check_role`) for a `role` requirement, a `ClaimView`
(from `extract_claim`) for a `claim` one — both already real values
produced by the identity feature (§5's "Identity / relying party"),
itself validating a token issued by an external IdP. Nirdosha never
mints its own proof of privilege; "user management" stays entirely
external, the same as `oidc_validate_token`'s own scope. `acquire`
checks the proof's field against the requirement string at runtime
(same spirit as `check_role`'s own string check) and returns
`Result(fn(params) -> ret, str)`.

This is deliberately different from an annotation-based checker like
Spring's `@PreAuthorize`: there's no ambient thread-local security
context to consult (or forget to consult) at each call site, and no AOP
proxy to accidentally bypass by calling the underlying implementation
directly — there *is* no underlying direct-call path to bypass to. The
acquired value is an ordinary first-class function once obtained: pass
it to code that has no idea it was privileged, store it in a struct,
return it, call it many times — the check happens exactly once, at
acquisition, not smeared across (or missing from) every call site.
Interpreter-only for now, like every construct past §10's compiled
subset — `nirdosha build` rejects `fn(..)->..`/`acquire` with a specific
reason, never silently mis-compiles one.

### 6b. `str` at function boundaries ("enum favoring")

A user-defined `fn`'s parameter or return type may not be, or contain,
`str` — checked recursively through `Result`/`Option`/generics, `box`/
`&`/`thread`/`chan`, `Vector`/`Matrix`, and `fn(...) -> ...` types
(`TypeErrorKind::StrInFnSignature`, `typeck.rs::check_fn`). The point is
to push stringly-typed control flow (`if status == "PENDING"`,
`match currency { "USD" => ..., "EUR" => ... }`) toward real `enum`s —
which already get exhaustive `match` and already render as searchable
dropdowns in `emit-ui` (§11) with zero extra work — instead of `==`/
literal-`match` over `str`.

`str` itself is completely unrestricted everywhere else: an ordinary
`struct` field type, a local `let` binding's type, a literal. Two
conventions carry what a bare `str` parameter/return used to:

- **A closed, categorical vocabulary** (a status, a currency code, a
  decision) becomes a small zero-payload `enum`.
- **Genuine free text** that still needs to cross a function boundary
  (a justification, a note, a reference, an identity subject) gets
  wrapped in a one-field carrier struct, conventionally named `Text`:
  ```
  struct Text {
      value: str,
  }
  ```
  Struct construction is an ordinary call to a name registered as a
  constructor (§3.1), never a `fn_decl` — so `Text("free text")` at a
  call site, and a function taking/returning `Text` instead of bare
  `str`, are both unaffected by the ban. A function that needs the raw
  string (to hand to a builtin like `db_execute`) reads `.value`.
  `Text` round-trips through JSON automatically wherever `nirdosha
  serve` decodes/encodes request/response bodies (`serve.rs`'s
  `decode_value`/`encode_value` are already generic over structs), and
  `emit-ui` renders it as a plain text input, not a nested group
  (`ui_gen.rs::build_field`'s one-field-`Text`-struct special case).

  Comparing two `struct`/`enum` values with `==`/`!=` — the natural next
  reach once a status/currency-style field is a real enum instead of a
  string — typechecked already (`unify_operands` permits `==`/`!=`
  generically for any matching type) but had no arm in the interpreter's
  binary-operator dispatch to actually evaluate it, so it trapped at
  runtime with a confusing `TypeMismatch` despite typechecking cleanly —
  the same kind of typeck/interpreter gap `str`'s own `==` once had
  (found the same way — by testing code that typechecks, not by
  re-reading either file; see `interpreter.rs`'s `Value::Str` binop
  arm). Fixed alongside this migration
  (`interpreter.rs::eval_binary`'s `Value::Struct`/`Value::Enum` arm,
  delegating to `Value`'s own already-correct `PartialEq`), since
  pushing code toward enums is pointless if comparing them then traps.

The ban applies only to entries in a program's own `fns` list. Three
things are exempt **by construction**, not by special-casing:
- **Builtins** (`http_get`/`db_query`/`json_get_str`/`oidc_validate_token`/
  `mock_issue_token`/`print`/... — §5) are resolved by name in
  `Expr::Call`, never appearing as `fn_decl`s — the language's actual
  external-I/O boundary (an HTTP body, SQL text, a JWT, a JSON document)
  is irreducibly `str` and stays that way.
- **Struct/enum constructors** are calls to a registered type name, also
  never `fn_decl`s — a struct can freely keep a `str` field.
- **`transact`'s synthesized `txn_id` parameter** (TRANSACT.md) is the
  one narrow, name-based exemption: `network`'s call must pass `txn_id`
  as a real `str` argument, and it must stay a plain scalar for WAL
  durability (`Ty::is_transact_scalar`) — it can't be wrapped in `Text`.
  A parameter literally named `txn_id` is skipped by `check_fn`'s scan.

An enum variant may itself carry a `str` payload (`enum ErrorCode {
NotFound, External(str) }`) without tripping this rule — the check only
inspects a `fn`'s own declared parameter/return *type expression*
(`Ty::contains_str`), which for a bare `Ty::Named("ErrorCode", [])` has
no argument to recurse into; the `str` lives inside the enum's own
declaration, not the signature. This is the same "a payload type is not
a signature type" reasoning that already exempts struct fields, applied
to enums — a legitimate, precedented pattern for a shared error type
that needs to both enumerate known application-level cases (`NotFound`)
and forward an unpredictable builtin failure message (`External(str)`)
uniformly through one `Result(_, ErrorCode)`, not a loophole around the
rule's intent (nothing compares an `External` payload with `==`/
literal-`match`).

---

## 7. Concurrency & I/O

```
spawn f(args)              // real OS thread, returns thread T
join(t)                    // blocks, consumes the handle, returns T
let c: chan T = chan
send(c, v)                 // never blocks (unbounded queue)
recv(c) -> T                // blocks until a value is available

sandbox f(args)             // real, separate OS process (re-execs the nirdosha binary)
stop(s) -> i64               // kills if still running, returns exit code

connect(host: str, port: i64) -> tcp
listen(port: i64) -> tcp_listener
accept(l: tcp_listener) -> tcp   // blocks for the next client
stop(conn)                       // closes a tcp or tcp_listener

open(path: str, mode: str) -> file   // mode is "r", "w", or "a"
send(f, s: str)                       // write (reuses tcp's keyword)
recv(f) -> str                        // read all currently-available bytes; "" at EOF, not an error
stop(f)                               // closes the file (reuses tcp's keyword)
```

`chan`/`sandbox` compose: a `chan T` (T a plain scalar) can cross into a
sandboxed process as a real cross-process transport (a Unix domain
socket under the hood). Race-freedom for concurrent code comes entirely
from the ownership checker — an affine value moved into `spawn`/`send`
can never be touched by the sender again.

---

## 8. Static guarantees

- **Type checking** (`typeck.rs`) — every program is fully typed before
  it runs; a type error is never discovered mid-execution.
- **Ownership/move-checking** (`ownership.rs`) — affine values statically
  proven single-owner, including across branches and loop iterations.
- **Two independent static bounds-provers**, feeding the same two report
  shapes:
  - **Interval analysis** (`refine.rs`) — no SMT solver, straight-line
    range propagation.
  - **Real Z3** (`smt.rs`) — can prove things interval analysis can't
    (e.g. an index narrowed by an `if` condition).
  - Both prove: (1) an arithmetic result fits its declared integer type,
    (2) a divisor is never zero, (3) a `Vector`/`Matrix` index falls
    inside its declared bounds. **All three are now consumed by codegen**
    (as of `Vector`/`Matrix` codegen landing, §10) — an unprovable index
    still gets a real runtime bounds guard (same `abort()`-trap idiom as
    (1)/(2)), a proven one emits no check at all.
- **`audited "justification" { ... }`** — the one escape hatch: suppresses
  codegen's guard emission inside the block. The compiler only enforces
  that a justification exists and is non-empty; judging its content is a
  review-process concern, not a compiler one.

---

## 9. Determinism

`rand_seed`/`rand_f64`/`rand_gaussian` are backed by a from-scratch
SplitMix64 stream stored **per `Interpreter` instance** (not a process
global) — same seed, same OS, same run, byte-for-byte identical draws,
every time. A `spawn`ed function gets its own independent, unseeded RNG
by default (an honest, documented gap — see `Interpreter::rng`'s doc
comment). `nirdosha build`'s compiled version of this (§10) necessarily
uses a process-wide store instead — there's no "interpreter instance" in
a native binary — but `thread`/`spawn` aren't compiled yet, so a
compiled program has exactly one thread to own it regardless, matching
the interpreter's per-instance guarantee in practice today; that
equivalence stops holding the moment compiled `thread`/`spawn` lands, at
which point this needs revisiting, not left as a stale assumption. No
other source of nondeterminism exists in the language (no ambient
clock/entropy reads anywhere in the builtin set).

---

## 10. What's compiled vs. interpreter-only

**Updated 22 Aug 2026** — corrected against `check_supported`'s actual
`unsupported(...)` call sites and real compiled-binary test runs, not
just this section's own prior prose: `box`/`&`/`*`, `str`, and
`tcp`/`tcp_listener`/`connect`/`listen`/`accept` were previously (and
incorrectly) listed below as interpreter-only — all three have real
codegen and were confirmed working end-to-end (a compiled `box i64`
round-tripped through a function param and `*`; a compiled `str`
program branched on `==` and printed the result; a compiled binary did
a real `connect`/`send`/`recv`/`stop` round trip against a live TCP
server). This drift is exactly why this doc note now says to verify
against `check_supported` directly before trusting this section, rather
than the reverse.

`nirdosha build`/`emit-llvm` now support:

- `i8`/`i16`/`i32`/`i64`, `bool`, `unit`, `f64` scalars, and their
  unsigned counterparts `u8`/`u16`/`u32`/`u64`/`usize` — same LLVM
  widths as the signed types. This backend computes all integer
  arithmetic at `i64` width internally regardless of a value's declared
  width (widening on load, narrowing back on store), and every unsigned
  type's legal range is capped at `[0, i64::MAX]` (`Ty::bounds()`,
  never touching the sign bit) — so the one real signed-vs-unsigned
  instruction choice this needs is at the widen-on-load step
  (`zext` for unsigned, `sext` for signed, `codegen.rs::widen_to_i64`);
  every downstream `+`/`-`/`*`/comparison/`/` is byte-identical between
  the two once correctly widened, confirmed by compiling and running
  comparison/division/boundary-value/underflow-trap programs for all
  five unsigned types, not just reasoned about.
- All scalar arithmetic/comparison operators, `if`/`while`, function
  calls (including recursion), `print` on integer/`f64`/`str`/`bool`/
  `unit` args — every scalar shape. `print` on a `bool` prints `1`/`0`,
  not `"true"`/`"false"` (the interpreter's own `render()`) — a
  disclosed, cosmetic-only difference, not semantic. `print` on a
  `unit`-typed argument (only reachable via a call to a `-> unit`
  function — there's no `unit` literal syntax) prints the fixed string
  `"()"`, matching the interpreter exactly.
- Tier-1/2 bounds and divide-by-zero guards (elided where proven safe —
  see §8), and `audited`'s suppression of them.
- **`box`/`&`/`*`** — real heap allocation (`nir_alloc`) *and* real,
  automatic free (`nir_free`) driven by `ownership.rs`'s `FreeMap`
  (`emit_box_free`, called at each binding's last use) — not a leak.
- **`str`** — literals, `==`/`!=`, use as an `if` condition, `print`,
  and function parameters/returns, `main`'s own included: `main() ->
  str` compiles directly now (prints the returned string and exits 0 —
  the same `%.*s`-format printf sequence `print` itself uses, since
  there's no sensible integer exit code for a `str` value the way there
  is for an integer/`f64` one).
- **`tcp`/`tcp_listener`** — `connect`/`listen`/`accept`/`send`/`recv`/
  `stop` over real sockets.
- **`sha256_hex`/`constant_time_str_eq`** — linked calls into a
  from-scratch SHA-256 in `compiler/src/runtime_kernels.rs` (that file
  has no access to the `sha2` crate `interpreter.rs` uses — it's
  compiled as an isolated `rustc --crate-type staticlib` invocation with
  no `--extern` flags, `build.rs`'s doc comment), verified bit-for-bit
  against the standard's own test vectors, an independent Python
  `hashlib.sha256` cross-check at every padding-boundary message length,
  and the interpreter's `sha2`-backed output. `sha256_hex`'s output
  buffer is heap-allocated (`nir_alloc`) and never freed — `str` isn't
  affine, so there's no scope-closing point to hook a matching
  `nir_free` onto (a real, small, disclosed leak, not a silent one; see
  `runtime_kernels.rs::nir_sha256_hex`'s doc comment).
- **`rand_seed`/`rand_f64`/`rand_gaussian`** — the same SplitMix64/
  Box-Muller algorithm as the interpreter (§9), now with real RNG state
  in generated code: a process-wide stream in `runtime_kernels.rs`
  (necessarily process-wide, not per-"instance" the way the interpreter
  keeps it — §9's determinism section explains why that's an honest
  equivalent today, and when it stops being one). Calling `rand_f64`/
  `rand_gaussian` before `rand_seed` aborts the process (`nir_alloc`'s
  own allocation-failure path is the same precedent), matching the
  interpreter's `RngNotSeeded` runtime error in spirit, just via
  `abort()` instead of a catchable `Result`.
- **`Vector`/`Matrix`, fully** — literals, dynamic (runtime-expression)
  indexing with a proven-or-checked bounds guard, all elementwise operators
  and every legal shape of `*`, `==`/`!=`, and every dense-linear-algebra/
  geometry/Kalman-filter builtin. Two different codegen strategies underly
  this, worth knowing about if you're reasoning about performance: shape-driven
  operations (elementwise ops, `*`, `transpose`, `dot`, `cross`, `zeros`/
  `ones`/`identity`, `sum`, `len`, the norms, `trace`, `is_*`, the geometry
  builtins, `kf_predict_*`) are **fully unrolled at compile time** into
  straight-line IR — dimensions are always compile-time literals, so this
  is always possible, and it's more aggressively optimizable than an
  equivalent runtime loop. The genuinely data-dependent ones (`det`, `inv`,
  `solve`, `rank`, `kf_update_*` — partial-pivot row selection is real,
  value-dependent control flow) instead **call into a small native runtime
  library** (`compiler/src/runtime_kernels.rs`, compiled once at `nirdosha`'s
  own build time via `compiler/build.rs`, linked into every output binary)
  rather than hand-emitting branchy LLVM IR — reusing the interpreter's own
  proven-correct algorithms instead of a second, independently-written copy.
  This is not a performance compromise (a native `call` costs what inlined
  IR costs) but it is a real, measured tradeoff against hand-specialized C:
  the runtime kernels are generic over matrix size `n`, so they can't be
  specialized for a fixed small `n` the way C's `det4()` can — see
  `benchmarks/RESULTS.md`'s Group A "honest asterisk" for the actual numbers
  this produces (Nirdosha beats C on the fully-unrolled operations, loses to
  it on the runtime-library ones).
- **`struct`/`enum`/`match` (Row 11), non-affine payloads** — construction
  (an ordinary `Expr::Call`, no dedicated AST node), `expr.field` access,
  and `match` (both enum-variant arms with a real LLVM `switch` on the
  declaration-order variant tag, and literal-pattern `str`/`i64`/`bool`
  arms — `str` as a sequential `nir_str_eq` chain, no native string
  switch). A `struct`/`enum` lowers to a real named LLVM type: a struct
  to `{ field_lltys... }` (LLVM computes the real padding), an enum to a
  hand-rolled `{ i64 tag, [N x i64] payload }` tagged union (`N` a
  compile-time word count, conservatively over-allocated so the same
  buffer fits every variant). Generic instantiations get distinct mangled
  named types (`%Result$i64$str`). **The one real caveat (Phase 4b,
  deferred):** a `struct`/`enum` whose fields/payloads *transitively*
  contain an affine type (`box`/`&`/`thread`/`chan`/`tcp`/`file`/`db`/`mq`)
  is still rejected — freeing an affine field nested inside a struct, or
  inside a *live* enum variant's payload, needs `ownership.rs`'s `FreeMap`
  generalized beyond its current `Ty::Box`-only `still_owned_boxes` plus
  a new `at_match_arm_end` entry for match-bound affine payloads, not
  attempted in this phase. `check_supported` rejects such a type up front
  with a message naming the affine/Phase-4b reason, the same
  "reject, don't mis-compile" treatment every other still-unsupported
  construct gets.

Interpreter-only (rejected by `check_supported` with a specific reason,
never silently mis-compiled):

- `struct`/`enum`/`match` (Row 11) with an **affine** field/payload
  (`box`/`&`/`tcp`/`file`/`db`/`mq`, transitively) — the Phase 4b boundary
  named above; a non-affine `struct`/`enum`/`match` compiles now.
- `thread`/`spawn`/`join`, `chan`/`send`/`recv` (channel — distinct from
  the already-compiled `tcp`), `sandbox`/`stop`.
- `file`/`open` (`PROTOLANG_PORT.md`'s file I/O port).
- `json`/`db`/`mq` (`Ty::Json`/`Ty::Db`/`Ty::Mq`) and every Row 12
  identity/session/API-key builtin (`oidc_validate_token`,
  `check_role(_path)`, `extract_claim(_path)`, sessions, refresh,
  revocation) — the identity ones are additionally blocked on
  `VerifiedIdentity`/`RoleView`/`ClaimView` being structs.
- `http_get`/`http_post`/`https_get`/`https_post`, `mock_issue_token` —
  every builtin not in `codegen.rs`'s `PHASE4_BUILTINS`/
  `PHASE5_BUILTINS`/`STR_CRYPTO_BUILTINS`/`RAND_BUILTINS` lists.
- `transact`.
- `workflow` (§14, `WORKFLOW.md`) — its desugared functions call
  `send_email`/`send_sms`/`send_push`/`notify`/`__workflow_*`, none of
  which are in `codegen.rs`'s builtin allowlists, so `check_supported`
  rejects them the same clean, named way it already rejects `transact`.
- `fn(..)->..`/`acquire`/`requires(...)` — first-class and privileged
  functions (§6a), joining the affine-field `struct`/`enum`/`match` entry
  above on this list.
- `screen`/`dashboard` (§11, Row 12) — consumed only by `nirdosha emit-ui`/
  `nirdosha serve`. Unlike everything else on this list, these aren't
  *rejected* by `nirdosha build`/`emit-llvm` — `codegen.rs` never
  inspects `Program.screens`/`.dashboard` at all, so a program containing
  them compiles cleanly; the declarations are just inert to codegen, with
  nothing for them to lower to.

**This matters directly for benchmarking**: a `Vector`/`Matrix` comparison
against Julia is now compiled-vs-JIT, not interpreter-vs-JIT — see
`benchmarks/RESULTS.md` for the re-run numbers (all four Group A benchmarks
now decisively beat Julia; the historical interpreted numbers are kept there
too, labeled, for the record). A benchmark touching an *affine-field*
`struct`/`enum`/`match`, `thread`/`chan`/`sandbox`, `file`, `json`/`db`/`mq`,
or any Row 12 identity builtin is still necessarily interpreted for now —
a non-affine `struct`/`enum`/`match`, and `box`/`tcp`/`str`, no longer carry
that caveat.

---

## 11. `screen`/`dashboard` — declarative UI DSL (Row 12, `emit-ui`/`serve` only)

`nirdosha emit-ui`/`nirdosha serve` already derive a full CRUD+dashboard
web UI from nothing but a program's `struct` declarations and its
`list_/create_/update_/delete_/get_<struct>` and `stat_/chart_<name>`
function-naming conventions (`compiler/src/ui_gen.rs`) — no syntax
needed at all for the common case. `screen`/`dashboard` blocks are an
**optional, additive** layer on top of that inference, for the parts a
naming convention can't express: a friendlier title, a relabeled field,
or a custom action beyond plain create/update/delete. A `struct` with no
matching `screen` block behaves exactly as before — nothing about this
DSL is load-bearing for a program that never uses it.

```nirdosha
struct Product {
    id: i64,
    name: str,
    price_cents: i64,
    stock: i64,
}

fn list_product() -> Result(json, str) { ... }
fn create_product(p: Product) -> Result(i64, str) requires(role: "admin") { ... }
fn restock_product(id: i64) -> Result(i64, str) requires(role: "admin") { ... }

screen Product {
    title: "Catalog"
    field name {
        label: "Product Name"
    }
    action "Restock +10" -> restock_product {
        style: "outlined"
        confirm: "Restock this product by 10 units?"
    }
}

dashboard {
    tile "Products" -> stat_product_count
    chart "By Price" -> chart_products_by_price
}
```

**Grammar** (see `GRAMMAR.md`'s `screen_decl`/`dashboard_decl`
productions for the full EBNF): `screen`/`dashboard` are real reserved
keywords, top-level items like `struct`/`fn`. Inside a body, `field`/
`action`/`paginate`/`tile`/`chart` are **contextual** keywords — matched
by identifier text only in that one leading position, the same "keyword
only within this slot" treatment `requires(role: ...)`'s own `role`/
`claim` already get — so they stay ordinary identifiers everywhere else
(a struct field or param can still be named `action`, as
`examples/trade-finance/trade_finance.nir` already does). Every `key:
value` slot is an ordinary expression — `parse_expr()` handles a string
(`title: "Catalog"`), an int (`page_size: 25`), a bare function name
(`list: list_product`), or a call (`view: role("admin", "analyst")`)
alike, with no separate value grammar to learn.

**What's checked today** (existence/shape only — typeck, not the
parser): `screen <Name>` must name a real `struct`; every `field
<fname>` must name a real field of it; `list`/`create`/`update`/
`delete`, and every `action`'s `->` target, must resolve to a real
function; `view`/`edit` must be `role(...)`/`claim(...)` calls with
string-literal arguments (the same shape `requires(...)` itself already
accepts); `dashboard`'s `tile`/`chart` targets must resolve to real
functions.

**What `screen`/`dashboard` currently change in the generated UI**:
`title` overrides the nav label/heading/toast text (default: the struct
name); `field <name> { label: "..." }` overrides that field's displayed
label everywhere one is shown (default: the raw field name); `list`/
`create`/`update`/`delete` override which function backs that slot
(default: the `<kind>_<snake_case_struct_name>` convention); a declared
`action "<label>" -> <fn> { style: "filled"|"outlined", confirm: "..." }`
renders as an extra per-row button beyond the inferred CRUD set, calling
`<fn>` with just the row's own primary-key-shaped first param (the same
single-param shape a declared `delete` action already uses) —
`window.confirm(...)`-gated when `confirm` is set.

**What's parsed and typechecked but not yet wired into the generated
UI**: `paginate { page_size, total }`, `field { searchable, sortable }`,
`field { view, edit }` (role/claim visibility — parses, but nothing
server-side enforces it yet), form insert-vs-update auto-hide-primary-key
behavior. Tracked, with the reason each is still open, in
`compiler/UI_DSL_TODO.md`.

## 12. `module "Name" { ... }` — nav grouping, not scoping

`emit-ui`'s nav is one flat list of screens by default. A `module "Display
Name" { ... }` block wrapping `fn`/`struct`/`enum` declarations tags each
one with that display name — `ui_gen.rs` groups nav screens by it into
collapsible primary-menu sections; `Dashboard` always stays outside every
group, first. That is the *only* thing `module` does: it is **pure
syntactic sugar**, not a namespace. Everything inside a `module` block
still registers into the exact same single flat global namespace a
top-level declaration would (`typeck.rs` never even inspects `module` —
only `ui_gen.rs` does), so two functions in different `module` blocks can
call each other exactly as freely as two top-level ones always could, and
a program that never uses `module` at all renders exactly as it did
before this construct existed (flat, ungrouped nav).

```nirdosha
module "Billing" {
    struct Invoice { id: i64, amount_cents: i64, status: str }
    fn list_invoice() -> Result(json, str) { ... }
    fn create_invoice(inv: Invoice) -> Result(i64, str) { ... }
}

module "Shipping" {
    struct Shipment { id: i64, invoice_id: i64, carrier: str }
    fn list_shipment() -> Result(json, str) { ... }
}
```

**Grammar** (`GRAMMAR.md`'s `module_decl`): `module` is a real reserved
keyword, dispatched like `struct`/`enum`/`screen`/`dashboard`, followed by
a string display name (not an `ident` — needs to hold spaces/punctuation
like `"B2B Trade Payments & Commission Engine"`), then a brace-delimited
list of `fn`/`struct`/`enum` declarations — each parsed by the exact same
`parse_fn_decl`/`parse_struct_decl`/`parse_enum_decl` the top level itself
uses. Single-level only: a `module` nested inside a `module`, or a
`screen`/`dashboard` inside one, is a parse error — the same fixed-arity,
no-arbitrary-nesting discipline `transact` slots already have (`screen`/
`dashboard` stay top-level-only declarations, since they're UI-specific
authoring, not business-organizational).

## 13. Auto-generated DB schema migrations (`nirdosha serve --db`)

Before this, every table was created by a hand-written, literal
`db_execute(conn, "CREATE TABLE IF NOT EXISTS ...")` inside individual
`.nir` functions — duplicated per function, never derived from the
`struct` itself, and never updated when a struct gained a field. `nirdosha
serve --db <path>` now derives schema from `struct` field declarations
directly and keeps it current automatically, once at every startup — no
new syntax, no new flag, just behavior layered on the `--db` flag that
already existed.

**What runs at startup, only when `--db` is given:** for every top-level
`struct` (skipping the built-in prelude structs), the declared fields are
diffed against the live SQLite schema:

- table doesn't exist yet → `CREATE TABLE IF NOT EXISTS <table> (<cols>)`
- table exists but is missing a column for some field → one `ALTER TABLE
  <table> ADD COLUMN ...` per missing field
- nothing missing → nothing happens (the common case on every
  steady-state restart)

Field type → SQL column type: `I8/16/32/64`/`U8/16/32/64`/`Usize` →
`INTEGER`; `F64` → `REAL`; `Bool` → `INTEGER` (0/1, matching
`db_execute`'s own existing encoding); `Str` → `TEXT`; `Option(T)` → same
type as `T` (SQLite columns are nullable by default, so this needs no
distinct shape); a zero-payload enum → `TEXT` (the variant name — same
round-trip `sql_bind_params`/`decode_enum_value` already give a plain
`db_execute`/`db_query` call). A field named literally `id` of type `i64`
becomes `INTEGER PRIMARY KEY AUTOINCREMENT`, the convention every
hand-written schema in this codebase already follows.

**Deliberately additive-only.** A struct field whose type has no
single-column SQL shape (a nested struct, `Vector`/`Matrix`, a
payload-carrying enum, an affine handle like `db`/`tcp`/`box`) causes that
struct's table to be skipped *entirely* — never a partial table — logged
as a warning naming the struct and field. A column whose type changed, or
whose field was removed from the struct, is **not** touched automatically
(SQLite can't safely change a column's type without a full table rebuild,
and dropping a column automatically at an unattended startup is a real
data-loss risk) — also just a warning, never attempted. A table with no
backing `struct` at all (hand-written SQL with no matching declaration) is
completely untouched, exactly as before this feature existed.

**Every applied change is written to disk first**, at
`<sibling-of---db-path>/migrations/NNNN_<slug>.sql` (`create_<table>` /
`alter_<table>_add_<col>...`, sequential across a single startup's run) —
a reviewable, commit-to-git audit trail, not a rollback-capable ledger:
there are no down-migrations, and these files are a generated record of
what ran, not something meant to be hand-authored or edited. A small
`_nirdosha_migrations` table inside the database itself separately
records `(filename, applied_at, sql)` for a DB inspected on its own.

Omitting `--db` leaves an app exactly as it always behaved — this
feature, like the `/_nirdosha/table/<name>` route it shares its
`--db`-gating with, only exists at all once that flag is passed.

---

## 14. `workflow` — durable state machines with notification actions

Full design in `WORKFLOW.md` (locked grammar, runtime protocol, deliberate
non-goals) — this section is the short version.

`workflow Name { data { ... } state ... }` is a durable, named state
machine: `state`s, `on <Event> -> <Target>` transitions (optionally
`link`-marked for an unauthenticated, single-use magic-link trigger), and
`on_entry`/`on_exit` action calls that can reach the new notification
builtins (`send_email`/`send_sms`/`send_push`/`notify`). Like `module`
(§12), it's **pure desugaring, not a new runtime primitive**:
`workflow_lower.rs` turns every `workflow` block into ordinary `fn`/
`enum`/`struct` declarations (a `start_*`, an `advance_*`, one
`<event>_via_link` per `link`-marked transition, plus a synthesized
`<Workflow>Event` enum and `<Workflow>Data` struct) right after parsing —
every later pass, including `nirdosha serve`'s automatic
`POST /api/<fn>` RPC exposure, sees only those, never `workflow` syntax
itself. A program that declares no `workflow` is byte-for-byte unaffected.

One thing this does **not** do, worth stating plainly since it's easy to
assume: it does not add WebSocket support to this codebase (`notify`'s
real-time path is a Redis `PUBLISH` an external gateway is expected to
relay — see `WORKFLOW.md`). `on_entry`/`on_exit` actions *are*
crash-durable, the same "log intent before running it, replay on
restart" shape `transact`'s own `network` slot already has
(`WorkflowLog::begin_pending_action`, `Interpreter::
replay_pending_workflow_actions` — called at `nirdosha serve` startup
right alongside `replay_pending_transactions`).

Interpreter-only, the same way `transact`/`db`/`mq` already are (§10):
`workflow`-desugared functions call builtins outside `codegen.rs`'s
`PHASE4_BUILTINS`/`PHASE5_BUILTINS`/... allowlists, so `nirdosha build`/
`emit-llvm` cleanly rejects a program using `workflow`, naming the
specific unsupported builtin — never a silent mis-compile.
