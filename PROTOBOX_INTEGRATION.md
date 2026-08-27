# protobox ↔ nirdosha integration

## Purpose

**protobox** turns product requirements into structured `Project`/`Module`/
`Actor`/`UserStory` data in Neo4j; **nirdosha** turns `.nir` source into a
real, running, role-gated web app. Neither system does the other's job —
protobox has no code generator, nirdosha has no requirements-authoring UI.
This document exists so the two can be wired together without either team
having to read the other's source: it's the **interface contract**
protobox needs to build against — exactly what data to hand over, in what
shape, via which commands, what the resulting app looks like on first boot
before anything is configured, and the gotchas already hit building the
first real pipeline (`trade-finance-b2b-2`) that would otherwise get
rediscovered per integration. Everything below is either shipped and
verified today, or explicitly marked as an open gap — no aspirational
claims.

## 1. The pipeline, end to end

```
protobox (Neo4j Aura)              nirdosha (this repo)
──────────────────────             ─────────────────────────────
(:Project)<-[:CHILD_OF]-           1. nirdosha init <project>
(:Module)-[:HAS_USER_STORY]->         -> starter <project>.nir +
(:UserStory {narrative, persona,         standing admin-panel fixtures
  title, action, benefit,
  preconditions, postconditions,   2. Per UserStory: apply the LLM
  acceptance_criteria_json,           prompt (scratch/nirdosha_llm_prompt.md)
  parent_feature, actor_ids})         -> append generated .nir to the file

                                    3. nirdosha serve <project>.nir ...
                                       -> real HTTP app: UI at GET /,
                                          API at POST /api/<fn>
```

protobox's job stops at handing over structured `UserStory` data (via the
Neo4j schema above) and driving the LLM generation step. Everything from
step 1 onward is a `nirdosha` CLI invocation — no protobox-specific code
needs to run inside `compiler/`.

## 2. Step 1 — scaffold the project

```sh
nirdosha init <project-name>
```

Writes `./<project-name>/`:

| File | Purpose |
|---|---|
| `<project-name>.nir` | Starter source — header comment with the naming-convention table (§3), plus `EmailProviderConfig`/`RoleMapping` fixtures by default (`--no-email`/`--no-roles` to drop them, `--sms`/`--push` to add more channels) |
| `nirdosha` (or `.exe`) | A copy of the `nirdosha` binary that ran `init` — same OS/arch only |
| `run.sh` / `run.bat` | Launches `nirdosha serve <project-name>.nir` with placeholder auth flags (§4) |
| `jwks.json` | Empty placeholder key set (`{"keys": []}`) so `run.sh` starts with zero setup |

`--dest <path>` places the folder under `<path>` instead of the current
directory; `--force` overwrites an existing one. This step should run
**once per protobox project**, before any user-story generation — it's what
replaces protobox having to hand-emit the `EmailProviderConfig`/
`RoleMapping` boilerplate itself (`ROADMAP.md` Track A6).

## 3. Step 2 — generate code per user story

Feed each `UserStory` node through the LLM prompt at
`scratch/nirdosha_llm_prompt.md` (tracked in this repo — the authoritative,
current generation contract; not a draft). Append its output into the
`.nir` file `init` created. Load-bearing rules the generation step must
follow, because `nirdosha emit-ui`/`serve` infer the UI from naming alone —
a correct-but-differently-worded function still type-checks and runs, it's
just invisible in the generated app:

| Story action shape | Required fn name | UI effect |
|---|---|---|
| creates a new record | `create_<entity>` | screen's create form |
| lists/browses records | `list_<entity>` | screen's table |
| fetches one record | `get_<entity>` | screen's detail view |
| modifies a record | `update_<entity>` | table row's Edit |
| removes a record | `delete_<entity>` | table row's Delete |
| a numeric KPI | `stat_<name>() -> i64\|f64` | dashboard tile |
| a labeled series for a chart | `chart_<name>() -> json` (rows `{label, value}`) | dashboard chart |
| a one-off action that doesn't fit CRUD | any name | wire explicitly via `screen <Entity> { action "<label>" -> <fn> }`, or it's dead code the UI can never call |

Other fixed contract points the prompt file encodes in full detail
(`GRAMMAR.md` is the ground truth if anything here and the grammar ever
disagree):

- **`str` is banned as a function parameter/return type.** Free text
  crossing a function boundary wraps in `struct Text { value: str }`
  (`init` already declares this once per project if any fixture needing it
  is enabled — declare it once yourself if generating a project that starts
  with `--no-email --no-roles`, and it turns out a story needs it).
