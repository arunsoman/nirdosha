# Nirdosha on Kubernetes — compliance matrix and remediation plan

**Status: P0 and most of P2/P3 implemented and verified 2026-08-27**
(same day as the original assessment — see the remediation-order section
below for exactly what shipped, what's still open, and the real evidence
for each). Originally an audit-only doc; every row below has been
re-checked against the code that now exists, not left as it was written
before implementation. This describes what `nirdosha serve` (and
protobox's use of it, see `PROTOBOX_INTEGRATION.md`) needs to be a fully
native, idiomatic Kubernetes workload: containerized, horizontally
scalable, observable by the platform, and safe under the orchestrator's
own lifecycle (rolling restarts, node drains, autoscaling). Referenced
from `ROADMAP.md` Track A2 ("Deployment story for the interpreted path")
and `PROTOBOX_INTEGRATION.md` §7/§9 — this file is the detail those
items point at, so it isn't duplicated inline there. For the *positive*
case — why nirdosha over a mainstream language once this work lands, not
just what's missing today — see `KUBERNETES_ADVANTAGE.md`.

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
the `.db` file — now real, see `deploy/helm/nirdosha/` and
`deploy/kustomize/base/`) already works with nothing more than a
container image and two HTTP routes added (both now shipped). **Multiple
replicas behind one Service** (`deploy/kustomize/overlays/
postgres-multi-replica/`, or the Helm chart with `db.mode=postgres`)
need the P1 items in the remediation order to be correct, not just
schedulable — see that section for exactly which parts of P1 landed.

## Compliance matrix

