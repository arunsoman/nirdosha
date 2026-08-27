# Why nirdosha for Kubernetes — the case, not just the gap list

**Status: positioning document, checked against real source on
2026-08-27.** Companion to `KUBERNETES.md` (the compliance-matrix/gap
assessment) — that doc answers "what's missing before this runs
natively on k8s"; this one answers "once it does, why is nirdosha the
better choice of *language* for what you're deploying, not just an
acceptable one." Same discipline as every other doc in this repo: every
claim below is checked against real source (file:line) or an actual
test run, and every advantage is stated with its honest scope, not
oversold. Where a property is narrower than the ideal, that's said
plainly, the same way `PHASE0.md`'s own "Twelfth update" discloses the
`recv`-hang gap in the deadlock-freedom claim rather than hiding it.

## Ready to use as-is, today

Before the comparative case: these are standing capabilities, already
shipped and verified, not designs. A protobox-generated (or hand-written)
`.nir` app gets all of them with zero extra code, zero extra pods, and
zero third-party services.

| Capability | What you get for free | Where it lives |
|---|---|---|
| **Workflow audit trail** | Declare a `workflow { state ... }` block and `get_<workflow>_history(...)` is auto-synthesized: every transition logged with `actor_subject` (who), a timestamp, `comment` (free-text why, prompted for in the generated UI), and `via_link` (flags an unauthenticated magic-link action vs. an authenticated one). Generated UI ships a "History" expander per row. | `WORKFLOW.md` §7 — shipped, Track A13 |
| **Durable transactions** | `transact` blocks survive a real `SIGKILL` mid-flight with zero lost or double-applied writes — proven with a live kill test under concurrent load, not just claimed. | `TRANSACT.md`, `ROADMAP.md` Track A1 |
| **Identity & sessions** | OIDC token validation (RS256/ES256, plus HS256), session/refresh-token issuance, real revocation checking — all server-side, no separate auth microservice. | `interpreter.rs` Row 12, `nirdosha_row12_functions_identity.md` |
| **RBAC, enforced not just declared** | `requires(role/claim:...)` on any `fn`, plus field-level `view`/`edit` gates on generated screens — checked by the type checker *and* enforced at request dispatch. | `typeck.rs::check_visibility_expr`, `serve.rs` |
| **Role-mapping admin console** | Live-editable, TTL-cached `app_role ↔ idp_role` translation table, so a renamed IdP group doesn't silently break every `requires(role:...)` check. | `ROADMAP.md` Track A6 |
| **Notifications** | `send_email`/`send_sms`/`send_push`, plus an admin-editable provider-config panel (`EmailProviderConfig`) that `nirdosha init` scaffolds as a standing fixture. | `WORKFLOW.md` §`send_email` section |
| **Whole UI, generated** | Auto-derived from `struct` + CRUD-fn naming convention — no hand-written frontend file is ever a valid task output. | `ui_gen.rs` |

## Top 5 reasons, with the comparison

### 1. Built for the exact failure mode Kubernetes guarantees will happen

Pods get killed without warning — preemption, node drain, OOM, rolling
deploys, HPA scale-down. `transact`'s durability log was tested against
that directly: 12 client threads driving 240 `transact`-wrapped
requests at a live `nirdosha serve` process, `SIGKILL`ed twice mid-flight
across two restart-and-reload cycles — zero lost writes, zero
double-applies, confirmed against the real business side-effect table,
not just the log (`ROADMAP.md` Track A1, 2026-08-26). In Go/Java/Node/
Python you get this by hand-rolling a saga/outbox pattern per service,
or by standing up Temporal as its own stateful cluster dependency.
Here it's one keyword and it's already been kill-tested.

### 2. A workflow engine and a regulator-grade audit trail live inside the app, not as another stateful pod

The "who approved what, when, and why" ask — SOX/banking-audit
territory — normally means Temporal, Camunda, or AWS Step Functions:
another Deployment, another database, another thing to secure, scale,
and patch. In nirdosha it's a `workflow` block in the same file, and
`get_<workflow>_history` ships automatically, generated-UI history view
included. Every dependency you don't have to run in the cluster is one
less thing that needs its own Service, NetworkPolicy, PDB, and on-call
runbook.