- **One `db_connect("<literal>")` string, reused byte-for-byte everywhere**
  in the file. `init` seeds it as `<project-name>.db`; every generated
  function must use that exact literal, not a fresh path per story.
- **Struct construction and enum variants are calls**, never `{ field: val
  }` literals: `Point(3.0, 4.0)`, `Circle(2.0)`.
- **Every `fn` needs an explicit `-> Type`** unless it truly returns
  nothing.

## 4. Configuration & first-boot lifecycle

Two independent things are configured completely differently — this is the
part most likely to surprise a first integration:

| | Where it lives | When it's set | Runtime-editable? |
|---|---|---|---|
| DB connection string | Literal in `.nir` source (`db_connect("...")`) | Written at generation time | No — edit source + restart |
| Identity-provider trust anchor (JWKS/issuer/audience) | `run.sh`/`run.bat` launch flags | Passed at `nirdosha serve` startup, held in memory | No — edit launcher + restart |
| Role → IdP-role mapping | `RoleMapping` DB table | Via its own admin CRUD screen (`init` scaffolds it) | **Yes** — 30s cache TTL, no restart |
| Email/SMS/Push provider settings | `*ProviderConfig` DB tables | Via their own admin CRUD screens (`init` scaffolds them) | **Yes** — read fresh on every send, no cache at all |

**What `init`'s placeholder config means on first boot**: `run.sh` ships
with a syntactically valid but empty JWKS (`jwks.json`) and fake
`--issuer`/`--audience` values specifically so the server *starts*
immediately with zero manual setup. Effect: the server runs, the UI
renders, every non-gated route works — but no token can ever validate
against an empty key set, so every `requires(role: ...)`-gated route
honestly 401s for everyone until real IdP values replace the placeholders.
This is deliberate fail-closed behavior, not a bug to work around.

To go live: replace the three placeholder flags in `run.sh` with a real
IdP's `--jwks-file`/`--issuer`/`--audience` and restart. There is currently
**no admin UI for either the DB string or the trust anchor itself** — see
§8.

### Testing locally with no real IdP yet

`examples/identity_mock.nir` is a working mock IdP shipped in this repo —
`nirdosha serve examples/identity_mock.nir --port 9090`, then:

```sh
curl -X POST http://localhost:9090/api/login \
  -H 'Content-Type: application/json' \
  -d '{"subject":"alice","role":"admin"}'
```

returns a real signed JWT that validates through the generated app's
*actual, unmodified* `oidc_validate_token` — a genuine round trip, not a
parallel fake-auth path. Point the generated project's `run.sh` at
`--identity-base http://localhost:9090` and this JWKS to develop/test
against before a real IdP exists.

## 5. Running the generated app

```sh
cd <project-name> && ./run.sh
# == nirdosha serve <project-name>.nir --host 127.0.0.1 --port 8080
#      --jwks-file jwks.json --issuer <...> --audience <...>
#      --db <project-name>.db
```

Full `serve` flag reference (`compiler/src/main.rs::cmd_serve`):

| Flag | Purpose |
|---|---|
| `--host`, `--port` | Bind address, default `127.0.0.1:8080` |
| `--jwks-file`, `--issuer`, `--audience` | Auth trust anchor — all three or none |
| `--identity-base URL` | Where a login flow points (e.g. the mock IdP above) |
| `--db PATH` | Enables the generic `/_nirdosha/table/<snake>` pagination/sort/filter route; **must be the same literal every `db_connect(...)` call in the source uses** (see gotcha below) |
| `--theme theme.json` | Design tokens, hot-reloaded on a TTL |
| `--presence-token` | `WORKFLOW.md`'s live-push bridge, if used |
| `--otel-port`, `--otel-token` | Observability layer 2a — live JSON-line spans while a client is connected |

On every startup, `migrate.rs` diffs every `struct` in the program against
the live SQLite schema at `--db`'s path and applies whatever's missing
(`CREATE TABLE` / additive `ALTER TABLE ... ADD COLUMN` only — a changed
column type or a removed field is logged, never auto-applied). Applied
changes are written to `<db's parent dir>/migrations/NNNN_<slug>.sql` as a
reviewable audit trail. protobox needs no separate migration step —
regenerating/extending the `.nir` file and restarting `serve` is the whole
migration story.

## 6. Calling the generated API programmatically