### Container & image

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| OCI-compliant image | `[DONE]` | Repo-root `Dockerfile` (multi-stage: `rust:1-slim-trixie` build stage with the `dist` feature — z3-src 416.0.2 needs libstdc++'s `<format>`, which requires Debian trixie/GCC≥13, confirmed by a real build failure against bookworm/GCC 12 first; `python:3.12-slim-bookworm` runtime stage with `pytest`/`requests` baked in for protobox's black-box QA tasks). Built and smoke-tested locally end-to-end: `docker build` succeeds, the resulting image's `nirdosha` binary runs. | Still needs an actual `docker push` to `ghcr.io/protobox/nirdosha-runtime` from a real CI run with registry credentials — `.github/workflows/docker.yml` (added, lints clean under `actionlint`) does this on every `v*` tag; not yet exercised against the real registry from this change. |
| Multi-arch (amd64/arm64) | `[DONE]` | `.github/workflows/docker.yml`: `docker/build-push-action` with `platforms: linux/amd64,linux/arm64` via QEMU (`docker/setup-qemu-action`) + Buildx (`docker/setup-buildx-action`). Lints clean under `actionlint`; not yet run for real (needs registry push). | Once run for real: confirm the emulated arm64 leg's build time is acceptable — `release.yml`'s existing native cross-compilation is faster per-arch but would need a second job shape to reuse those binaries in a multi-arch manifest, judged not worth it for a once-per-tag publish. |
| Non-root user, read-only rootfs, dropped capabilities | `[DONE]` | Dockerfile: runtime stage creates and runs as `nirdosha` (UID/GID 10001), only `/data` is writable (`chown`'d), everything else can run under `readOnlyRootFilesystem: true` with no further image change — verified by the Helm chart's/`kubernetes.py`'s own `securityContext` (`runAsNonRoot`, `readOnlyRootFilesystem`, `capabilities.drop: [ALL]`) rendering and applying cleanly against this image shape. | — |
| SBOM / image signing (Sigstore, SLSA) | `[DONE]`, unverified against a real registry | `.github/workflows/docker.yml`: `cosign sign --yes` (keyless, GitHub OIDC — no key material) against the pushed digest, plus `anchore/sbom-action` (syft, SPDX JSON) attached via `cosign attest`. Lints clean; not yet run for real. | Run once against the real `ghcr.io/protobox/nirdosha-runtime` to confirm the OIDC keyless flow and Rekor transparency-log entry actually succeed from this repo's Actions identity. |

### Configuration (12-factor)

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Config externalized from code | `[PARTIAL]` | `main.rs::cmd_serve`'s flag parser: `--host --port --db --jwks-file --issuer --audience --presence-token --otel-port --otel-token` are all CLI flags. Zero app-level `std::env::var` reads for any of these (only `NIRDOSHA_TEST_*` and `NIRDOSHA_{prefix}_POOL_*` tuning vars exist, `pool.rs`). | CLI args are workable in a Pod spec's `command`/`args` (env-substitutable via `$(VAR)`), but not idiomatic. Lower priority than the items below. |
| Secrets never baked into the image | `[DONE]` | Every credential is a runtime flag/file path; none hardcoded. | — |
| Secrets sourced from files, not bare CLI values | `[DONE]` | `main.rs::cmd_serve`: `--presence-token-file`/`--otel-token-file` added, mirroring `--jwks-file`'s existing precedent exactly — mutually exclusive with their raw-value counterparts (a clear startup error if both are given), file content read and trimmed of exactly one trailing newline. Verified live: `tests/serve.rs`'s `presence_token_file_authenticates_a_real_presence_request` spawns the real binary, points it at a Secret-shaped token file, and confirms a matching bearer token authenticates while a wrong one still 401s; `otel_token_file_satisfies_the_otel_port_all_or_nothing_check` confirms `--otel-token-file` alone (no raw `--otel-token`) satisfies the existing all-or-nothing gate. | — |
| Business DB connection is environment-specific | `[DONE]` | `.nir` still has no env-var-read builtin — this stays a codegen-time decision, not a runtime one, as this row itself concluded. protobox: `core.graph.repository.{get,set,effective}_project_nirdosha_db_connect` (new `Project.nirdosha_db_connect` field) + `plugins.languages.nirdosha.resolve_db_connect_literal` (stored override, else the local-sqlite default — single source of truth) + `PUT/GET /api/projects/{id}/nirdosha-db-connect` route to set it before codegen runs; `nirdosha_screen_plan_routes.py`'s two `preview_html` call sites now read through the resolver instead of always hardcoding `_db_filename`. | Not yet wired into forge's per-construct CRUD codegen (that codegen doesn't exist yet either — `nirdosha-default-pipeline-plan.md` Phase 5) — when it lands, it must read `resolve_db_connect_literal` too, not invent a third call site. |

### Health & lifecycle

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Liveness probe route | `[DONE]` | `serve.rs`: `GET /healthz` — answers `200` the instant it's reached; by construction unreachable until schema migration + crash replay have already succeeded (both happen before `Server::http` binds), so there's nothing further to check for liveness specifically. `tests/serve.rs::healthz_returns_200_immediately`. | — |
| Readiness probe route | `[DONE]` | `serve.rs`: `GET /readyz` — `200`/`"ready"` when no `--db` is configured (nothing to check); when `--db` IS configured, runs a real `SELECT 1` against it and returns `503`/`"not_ready"` on failure. `tests/serve.rs::readyz_returns_200_with_no_db_configured` and `::readyz_reports_real_db_connectivity_when_db_is_configured` (the latter against a real SQLite file, not mocked). | — |
| Startup probe (slow boot) | `[DONE]` | Same `/readyz` route, with a longer `failureThreshold` in the Pod spec — both the Helm chart (`templates/_pod.tpl`) and `kubernetes.py::_probe` set `failureThreshold: 30` on the readiness probe for exactly this reason (a generous migration/crash-replay budget). | — |
| Graceful SIGTERM handling | `[DONE]` | `serve.rs`: `install_shutdown_signal_handlers` (`signal-hook` crate, new dependency) sets a shared flag on `SIGTERM`/`SIGINT`; the main loop switched from a blocking `server.incoming_requests()` to a `server.recv_timeout(200ms)` poll that checks the flag before picking up each new request — since this server handles exactly one request at a time (module doc), "drain in-flight" reduces to "don't interrupt the request already being processed," which needs no further change. `tests/serve.rs::sigterm_causes_prompt_graceful_shutdown` spawns the real binary, sends a real `SIGTERM`, and asserts a clean exit-0 within 5s (observed well under 200ms in practice). Durability under a hard `SIGKILL` was already proven (Track A1); this closes the politeness gap on top of that, as this row originally called for. | — |

### State, data & horizontal scaling

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Same-host double-start protection | `[DONE]` | `instance_lock.rs` — OS-level exclusive SQLite lock on a sidecar file, held for the process's whole lifetime. Verified live: two real processes pointed at the same log file, the second refuses to start. | Covers rolling-restart overlap on one host only, not real multi-replica — by design (see the module's own doc comment). |
| `workflow`/`transact` durability across replicas | `[DONE]` | `ROADMAP.md` Track A17, `durability.rs`: `--transact-log`/`--workflow-log` accept a `postgres://`/`postgresql://` value (`LogTarget`, `pool.rs`'s `PoolRegistry`). Verified live against a real Postgres server: two independent `WorkflowLog`/`TransactLog` handles sharing one database never collide on `instance_id` and do see each other's `txn_id` rows; two real `serve` processes on the same database both serve concurrent requests correctly. | Protobox deploy config: point both flags at Postgres for any project that will run >1 replica. No further runtime work needed. |
| Business/CRUD data (`db_connect`) on shared storage | `[PARTIAL]` | `dbconn.rs`: `DbConn::{Postgres, PostgresTls}`, pooled (`POSTGRES_POOLS`), TLS-optional — the runtime capability is real and shipped. protobox's `gen-crud`/`nirdosha_screen_plan.py` always emit a local `<project_id>.db` literal today (see `PROTOBOX_INTEGRATION.md` §8's first gotcha for the related `db_connect` vs `--db` mismatch trap). | Protobox-side codegen change only — no nirdosha runtime work needed. |
| UI's generic table browser + role-mapping cache | `[OPEN]`, now fails fast instead of silently misbehaving | `serve.rs:218`'s `--db <path>` still opens a bare `rusqlite::Connection` directly — the "extend to Postgres" half of this row's own two options was judged too large/risky to do safely alongside everything else in this pass (`dispatch_table_query`/`load_role_mapping`/the view-gated pass-through-on-omit step in `dispatch` all build raw SQL and read back `rusqlite::types::Value` directly — a materially bigger, separately-scoped rewrite). The "explicitly scope" half WAS taken: `serve::run` now rejects a `postgres://`/`postgresql://` `--db` value outright at startup with a clear, actionable error, instead of silently trying `rusqlite::Connection::open` on it (which would previously either fail confusingly or silently create/serve a garbage local file). `tests/serve.rs::db_flag_pointed_at_postgres_is_rejected_with_a_clear_error_not_silently_misused`. | Still a real nirdosha-level gap for anyone who actually needs the generic table browser at >1 replica — extending it to `dbconn.rs`'s `DbConn` enum remains the fix, just not done here. |
| Cross-machine SQLite replication (no Postgres) | `[N/A]` | `ROADMAP.md` Track A17 Phase 2 names rqlite/dqlite (Raft) and cr-sqlite (CRDT) as real prior art, deliberately not adopted — Postgres already solves the actual need. | Revisit only if a deployment specifically must avoid a Postgres dependency. |
| Single-replica deployment shape (`StatefulSet` + PVC) | `[DONE]` | `deploy/helm/nirdosha/templates/statefulset.yaml` and `deploy/kustomize/base/` (`db.mode=sqlite`, the default in both): one `StatefulSet`, `replicas: 1` (hardcoded, not just defaulted), one `volumeClaimTemplates` entry named `data`. protobox: `plugins/deploy_targets/kubernetes.py::render_manifests` renders the identical shape and raises `ValueError` if asked for `replicas > 1` with `db_mode="sqlite"` — verified via `helm lint`/`helm template`/`kustomize build` (all clean) and 29 passing tests in `tests/plugins/deploy_targets/test_kubernetes.py`. | — |

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
| Logs to stdout/stderr | `[DONE]` | `serve.rs`: `NIRDOSHA_LOG_FORMAT=json` switches `serve`'s own lifecycle log lines (bind, migration, crash replay, shutdown) from plain `eprintln!` text to one-line JSON objects (`{"ts", "level", "component", "msg"}`) via a new `log_lifecycle` helper — an env var, not a new `run()` parameter, so every existing test call site (`tests/*.rs`) keeps compiling unchanged. `false`/unset stays byte-for-byte the original plain-text output. | Per-request error bodies (already structured JSON in the HTTP response itself) and the interpreter's own `--format=json` diagnostic output are unaffected/unchanged — this row was specifically about `serve`'s lifecycle log, which is now covered. |
| Prometheus `/metrics` | `[DONE]` | `serve.rs`: `GET /metrics`, hand-rolled Prometheus text exposition format (v0.0.4) — `nirdosha_requests_total` (counter), `nirdosha_responses_total{class="2xx"\|"4xx"\|"5xx"\|"other"}` (counter), `nirdosha_request_latency_ms_sum`/`_avg`, `nirdosha_uptime_seconds`. Plain `AtomicU64`s, no new crate (justified in `Metrics`'s own doc comment — five counters is well under where `prometheus`-the-crate earns its cost). `tests/serve.rs::metrics_endpoint_reports_prometheus_text_format_with_real_counts` asserts real counts after real requests, not just that the route exists. | Per-route/per-fn-name latency breakdown (only a process-wide aggregate today) and a real Prometheus/`promtool` scrape validation (only the text format was hand-checked against the spec) are natural follow-ups if this needs to get more granular. |
| Distributed tracing (OpenTelemetry) | `[PARTIAL]`, unchanged | `observability.rs`: real zero-cost-when-disabled local tracer; Layer 2a (`--otel-port`/`--otel-token`(`-file`, now — see the "Configuration" section above), opt-in loopback JSON stream) built and tested (`tests/observability_layer2a.rs`). | Layer 2b (real OTLP export to an actual collector) is the named next step in `observability.rs`'s own doc comment — genuinely not started in this pass either; `main.rs` already gives `--otel`/`--otel-endpoint` a clear "not implemented yet" error rather than pretending. |

### Security posture (Pod-level)

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Pod Security Standards ("restricted" profile) | `[DONE]` | Container image now exists (non-root UID 10001, no setuid binaries added). Both the Helm chart (`values.yaml`'s `securityContext`/`podSecurityContext`) and `kubernetes.py::_security_context` set `runAsNonRoot: true`, `readOnlyRootFilesystem: true`, `allowPrivilegeEscalation: false`, `capabilities.drop: [ALL]`, `seccompProfile: RuntimeDefault` — satisfies the Pod Security "restricted" profile's container-level requirements. Verified via `helm template`/`kustomize build` rendering these fields and `tests/plugins/deploy_targets/test_kubernetes.py::test_container_runs_as_non_root_with_read_only_rootfs_and_no_capabilities`. | Namespace-level `pod-security.kubernetes.io/enforce: restricted` labeling is an operator/cluster-admin action, not something a chart or deploy target can set on the operator's behalf — left as an operator step (documented in `deploy/README.md`). |
| NetworkPolicy compatibility | `[N/A]` | Standard Kubernetes object; nothing app-side needs to change. | — |

### Deployment manifests & GitOps

| Requirement | Status | Evidence | Gap / action |
|---|---|---|---|
| Dockerfile | `[DONE]` | Repo-root `Dockerfile` — see "Container & image" above. | — |
| Helm chart / Kustomize overlay | `[DONE]` | `deploy/helm/nirdosha/` (full chart: StatefulSet/Deployment, Service, Ingress, ServiceAccount, PodDisruptionBudget, `_helpers.tpl`/`_pod.tpl` shared templates, `NOTES.txt`) — `helm lint` clean, `helm template` verified for the sqlite default, a postgres+3-replica+ingress+auth+presence override combination, and the sqlite-with->1-replica combination correctly `fail`s at render time. `deploy/kustomize/base/` (static sqlite render) and `deploy/kustomize/overlays/postgres-multi-replica/` (static postgres render) — both verified with `kustomize build`. protobox's own `plugins/deploy_targets/kubernetes.py` (new deploy target, registered the same auto-discovery way `neon.py`/`render.py` are) renders the identical shape programmatically and additionally builds+pushes a per-project derived image (`_derived_dockerfile`: the base runtime image + that project's `.nir`/`theme.json` layered on top, reusing `_render_api.build_and_push_image`, the same GHCR-push helper `render.py` already uses) before applying — 29 tests in `tests/plugins/deploy_targets/test_kubernetes.py`. | `is_configured()` (a `KUBE_CONTEXT` env var + `kubectl` on `PATH`) gates the live-`kubectl apply` half only — manifests are always written to the workspace even without a live cluster, so an operator can apply them by hand. Not yet exercised against a real live cluster from this change (only `helm template`/`kustomize build`/mocked-`kubectl` paths were verified). |
| CI already produces the binaries an image needs | `[DONE]` | `.github/workflows/release.yml` cross-compiles linux/macOS/Windows × amd64/arm64 on every `v*` tag; `.github/workflows/docker.yml` (new) builds+pushes the multi-arch image, signs it, and attaches an SBOM on the same trigger. | The new workflow lints clean under `actionlint` but hasn't been run for real yet (needs an actual tag push with registry credentials) — first real run should be watched closely. |

## Remediation order

Real dependency order, not a priority wishlist — each phase is a
prerequisite for the next to matter. **Status as of 2026-08-27: P0
done, P1 done except the one genuine nirdosha-level gap it always named
as possibly out of reach, P2 done except OTel Layer 2b (never claimed
otherwise), P3 done except mTLS (deliberately, by design — see below).**

**P0 — make it schedulable at all — ✅ done.**
- ✅ `ghcr.io/protobox/nirdosha-runtime`'s `Dockerfile` written, built,
  and smoke-tested locally (multi-stage: `rust:1-slim-trixie` build +
  `python:3.12-slim-bookworm` runtime with `pytest`/`requests` baked
  in). `.github/workflows/docker.yml` publishes it on every `v*` tag
  (multi-arch, signed, SBOM'd) — lints clean, not yet run against the
  real registry.
- ✅ `/healthz` and `/readyz` added to `serve.rs`, both covered by
  real integration tests (`tests/serve.rs`).
- ✅ SIGTERM handling added (`signal-hook` + a `recv_timeout` poll
  loop) — verified with a real subprocess + real `SIGTERM` in
  `tests/serve.rs::sigterm_causes_prompt_graceful_shutdown`.

**P1 — make more than one replica correct — done except one disclosed
gap.**
- ✅ `--transact-log`/`--workflow-log` -> Postgres is deploy-config
  wiring only, already shippable (Track A17 did the runtime work
  earlier) — both the Helm chart and `kubernetes.py` wire this via a
  `db.mode=postgres`/`db_mode="postgres"` Secret reference.
- ✅ protobox's `gen-crud`/`nirdosha_screen_plan.py` now goes through
  `resolve_db_connect_literal`, which reads an operator-set
  `Project.nirdosha_db_connect` override before falling back to the
  local `.db` default — the "environment-appropriate literal" this
  item asked for.
- **Decided, not extended:** `serve.rs`'s `--db` table/role-cache layer
  still doesn't accept Postgres — this was judged too large a rewrite
  to do safely in the same pass as everything else here (see the
  "State, data & horizontal scaling" table's own note). The "explicitly
  scope" half of this item's own two options WAS taken: `--db
  postgres://...` now fails fast with a clear error instead of being
  silently misused. A real follow-up, not a silent gap.

**P2 — production hardening — done except OTel Layer 2b.**
- ✅ Prometheus `/metrics` endpoint (hand-rolled exposition format,
  real counters).
- ❌ OTel Layer 2b (real OTLP export) — still not started; this row
  never claimed otherwise, and `main.rs` already gives `--otel`/
  `--otel-endpoint` a clear "not implemented yet" error rather than a
  silent no-op.
- ✅ Structured JSON operational logs — `NIRDOSHA_LOG_FORMAT=json`
  switches `serve`'s lifecycle log lines to one-line JSON.
- ✅ `--presence-token-file`/`--otel-token-file`, mirroring
  `--jwks-file`, both verified end-to-end (not just parsed).
- **Unchanged, by design:** rate limiting stays an Ingress/mesh
  annotation, not in-app — this was always the recommendation, not a
  gap this pass closes in code.

**P3 — platform polish — done except mTLS (by design).**
- ✅ Helm chart (`deploy/helm/nirdosha/`) AND a Kustomize base +
  postgres-multi-replica overlay (`deploy/kustomize/`) — the doc
  originally offered either; both got built since they're genuinely
  different audiences (templated vs. static). Plus protobox's own
  `plugins/deploy_targets/kubernetes.py`, following the Render/Vercel
  registration pattern exactly (`PLUGIN = _Target()`, auto-discovered).
- ✅ Pod Security "restricted" profile: non-root UID, read-only
  rootfs, dropped capabilities — set in the image, the Helm chart, and
  `kubernetes.py` alike.
- ❌ mTLS via service-mesh sidecar — still deliberately NOT built into
  nirdosha itself, consistent with how TLS-for-`serve` is already
  deferred to a reverse proxy. This was always the intended end state
  for this item, not a gap.

## Sources checked

`compiler/src/{main,serve,dbconn,durability,instance_lock,
observability,pool}.rs`, `ROADMAP.md` (Track A items A1/A2/A17, the
Standards & compliance matrix section), `.github/workflows/{build,
release}.yml`, `PROTOBOX_INTEGRATION.md` §§7-9, and protobox's
`be-v2/src/plugins/languages/nirdosha.py` and
`be-v2/docs/plans/nirdosha-default-pipeline-plan.md` (Phase 8).
**2026-08-27 implementation pass, additionally checked against:** the
new `serve.rs` routes/tests directly (`cargo test --test serve` — 21
passing, plus `cargo test --lib` — 40 passing), a real `docker build`
of the repo-root `Dockerfile` (succeeded after one real, disclosed
failure — `<format>` needs GCC≥13, `rust:1-slim-bookworm`'s GCC 12
doesn't have it), `helm lint`/`helm template` against
`deploy/helm/nirdosha/` (clean, including the sqlite->1-replica
`fail()` guard actually firing), `kustomize build` against both
`deploy/kustomize/` trees (clean), `actionlint` against
`.github/workflows/docker.yml` (clean), and protobox's
`plugins/deploy_targets/kubernetes.py` + its 29-test suite
(`tests/plugins/deploy_targets/test_kubernetes.py`) plus the
`resolve_db_connect_literal`/routes changes' own tests (all green).
General Kubernetes production-readiness conventions (probes, graceful
termination, 12-factor config) cross-checked against current published
guidance, not assumed from memory alone.
