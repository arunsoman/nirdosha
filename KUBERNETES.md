# Nirdosha on Kubernetes — compliance matrix and remediation plan

**Status: assessment only, checked against real source on 2026-08-27.**
This is not a design doc for a new feature — it's an audit of what
`nirdosha serve` (and protobox's use of it, see
`PROTOBOX_INTEGRATION.md`) would need to be a fully native, idiomatic
Kubernetes workload: containerized, horizontally scalable, observable
by the platform, and safe under the orchestrator's own lifecycle
(rolling restarts, node drains, autoscaling). Referenced from
`ROADMAP.md` Track A2 ("Deployment story for the interpreted path") and
`PROTOBOX_INTEGRATION.md` §7/§9 — this file is the detail those items
point at, so it isn't duplicated inline there. For the *positive* case —
why nirdosha over a mainstream language once this work lands, not just
what's missing today — see `KUBERNETES_ADVANTAGE.md`.

Same discipline as `ROADMAP.md`'s own standards table: every
`[DONE]`/`[PARTIAL]`/`[OPEN]`/`[N/A]` row below was checked against real
source (file:line) or an actual grep/test run, not assumed from a doc
comment. `[N/A]` means "correctly delegated to the platform/operator,
not a gap this codebase should close" — e.g. TLS termination belongs at
an Ingress/mesh, not in `tiny_http`.

## The one question that actually matters: what happens at >1 replica?