### 3. One process is the whole application — the smallest deployment footprint per app

UI and API are the same static binary on the same port — `GET /` and
`POST /api/<fn>` share one `tiny_http` listener (`serve.rs`). Every
other stack's "one CRUD app" costs at minimum two Deployments
(frontend + backend), two images, two CI build pipelines, and CORS/
API-base-URL configuration between them. Nirdosha needs one container,
no Node/npm toolchain baked into the image, no JVM warm-up — a smaller
image and a faster cold start, which lands directly on HPA scale-out
latency, not just on developer convenience.

### 4. Authorization is a compiled, type-checked language feature — not middleware someone can forget to attach

`requires(role/claim:...)` and field-level `view`/`edit` gates live in
the same source file as the logic they guard, and are enforced at
dispatch (`serve.rs`), not bolted on per-route via a framework's
middleware chain. The single most common real-world bug class in a
multi-service Kubernetes cluster — "someone forgot to attach the auth
middleware on this one route" — is structurally harder to produce here,
because there's no separate middleware layer to forget in the first
place.

### 5. Compile-time-proven memory and overflow safety, no GC, with a real (if narrower-than-ideal) deadlock story — at native speed

A static move-checker rules out data races (`ownership.rs`); there is
no mutex primitive in the language at all, so the classic "two locks
acquired in opposite order" deadlock is not *expressible*, not merely
discouraged; a real Z3 solver discharges integer-overflow/bounds proofs
at compile time (`smt.rs`, 68/68 tests, including a case interval
analysis alone cannot prove). No GC means no stop-the-world pause
skewing p99 latency exactly when an HPA is watching it. **Honest
caveat, stated the way this project states its own limits**: this
closes off *lock-order* deadlocks specifically — a `recv` with no
matching `send` can still hang forever, a disclosed liveness gap, not
a hidden one (`PHASE0.md`'s "Twelfth update"). "No lock-order
deadlocks by construction" is the accurate claim; "deadlock-proof,
full stop" is not.

## Side-by-side

| Axis | Nirdosha | Go | Java / JVM | Node.js | Python |
|---|---|---|---|---|---|
| Crash-safety of a multi-step operation under `SIGKILL` | `transact` — built-in, durability-logged, kill-tested live | Hand-rolled saga/outbox pattern, or an external orchestrator | Same — Spring/Camunda as a bolt-on | Same — a queue + idempotency keys, hand-built | Same, hand-built |
| Durable workflow + audit trail | `workflow` block — audit trail auto-generated | External: Temporal, Camunda, Step Functions | Same externals | Same externals | Same externals |
| Deployment footprint for one UI+API app | 1 static binary, 1 process, 1 port | Backend binary + a separate frontend build/Deployment | JVM image + separate frontend | Node process + separate frontend (or SSR framework complexity) | WSGI/ASGI process + separate frontend |
| Authorization enforcement | Compiled, type-checked, enforced at dispatch | Middleware (`net/http` chain, gin/echo) — per-route, easy to omit | Spring Security — configuration-heavy, easy to misconfigure | Middleware (Express/Koa) — per-route | Middleware/decorators (Django/FastAPI) — per-route |
| Memory safety / GC pauses / overflow proofs | No GC (ownership-checked), Z3-proven overflow bounds, lock-order deadlocks inexpressible | GC pauses; no overflow proofs; real mutexes, real deadlocks possible | GC pauses (typically worse tail latency); no overflow proofs | Single-threaded event loop — different tradeoffs, no memory-safety proofs, easy to block the loop | GC + GIL; no compile-time safety of any kind |

## What this document is not claiming

Everything in `KUBERNETES.md` still stands: there is no published
container image, no `/healthz`/`/readyz`, no SIGTERM handling, and
`serve --db`'s table-browser/role-cache layer has no Postgres option —
none of the five advantages above are an argument that nirdosha is
*already* running well on Kubernetes. They're the argument for *why it's
worth finishing that work* rather than defaulting to a mainstream
stack that would need to rebuild items 1, 2, and 4 above from scratch,
and can't obtain item 5 at all.
