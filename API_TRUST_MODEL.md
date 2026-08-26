# Nirdosha API trust model — identity, execute-permission, data masking, and self-correction

## Purpose

A generated Nirdosha app's `/api/<fn>` surface has to answer four
questions on every call: **who is calling**, **may they execute this
function at all**, **of the data this function returns, which parts may
this specific caller see**, and — for the self-correcting generation
pipeline now being planned — **when the compiler "fixes" a function
automatically, how do we know the fix is actually right and not just
confidently wrong**. Parts of this are shipped and real today; parts are
open gaps with no design yet; parts are proposed syntax nobody has
reviewed. This doc keeps those three categories separate throughout —
`[DONE]` (verified against the current source), `[OPEN]` (a named gap,
no design), or **proposed** (a sketch for review, explicitly not
implemented) — the same convention `ROADMAP.md` and
`PROTOBOX_INTEGRATION.md` already use. Nothing below should be read as
"this exists" unless it's tagged `[DONE]` with a citation.

This document was reviewed once already (an external pass covering 24
points) and revised against a curated subset of that review — the parts
that raised its rigor without requiring new infrastructure this codebase
doesn't have. §11 (Non-goals) names, and deliberately declines to design,
the parts of that review judged out of scope for now.

**Third pass (adversarial/buildability, this revision).** Every
load-bearing citation below was re-read against the actual source rather
than trusted from the previous draft, and every proposed syntax sketch
was checked against `parser.rs`/`token.rs`/`typeck.rs` for whether it
could actually be built as written. That pass changed real conclusions,
not just wording: it found four **live** gaps not previously in this
document (§4's default-open API surface, §5's gaps 3, 4 and 5), corrected
two claims that read as "this works today" when they don't (§3's JWKS
posture, §5's "every response"), corrected one factual mis-citation
(§7.4: `refine.rs` does *not* use Z3), replaced §7.6's example contract
because the predicate as written was vacuous, and downgraded the
`Err(...)`-path finding from "real leak" to "latent mechanism gap" while
escalating its `"ok"`-branch sibling in the opposite direction. Where
this revision disagrees with the earlier draft, the disagreement is
stated in place rather than silently edited over, so a reader who saw
the earlier version can tell what changed and why.