`nirdosha serve` fuses UI and API into **one process** — it answers
`GET /` (the `emit-ui`-derived UI) and `POST /api/<fn>` on the same
port (`serve.rs`). For Kubernetes that's a structural simplification,
not a complication: one container, one Deployment, one Service, one
Ingress path. There is no separate frontend pod to keep in sync, no
CORS configuration, and no API-base-URL wiring between two
deployments — the generated UI's `fetch()` calls default to
same-origin `/api` (`ui_gen.rs`'s own doc comment). Login/session
validation is stateless per-request JWT/OIDC verification
(`interpreter.rs`'s `validate_oidc_token`), so ordinary round-robin
load balancing across replicas needs no sticky sessions for auth.

Where replica count actually bites is **data**, and it's two distinct
stores with two different answers:

1. **The program's own business data** — whatever a `struct`'s
   `list_/get_/create_/update_/delete_` functions read and write via
   `db_connect(...)`. `dbconn.rs`'s `DbConn::{Postgres, PostgresTls}`
   already gives this a real, pooled, TLS-optional Postgres backend —
   solvable *today* at the runtime level. It isn't wired up on
   protobox's side yet: `gen-crud`/`nirdosha_screen_plan.py` always
   emit a local `<project_id>.db` literal, never a `postgres://` one.
2. **The UI's own housekeeping layer** — the generic paginated/
   filterable table view and the role-mapping cache that
   `nirdosha serve --db <path>` backs. `serve.rs:218` opens this with a
   bare `rusqlite::Connection::open(p)` directly, bypassing
   `dbconn.rs`'s backend abstraction entirely. There is no Postgres
   option for this layer at all. This is the one piece that stays
   single-instance no matter what gets fixed elsewhere — a real gap in
   this repo, not a protobox config gap.

So: **one replica** (a `StatefulSet` + one `PersistentVolumeClaim` for
the `.db` file) already works with nothing more than a container image
and two HTTP routes added (below). **Multiple replicas behind one
Service** need the P1 items in the remediation order to be correct,
not just schedulable.

## Compliance matrix

### Container & image

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| OCI-compliant image | `[OPEN]` | protobox's `plugins/languages/nirdosha.py` already names the target, `ghcr.io/protobox/nirdosha-runtime:latest` — unpublished. | Build once; bake in the `nirdosha` binary + `python3`/`pytest`/`requests` (needed for protobox's black-box QA tasks). Already named as a blocker in that plugin's own module docstring. |
| Multi-arch (amd64/arm64) | `[PARTIAL]` | `.github/workflows/release.yml` already cross-compiles native binaries for 4 targets (`x86_64-unknown-linux-gnu`, `x86_64-apple-darwin`, `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`) on every `v*` tag. | Wrap the linux leg(s) into a `docker buildx` multi-platform manifest — the hard part (cross-compiling) is already solved. |
| Non-root user, read-only rootfs, dropped capabilities | `[OPEN]` | No Dockerfile exists to assess. | Trivial once the image exists: static binary, non-root UID, writable volume mounted only where the `.db`/log files live. |
| SBOM / image signing (Sigstore, SLSA) | `[OPEN]` | No signing step in `release.yml`. | Add `cosign sign` + `syft` SBOM generation to the release workflow once images are published. |

### Configuration (12-factor)

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Config externalized from code | `[PARTIAL]` | `main.rs::cmd_serve`'s flag parser: `--host --port --db --jwks-file --issuer --audience --presence-token --otel-port --otel-token` are all CLI flags. Zero app-level `std::env::var` reads for any of these (only `NIRDOSHA_TEST_*` and `NIRDOSHA_{prefix}_POOL_*` tuning vars exist, `pool.rs`). | CLI args are workable in a Pod spec's `command`/`args` (env-substitutable via `$(VAR)`), but not idiomatic. Lower priority than the items below. |
| Secrets never baked into the image | `[DONE]` | Every credential is a runtime flag/file path; none hardcoded. | — |
| Secrets sourced from files, not bare CLI values | `[PARTIAL]` | `--jwks-file` already takes a path (Secret-volume-friendly, and the identity trust anchor is exactly the kind of thing that shouldn't be a bare arg). `--presence-token`/`--otel-token` take the raw value directly on the command line — visible via `/proc/<pid>/cmdline` inside the container. | Add `--presence-token-file`/`--otel-token-file`, mirroring the `--jwks-file` precedent already in this codebase. |
| Business DB connection is environment-specific | `[OPEN]` | `.nir` has no env-var-read builtin (zero matches for `env_var`/`env_get` anywhere in `interpreter.rs`/`runtime_kernels.rs`) — a program's `db_connect("…")` literal is compiled into the source text itself, not resolved at runtime. | Protobox-side: generate the literal per deploy target (a local sqlite path in dev, `postgres://…` in prod) at `assemble()` time, instead of always emitting `<project_id>.db`. |

### Health & lifecycle

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Liveness probe route | `[OPEN]` | `serve.rs` routes are `GET /`, `POST /api/<fn>`, `_presence_connect`/`_presence_disconnect`. No `/healthz`. | Add a route that answers `200` the instant the listener is bound — no dependency check needed for liveness specifically. |
| Readiness probe route | `[OPEN]` | Same — no route checks DB/durability-log connectivity before answering. | Add `/readyz`: fail while startup migration/crash-replay is still in progress. |
| Startup probe (slow boot) | `[OPEN]` | Startup runs schema migration (`migrate.rs`) and durability-log crash replay (`workflow_log.rs`/`transact_log.rs`) before the listener binds — not instant under a large log. | Same route as readiness, with a longer `failureThreshold`, works as a startup probe. |
| Graceful SIGTERM handling | `[PARTIAL]` | Zero signal-handling code anywhere in `main.rs`/`serve.rs` (confirmed by grep) — a real gap. **But**: `tests/transact_process_kill.rs` already `SIGKILL`s a live `nirdosha serve` process mid-transaction under real concurrent HTTP load and verifies zero lost/double-applied writes via crash replay (`ROADMAP.md` Track A1, done 2026-08-26, run across repeated restart cycles). | A default 30s `terminationGracePeriodSeconds` SIGKILL is **correctness-safe today** — durability already survives it. What's missing is draining in-flight HTTP requests before exit, not data safety. |

### State, data & horizontal scaling

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Same-host double-start protection | `[DONE]` | `instance_lock.rs` — OS-level exclusive SQLite lock on a sidecar file, held for the process's whole lifetime. Verified live: two real processes pointed at the same log file, the second refuses to start. | Covers rolling-restart overlap on one host only, not real multi-replica — by design (see the module's own doc comment). |
| `workflow`/`transact` durability across replicas | `[DONE]` | `ROADMAP.md` Track A17, `durability.rs`: `--transact-log`/`--workflow-log` accept a `postgres://`/`postgresql://` value (`LogTarget`, `pool.rs`'s `PoolRegistry`). Verified live against a real Postgres server: two independent `WorkflowLog`/`TransactLog` handles sharing one database never collide on `instance_id` and do see each other's `txn_id` rows; two real `serve` processes on the same database both serve concurrent requests correctly. | Protobox deploy config: point both flags at Postgres for any project that will run >1 replica. No further runtime work needed. |
| Business/CRUD data (`db_connect`) on shared storage | `[PARTIAL]` | `dbconn.rs`: `DbConn::{Postgres, PostgresTls}`, pooled (`POSTGRES_POOLS`), TLS-optional — the runtime capability is real and shipped. protobox's `gen-crud`/`nirdosha_screen_plan.py` always emit a local `<project_id>.db` literal today (see `PROTOBOX_INTEGRATION.md` §8's first gotcha for the related `db_connect` vs `--db` mismatch trap). | Protobox-side codegen change only — no nirdosha runtime work needed. |
| UI's generic table browser + role-mapping cache | `[OPEN]` | `serve.rs:218` — `--db <path>` opens a bare `rusqlite::Connection` directly, bypassing `dbconn.rs`'s backend abstraction entirely (`table_db: Option<Arc<Mutex<rusqlite::Connection>>>`). No Postgres branch exists for this layer; also backs `RoleMappingCache`. | Genuine nirdosha-level gap. Either extend this route to go through `dbconn.rs`'s `DbConn` enum, or explicitly scope multi-replica deployments to not rely on this feature until it does. |
| Cross-machine SQLite replication (no Postgres) | `[N/A]` | `ROADMAP.md` Track A17 Phase 2 names rqlite/dqlite (Raft) and cr-sqlite (CRDT) as real prior art, deliberately not adopted — Postgres already solves the actual need. | Revisit only if a deployment specifically must avoid a Postgres dependency. |
| Single-replica deployment shape (`StatefulSet` + PVC) | `[OPEN]` | No Kubernetes manifests exist anywhere in this repo or protobox's. | Correct shape for the default SQLite mode; needs writing, not new runtime behavior. |

### Networking

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| One Service/Ingress for UI + API | `[DONE]`, by design | `serve.rs` answers `GET /` and `POST /api/<fn>` on the same listener; generated UI's `fetch()` defaults to same-origin `/api` (`ui_gen.rs` module doc). | This is the fused-process advantage — no separate frontend Ingress/CORS config, ever. |
| TLS termination for `serve` itself | `[N/A]` | `serve.rs:267` calls `tiny_http::Server::http(...)` — plain HTTP only, confirmed. | Correctly delegated to an Ingress/mesh, same posture already documented ("production HTTPS needs a reverse proxy in front"). |
| mTLS between services | `[OPEN]` | Confirmed absent repo-wide; also the FAPI blocker per `compiler/UI_DSL_TODO.md:353-357`. | Delegate to a service-mesh sidecar (Istio/Linkerd) rather than building it into nirdosha. |
| Rate limiting | `[OPEN]` | Zero matches in `serve.rs` — already named as a real gap in `ROADMAP.md`'s OWASP Top 10 compliance row. | Push to an Ingress annotation/mesh policy as the near-term mitigation. |

### Observability

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Logs to stdout/stderr | `[PARTIAL]` | Operational lines are plain-text `eprintln!` calls; `--format=json` exists only for the interpreter's own diagnostic/error output, not `serve`'s lifecycle log. | Fine for `kubectl logs`; not structured for a log pipeline (Loki/ELK) without a regex parser. |
| Prometheus `/metrics` | `[OPEN]` | No metrics crate, no route. | Net-new — request counts/latencies from `serve.rs`'s dispatch loop would be the natural first cut. |
| Distributed tracing (OpenTelemetry) | `[PARTIAL]` | `observability.rs`: real zero-cost-when-disabled local tracer; Layer 2a (`--otel-port`/`--otel-token`, opt-in loopback JSON stream) built and tested (`tests/observability_layer2a.rs`). | Layer 2b (real OTLP export to an actual collector) is the named next step in `observability.rs`'s own doc comment — not started. |

### Security posture (Pod-level)

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Pod Security Standards ("restricted" profile) | `[OPEN]` | Blocked on the container image not existing. | Sequenced after container-image work — nothing to assess independently yet. |
| NetworkPolicy compatibility | `[N/A]` | Standard Kubernetes object; nothing app-side needs to change. | — |

### Deployment manifests & GitOps

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Dockerfile | `[OPEN]` | None in this repo or protobox's. | P0. |
| Helm chart / Kustomize overlay | `[OPEN]` | None. | A new `plugins/deploy_targets/kubernetes.py` in protobox's be-v2, matching the existing Render/Vercel plugin pattern (`nirdosha-default-pipeline-plan.md` Phase 8). |
| CI already produces the binaries an image needs | `[DONE]` | `.github/workflows/release.yml` cross-compiles linux/macOS/Windows × amd64/arm64 on every `v*` tag. | Reuse the linux leg directly as the image's `COPY` source — no in-image compile step needed. |

## Remediation order

Real dependency order, not a priority wishlist — each phase is a
prerequisite for the next to matter.

**P0 — make it schedulable at all.** Nothing below matters until
there's an image and a way for the orchestrator to know if it's alive.
- Publish `ghcr.io/protobox/nirdosha-runtime` — wrap `release.yml`'s
  linux binary + `python3`/`pytest`/`requests`. Already named as a
  blocker in protobox's `nirdosha.py` module docstring.
- Add `/healthz` and `/readyz` to `serve.rs`.
- Add SIGTERM handling: stop accepting new connections, drain
  in-flight requests, then exit. Durability is already safe without
  this (Track A1); this closes the "politeness" gap, not a
  correctness one.

**P1 — make more than one replica correct.** This is the phase that
answers "how will the UI and backend work" once you scale past one
pod.
- Point `--transact-log`/`--workflow-log` at Postgres for any project
  that will run >1 replica — capability already shipped (Track A17),
  this is deploy-config wiring only.
- Change protobox's `gen-crud`/`nirdosha_screen_plan.py` to emit an
  environment-appropriate `db_connect(...)` literal instead of always
  hardcoding a local `.db` path.
- Decide (a product call, not just an engineering one): either extend
  `serve.rs`'s `--db` table/role-cache layer to accept Postgres, or
  explicitly scope multi-replica deployments to not use that feature
  until it does.

**P2 — production hardening.** Makes it observable and defensible in
an incident, not just running.
- Prometheus `/metrics` endpoint.
- OTel Layer 2b — real OTLP export to a collector.
- Structured JSON operational logs (today's diagnostic `--format=json`
  doesn't cover the `serve` lifecycle log).
- `--presence-token-file`/`--otel-token-file`, mirroring the existing
  `--jwks-file` convention.
- Rate limiting, at the Ingress/mesh layer if not in-app.

**P3 — platform polish.** The parts that make it feel native, not
just deployable.
- Helm chart / Kustomize overlay as a new protobox
  `plugins/deploy_targets/kubernetes.py`, following the Render/Vercel
  pattern already established there.
- Pod Security "restricted" profile: non-root UID, read-only rootfs,
  dropped capabilities.
- mTLS via service-mesh sidecar — deliberately not built into
  nirdosha itself, consistent with how TLS-for-`serve` is already
  deferred to a reverse proxy.

## Sources checked

`compiler/src/{main,serve,dbconn,durability,instance_lock,
observability,pool}.rs`, `ROADMAP.md` (Track A items A1/A2/A17, the
Standards & compliance matrix section), `.github/workflows/{build,
release}.yml`, `PROTOBOX_INTEGRATION.md` §§7-9, and protobox's
`be-v2/src/plugins/languages/nirdosha.py` and
`be-v2/docs/plans/nirdosha-default-pipeline-plan.md` (Phase 8). General
Kubernetes production-readiness conventions (probes, graceful
termination, 12-factor config) cross-checked against current published
guidance, not assumed from memory alone.