`POST /api/<fn>` — request body is a JSON object keyed by the function's
own **parameter names**, not a flat object:

```sh
# create_trade_record(t: TradeRecord) needs:
curl -X POST http://localhost:8080/api/create_trade_record \
  -H "Authorization: Bearer $TOKEN" -H 'Content-Type: application/json' \
  -d '{"t": {"...trade record fields...": "..."}}'
```

**Known client-side gotcha**: the browser UI's error toast for a failed
call currently just shows `[object Object]`, not the real message — if
debugging a call from protobox tooling, read the raw `/api/<fn>` response
body directly, it carries the actual error.

## 7. Deployment handoff

`nirdosha init`'s output folder (§2) *is* the deployment artifact — it
already contains a bundled `nirdosha` binary and launcher. Once code
generation (§3) has populated `<project-name>.nir`, the whole folder can be
copied to a target machine (same OS/arch) and run with `./run.sh` — no
separate `nirdosha` install needed there. Real production hardening
(containerization, secrets management) is still open — `ROADMAP.md` Track
A2. For running this on Kubernetes specifically — container image,
health/readiness probes, graceful shutdown, and (the part that actually
needs care) what changes once a project runs more than one replica —
see `KUBERNETES.md`. For the case *for* nirdosha over a mainstream
backend language for a k8s-targeted protobox project — built-in
kill-tested transaction durability, a workflow audit trail with no
extra service to run, one-process UI+API, compiled/enforced RBAC — see
`KUBERNETES_ADVANTAGE.md`.

## 8. Known gotchas checklist

- `db_connect("<literal>")` inside `.nir` source is the actual DB the
  interpreter opens — independent of `serve --db <path>` (that flag only
  backs the generic table-query route). Mismatched literals silently
  produce "no such table" errors even though migrations logged as applying
  fine (they applied against `--db`'s file, not the one `db_connect`
  opens).
- Never reuse a DB filename another running `nirdosha serve` instance in
  the same environment already uses — two processes fighting over one
  SQLite file causes hard-to-diagnose failures. Give every protobox-
  generated project its own filename (`init` already does this by using
  `<project-name>.db`).
- `POST /api/<fn>` body must be keyed by parameter name (`{"t": {...}}`),
  not the struct's fields flattened directly into the top-level object.
- A function named for the right *effect* but the wrong *verb*
  (`ingest_document` instead of `create_document`) type-checks and runs
  but is invisible to the generated UI — always name for the table in §3,
  never for readability alone.

## 9. Open gaps (tracked in `ROADMAP.md`)

- **No admin UI for the DB connection string or the IdP trust anchor
  itself** — both are edit-the-file-and-restart today. Only role mapping
  and provider-channel settings are live-editable (§4).