**Fourth pass (this revision): pre/post conditions and acceptance
criteria were being treated as one artifact — they aren't.** §7.1a is
new: a function's `pre_logic`/`post_logic` (a Hoare contract on that one
function's own params/return) and a story's `acceptance_criteria` (an
end-to-end Given/When/Then, often spanning several functions and real
DB state) are extracted independently by the same pipeline
(`scratch/hoare_userstory_prompt.py`), can diverge from each other
before any code exists, and require different verification — a passing
`post_logic` proof says nothing about whether the acceptance criterion
it was meant to formalize holds end-to-end. §7.5's tier 2 is now split
(2a unit-level, exactly `nir_scenario!`'s existing shape; 2b
scenario-level, not yet built) and tier 3 gained a fifth case, (E),
for the two artifacts disagreeing with each other. Also added two
`ROADMAP.md` items this pass surfaced: **A10** (the dispatcher is
default-open, not default-deny — 79 of 246 functions in the shipped
trade-finance example are callable with no token) and **A11** (JWKS
validation is symmetric-only, so no mainstream IdP's tokens can be
validated today). **Both fixed 2026-08-26** — A11 with real RSA/EC
signature verification (§3); A10 with a `requires(public)` marker plus a
non-fatal typeck warning that surfaces every ungated `fn`, **not** a
runtime default-deny change (§4 still describes the runtime as
default-open — see that section's own 2026-08-26 update for exactly
what did and didn't change).

---

## 1. Security invariant

Every section below is one piece of a single guarantee the runtime is
supposed to make on every `/api/<fn>` call:

```text
Allow(request, fn, result) iff

    IdentityValid(request)
 ∧  ExecuteAllowed(identity, fn)
 ∧  ResourceScopeSatisfied(identity, fn, resources)
 ∧  OutputVisibilitySatisfied(identity, fn, result)

Failure of any one conjunct => deny / fail closed.
```

Naming it here, once, is the point — §3-§5 below are what each conjunct
actually cashes out to today. Be precise about current coverage, not
aspirational — and the precision matters more than the invariant's shape,
because **three of the four conjuncts are narrower than their names
suggest**:

- `IdentityValid` — real and enforced server-side (§3). **Fixed
  2026-08-26 (`ROADMAP.md` A11):** previously only a **symmetric
  (HMAC-SHA256) JWKS** could be consumed at all; `validate_oidc_token`
  now dispatches on the JWT header's own `alg` and verifies real
  RSA (`RS256`) and EC P-256 (`ES256`) signatures via `ring`, so the
  asymmetric JWKS every mainstream IdP actually publishes is now
  consumable (§3's "What `oidc_validate_token` actually validates").
- `ExecuteAllowed` — real and enforced server-side (§4), and **still
  opt-in per function, not default-deny — that part is unchanged by the
  2026-08-26 fix below.** Every `fn` a program declares is automatically
  reachable at `POST /api/<fn>`; a function with no `requires(...)` and
  no `VerifiedIdentity` parameter is callable by an unauthenticated
  caller (§4's "The default is open"). So the conjunct is still, exactly
  as before, `ExecuteAllowed(identity, fn) = fn.requires.is_none() ∨
  satisfies(identity, fn.requires)` — a gate that returns *true* when
  nobody declared one. **What did change (`ROADMAP.md` A10):** this is no
  longer *silent*. A new `requires(public)` marker lets an author state
  "this one's open on purpose" without gating it, and a new typeck
  warning (`typeck::ungated_fn_warnings`, wired into `serve`/`emit-ui`)
  now names every `fn` this conjunct is vacuously true for. That's a
  visibility fix, not a remediation — the runtime still executes an
  unmarked ungated `fn` for an anonymous caller exactly as before; a
  project-level default-deny mode (this section's option (b)) is still
  undesigned.
- `OutputVisibilitySatisfied` — real for top-level struct fields, but
  only on the ≤4 CRUD-slot functions a `screen` block resolves to, with
  five disclosed gaps (§5), three of which are live today.
- `ResourceScopeSatisfied` — **vacuously true today.** No row-level or
  tenant-level scoping exists anywhere in this codebase (§6).

So the invariant above is the *target*, not a description of the
runtime. What the runtime actually enforces today is: a valid
symmetric-JWKS token when one is presented, a declared `requires(...)`
when one exists, and field masking on the CRUD-slot functions of structs
that declare a `screen` block. Everything else in the conjunction is
`[OPEN]`. Any section below that reads as broader than that paragraph is
wrong, not this paragraph.

**One deliberate carve-out, stated here so it isn't mistaken for a
bug.** `workflow`'s `link`-marked transitions desugar to
`<event>_via_link` functions that are **unauthenticated by design**
(`LANGUAGE.md` §14, ~line 903 — "optionally `link`-marked for an
unauthenticated, single-use magic-link trigger"); the hand-written
equivalent is `decide_approval_via_link`
(`examples/trade-finance/trade_finance.nir:689`), whose only protection
is a constant-time single-use token compare, not `IdentityValid`. That
is an intentional exception to the first conjunct, not coverage.

---

## 2. Threat model

| # | Threat | Mechanism (or status) | Status |
|---|---|---|---|
| T1 | Forged/invalid identity | `oidc_validate_token` checks issuer/audience/signature against a JWKS trust anchor — `HS256`/`RS256`/`ES256` all real, alg-and-`kty` matched (§3) | `[DONE]` — RSA/EC support added 2026-08-26 (`ROADMAP.md` A11) — §3 |
| T1a | Token minted by the relying party itself | Depends on the deployment's own JWKS choice: a **symmetric** JWKS still gives the server the IdP's own HMAC secret, and `mock_issue_token` uses it (`compiler/src/interpreter.rs:1221`); an **asymmetric** (`RS256`/`ES256`) JWKS holds only a *public* key server-side, so this risk doesn't apply | `[OPEN]`, structural, **only for a symmetric-JWKS deployment** — real IdPs default to RS256/ES256, which closes this by construction — §3 |
| T1b | Unauthenticated call to an ungated `fn` | Runtime unchanged — `/api/<fn>` still exposes every declared `fn`; gating is still opt-in, not default-deny (§4). What's new: `requires(public)` + a typeck warning (`typeck::ungated_fn_warnings`) now name every such `fn` at `serve`/`emit-ui` time instead of silently | `[OPEN]`, live — no longer *silent* as of 2026-08-26 (`ROADMAP.md` A10), but still exploitable until an author acts on the warning — §4 |
| T2 | Expired identity | `resolve_identity` checks `now > expires_at` on every request and 401s (`compiler/src/serve.rs:854-855`) | `[DONE]` |
| T3 | Privilege escalation via role/claim forgery | `requires(role/claim)` matched against the *verified* token's own claims, not a client-supplied value (§4) | `[DONE]`, single-predicate ceiling — §4 |
| T4 | Function-invocation bypass (calling a gated fn without proof) | Static `TypeErrorKind::PrivilegedFnNotAcquired` — no direct-call path exists for an unacquired gated `fn` (§4) | `[DONE]` |
| T5 | Row/tenant isolation bypass | No mechanism exists (§6, §11) | `[OPEN]`, no design |
| T6 | Field disclosure to an unauthorized caller | `redact_gated_fields`, server-side, on the responses it is actually wired to (§5) | `[DONE]` for top-level fields **on CRUD-slot fns only**; `[OPEN]` for nested structs (§5 gap 1) |
| T6a | Field disclosure via a non-CRUD function returning the same rows | None — `field_gates_for_fn` returns no gates at all for any fn name outside a `screen`'s four CRUD slots (§5 gap 3) | `[OPEN]`, live |
| T6b | Field disclosure via `"ok"`/`"err"` key confusion on an ordinary struct | None — `redact_gated_fields` treats any object with an `ok`/`err` member as a `Result` envelope (§5 gap 4) | `[OPEN]`, live |
| T7 | Aggregate/inference disclosure via `stat_`/`chart_` | None — aggregates compute over unredacted rows (§5 gap 2) | `[OPEN]`, no design |
| T8 | Error/log/telemetry disclosure | `redact_gated_fields` returns `Err(...)` responses unredacted (`compiler/src/serve.rs:994-996`); an interpreter trap returns its raw message to the client (`compiler/src/serve.rs:1192`); `ErrorCode::External(str)` is a *blessed* pattern for forwarding raw driver text (`LANGUAGE.md` §6b, ~line 316-329); log/telemetry channels have no visibility policy at all | `[OPEN]`, no design — §11 |
| T8a | Silently-ineffective security annotation (typo'd `view`/`edit`/screen slot) | None — `typeck.rs::check_screen` ignores every key it doesn't recognize (§5 gap 5) | `[OPEN]`, live |
| T8b | Self-correction suppressing a static guard with `audited "..." { }` | None — the compiler only checks the justification string is non-empty (`LANGUAGE.md` §8) | `[OPEN]`, proposed mitigation in §7.2 |
| T9 | Client-side authorization bypass (a native/modified client skipping its own UI-level gating) | Every check above that is `[DONE]` at all is enforced server-side, independent of what the client does or skips — but a check nobody declared (T1b) or that isn't wired to this fn (T6a) is not made server-side either | `[DONE]` for the declared checks; does **not** compensate for T1b/T6a |
| T9a | Guarantee divergence between the interpreted and compiled backends | `codegen.rs::check_supported` rejects every construct in this document outright rather than mis-compiling it (§4a) | `[DONE]`, fail-closed |
| T10 | Generated-code regression from an automatic repair | Proposed tier-1/tier-2 verification (§7.4, §7.5) | `[OPEN]`, proposed |
| T11 | Self-correction weakening an authorization boundary to make a test pass | Proposed monotonicity invariant + non-regression check (§7.2, §7.3) | `[OPEN]`, proposed, not enforced anywhere yet |
| T12 | Malicious or incorrect specification driving self-correction toward the wrong answer | Proposed tier-3 escalation (§7.5) | `[OPEN]`, proposed |
| T13 | Cross-version compatibility regression (client relies on behavior an auto-correction changes) | `ROADMAP.md` Track A4 (§8) | `[OPEN]`, tracked, undesigned |
| T14 | Audit/provenance tampering for a code-generation event | Business-*data* mutations already get a real hash-chained audit trail (`finish_with_audit`); code-generation events do not (§9) | `[OPEN]`, proposed |

Several `[OPEN]` rows above (T5, T7, T8, T8a, T1a) have no proposed
mechanism at all, not even a sketch — that's an honest gap, not an
oversight in this table.

Rows marked **live** (T1b, T6a, T6b, T8a) are not future risks: they are
reachable against a program written the way this repo's own examples are
written today, and each names the exact source line that makes it so.
They were found by re-reading the source for this revision, not inherited
from an earlier draft.

---

## 3. Caller identity — `[DONE]` (web, HS256/RS256/ES256) / `[OPEN]` (mobile)

**Web — shipped, against `HS256`, `RS256`, or `ES256` JWKS.** `oidc_validate_token(token,
expected_issuer, expected_audience, jwks_json) -> Result(VerifiedIdentity,
str)` (`LANGUAGE.md` §5, "Identity / relying party", ~line 152) validates
a JWT against a JWKS trust anchor and returns a `VerifiedIdentity`.
`serve.rs::resolve_identity` runs this on every request carrying a
Bearer token, including the expiry check (`compiler/src/serve.rs:854-855`);
`check_role`/`check_role_path`/`extract_claim`/`extract_claim_path`/
`identity_expired` (`LANGUAGE.md` §5, ~line 159-164) build proofs and
check expiry from the resulting `VerifiedIdentity`. All of that is real
and enforced.

**What `oidc_validate_token` actually validates.** Verified line by line
against `compiler/src/interpreter.rs:1085-1184`, **updated 2026-08-26**
after `ROADMAP.md` A11's fix (RSA/EC verification via `ring`):

- It parses the three JWT segments, reads **`kid` and `alg`** from the
  header, and checks `iss`/`aud` against the expected values.
- Signature verification now dispatches on the header's own `alg`
  (`verify_jwt_signature`): `HS256` keeps the original constant-time HMAC
  path; `RS256` verifies via `ring::signature::RsaPublicKeyComponents`/
  `RSA_PKCS1_2048_8192_SHA256`; `ES256` via `ring::signature::
  UnparsedPublicKey`/`ECDSA_P256_SHA256_FIXED` over the raw uncompressed
  SEC1 point (`x`/`y`, 32 bytes each, prefixed `0x04`).
- `jwks_key` now resolves a `JwksKeyMaterial` (`Symmetric`/`Rsa { n, e
  }`/`Ec { crv, x, y }`) keyed off each JWKS key's own `kty`
  (`oct`/`RSA`/`EC`), instead of unconditionally reading a `k` member.
  `alg` and the resolved key's `kty` must agree — there is no match arm
  in `verify_jwt_signature` that accepts, say, `HS256` against an `Rsa`
  key — which is what actually prevents algorithm confusion (a caller
  presenting `alg: "HS256"` and replaying the JWKS's own public RSA/EC
  bytes as an HMAC secret), not just "no dispatch to confuse" as the
  previous, HS256-only version of this code could claim.

Two consequences a developer building a real app should know, **the
first of which is now resolved**:

1. **A mainstream IdP's published JWKS is now consumable.**
   Auth0/Okta/Entra/Google/Keycloak all publish `RS256` (some also
   `ES256`) asymmetric public keys, and `oidc_validate_token` now
   verifies both — real key material, not a mock, verified end-to-end in
   `compiler/tests/oidc_jwt_algorithms.rs` against a freshly generated
   2048-bit RSA keypair and a `ring`-generated P-256 keypair. `LANGUAGE.md`
   §5's "validates a **mock** OIDC/JWT ID token" phrasing referred to
   `mock_issue_token` (the token-*minting* helper, still HS256-only and
   for local dev/test only, unchanged by this fix) — it was never a
   claim about `oidc_validate_token`'s *verification* path, which is
   real either way. What's still genuinely missing before a real IdP
   integration is turnkey: JWKS discovery (`/.well-known/...`), a
   `/userinfo` call, and the multi-IdP registry named in `ROADMAP.md`
   Track A6 — `--jwks-file` today is still one fixed file per `serve`
   process, just no longer restricted to a symmetric one.
2. **"Nirdosha never mints its own tokens" is a policy for the
   symmetric case, and now a real cryptographic property for the
   asymmetric one.** With a **symmetric** JWKS the relying party still
   holds the *signing* key, so the server could still forge a token
   indistinguishable from the IdP's — `mock_issue_token`
   (`compiler/src/interpreter.rs:1221`) is exactly that capability, and
   it's still HS256-only by design (local dev/test, per its own doc
   comment). But with an **asymmetric** (`RS256`/`ES256`) JWKS — the
   configuration a real IdP integration actually uses — the JWKS
   document a relying party holds contains only the IdP's *public* key;
   there is no private key anywhere in `nirdosha serve` to forge with.
   For that configuration the phrase is now literally true, not just a
   policy. `[OPEN]` (T1a) remains accurate only for the symmetric/mock
   configuration — see the threat table's updated T1a row.

**Role-vocabulary drift is closed** (`LANGUAGE.md` §11a, `ROADMAP.md`
Track A6, `[DONE]` as of 2026-08-24): an admin-editable `RoleMapping
{ app_role, idp_role }` table, cached in-memory with a 30s TTL, so a
renamed IdP group doesn't silently stop matching every
`requires(role: ...)` check. Still string-literal match underneath —
see §4's ceiling below.

**Mobile — open, and already named as such in `MOBILE.md`, not a new
finding here.** `MOBILE.md`'s own "D2" item (~line 218-235) states
plainly: `VerifiedIdentity`/`TokenReference` are server-side cache
slots, and `ApplicationSession` is "explicitly an HTTP-only browser
cookie" — none of these are a credential shape a native app can hold in
Keychain/Keystore. `MOBILE.md` proposes (not built) a new device-bound
artifact built on `RefreshTokenHandle`'s existing shape, unlocked
locally via biometrics before each use, with a new `action { step_up:
biometric }` screen-DSL surface — explicitly gating whether the native
app *presents* the stored credential, never replacing the server's own
`requires(role/claim)` re-check. This doc adds nothing to that
design — it's the authoritative source for mobile identity, cite it
directly rather than duplicating it here.

---

## 4. Execute-permission — `[DONE]` where declared, `[OPEN]` (live, now visible not silent) by default

`requires(role: "<name>")` / `requires(claim: "<name>", "<value>")`
(`LANGUAGE.md` §6a, ~line 211-234) gates a function's *value*, not just
its behavior: an unacquired gated `fn`'s name has no direct-call path at
all — calling it or taking it as a first-class value is a **static**
`TypeErrorKind::PrivilegedFnNotAcquired` error. `acquire fn_name(proof)`
is the only way to obtain a callable, and it demands a real proof — a
`RoleView` from `check_role` or a `ClaimView` from `extract_claim` — so
"who is allowed to call this" is checked at the call site, not trusted
(this is literally requirement 12 in `README.md`'s own design-requirement
table, added same session as the ownership-example commit below:
`git show 64f286f -- README.md`).

**The default is open, and that is the more urgent finding than the
ceiling below.** `nirdosha serve` exposes **every** function a program
declares: `dispatch` resolves the route by
`program.fns.iter().find(|f| f.name == fn_name)`
(`compiler/src/serve.rs:1030`) with no allowlist, no
"only functions a `screen`/`dashboard` references", and no opt-in step.
Authorization is then applied *only if the function declared it*:
`f.requires` is checked at `compiler/src/serve.rs:1050-1063`, and a
`VerifiedIdentity` parameter forces a 401 for an anonymous caller at
`compiler/src/serve.rs:1129-1136`. A function with **neither** is
reachable by anyone who can reach the port, with no token at all.

This is not theoretical. Counted directly against this repo's flagship
example (`examples/trade-finance/trade_finance.nir`, 246 declared `fn`s):
34 carry `requires(...)`, 28 more take a `VerifiedIdentity` parameter,
and **79 take neither and no `db` handle** — i.e. 79 functions decodable
from a plain JSON body by an unauthenticated caller. Among them:
`issue_letter_of_credit` (`:1195`), `clear_sanctions_override`
(`:849`), `create_purchase_order` (`:1065`), `update_counterparty`
(`:776`), `amend_letter_of_credit` (`:1211`),
`apply_discrepancy_waiver` (`:1345`), and every `stat_`/`chart_`
aggregate. (The `_inner(conn: db, ...)` helpers are *not* in that count:
`decode_value` refuses to build a `db` handle from a request body
(`compiler/src/serve.rs:1430`), so those fail 400 — an accident of the
type system, not a deliberate gate.)

Note what this does to §5's protections in combination.
`update_counterparty` (`examples/trade-finance/trade_finance.nir:776-781`)
opens its own `db_connect("trade_finance.db")` and writes — it needs no
caller-supplied handle, so it is fully reachable anonymously. Field-level
`edit` gating still fires for `risk_rating` *if* the server was started
with `--db` (`check_edit_gates` is skipped entirely otherwise —
`compiler/src/serve.rs:1170-1174` guards on `table_db`), rejecting a
*changed* value with 403 since an anonymous caller satisfies no gate. But
**every non-gated field on that row is rewritten by an unauthenticated
caller either way**, because nothing above the field layer ever asked who
was calling. Field-level RBAC is not a substitute for the function-level
gate nobody declared.

**2026-08-26 — no longer silent, still `[OPEN]` at runtime
(`ROADMAP.md` A10).** Two directions were named here for a fix: (a) an
explicit, typechecked "this fn is intentionally public" marker so an
*absent* `requires(...)` is a typeck warning instead of silent, or (b) a
project-level default-deny mode `serve` can opt into. **(a) shipped**:
`requires(public)` (`ast::FnDecl::explicit_public` — deliberately not a
`Requirement` variant, so it silences the warning without gating direct
calls the way `requires(role/claim: ...)` does) plus
`typeck::ungated_fn_warnings`, a new non-fatal diagnostic wired into
`nirdosha serve`/`emit-ui` that names every `fn` this section's gap
applies to. Run today against the very example this section counts:
`nirdosha emit-ui examples/trade-finance/trade_finance.nir` prints
exactly **79** such warnings — confirming the count above wasn't just a
one-time manual audit, it's now a standing, reproducible check anyone
running this compiler gets automatically. **(b) is not built** — this
fix changes what an author *sees*, not what `dispatch` *does*: an
unmarked ungated `fn` is exactly as reachable by an anonymous caller
after this fix as before it. So `[OPEN]`, live, remains the accurate tag
for the underlying runtime posture; what changed is "no design, no
tracking item, first written down here" — now designed, tracked
(`ROADMAP.md` A10), and enforced as a warning, with (b) still open as
the only path to an actual runtime default-deny guarantee. Full
implementation detail: `LANGUAGE.md` §6a.

**The ceiling**: `requires(...)` accepts exactly one `role` or one
`claim` predicate, matched by string equality (through the role-mapping
cache above). There is no boolean composition ("role A OR role B", "role
A AND NOT role B"), and — the gap load-bearing for the rest of this
doc — **no resource-scoped or row-level permission at all.**
`requires(role: "treasury_user")` can gate *whether* a caller may call
`get_trade_payment`, but nothing in today's grammar can express "only
if this payment belongs to the caller's own counterparty." Confirmed by
grep: no `row_scope`/`owner_field`/row-level construct exists anywhere
in `LANGUAGE.md`, `ROADMAP.md`, or `compiler/src/*.rs` today.

**On the "ownership" commit** (`64f286f`, "Add example for ownership
error handling in Nirdosha") — checked directly for this doc: it does
**not** touch row-level data-access control. It adds `examples/broken.nir`
and a `README.md` passage demonstrating the *affine-type move-checker*
— `box`/`thread`/`sandbox`/`tcp`/`file` ownership (a value can be moved
exactly once; reusing it after a move is a static `UseAfterMove` error).
That is memory/resource ownership, unrelated to "does this user own this
row." No prior art for row-level ACL exists in this codebase — see §6's
proposed sketch, which starts from zero.

---

## 4a. Which backend enforces any of this — `[DONE]`, fail-closed

Not previously covered in this document, and load-bearing for anyone
reading `LANGUAGE.md` §10's compiled-vs-interpreted split: **every
mechanism in §3-§5 exists only on the interpreted path.**
`nirdosha serve` builds an `Interpreter` per request
(`compiler/src/serve.rs:1177-1183`); there is no compiled serving mode.

The LLVM backend does not silently lose these guarantees — it refuses
the programs that need them. `codegen.rs::check_supported` rejects
`Ty::Fn` (`compiler/src/codegen.rs:365-367`), `Expr::Acquire` (`:711`),
`Ty::Json`/`Ty::Db`/`Ty::Mq` (`:301-303`), `sandbox`/`file`/`transact`
(`:739`, `:747`, `:789`), and every Row 12 identity builtin — the
module doc's own summary at `compiler/src/codegen.rs:36-41` names
"`fn(..)->..`/`acquire`/`requires(...)`" and "every Row 12 identity
builtin" explicitly. A program using `requires`/`acquire`/`db` is a
clean `nirdosha build` error naming the specific unsupported construct,
never a binary with the authorization silently dropped. That is the
right posture, and worth stating so nobody assumes an `emit-llvm`'d
build of a gated app is "the same app, faster."

The inverse also holds, and matters for §7: **Tier-1 proof elision
exists only on the compiled path** (`compiler/src/codegen.rs:42-51`).
The interpreter keeps every Tier-2 runtime check regardless of what
`smt.rs` proved (`compiler/src/smt.rs:48-51`,
`compiler/src/refine.rs:30-38`, `compiler/src/ast.rs:456-463`). So
nothing §7.5's tier 1 proposes would change what `nirdosha serve` — the
thing that actually enforces this document — executes; it would change
what a `nirdosha build` of a program that cannot use any of §3-§5
executes. `[DONE]` as a description of today; `[OPEN]` as a design
question §7 does not currently address.

---

## 5. Return-data masking — `[DONE]` for CRUD-slot fields, `[OPEN]` for five real gaps

**Shipped, and a real security boundary, not cosmetic.** A `screen`
field-level gate —

```nirdosha
screen Product {
    field salary {
        view: role("finance")
    }
}
```

— is computed by `ui_gen.rs` (`view_roles`/`view_claim`/`edit_roles`/
`edit_claim` per field, `LANGUAGE.md` §11 ~line 666-674) and enforced
**server-side**, independent of the client, by
`serve.rs::redact_gated_fields` (`compiler/src/serve.rs:974`), which
nulls every view-gated field in the response before it ever leaves the
process. `check_edit_gates` separately rejects (`403`) a real write to
an edit-gated field from an unauthorized caller. The client-side
hide/disable in the generated UI is convenience only; this is the actual
boundary.

**But not on "every response" — that phrasing (this document's own
earlier draft, and `LANGUAGE.md` §11's ~line 668) is wrong, and gap 3
below is why.** Redaction reaches exactly two call sites: the generic
`/_nirdosha/table/<name>` route, which resolves gates **by struct name**
(`compiler/src/serve.rs:715-720`, `ui_gen::field_gates_for_struct`) and
is therefore genuinely complete for that route; and `dispatch`'s
`/api/<fn>` path, which resolves them **by function name**
(`compiler/src/serve.rs:1186-1187`, `ui_gen::field_gates_for_fn`) and
is not.

**Gap 1 — masking is defined over JSON object shape, not over
Nirdosha's own type graph, and that's the more durable way to state
this finding.** `redact_gated_fields`'s own doc comment
(`compiler/src/serve.rs:968-971`) is explicit about the immediate
symptom: "Deliberately shallow: redacts only an object's own top-level
keys, no nested-struct recursion, since no struct in this codebase
currently nests another — extend if that ever changes." But the
underlying issue is broader than "recurse one level deeper." Nirdosha's
real container/generic shapes today are `Result(T, E)` and `Option(T)`
(both real, and recursed through by the type system's own checks —
`LANGUAGE.md` §6b ~line 252, "checked recursively through
`Result`/`Option`/generics") plus `Vector(T, N)`/`Matrix(T, R, C)` (fixed-shape,
`N`/`R`/`C` compile-time literals — `LANGUAGE.md` §2). There is **no**
general `List<T>`/`Map<K, V>` — a `list_<entity>` function returns
untyped `json` (an array of row objects with no static shape at all),
which is exactly why `redact_gated_fields` has to walk a dynamic
`JsonVal` tree rather than a typed one. So visibility is currently
defined ad hoc, over whatever JSON shape a response happens to have —
matched against a flat `gates: &[GatedField]` list keyed by field name —
rather than recursively over `T`/`Result(T,E)`/`Option(T)`/`struct`
containing another `struct`, the way the type system itself already
understands nesting. Every new container shape this language grows
(struct-in-struct, or a future `List<T>`) is a potential masking bypass
until visibility is defined structurally against the type graph instead
of enumerated per response shape. `[OPEN]` — no tracking item yet;
should get one before struct nesting or a `List<T>` lands.

**Gap 2 — aggregate endpoints bypass field-level masking entirely. This
is a new finding, written up here for the first time; no prior doc
mentions it.** `stat_<name>() -> i64|f64` and `chart_<name>() -> json`
(`LANGUAGE.md` §11, the naming-convention table) are ordinary
hand-written `.nir` functions that compute over whatever rows they
query — server-side, over the *unredacted* data — and return a
`{label, value}` shape. `redact_gated_fields` matches responses against
`gates: &[GatedField]`, keyed by the underlying struct's own field
names; a chart's `label`/`value` keys never collide with a gated field
name, so **nothing stops `chart_average_salary_by_department()` from
segmenting or aggregating by a field that's `view`-gated on the raw
`Employee` struct.** The field-level RBAC promise ("this caller can't
see `salary`") is silently unenforced the moment `salary` is folded into
an aggregate instead of returned as a raw field. `[OPEN]` — proposed
direction: either (a) require `stat_`/`chart_` functions to declare
which underlying struct fields they read, and reject/redact at
`ui_gen`/typeck time if that includes a view-gated field the function's
own `requires(...)` doesn't already imply access to, or (b) treat this
as an inherent limit of field-level (vs. purpose-based) access control
and document it as a known non-goal — a decision for the team, not made
here. This is a narrower, field-provenance-tracking version of the
general information-flow question named in §11 — solving it doesn't
require solving information-flow control in general.

**Gap 3 — `/api/<fn>` masking only reaches a struct's four CRUD-slot
function names. Every other function returning the same rows is
unredacted. This is a live bypass, and the most serious finding in this
document.** `dispatch` obtains its gates from
`ui_gen::field_gates_for_fn` (`compiler/src/ui_gen.rs:685-713`). That
function walks the program's structs, finds each one's `screen` block,
and returns its gates **only if `fn_name` matches one of exactly four
names** — `list_<snake>`, `get_<snake>`, `create_<snake>`,
`update_<snake>`, or whatever a `list:`/`get:`/`create:`/`update:`
screen entry overrides them to (`:699-706`). For any other `fn_name` it
falls through every struct and returns `vec![]` (`:712`) — and
`redact_gated_fields` returns immediately on an empty gate list
(`compiler/src/serve.rs:980-982`). No redaction, no warning, no log
line.

So in an app that declares

```nirdosha
screen Counterparty {
    field risk_rating {
        view: role("compliance_officer", "bank_ops")
        edit: role("compliance_officer")
    }
}
```

(verbatim `examples/trade-finance/trade_finance.nir:1009-1014` — the
only field gate in a 246-function app),
`risk_rating` is masked on `list_counterparty` — and is returned in full
to every caller by any hand-written sibling: `search_counterparty()`,
`counterparty_by_lei()`, `my_counterparties()`, `export_counterparty()`,
a `db_query`-backed report, or any of this codebase's own
`Result(json, ErrorCode)` functions (141 of them across the examples)
that happen to `SELECT *` from the same table. Nothing about writing
that function looks unsafe, and nothing in the compiler or the server
says otherwise.

Note the shape of the mistake, because it generalizes: the gate is keyed
on **the function's name**, but the thing being protected is **the data's
type**. `field_gates_for_struct` — used by the `/_nirdosha/table/<name>`
route (`compiler/src/serve.rs:715-720`) — gets this right by keying on
the struct. `dispatch` can't do the same today because a hand-written
`.nir` function's declared return is frequently the opaque `json`
(`db_query`'s natural shape), so there is no struct to key on at all —
which is `field_gates_for_fn`'s own doc comment's stated reason for
resolving "purely from the declared `screen` block, never from the fn's
own body" (`compiler/src/ui_gen.rs:678-684`). That is an honest
explanation of the mechanism and simultaneously the reason it doesn't
cover the general case. `[OPEN]`, live, no tracking item. Directions,
none designed here: require a struct-typed return for any function
returning gated data; declare the backing struct on the function
(`returns(Counterparty)`); or make an ungated function that reads a
gated table a typeck error rather than silent.

**Gap 4 — the `"ok"`/`"err"` envelope test is a JSON-key test, so an
ordinary struct field named `ok` or `err` disables masking for that
whole response. Also live.** `redact_gated_fields` decides "is this a
`Result` envelope?" by looking for those member names
(`compiler/src/serve.rs:990-996`): an `"ok"` member means recurse into
it and `return`; an `"err"` member means `return` outright. But
`encode_value` emits an ordinary struct as a flat object of its own
field names (`compiler/src/serve.rs:1476-1486`), so
`struct Invoice { ok: bool, salary: i64 }` encodes to
`{"ok": true, "salary": 42}` — the `ok` branch fires, recursion into
`true` does nothing, and the function **returns before the gate loop
ever runs**. Every gated field on that row ships in the clear. A field
named `err` is worse: immediate return, no recursion at all. This is one
plausible field name away from a full masking bypass on an otherwise
correctly-declared `screen`, and it is the same root cause gap 1 names
— visibility defined over JSON shape rather than over the type graph,
where `Result` is a real, unambiguous `Ty::Named("Result", [_, _])` that
could be tested for directly. `[OPEN]`, live. (This is *also* why §11's
error-path non-goal is stated as a mechanism gap rather than an
exploitable leak — see that entry for the payload-shape evidence.)

**Gap 5 — a security annotation the compiler doesn't recognize is
silently ignored, not rejected.** `typeck.rs::check_screen` matches
screen-level entry keys against `"list" | "create" | "update" |
"delete"` and falls through to `_ => {}` for everything else
(`compiler/src/typeck.rs:1204-1212`); field-level entry keys match
`"view" | "edit" | "pattern" | "format" | "min" | "max"` with the same
`_ => {}` fallthrough (`compiler/src/typeck.rs:1233-1241`); and
`ui_gen::gates_from_screen_decl` reads only the exact keys `"view"` and
`"edit"` (`compiler/src/ui_gen.rs:642-654`). So

```nirdosha
screen Employee {
    field salary { veiw: role("finance") }   // typo — compiles clean
}
```

typechecks, emits a UI, serves traffic, and gates nothing. There is no
"unknown key" diagnostic anywhere in the screen DSL. Two smaller
instances of the same thing: `get:` is read by `field_gates_for_fn`
(`compiler/src/ui_gen.rs:701`) but is **not** in `check_screen`'s
validated key list, so `screen X { get: mispelled_fn }` silently
disables gap-3's already-narrow coverage for that slot; and `delete:`
is validated but never read back for gates. `[OPEN]`, live. This is a
prerequisite for §6 — a `row_scope:` entry added to today's grammar
would inherit exactly this failure mode (see §6's Option B).

---

## 6. Row-level (ownership-scoped) access control — `[OPEN]`, no prior art

§4 already established: nothing in this codebase expresses "caller may
only see/edit rows they own" today — only "caller may/may not call this
function at all" (function-level) and "caller may/may not see this
field on any row" (field-level). A real trade-finance app needs a third
axis: a `treasury_user` calling `get_trade_payment(id)` for a payment
belonging to a different counterparty should not succeed just because
they hold the right role.

**A prerequisite the grammar sketches below both depend on: not every
claim is authorization-bearing.** `extract_claim(identity, name: str)`
(`LANGUAGE.md` §5, ~line 161) pulls *any* string claim from
`identity.claims_json` by name — verified directly against
`compiler/src/interpreter.rs`'s `extract_claim`
handler (~line 2539) and its underlying `identity_claim` helper
(~line 1289): there is no declared vocabulary restricting which claim
*names* may be extracted, and by extension none restricting which
claims may legitimately drive an authorization decision versus which
are ordinary identity metadata (`sub`, `iss`, `aud`) that happen to be
present on the token. A `counterparty_id` claim on the token is not, by
itself, proof the caller *owns* counterparty `ABC` — it's just a value
the IdP put there; whether it's authorization-bearing is a decision the
application has to make explicitly, not something Nirdosha can infer
from the claim existing. Proposed rule for whatever row-scope construct
ships: **only claims explicitly declared as authorization-bearing may
participate in `requires(...)`, a future `row_scope(...)`, or field
gates** — an arbitrary, undeclared claim cannot silently become a
security primitive just because a JWT happens to carry it.

**Proposed grammar sketch — not implemented, for review only.**
Two shapes, both deliberately reusing `requires(...)`'s existing
"identifier + string-literal argument" shape (`LANGUAGE.md` §6a) rather
than inventing a new expression grammar, matching how `screen`'s own
`field`/`action` stay contextual keywords instead of new syntax
categories (`LANGUAGE.md` §11, ~line 616-621). Both were checked against
the real parser for this revision; the verdicts are recorded under the
sketches, because they differ:

```nirdosha
// Option A — function-level, extends requires(...) with a third kind
// alongside role/claim. `owner` names a parameter of the gated fn whose
// value must equal some field on the identity/session (exact
// comparison mechanism is the open design question — see below).
fn get_trade_payment(id: i64) -> Result(TradePayment, ErrorCode)
    requires(owner: "buyer_counterparty_id") { ... }

// Option B — screen-level, declarative, mirrors field { view: role(...) }
screen TradePayment {
    row_scope: owner_field(buyer_counterparty_id)
}
```

**Grammar verdict, Option A — realistic, and cheaper than the doc
previously implied.** Checked against `parser.rs::parse_requires_annotation`
(`compiler/src/parser.rs:850-876`): `requires` itself is a *real reserved
token* (`Tok::Requires`, `compiler/src/token.rs:73`, lexed at `:380`),
but the requirement **kind** is matched by raw identifier text in a
`match kind.as_str()` over `"role"`/`"claim"` (`:859-871`), with an
`other =>` arm that already produces "unknown requirement `{other}` —
expected `role` or `claim`". Adding `owner` is one arm there plus one
`ast::Requirement` variant — no new token, no lexer change, no
LL(1) risk, and it inherits `expect_str_lit`'s string-literal discipline
(`:878-892`) unchanged. The `requires_annotation` production in
`GRAMMAR.md:124` would need one alternative added. This one is
genuinely small; the hard part is entirely semantic, below.

**Grammar verdict, Option B — parses today, and that is the problem.**
A `screen` body's non-`field`/`action`/`paginate` entries are ordinary
`kv_entry`s whose value is a full `parse_expr()`
(`compiler/src/parser.rs:373-378`), so `row_scope:
owner_field(buyer_counterparty_id)` already lexes and parses as an
`Expr::Call("owner_field", [Expr::Ident("buyer_counterparty_id")])`
with **no parser change at all**. It then typechecks clean, because
`check_screen` drops every unrecognized screen-level key into `_ => {}`
(`compiler/src/typeck.rs:1204-1212`), and it enforces nothing, because
`gates_from_screen_decl` reads only `"view"`/`"edit"`
(`compiler/src/ui_gen.rs:642-654`). In other words, Option B as written
is a security annotation that compiles, ships, and silently does
nothing — §5's gap 5 exactly. Two further checks a real implementation
must add, neither of which exists: a validator in the shape of
`check_visibility_expr` (`compiler/src/typeck.rs:1087-1100`, which today
accepts only `role("..")` / `claim("..","..")` with string-literal
arguments), and a check that `buyer_counterparty_id` names a real field
on the screen's struct — the bare `Expr::Ident` is resolved against
nothing today, so a misspelling would be invisible. Option B is
therefore **more speculative than Option A**, not equally so: it needs
typeck work, `ui_gen` work, and `serve.rs` work, and it needs gap 5
fixed first so that a mistyped `row_scope` is an error rather than a
no-op.

Neither sketch resolves the actual hard question: **owner of what,
against what?** `requires(role/claim: ...)` compares a static string
literal against the caller's token claims — a row-scope check instead
has to compare a *runtime* value (the row's `buyer_counterparty_id`)
against something derived from the caller's identity (a claim naming
their own counterparty id, most likely — and per the rule above, one
that's been explicitly declared authorization-bearing) — a
fundamentally different shape of check than anything `requires(...)`
does today, closer to `check_edit_gates`'s per-request logic than to a
type-level gate. This needs its own design pass before either sketch
above is buildable; they are starting points for that conversation, not
a spec.

**A second open question this section surfaces but doesn't resolve:
what does the server return when a row exists but isn't the caller's?**
`GET /api/get_trade_payment` for a payment ID that's real but belongs to
a different counterparty — does the response say `403 Forbidden`
(correct semantically, but confirms the row exists at all to a caller
who shouldn't even know that) or `404 Not Found` (hides existence,
closer to real object-level authorization, the standard mitigation for
this exact "insecure direct object reference" class of leak)? Nirdosha
has no answer today because nothing enforces row scope at all yet
(`403` vs `404` is moot until §6's actual check exists) — but it's a
real decision with real consequences the moment it does, not a footnote
to settle later. `[OPEN]`, flagged here so whoever designs the row-scope
check decides it deliberately.

---

## 7. Self-correction against Hoare pre/post + acceptance criteria — `[OPEN]`, proposed model

### 7.1 The central risk: a self-correction loop is only as trustworthy as its spec

This is the load-bearing idea in this section — everything else here is
in service of it. A self-correcting generator that regenerates code
until some predicate holds **cannot tell the difference between "the
code was wrong" and "the spec itself was ambiguous or wrong."** It will
converge on *something* either way, with equal confidence, and a
confident wrong answer is worse than a visibly-hardcoded placeholder,
because it no longer looks unresolved.

This is not hypothetical for this codebase — it already happened.
`US-TRDPAY-002`'s extracted `post_logic`
(`scratch/extracted_userstories_v2.json`) states:

```
routed_to_six_eyes == (payment_amount > high_value_threshold)
```

— strict `>`. The shipped rule, `required_eyes_for_amount`
(`examples/trade-finance/trade_finance.nir:1733-1735`), uses `>=`.
Exactly-at-threshold is six-eyes in code, Maker-Checker per the spec's
own wording. The threshold *value* diverges too, though in a different
way worth stating precisely: the story never fixes one at all — it says
"the configured high-value threshold," and its acceptance criteria only
illustrate with "e.g., >$1,000,000" (the PRD's own hedge, per
`ROADMAP.md` A9), while the shipped rule hardcodes $50,000 and
self-discloses that as "a fixed illustrative cutoff." So the operator is
a genuine contradiction; the threshold is a genuine *omission*. They are
tier-3 cases (B) and (C) respectively, which is why §7.5 needs both.
Both discrepancies are tracked as `ROADMAP.md` Track **A9** and
`PROTOBOX_INTEGRATION.md` §9, and both are *demonstrated*, not just
described, by `compiler/tests/trade_finance_governance_routing.rs`'s
`boundary_case_at_exact_threshold_is_six_eyes_per_shipped_code` test —
which asserts what's actually shipped, not what the story implies, and
says so in its own comment. A self-correcting loop pointed at this
`post_logic` today would have no way to know the shipped `>=` is the
"wrong" one (or that it's "right" — nobody has actually decided). It
would just pick one and keep it.

**Consequence for design**: self-correction must be able to say "I
cannot resolve this — the spec is ambiguous" and stop, rather than loop
to convergence on an arbitrary interpretation. §7.5's tier 3 exists
specifically for this.

### 7.1a Pre/post conditions and acceptance criteria are different verification targets — not the same thing

The rest of this section, in an earlier revision, used "`post_logic`" and
"acceptance criteria" almost interchangeably. They are not the same
artifact, they are not checked the same way, and they can diverge from
each other independently of whether the code is right — this needs to
be explicit before the tiers below are read as a single pipeline.

**Pre/post conditions** (`pre_logic`/`post_logic`) are a Hoare-style
contract on *one function*: `{P} f {Q}` — `P` constrains that function's
own parameters at entry, `Q` constrains its own return value at exit.
`routed_to_six_eyes == (payment_amount > high_value_threshold)` is this
shape: one function, its own params, its own return. This is what §7.5
tier 1's proposed SMT extension targets, and it's the only shape
`smt.rs` could ever discharge even with the missing pieces named there
built.

**Acceptance criteria** (a story's Given/When/Then) describe
*observable end-to-end behavior*, which is very often not one
function's input/output at all. Look at `US-TRDPAY-002`'s own
postconditions, not just its acceptance-criteria prose: "The payment is
assigned to exactly one governance queue," "the routing decision is
consistent with the configured threshold" — and its own action list
ends with "System places the payment into the assigned approval queue,
ready for the first approval step." That's the state of the database
after `submit_trade_payment` calls `submit_approval`, which itself
calls `submit_approval_inner`, which writes a row
(`examples/trade-finance/trade_finance.nir:1741-1743`, `:384-389`,
`:359-375`). No single function's return value is what the acceptance
criterion is actually about — it's about persisted state after a call
sequence.

**Consequence — they can each be wrong independently, and they can
disagree with each other independently of the code:**

1. A function's `post_logic` can hold in complete isolation while the
   acceptance criterion it was meant to formalize still fails
   end-to-end — e.g. the routing decision is computed correctly but a
   downstream step never persists it, or a later call overwrites it.
   Tier 1/2 below, aimed at a single function, would report green while
   the thing a user actually cares about is broken.
2. `pre_logic`/`post_logic` and `acceptance_criteria` are both produced
   by the same extraction pipeline from the same workflow/NFR text
   (`scratch/hoare_userstory_prompt.py`, rules 5 and 11 respectively)
   but **independently** — nothing in that prompt cross-checks that the
   formal predicate and the prose scenario agree. In `US-TRDPAY-002`
   they happen to agree (both imply strict `>`) — that's a fact about
   this one story, not a guarantee the pipeline enforces. A future story
   could ship a `post_logic` and an acceptance criterion that
   contradict each other before a single line of `.nir` is generated,
   and nothing today would catch it.

**Consequence for the tier model below**: tier 1 (SMT) can only ever
discharge a pre/post condition of one pure function — it has no way,
even in principle until interprocedural summaries exist (§7.5 tier-1
caveat 2), to check an acceptance criterion spanning multiple functions
or DB state. Tier 2 needs to be read as two distinct sub-cases, not one
— see the split below. And tier 3's escalation set gains a case that
exists before any code is written at all: the pre/post condition and
the acceptance criteria disagreeing with each other.

### 7.2 Repair must not weaken security

**The single most important rule in this document.** A self-correction
that resolves a *functional* test failure must never do so by removing
or narrowing a *security* boundary — deleting or loosening a
`requires(role/claim)` gate, a `view`/`edit` field gate (§5), or, once
it exists, a row-scope predicate (§6) — even when doing so happens to
make the failing test pass. A repair loop optimizing purely for "the
check now passes" has no built-in reason to prefer a fix that keeps
authorization intact over one that just removes the thing that was
failing; nothing today prevents that shortcut, because no self-correction
pipeline exists yet to prevent it from.

Proposed as a hard, named invariant: **security annotations and
authorization constraints are monotonic unless an explicitly authorized
human changes the specification.** A generator is free to change *what*
a function computes in response to a failing `post_logic`/acceptance
test; it is never free to change *who is allowed to call it* or *what
they're allowed to see* as a side effect of chasing that test to green.
Those are a different, higher-trust category of change and should
require an explicit, human-authorized spec change, not just a passing
proof or test.

**One concrete escape hatch the invariant has to name explicitly, found
reading `LANGUAGE.md` §8 for this revision.** `audited "justification"
{ ... }` is a real, shipped statement form (`GRAMMAR.md:341`,
`compiler/src/parser.rs:916`) whose documented effect is to **suppress
codegen's guard emission inside the block** — and "the compiler only
enforces that a justification exists and is non-empty; judging its
content is a review-process concern, not a compiler one"
(`LANGUAGE.md` §8). That is exactly the shape of thing a repair loop
optimizing for "the check now passes" would reach for: wrapping a
failing region in `audited "auto-repair"` makes a static complaint go
away without changing the code's behaviour, and the compiler will accept
the justification string unread. Any self-correction pipeline must treat
**introducing or widening an `audited` block as a security-relevant
change** under the monotonicity rule above, on the same footing as
loosening a `requires(...)`. `[OPEN]`, proposed — nothing enforces this,
because nothing generates code automatically yet.

### 7.3 Authorization non-regression

A directly testable corollary of §7.2, proposed as a required check in
any future self-correction acceptance gate, not implemented anywhere
today: for every authorization scenario that was previously denied,

```text
denied_before ∧ no_explicit_policy_change  =>  denied_after
```

i.e. `SecurityPolicy(N+1) ⊇ SecurityPolicy(N)` — a correction must never
newly authorize a principal or a data path that was denied in the
version it's replacing, unless a human explicitly changed the policy as
part of that correction. This is meant to be mechanically checkable
once §7.5's tiers exist (replay every previously-denied case against the
candidate correction, same shape as `nir_scenario!`'s Given/When/Then,
asserting continued denial) — proposed, not built.

### 7.4 Existing infrastructure to build on, not from zero

- **Static proof, already real, considerably narrower scope than an
  earlier draft of this section claimed.** Two separate passes exist,
  and only one of them uses Z3 — the earlier "`refine.rs`/`smt.rs`
  already use Z3" was wrong. `refine.rs` is **interval analysis with no
  SMT solver at all**, by explicit design ("implemented **without** an
  SMT solver," `compiler/src/refine.rs:1-20`; `LANGUAGE.md` §8 lists the
  two as separate provers for this reason). `smt.rs` is the Z3 one
  (`z3 = "0.20.2"`, a real `compiler/Cargo.toml` dependency), and only
  its `SmtReport` is consumed by codegen (`compiler/src/codegen.rs:75`,
  `:928`) — `refine.rs` is the documented fallback for an environment
  with no Z3 and feeds nothing that runs.

  What `smt.rs` proves is three compiler-generated obligations: an
  arithmetic result fits its declared integer type, a divisor is
  non-zero, and a `Vector`/`Matrix` index is in bounds
  (`compiler/src/smt.rs:26-31`, `SmtReport`'s three fields at `:61-73`).
  Those feed a real Tier-1/Tier-2 split in codegen: a proven-safe
  `let`/assignment gets **no runtime check at all** emitted (Tier 1); an
  unproven one gets a real compare-and-trap sequence in the compiled
  binary (Tier 2) (`compiler/src/codegen.rs:42-51`, self-described as
  "the first place in the whole codebase where a static proof actually
  changes what runs," `compiler/src/codegen.rs:51`). Real, working
  infrastructure — but see §7.5's tier-1 caveats for how far it is from
  discharging a user-written business predicate.
- **Runtime example-based testing, established this session, not
  before.** `compiler/tests/trade_finance_governance_routing.rs`'s
  `nir_scenario!` macro is the first instance in this repo of running a
  named `.nir` function through the real parser/typeck/interpreter and
  asserting a GWT-shaped scenario against it — the concrete precedent
  for tier 2 below.
- **An existing repair loop, but the wrong failure signal.**
  `../protobox/be-v2/src/plugins/languages/nirdosha_direct_codegen.py`
  already runs a generate→compile→fix loop today, bounded by
  `max_repairs=3` (`nirdosha_direct_codegen.py:179,226`). It repairs
  against **compiler type errors only** — nothing today feeds
  `pre_logic`/`post_logic` (already produced as JSON by
  `scratch/hoare_userstory_prompt.py`'s extraction pipeline, rule 11)
  into that loop, or into anything else executable. The one place
  those predicates have been checked against real shipped code at all
  is the hand-written test above. Note also: this loop has no concept
  of §7.2/§7.3's security-monotonicity rule either — another reason
  those need to be stated explicitly before self-correction touches
  anything security-relevant.

### 7.5 Proposed model: three tiers, by what's actually decidable

Not implemented — a proposed classification for review.

1. **Tier 1 — pure arithmetic/boolean predicates over a function's own
   parameters** (the six-eyes rule is exactly this shape: one `i64` in,
   one comparison out). The *logic* is decidable and cheap — ordinary
   linear integer arithmetic, squarely inside what Z3 already handles.

   **2026-08-26 — built, as its own module, not folded into `smt.rs`
   (`ROADMAP.md`'s contract-checking entry, new `compiler/src/
   contract_check.rs`).** Deliberately a separate file rather than an
   extension of `smt.rs`'s existing `Checker` — same "duplicate a
   focused walker rather than couple two independently-evolving
   analyses" precedent `smt.rs`'s own module doc already sets for
   `assigned_names` vs. `refine.rs`'s copy of it. `check_fn_contract(
   program, fn_name, pre_logic, post_logic, extra_bindings)` takes real
   `.nir` source (a Hoare pair, straight out of `scratch/
   extracted_typed_v1.json`'s `routing_fn.pre_logic`/`post_logic`
   shape), parses each predicate with the *same* grammar/parser every
   `.nir` expression gets (`parser::parse_standalone_expr`, one new
   `pub(crate)` entry point — no separate mini-language to keep in
   sync), asserts every `pre_logic` entry as a hypothesis before walking
   the body (a real Hoare triple, `{P} f {Q}`, not "prove `Q` for
   literally every input regardless of `P`"), and at every `return`
   reached under that hypothesis either proves each `post_logic` entry
   or returns a **real, concrete counterexample** (extracted from Z3's
   own model — not a symbolic report) naming exactly which clause it
   violates.

   Below addresses each of the four gaps this section named, in order:
   - **The user-facing obligation channel now exists** — the four
     paragraphs above are it.
   - **No interprocedural summaries — still true, and still the actual
     boundary**, not loosened: `check_fn_contract` only ever reasons
     about one named function's own params/`result`; a `Call` anywhere
     in the function body or the predicate itself is `Unsupported`,
     never silently approximated. This is a *load-bearing* design
     choice, not an oversight: approximating an unmodelable
     sub-expression with a fresh unconstrained value is sound for a
     universal *proof* (over-approximation only weakens what can be
     shown) but **unsound for a counterexample** — a "violation" built
     partly from a meaningless free variable might not correspond to
     any real input/output at all. So the walker aborts the moment it
     can't model something, on both sides, rather than ever risk a
     misleading report.
   - **Integers only — unchanged, by the same choice, not a limitation
     discovered too late.** `check_fn_contract` requires every parameter
     and the return type to be an integer `Ty` (`Ty::is_integer()`);
     anything else is `Unsupported` with the actual type named. Verified
     concretely: `required_eyes_for_amount(amount_cents: i64) -> i64` is
     exactly this shape, and its real body — `if amount_cents >= 5000000
     { 2 } else { 1 }` — is proved to satisfy the extraction's own
     `post_logic`, `(result == 2) == (amount_cents >= high_value_
     threshold)`, for **every** `i64` `amount_cents`, once told what
     `high_value_threshold` concretely is (see below) — not just the
     handful of inputs `compiler/tests/
     trade_finance_governance_routing.rs`'s `nir_scenario!` happens to
     try. New `bool_expr` case needed getting this right that `smt.rs`'s
     own didn't have (nothing it synthesizes itself is shaped this way):
     the predicate's outer `==` is a **biconditional between two
     comparisons**, not integer equality between two numbers —
     `is_bool_shaped` recurses into `bool_expr` on both sides when
     they're themselves comparison/logical expressions.
   - **Loops — still not modeled, unchanged**: `Stmt::While` is an
     immediate `Unsupported`, for the same "no invariant synthesis"
     reason.

   **The `high_value_threshold` case — §7.1a's exact "the spec
   references a quantity the code doesn't parameterize on" gap, made a
   named, required input instead of a silent wrong answer.**
   `required_eyes_for_amount` takes only `amount_cents` —
   `high_value_threshold` is a PRD concept the code hardcodes as a
   literal (`ROADMAP.md` A9), not a parameter. `check_fn_contract`
   requires the caller to supply a concrete value for every such
   identifier via `extra_bindings`; omitting it returns
   `UnboundIdentifier("high_value_threshold")` rather than silently
   treating it as "any value" (which would make the predicate
   unprovable for a reason that has nothing to do with the code being
   wrong) or "some value nobody chose." Supplying the code's actual
   5,000,000 proves the contract; supplying a *wrong* value (say,
   6,000,000) correctly comes back `Counterexample` with a real
   `amount_cents` and the real `result` it produces — verified
   independently against the real function's own logic in the test, not
   just trusted from the solver.

   **What this does and doesn't close, precisely.** It closes Tier 1 for
   exactly `routing_fn`-shaped entries — one real, named, pure,
   loop-free, integer function with its own Hoare pair — demonstrated
   end-to-end against `scratch/extracted_typed_v1.json`'s
   `WF-TRDPAY-001.routing_fn` in `compiler/tests/
   extracted_typed_v1_verification.rs` (8 tests, including two that
   deliberately mutate the `.nir` source to prove the checker actually
   detects drift rather than trivially matching — see the workflow-
   conformance construct below for the other half of that file). It does
   **not** close a user story's own `pre_logic`/`post_logic` (e.g.
   `US-COMM-006`'s `withdrawal_amount > 0`) — the extraction schema
   (`extraction_schema::ExtractedUserStory`) has no field binding a
   story to the real `.nir` function(s) that implement it (confirmed:
   `scratch/extracted_typed_v1.json`'s `user_stories[]` entries have no
   such field; only a `workflow`'s `routing_fn.name` names a real
   function today). `implements: Vec<String>` exists on
   `ExtractedUserStory` as a `#[serde(default)]` placeholder for exactly
   this, always empty until the extraction prompt is extended to emit
   it — the one remaining precondition for user-story-level Tier-1
   checking, not a design gap in `contract_check.rs` itself. And per
   §7.1a, most user-story postconditions are Tier 2b's shape anyway
   (end-to-end DB state after several functions), not this one.

   Scope rule, now enforced by the code rather than just proposed:
   `check_fn_contract` never inspects `effect(...)` directly — it
   doesn't need to, since anything an effectful function does that this
   walker can't model (a `Call` into `db_query`/`http_post`/etc.) is
   already `Unsupported` structurally, the same outcome the
   effect-based rule was reaching for.

   **A sibling construct, outside this tier numbering — 2026-08-26,
   `compiler/src/workflow_conformance.rs`.** A `workflow`'s
   states/transitions/data fields need no solver at all: they're a
   finite, fully-known structure the moment `.nir` source parses, so
   "does the real `workflow { ... }` declare exactly the states,
   transitions, and data fields the extraction says it should" is
   ordinary set/relation equality (`check_workflow_conformance(program,
   extracted) -> ConformanceReport`), not a Tier-1 proof or a Tier-2
   test. This makes it the more *complete* of the two new constructs —
   always a real match or a real, named diff, never `Unsupported` — but
   the narrower one: it verifies shape (which states exist, which
   transitions connect them, whether each is `terminal`), not behavior
   (`on_entry`/`on_exit` actions are compared by *count* only, not by
   matching prose like `"notify the checker role..."` against a real
   `notify(...)` call's actual role argument — a natural-language-to-
   call binding this deliberately doesn't attempt). Demonstrated against
   all three of `scratch/extracted_typed_v1.json`'s `workflows[]`
   entries in `compiler/tests/extracted_typed_v1_verification.rs`,
   including two tests that mutate the real `.nir` source (drop a
   transition; flip a `terminal` flag) and confirm the specific,
   named mismatch is actually reported, not just that *some* diff
   exists.
2. **Tier 2 — effectful/DB-touching logic. No static proof is possible
   here** (an SMT solver can't reason about what a `db_query` returns),
   so the only available check is runtime, example-based testing — but
   per §7.1a, that splits into two genuinely different test shapes, not
   one:
   - **Tier 2a — unit-level, one function's own pre/post condition.**
     Exactly what `nir_scenario!` already does today: parse/typecheck/
     run one named `.nir` function directly against concrete
     Given/When/Then inputs and assert its `post_logic` predicate on
     the returned value (`compiler/tests/
     trade_finance_governance_routing.rs`). This proves nothing about
     what happens once that function's result is passed downstream.
   - **Tier 2b — scenario-level, an acceptance criterion's end-to-end
     behavior.** Running the actual call sequence a story's
     Given/When/Then describes (e.g. `submit_trade_payment` →
     `submit_approval` → the resulting `approval_request` row) and
     asserting on *persisted state*, not a single return value. This is
     a heavier test shape — real `db_connect`/multiple interpreter
     calls sharing a DB, not one `fn main() { return ... }` snippet —
     and **it does not exist yet**. `nir_scenario!` is a worked example
     of tier 2a only; the one scenario this session actually exercised
     (`required_eyes_for_amount`) happens to be an unusually easy case
     where the acceptance criterion collapses to a single pure
     function's post-condition (routing is a pure `i64 -> i64`
     decision). That should not be read as evidence tier 2b works too —
     most of `US-TRDPAY-002`'s own acceptance criteria (§7.1a) are not
     that shape.
   Either tier proves specific cases, never that a predicate holds for
   *all* inputs the way tier 1 can.
3. **Tier 3 — the spec itself doesn't pin down a unique implementation.**
   Not one case, but at least five distinguishable ones:
   - **(A) Satisfiable and unambiguous** — the candidate implementation
     provably (tier 1) or testably (tier 2) satisfies the spec, and no
     other reading of the spec text disagrees. Accept.
   - **(B) The specification is internally inconsistent.** E.g. three
     acceptance criteria that, read together, can't all hold (a
     boundary value where AC1 implies six-eyes and AC2 implies
     Maker-Checker for the same input). This is concretely checkable
     with today's *solver*, though not with today's *pipeline* — the
     obligation channel tier 1 needs (above) is the same missing piece
     here: conjoining a
     story's acceptance-criteria predicates and asking Z3 whether the
     conjunction is UNSAT is exactly the shape `smt.rs` already solves
     for range proofs (§7.4 — `refine.rs` is the non-SMT prover, not
     this one) — a plausible extension path, not
     implemented. Escalate.
   - **(C) The specification is under-specified.** Exactly §7.1's `>`
     vs `>=` case — the boundary value's classification is genuinely
     unstated, not contradictory, just missing. Neither a static proof
     nor a passing test resolves this, because both readings "pass" some
     technically-valid interpretation of the story text. Escalate,
     surfacing exactly the question `ROADMAP.md` Track A9 proposes
     asking at `nirdosha init` time ("what's the threshold, and is the
     boundary inclusive or exclusive?"). A tier-3 escalation with no
     human answer yet should block, not guess.
   - **(D) Multiple valid implementations produce identical
     externally-observable behavior.** Two different function bodies
     that both satisfy the contract for every input the contract
     constrains. This is not an ambiguity to escalate — self-correction
     should target **semantic equivalence with respect to the declared
     contract**, not a unique implementation. Don't reject or flag a
     correction merely because it differs syntactically from a previous
     version if both satisfy the same proof/tests.
   - **(E) The pre/post condition and the acceptance criteria disagree
     with each other — before any code exists.** §7.1a's second
     consequence: since `pre_logic`/`post_logic` and
     `acceptance_criteria` are extracted independently from the same
     source text with no cross-check, a story can ship with a formal
     predicate and a prose scenario that contradict each other. This
     must be checked *before* generation starts, the same way (B)'s
     conjoined-predicates UNSAT check works, but conjoining across the
     two artifacts instead of within one — proposed, not built, and a
     precondition for tier 1/2 to even mean anything: there's no point
     proving code satisfies a `post_logic` that doesn't match the
     acceptance criteria it was supposed to formalize.

### 7.6 Pre/post annotation grammar sketch — proposed, unimplemented

For review only. Shaped after `requires(...)`'s existing
"identifier(...)" annotation slot rather than a new expression syntax:

```nirdosha
fn required_eyes_for_amount(amount_cents: i64) -> i64
    ensures(post: "(result == 2) == (amount_cents > 5000000)")
{
    return if amount_cents >= 5000000 { 2 } else { 1 }
}
```

**This example was corrected in this revision, and the reason is the
most important thing in §7.6.** The previous draft wrote
`ensures(post: "routed_to_six_eyes == (result == 2)")`. That predicate
is **vacuous**: `routed_to_six_eyes` is bound by nothing — not a
parameter, not the return value, not any declared name in the program —
so the equation *defines* a free variable rather than constraining the
function, and holds for every possible body. A contract that any
implementation satisfies is worse than no contract, and a
self-correction loop pointed at it would report success on arbitrary
code. The version above is deliberately falsifiable instead: it is
written entirely over `amount_cents` and `result`, and Z3 would return
`amount_cents == 5000000` as a counterexample — which is precisely
§7.1's `>` vs `>=` disagreement, surfaced as a number.

**The corollary the tier model has to absorb: the extraction pipeline's
predicates are not in the function's vocabulary, and nothing translates
them.** `US-TRDPAY-002`'s actual `post_logic` is
`routed_to_six_eyes == (payment_amount > high_value_threshold)` — three
free names, **none** of which is `required_eyes_for_amount`'s parameter
or return, plus a unit mismatch (the story's acceptance criteria speak
in dollars, the shipped rule in cents) and a "configured threshold" the
story never fixes to a value. Getting from that string to the
falsifiable predicate above required a human to decide that
`routed_to_six_eyes` means `result == 2`, that `payment_amount` means
`amount_cents`, and that `high_value_threshold` means the literal
`5000000`. **No artifact in this repo performs or records that
mapping.** So the missing piece for tier 1 is not solver power (§7.5's
caveats already cover that); it is a *binding/refinement layer* from
story vocabulary to a function signature, which nothing has designed —
and which is itself a place a self-correction loop could quietly get the
semantics wrong while every proof passes. `[OPEN]`, unnamed anywhere
before this revision.

**Grammar realism, checked rather than asserted.** The earlier draft
called this shape "consistent with `requires(...)`'s existing
'identifier(...)' shape" — true of the *slot*, misleading about the
*keyword*. `requires` is a real reserved word in the lexer
(`Tok::Requires`, `compiler/src/token.rs:73`, keyword table at
`:379-390`), not a contextual identifier like `role`/`claim`/`field`/
`action`. So `ensures` must become a **new reserved word**, which is a
breaking change for any existing program using `ensures` as an
identifier — a different and larger step than §6's Option A, which adds
only a `match` arm inside an existing keyword. The slot itself is clean:
`parse_fn_decl` reads `effect_annotation?` then `requires_annotation?`
then the block (`compiler/src/parser.rs:783-786`), so an
`ensures_annotation?` between them is LL(1) with no lookahead, and
`GRAMMAR.md:107`'s `fn_decl` production takes one more optional term.
Buildable — just not free, and not "the same as `requires`."

**A type-system constraint the sketch has to respect, not previously
noted.** A postcondition is evaluated conceptually *after* the body, so
it can only mention values still observable there. Nirdosha's affine
types — `box`/`thread`/`sandbox`/`tcp`/`tcp_listener`/`file`/`db`/`mq`
(`compiler/src/ast.rs:439-454`), and transitively any `struct`/`enum`
containing one (`compiler/src/ast.rs:1531-1549`) — can be moved exactly
once. A contract naming a `db` parameter that the body consumed is a
static `UseAfterMove` from `ownership.rs`, not a checkable predicate;
that is the shape of essentially every `_inner(conn: db, ...)` function
in `examples/trade-finance/trade_finance.nir`. And a predicate that
wants to *observe* database state to decide whether it holds is not pure
— reaching `db_query` would give the contract `Effect::Io`, which by
§7.5's own scope rule pushes the function out of tier 1 and into tier 2
regardless. Both point the same way: **tier-1 contracts are restricted
to non-affine, scalar-shaped params and returns**, and the sketch should
say so rather than leave it to be discovered.

Open questions this sketch does *not* resolve, flagged rather than
answered: what expression language the string body should actually be
(reusing `pre_logic`/`post_logic`'s existing arithmetic/comparison/`&&`/
`||`/`!`/`sum(...)` subset — `scratch/hoare_userstory_prompt.py` rule
11 — is the obvious starting point, since it's already what the
extraction pipeline emits); how a `post_logic` predicate refers to a
function's own return value (`result`, above, is a placeholder, not a
decided keyword); and, most load-bearing per §7.1, what actually happens
when tier 1 finds a counterexample — is that always a code bug, or does
the counterexample itself need to be shown to a human before anyone
decides which side (code or spec) is wrong.

Two further caveats worth recording now, without designing either
resolution:

- **This string-embedded predicate is provisional, not a permanent
  semantic representation.** The moment `ensures(...)` becomes
  compiler-enforced rather than documentation, embedding a second
  language inside a Nirdosha string literal (Nirdosha → string → Hoare
  expression parser → SMT representation) is a real cost — two parsers,
  two semantic systems to keep in sync. A typed, AST-level contract
  syntax may be worth it once this is load-bearing. Not designing that
  now; recording that the string form must not be assumed permanent.
- **`ensures`/`requires` need to stay conceptually, and eventually
  syntactically, distinct from ordinary Hoare preconditions.** Hoare
  reasoning is `{P} C {Q}` — a precondition *and* a postcondition, not
  just `C -> Q`. This sketch only covers the postcondition half; a real
  precondition (`amount > 0`, say) would gate what the postcondition is
  even required to hold under. Reusing the word `requires` for that
  functional precondition, alongside `requires(role: ...)`'s existing
  *authorization* meaning, risks exactly the kind of semantic collision
  this document has spent §1-§6 trying to keep apart — three unrelated
  concepts under one keyword. Flagged as a naming/scope question to
  settle deliberately before any of this becomes real syntax, not
  resolved here.

---

## 8. Versioning/compatibility — `[OPEN]`, existing item, now load-bearing

`ROADMAP.md` Track **A4** ("Compatibility/versioning policy") is
already an open item, filed for an unrelated reason (the str-ban
language change shipping in one session with no policy). Self-correction
makes it load-bearing in a new way: once a function's implementation can
change automatically in response to a failed tier-1/2 check (§7.5), any
client with a cached contract for that function — most concretely a
mobile app built against an older UI/API manifest, per `MOBILE.md`'s own
"Standard vs Rich profiles" design — can silently break the moment an
auto-correction changes a signature or observable behavior it depended
on. Proposed, not designed here: an auto-correction should carry an
explicit signature/behavior-compatibility check against the previous
version before being accepted, at minimum for any `fn` a generated
mobile manifest references — this needs its own pass under A4, not a
sketch in this doc.

Worth naming explicitly, since it's easy to miss: **compatibility here
is broader than type-signature compatibility.** A function can keep an
identical `fn` signature while its authorization behavior, response
shape, field visibility (§5), error semantics, or nullability changes —
any of which can break a mobile client that compiles fine against the
unchanged signature but starts receiving a field as `null` because a
security gate changed, or an error where it used to get a value. A
future compatibility check under A4 needs to cover more than "does the
signature still typecheck" — not designed further here.

---

## 9. Audit trail for self-corrections — `[OPEN]`, proposed

Business-data mutations in this codebase already get a real,
tamper-evident audit trail: `finish_with_audit`
(`examples/trade-finance/trade_finance.nir:339-357`) appends one
hash-chained entry per action (`hash = sha256_hex(prev_hash, note)`,
independently verifiable by `verify_audit_chain`). A self-correcting
compiler that changes *code*, not just data — in a regulated domain
where that code is a compliance-governance rule like six-eyes routing —
is at least as audit-sensitive as the payments it routes. Proposed:
extend the same hash-chained pattern to code-generation events
themselves — what changed, which failing tier-1 proof or tier-2 test
triggered the change, and a timestamp — so "the six-eyes threshold
function was auto-modified on 2026-09-03 because a proof found a
counterexample at exactly $50,000" is itself a reviewable, tamper-evident
record, not silently overwritten source. Not designed further here —
this is a proposal for the team to scope, likely as its own doc once
§7's tier model is actually built.

---

## 10. Security decision matrix

A one-screen summary of §1-§9 — nothing here is a new claim, just a
scannable index into them.

| Layer | Question | Status | Enforcement / pointer |
|---|---|---|---|
| Identity (web) | Who is calling? | `[DONE]` — `HS256`/`RS256`/`ES256` all real as of 2026-08-26 (`ROADMAP.md` A11) | `oidc_validate_token` / `resolve_identity` (§3) |
| Identity (mobile) | Who is calling, from a native client? | `[OPEN]` | `MOBILE.md` "D2", not built (§3) |
| Function (declared gate) | If a gate is declared, is it enforced? | `[DONE]`, ceiling noted | `requires(role/claim)` + `acquire` (§4) |
| Function (default) | What happens when no gate is declared? | `[OPEN]`, **live**, no longer silent — the call still succeeds, unauthenticated, but now warns | every `fn` is routed; `requires(public)` + `ungated_fn_warnings` as of 2026-08-26 (`ROADMAP.md` A10); §4 "The default is open" |
| Backend parity | Do compiled and interpreted binaries enforce the same thing? | `[DONE]`, fail-closed | `check_supported` rejects the gated constructs (§4a) |
| Tenant | Which tenant do they belong to? | `[OPEN]` | none — §11 |
| Row | Which specific rows may they see/edit? | `[OPEN]` | proposed sketch only (§6) |
| Field (CRUD-slot fn) | Which fields on an otherwise-visible row? | `[DONE]` | `redact_gated_fields` / `check_edit_gates` (§5) |
| Field (any other fn) | Same question, non-CRUD function name | `[OPEN]`, **live** | gap 3 (§5) |
| Field (`ok`/`err` field name) | Same question, struct with an `ok`/`err` member | `[OPEN]`, **live** | gap 4 (§5) |
| Field (nested) | Same, inside a nested struct/container? | `[OPEN]` | gap 1 (§5) |
| Annotation integrity | Is a mistyped `view`/`edit`/screen slot caught? | `[OPEN]`, **live** | gap 5 (§5) |
| Aggregate | What may a derived stat/chart reveal? | `[OPEN]` | gap 2 (§5) |
| Error path | What may an `Err(...)` response reveal? | `[OPEN]` | none — §11 |
| Logs/telemetry | What may a log line or OTel span reveal? | `[OPEN]` | none — §11 |
| Contract (pure fn) | Does the implementation satisfy `post_logic` for *all* inputs? | `[DONE]` for a `routing_fn`-shaped entry (real, named, pure, loop-free, integer function); `[OPEN]` for a user story's own `pre_logic`/`post_logic` (no fn-binding field exists yet) | `contract_check::check_fn_contract`, 2026-08-26 (§7.5 tier 1) |
| Workflow (structural) | Does a real `workflow { ... }` declare exactly the states/transitions/data an extraction says it should? | `[DONE]` | `workflow_conformance::check_workflow_conformance`, 2026-08-26 (§7.5, sibling construct) |
| Contract (effectful fn, unit) | Does one function satisfy `post_logic` for *tested* cases? | `[OPEN]` | proposed tier 2a (§7.5), precedent `nir_scenario!` |
| Contract (acceptance criterion, scenario) | Does the end-to-end call sequence satisfy the story's Given/When/Then? | `[OPEN]` | proposed tier 2b (§7.5) — no precedent yet, see §7.1a |
| Contract (cross-artifact) | Do `post_logic` and `acceptance_criteria` even agree with each other? | `[OPEN]` | proposed tier 3(E) (§7.5), §7.1a |
| Security regression | Did a repair weaken an authorization boundary? | `[OPEN]` | proposed invariant (§7.2, §7.3) |
| Compatibility | Did a repair change behavior a client depends on? | `[OPEN]` | `ROADMAP.md` A4 (§8) |
| Provenance | Why/when did generated code change? | `[OPEN]` | proposed (§9) |

---

## 11. Non-goals

Named here so a future reader doesn't assume any of the following were
promised by this document. Each is a real, legitimate concern raised
during review; each is deliberately **not** designed here, because doing
so would spec out infrastructure this codebase doesn't have for a
feature (self-correction) that itself doesn't exist yet beyond one
hand-written test (§7.4).

- **Authentication, user management, and IdP provisioning.** Entirely
  external by design (§3) — Nirdosha consumes tokens rather than
  managing users, and that's not revisited here. Note this was *not* the
  same as "cannot mint them" until 2026-08-26: §3 used to show the
  symmetric-only JWKS design put the IdP's signing key in the server's
  hands whenever `mock_issue_token` was used, and asymmetric
  verification (`ROADMAP.md` A11) is what made the non-goal
  cryptographically true for an `RS256`/`ES256` deployment — `[DONE]`,
  no longer `[OPEN]`, though a symmetric-JWKS deployment still carries
  the original caveat by choice (§3, T1a).
- **Row-level authorization, implemented.** §6 sketches the shape of the
  problem; it does not resolve "owner of what, against what," and no
  implementation exists.
- **Tenant isolation, implemented.** Distinct from row-level ownership
  (§6) — a caller could legitimately own a row in one tenant while
  still needing to be blocked from a different tenant's data entirely.
  Real, per the trade-finance PRD's own "per-tenant" language, but
  today's architecture is explicitly single-IdP-per-server
  (`ROADMAP.md:525`, Track A6's open "Multi-IdP registry" item) —
  building real tenant isolation is a separate, large undertaking
  (schema, connection routing, IdP-per-tenant) that deserves its own
  tracked `ROADMAP.md` item, not a subsection of this doc.
- **Aggregate/inference disclosure as full information-flow control.**
  §5's gap 2 proposes a narrow, field-provenance-tracking fix scoped to
  `stat_`/`chart_` functions specifically. A general information-flow
  type system — classifying every derived value by the sensitivity of
  what it was computed from — is a research-scale initiative on its
  own, not scoped here.
- **Error-path masking.** `redact_gated_fields` returns immediately,
  with no redaction applied at all, the moment a response object
  contains an `"err"` key (`compiler/src/serve.rs:994-996`), so today's
  masking guarantee (§5, §1) covers only the `Ok(...)` path.

  **Severity, re-checked against real error payloads for this
  revision** — the earlier draft called this "a real, currently open,
  unaddressed leak," which overstates one half and understates another.
  Splitting them:

  - *As a field-RBAC bypass, it is latent, not live.* An `Err` payload
    would have to be an object whose member names collide with a
    `screen`-gated field name. The shapes actually in use across this
    repo's examples are (a) `Result(_, ErrorCode)` — 228 `Result(i64,
    ErrorCode)` plus 141 `Result(json, ErrorCode)` — with
    **zero-payload** variants, which `encode_value` renders as
    `{"err":{"variant":"DbError","payload":[]}}`
    (`compiler/src/serve.rs:1455-1473`), and (b) `Result(_, Text)`,
    rendering as `{"err":{"value":"..."}}`. Neither carries a plausible
    gated field name. `examples/trade-finance/trade_finance.nir:41-56`
    is explicit that it maps raw SQL driver text down to a bare
    `DbError` tag rather than forwarding it. So no gated field leaks
    through this path in any code shipped here today; it bites the first
    time someone returns a richer error struct. Keeping it a non-goal is
    the right call **for this specific mechanism**.
  - *The same code block's `"ok"` branch, however, is a live bypass* —
    an ordinary struct with a field named `ok` or `err` triggers the
    envelope test and skips redaction entirely. That is escalated out of
    this non-goal list and into §5 as **gap 4**, because it is reachable
    today without anyone writing an unusual error type.
  - *And the error channel does leak server internals, just not via this
    function.* Two live paths, neither addressed anywhere: an
    interpreter trap returns its raw message straight to the client
    (`(500, json_err(&e.to_string()))`, `compiler/src/serve.rs:1192`);
    and `LANGUAGE.md` §6b (~line 316-329) explicitly *blesses*
    `enum ErrorCode { NotFound, External(str) }` as the pattern for
    "forward an unpredictable builtin failure message" — i.e. raw
    driver/SQL/HTTP text — "uniformly through one `Result(_,
    ErrorCode)`." Any app that takes that advice ships driver internals
    to unauthenticated callers (§4). Still `[OPEN]`, still not designed
    here, but it is a real disclosure channel with an active
    recommendation pointing at it, not a hypothetical.
- **Log/telemetry-channel masking.** Response-level redaction (§5) says
  nothing about what `serve.rs` or the interpreter might write to a log
  line or the OTel span stream (`observability.rs`, `ROADMAP.md` Track
  A3) — a separate channel with no visibility policy of its own.
  Response visibility, log visibility, audit visibility, and telemetry
  visibility are four different channels; this document only addresses
  the first.
- **Arbitrary theorem proving.** §7.5's tier-1 static proof stays scoped
  to what `smt.rs`'s Z3 already proves today — integer range,
  divide-by-zero and index-bounds arithmetic, extended to simple linear
  postcondition predicates. (Not `refine.rs`, which uses no solver at
  all — see §7.4.) General-purpose program verification is not the goal.
- **Automatic correction of ambiguous specifications.** The opposite of
  the goal — §7.5's tier 3 exists specifically to detect this case and
  stop, not resolve it.
- **Automatic deployment of generated corrections.** No deployment
  pipeline exists in this codebase at all today
  (`PROTOBOX_INTEGRATION.md` §7: deployment is "copy the folder, run
  `run.sh`"). A correction reaching a running server without an explicit
  human step is out of scope for anything proposed in §7-§9.
- **A single authoritative "authorization IR" compiler layer.** Today's
  enforcement is split across `typeck`, `ui_gen.rs`'s UI inference, and
  `serve.rs`'s runtime checks. Worth watching for drift if that split
  ever causes two layers to disagree about who can see or do what — but
  replacing it with a new compiler IR is a compiler-architecture change,
  out of scope for a trust-model document.
- **Reproducible code provenance** (a full hash-chained record of
  exactly which specification, compiler version, model/prompt version,
  and proof/test evidence produced a given auto-correction). §9 proposes
  the simpler, narrower version of this — that a correction happened,
  why, and when — not a fully reproducible build/provenance pipeline.
  Real future value once §7's tiers have a working prototype; not
  designed here.

---

## References

- `LANGUAGE.md` §5 (Identity/relying party, ~line 108-166), §6a
  (Privileged first-class functions, ~line 211-234), §11 (`screen`/
  `dashboard`, field RBAC, ~line 569-690), §11a (Role-mapping cache,
  ~line 692-730) — §3-§5 above.
- `compiler/src/serve.rs:832-855` (`resolve_identity`, expiry check) —
  §2/§3's identity-validity claims.
- `compiler/src/serve.rs:964-1011` (`redact_gated_fields`) — §5's
  shipped masking, gaps 1 and 4, and the `Err(...)`-path mechanism gap
  re-assessed in §11.
- `compiler/src/serve.rs:1030` (route lookup over `program.fns`),
  `:1050-1063` (`requires` enforcement), `:1129-1136`
  (`VerifiedIdentity` 401) — §4's default-open finding, runtime
  unchanged by the 2026-08-26 fix.
- `compiler/src/typeck.rs` (`ungated_fn_warnings`, `TypeWarning`,
  `TypeWarningKind::UngatedFnReachableWithNoToken`),
  `compiler/src/ast.rs` (`FnDecl::explicit_public`),
  `compiler/src/parser.rs::parse_requires_annotation` (`public` arm),
  `compiler/src/main.rs::print_ungated_fn_warnings` — §4's
  `requires(public)` + typeck-warning fix (`ROADMAP.md` A10).
- `compiler/tests/ungated_fn_warning.rs` — the warning fires/is silenced
  in exactly the cases §4 describes, and `requires(public)` doesn't gate
  a direct call — §4.
- `compiler/src/contract_check.rs` (`check_fn_contract`, `ContractCheckResult`,
  `Eval`), `compiler/src/parser.rs::parse_standalone_expr` — §7.5 tier 1's
  real obligation channel.
- `compiler/src/workflow_conformance.rs` (`check_workflow_conformance`,
  `ConformanceReport`, `Mismatch`), `compiler/src/extraction_schema.rs`
  (`ExtractionFile` and friends — the typed mirror of `scratch/
  extracted_typed_v1.json`'s own shape) — §7.5's sibling structural
  construct.
- `compiler/tests/extracted_typed_v1_verification.rs` — both constructs
  run end-to-end against the real, checked-in
  `scratch/extracted_typed_v1.json`, not a synthetic fixture: all three
  `workflows[]` entries conformance-checked (plus two tests that mutate
  the real `.nir` source and confirm the specific mismatch is caught),
  and `WF-TRDPAY-001.routing_fn`'s Hoare pair proved/counterexampled
  against the real `required_eyes_for_amount` — §7.5.
- `compiler/src/serve.rs:1186-1187` (`dispatch`'s gate lookup) and
  `compiler/src/ui_gen.rs:685-713` (`field_gates_for_fn`'s four-CRUD-slot
  match) — §5's gap 3.
- `compiler/src/typeck.rs:1204-1212`, `:1233-1241` (`check_screen`'s
  `_ => {}` fallthroughs), `:1087-1100` (`check_visibility_expr`),
  `compiler/src/ui_gen.rs:642-654` (`gates_from_screen_decl`) — §5's
  gap 5 and §6's Option B verdict.
- `compiler/src/interpreter.rs` (`validate_oidc_token`, `jwks_key`,
  `verify_jwt_signature`, `JwksKeyMaterial`, `hmac_sha256_base64url`) and
  `mock_issue_token` — §3's `RS256`/`ES256` fix (`ROADMAP.md` A11) and
  the now-conditional T1a.
- `compiler/tests/oidc_jwt_algorithms.rs` — real RSA/EC key material
  round-tripped through `oidc_validate_token`, plus the algorithm-
  confusion rejection test — §3.
- `compiler/src/interpreter.rs:1289,2539` (`identity_claim`,
  `extract_claim` handler) — §6's claim-trust-boundary rule.
- `compiler/src/parser.rs:373-378` (`parse_kv_entry` = `parse_expr`),
  `:783-786` (fn annotation slot order), `:850-876`
  (`parse_requires_annotation`'s identifier-text `match`), `:806-826`
  (effect vocabulary), `compiler/src/token.rs:73,379-390` (reserved
  keywords) — §6's and §7.6's grammar verdicts.
- `compiler/src/ast.rs:439-454`, `:1531-1549` (`is_affine`, including
  transitive struct/enum affinity), `:793-805` (`Effect`) — §7.6's
  ownership constraint, §7.5's effect-vocabulary note.
- `compiler/src/refine.rs:1-20` (interval analysis, explicitly **no**
  SMT solver) vs. `compiler/src/smt.rs:1-51,61-73,424`
  (Z3, three proof targets, integers only, no interprocedural
  summaries) — §7.4's corrected attribution and §7.5's tier-1 caveats.
- `compiler/src/codegen.rs:1-60` (module doc, Tier-1/2 proof discipline;
  `:36-41` the unsupported list), `:301-303,365-367,711,739,747,789`
  (`check_supported` rejections) — §7.4's static-proof infrastructure
  and §4a's backend-parity claim.
- `compiler/tests/trade_finance_governance_routing.rs` — the
  boundary-operator/threshold drift §7.1 is built on, and the `nir_scenario!`
  precedent §7.5's tier 2 proposes extending.
- `examples/trade-finance/trade_finance.nir:339-357` (`finish_with_audit`),
  `:1728-1735` (`required_eyes_for_amount`) — §7.1's worked example, §9's
  audit-chain precedent.
- `examples/trade-finance/trade_finance.nir:41-56` (`enum ErrorCode`,
  zero-payload by deliberate choice), `:689` (`decide_approval_via_link`,
  the unauthenticated magic-link carve-out), `:1009-1014`
  (`screen Counterparty`, the only field gate in the app) — §1's
  carve-out, §4's count, §5's gap 3, §11's error-payload evidence.
- `LANGUAGE.md` §6b (~line 316-329, `ErrorCode { External(str) }` as a
  blessed forwarding pattern), §8 (static guarantees and the `audited`
  escape hatch), §14 (~line 903, `link`-marked transitions are
  unauthenticated by design) — §11's error-channel finding, §7.2's
  `audited` addition, §1's carve-out.
- `scratch/hoare_userstory_prompt.py` (rule 11), `scratch/
  extracted_userstories_v2.json` (`US-TRDPAY-002`) — the `pre_logic`/
  `post_logic` extraction pipeline §7.6 proposes wiring into the
  language itself.
- `MOBILE.md` (~line 205-260, "D2" biometric step-up) — §3's mobile
  identity gap, authoritative source, not duplicated here.
- `ROADMAP.md` Track A4 (compatibility/versioning), A6 (role mapping
  `[DONE]`; multi-IdP registry `[OPEN]`, `ROADMAP.md:525`, cited in
  §11's tenant-isolation non-goal), A9 (threshold/boundary/currency
  drift, `[OPEN]`) — cited throughout §3, §7, §8, §11.
- `PROTOBOX_INTEGRATION.md` §7 (deployment handoff, "copy the folder,
  run `run.sh`" — cited in §11's automatic-deployment non-goal), §9
  (open gaps) — the existing cross-reference to A9 that this doc's §7
  expands on.
- `../protobox/be-v2/src/plugins/languages/nirdosha_direct_codegen.py:179,226`
  (`max_repairs=3`) — §7.4's existing repair-loop precedent.
- `git show 64f286f` — the ownership-example commit checked for §4/§6;
  confirmed to cover affine-type move-checking only, no row-level
  data-access pattern.