- **Single fixed IdP per running server** (`ROADMAP.md` Track A6, "Multi-
  IdP registry") — `serve` takes exactly one `--jwks-file`/`--issuer`/
  `--audience` triple; a protobox project needing multiple trusted IdPs
  isn't supported yet.
- **Containerized/production deployment story** (`ROADMAP.md` Track A2) —
  `init`'s bundled-folder handoff (§7) covers "copy and run," not
  orchestration, secrets rotation, or horizontal scaling. Full
  Kubernetes-specific gap breakdown (container image, probes, graceful
  shutdown, and — the sharpest one — `serve --db`'s table-browser/
  role-mapping layer having no Postgres option, so it stays
  single-instance even once the business DB and durability logs are
  pointed at Postgres) in `KUBERNETES.md`.
- **Business-rule parameters (thresholds, boundary operators, currency)
  have no elicitation or config-store path** (`ROADMAP.md` Track A9) —
  `nirdosha init` (§2) only scaffolds identity/communications fixtures
  today; it never asks domain-specific questions about the numeric/
  boolean parameters a generated business rule actually needs (a
  threshold amount, `>` vs `>=`, a per-currency conversion). Concretely:
  `required_eyes_for_amount` (`examples/trade-finance/trade_finance.nir:1733`)
  hardcodes a threshold and a boundary operator that neither the PRD
  nor the LLM had real authority to decide, and never consults
  `currency` at all despite `TradePayment` carrying one — so a
  5,000,000-cent JPY payment and a 5,000,000-cent USD payment route
  identically. Proposed fix (not started): a domain-aware question
  phase in `init`, with answers landing in a runtime-editable config
  table rather than inlined `.nir` literals — see A9 for the full
  writeup, including why this isn't fixable at the LLM-prompt level
  (the PRD text itself is genuinely ambiguous on the boundary case, and
  nobody with real business authority was ever asked to resolve it).

## 10. Found reading `../protobox` directly: this doc's pipeline (§1-§4)
## isn't what protobox's current default pipeline actually runs

Everything below was verified by reading `../protobox`'s actual source
(`be-v2/src/plugins/languages/nirdosha.py`,
`.../nirdosha_direct_codegen.py`,
`.../design_studio/nirdosha_screen_plan*.py`,
`.../story_2_tasks/dev_tasks/prompt.py`,
`.../story_2_tasks/qa_tasks/{prompt,nirdosha_qa_conftest}.py`,
`be-v2/docs/plans/nirdosha-default-pipeline-plan.md`) as of this writing
— not assumed from this doc's own description of the pipeline.

- **`nirdosha init` (§2) is never invoked anywhere in protobox's actual
  default pipeline.** `plugins/languages/nirdosha.py::_Stack.assemble()`
  seeds a brand-new project by writing a bare `_ENTRYPOINT_SEED` comment
  string directly — it does not shell out to `nirdosha init`, so it never
  gets `EmailProviderConfig`/`RoleMapping` fixtures, `run.sh`/`run.bat`,
  or a placeholder `jwks.json`. Confirmed by grep: no `nirdosha init`
  subprocess call exists anywhere under `be-v2/src`. This means
  **everything §2 and §4 describe is specific to the older, manual
  pipeline** (`scratch/nirdosha_llm_prompt.md` fed by hand per user
  story — what `trade-finance-b2b-2` actually used) — protobox's newer
  automated `LanguageStack`-driven pipeline
  (`nirdosha-default-pipeline-plan.md`, Phases 1-8) is a **second,
  parallel integration path this doc doesn't mention at all**, and it
  starts every project from zero standing fixtures, not `init`'s
  scaffolded ones.
- **QA testing currently cannot exercise a `requires(role/claim)`-gated
  fn's positive path at all.** `nirdosha_qa_conftest.py`'s
  `nirdosha_server` fixture boots exactly `nirdosha serve <entrypoint>
  --port <port>` — no `--jwks-file`/`--issuer`/`--audience`, no `--db`,
  no `--identity-base`. But `serve.rs::resolve_identity` (§4's fail-
  closed behavior) needs all three JWKS flags together to validate any
  Bearer token — with none configured, it 500s
  ("this server has no --jwks-file/--issuer/--audience configured to
  validate a bearer token against") rather than accepting one. Meanwhile
  `qa_tasks/prompt.py`'s own Rule 5 tells the code-gen LLM the positive
  scenario needs "a valid Bearer token... e.g. via a test-only
  mock-identity endpoint if the project's `--db`/`--identity-base` setup
  provides one" — but the actual conftest template wires up neither. The
  fix is exactly what §4's "Testing locally with no real IdP yet"
  section already documents (`examples/identity_mock.nir` as a companion
  `--identity-base`, or real `--jwks-file`/`--issuer`/`--audience`
  passed to the QA harness's `serve` invocation) — protobox's QA
  conftest template just doesn't do it yet.
- **`_Stack.prompt_rules()` (`plugins/languages/nirdosha.py`) — the
  single source of truth both `dev_tasks/prompt.py`'s task-mining prompt
  and `nirdosha_screen_plan_prompt.py`'s whole-project planner read —
  never mentions**: the `str`-banned-at-function-boundary rule
  (`LANGUAGE.md` §6b — free text crossing a `fn` boundary must wrap in
  `struct Text { value: str }`), field-level `pattern`/`format`/`min`/
  `max` validation (`LANGUAGE.md` §11), or the fixed chart/animation/
  form-control sets (`compiler/UI_DSL_TODO.md`'s "Deliberate non-goals",
  added this session — one bar-chart type, four `@keyframes`, seven form
  controls, nothing else, ever). A hallucinated bare-`str`-param
  signature from task-mining does get caught eventually — the real
  compiler's own error message ("`str` can't cross a function boundary;
  use an `enum`...", `typeck.rs` line ~766) is self-explanatory enough
  for a repair loop to fix blind — but it's a wasted round-trip against
  a capped repair budget (`nirdosha_direct_codegen.py`'s
  `max_repairs=3`), and `nirdosha_screen_plan_prompt.py`'s planner never
  gets a repair loop at all (it locks the plan once). Contrast:
  `nirdosha_direct_codegen.py`'s own system prompt is
  `scratch/nirdosha_llm_prompt.md` read whole (comprehensive — str-ban,
  validation DSL, and generation contract all included, per that file's
  own docstring) — `prompt_rules()` is a **second, shorter, independently
  hand-maintained summary of the same language**, and it's the one
  that's drifted. Recommend protobox point `prompt_rules()` at (or
  generate it from) the same comprehensive source rather than
  hand-updating two summaries in parallel going forward.
- **`--theme`/`DesignSpec` integration is real, shipped, and load-bearing
  today, but this doc never mentions it.** `_theme_json_from_design_
  spec` (same file) is a verbatim pass-through of protobox's own
  `resolve_design_tokens(spec)` — `ui_gen::Theme` was redesigned
  specifically as a 1:1 mirror of that function's JSON shape (`LANGUAGE.md`
  §11b) so this needs zero translation layer on either side, and
  `assemble()` writes `theme.json` next to the entrypoint from the
  project's saved `DesignSpec` on every call. Worth a §4-adjacent mention
  here so a future reader of this doc doesn't miss that this exists and
  is already wired, live-reloaded on serve's own 30s TTL.
- **`docker_image = "ghcr.io/protobox/nirdosha-runtime:latest"` doesn't
  resolve to anything yet** (self-disclosed in `nirdosha.py`'s own
  module docstring) — blocks `features/build_app/run_docker_tests.py`'s
  Docker-based test run end to end for any nirdosha-lane project today.
  Relevant to this doc's §7: the "copy the `init` folder and run"
  handoff works standalone, but protobox's own containerized test path
  is separately blocked on publishing this image (bake the `nirdosha`
  binary + `python3`/`pytest`/`requests`), tracked as a real prerequisite
  on protobox's side, not a nirdosha-repo gap.
- **Do not start emitting the `target: "web"|"mobile"|"all"` screen key**
  if `nirdosha_screen_plan_prompt.py`/`prompt_rules()` ever grow mobile
  awareness — it's a design proposed in `MOBILE.md`'s "Per-target
  screen/dashboard exclusion" section this session, not implemented in
  the compiler (no `mobile_gen.rs`, no `--target` filtering in
  `ui_gen.rs` yet). A `.nir` file using it today would fail typecheck.

## References

- `scratch/nirdosha_llm_prompt.md` — the generation prompt (§3), tracked in
  this repo.
- `compiler/src/init.rs`, `compiler/src/main.rs::cmd_init` — §2.
- `compiler/src/serve.rs`, `compiler/src/main.rs::cmd_serve` — §5.
- `compiler/src/migrate.rs` — §5's migration behavior.
- `LANGUAGE.md` §6b (str-ban/`Text`), §11a (role mapping), §13 (migrations).
- `WORKFLOW.md` — provider-config/notification mechanics behind
  `EmailProviderConfig` et al.
- `ROADMAP.md` Track A (A2, A6, A9) — §9's open gaps, in full detail.
- `compiler/tests/trade_finance_governance_routing.rs` — the threshold/
  boundary-operator drift cited in §9's A9 item, verified against the
  real `.nir` routing rule rather than asserted from prose.
- `MOBILE.md` — the `target:` key proposal referenced in §10's last item.
- `compiler/UI_DSL_TODO.md` — the "Deliberate non-goals" section §10
  says `prompt_rules()` doesn't yet reflect.
- `../protobox/be-v2/docs/plans/nirdosha-default-pipeline-plan.md` — the
  design doc for protobox's actual current default pipeline (§10),
  parallel to and not the same as this doc's §1-§4.
- `../protobox/be-v2/src/plugins/languages/nirdosha.py` — `_Stack.
  assemble()`/`prompt_rules()`/`_theme_json_from_design_spec`, all cited
  in §10.
- `../protobox/be-v2/src/plugins/languages/nirdosha_direct_codegen.py` —
  the one-call-per-story direct codegen path that *does* use the
  comprehensive `scratch/nirdosha_llm_prompt.md`, cited in §10.
- `../protobox/be-v2/src/features/design_studio/nirdosha_screen_plan*.py`
  — the whole-project screen planner, cited in §10.
- `../protobox/be-v2/src/features/mine/story_2_tasks/{dev_tasks,
  qa_tasks}/prompt.py`, `qa_tasks/nirdosha_qa_conftest.py` — task-mining
  and QA-test generation prompts/harness, cited in §10.
