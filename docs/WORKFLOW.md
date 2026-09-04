# Nirdosha `workflow` — durable, notification-driven state machines

This is the Nirdosha-native answer to "we need a real construct for
multi-step, human-in-the-loop processes" (user onboarding, KYC approval,
salary disbursal, and "many more" like them) — see the design
conversation this doc grew out of, which first tried and rejected a
general-purpose async/continuation-based `durable fn` (`docs/PROTOLANG_PORT.md`
row 5/row 4: Nirdosha's concurrency story is already `spawn`+`chan`, and
`transact` already covers durable effect sequencing; neither needed
reopening). The actual gap, once n-eyes approval was built by hand in
`trade_finance.nir`, was narrower: a *named, multi-state* pipeline with
notification actions and external triggers, generalizing the
`status TEXT` + hand-rolled `CREATE TABLE`/append-only-log pattern every
one of those processes was independently re-deriving.

The finding that matters, same shape as `docs/TRANSACT.md`'s own: **this
doesn't need a new runtime.** `workflow` is a compiler-stage desugaring —
every `workflow` block becomes ordinary `fn`/`enum`/`struct` declarations
(`workflow_lower.rs`) that flow through every existing pass (typeck, the
interpreter, `nirdosha serve`'s automatic `POST /api/<fn>` RPC exposure)
unchanged. The only genuinely new runtime pieces are: a durable store
for instance state (`workflow_log.rs`, modeled directly on
`transact_log.rs` — SQLite-file-backed by default, or a shared Postgres
database (`src/durability.rs`) or a Raft-replicated SQLite cluster
(`src/rqlite.rs`) for multi-instance deployments, `docs/ROADMAP.md`'s
multi-instance fix), and four new interpreter builtins
(`send_email`/`send_sms`/`send_push`/`notify`) for the notification
actions the design conversation specifically asked for.

## What it brings to the table

**1. Named states and transitions instead of a `status TEXT` column
re-derived per process.** `trade_finance.nir`'s own n-eyes approval
(`Module 1`) already discovered the right *shape* — `status`, an
append-only decision log, never a mutated "current stage" field — but had
to hand-write it, including its own `CREATE TABLE IF NOT EXISTS`, for
every one of 15 action types. `workflow` makes that shape a declaration:
named `state`s, `on <Event> -> <Target>` transitions, one shared durable
store for every workflow in a program.

**2. `on_entry`/`on_exit` actions with a real notification vocabulary.**
`send_email`/`send_sms`/`send_push` for direct channel sends, and
`notify` — the "smart" one — for "message this person however they're
actually reachable right now": online routes through a real-time push
bridge, offline falls back to email. This is what the design conversation
asked for by name ("send sms, or send email to an identity identified with
its role, send notifications if that user is online, or send push
notification if the user is not online").

**3. External triggers reuse the RPC layer that already exists, instead
of inventing a webhook subsystem.** Every top-level `fn` is already a
`POST /api/<fn>` endpoint (`serve.rs`) — a `workflow`'s "accept this OTP"
or "verify by clicking an email link" needs is not new HTTP routing, just
new *functions*, which `workflow_lower.rs` synthesizes:
`advance_<workflow>` for ordinary authenticated triggers,
`<event>_via_link` for unauthenticated, single-use, magic-link-token
triggers — built the same way `trade_finance.nir`'s hand-written
`decide_approval_via_link` already works, just generated instead of
copy-pasted per approval type.

**4. Provider configuration is runtime data, admin-editable, not compile-
time syntax.** An app declares an ordinary `struct` (e.g.
`EmailProviderConfig`) for its SMS/email/push provider settings — that
already gets a free CRUD screen via the existing `struct` → `ui_gen.rs`
pipeline (`docs/LANGUAGE.md` §11). No new `resource { ... }` declaration, no
secrets baked into `.nir` source: `send_email`/`send_sms`/`send_push`
read the live, admin-filled-in config row at send time.

## The construct, locked

```ebnf
workflow_decl  ::= "workflow" IDENT "{" data_block? state_decl+ "}"
data_block     ::= "data" "{" field ("," field)* ","? "}"
state_decl     ::= "state" IDENT "terminal"? "{"
                      on_entry_block?
                      on_exit_block?
                      transition*
                    "}"
on_entry_block ::= "on_entry" "{" action_call* "}"
on_exit_block  ::= "on_exit"  "{" action_call* "}"
action_call    ::= IDENT "(" (expr ("," expr)*)? ")"
transition     ::= "on" "link"? IDENT "->" IDENT
```

- `workflow`/`state` are real reserved keywords (like `struct`/`enum`/
  `screen`/`dashboard`/`module`). `data`/`on_entry`/`on_exit`/`on`/
  `terminal`/`link` are contextual — matched by identifier text only
  inside `parser.rs::parse_workflow_decl`/`parse_state_decl`, the same
  "keyword only within one specific syntactic slot" treatment `transact`'s
  own slot names (`network`/`verify`/`commit`/...) already get. `on` as a
  bare identifier is legal everywhere else in a program.
- `on_entry`/`on_exit` action calls are `TransactSlot`-shaped — a bare
  `name(args)` call, never an arbitrary expression, same "parse normally,
  then validate what came out" restriction `transact`'s own slots
  already enforce. Unlike `transact`'s slots, a workflow action call
  **may** name a builtin (`send_email` etc. are builtins) — the opposite
  restriction, because these builtins are exactly the point.
- Implicit bindings inside an `on_entry`/`on_exit` action call's own
  arguments: `instance_id: i64` (always); `data.<field>` for each `data`
  block field, read-only, deserialized from the stored instance row; and
  — only inside the `on_entry` of a state that declares a `link`-marked
  outgoing transition named `E` — `link_E`, a freshly minted,
  not-yet-consumed link-token value, minted immediately before that
  state's `on_entry` runs.
- A non-`terminal` `state` with no outgoing `on` needs to be
  `terminal`, or is a compile error (`WorkflowStateHasNoTransitions`) —
  a declared dead end has to be *declared* dead, not accidental.

## Desugaring (`workflow_lower.rs`)

Run once, right after parsing (`Parser::parse_program`'s own tail call) —
the same "pure lowering, zero new dispatch machinery" shape `module`
already uses (`docs/LANGUAGE.md` §12), just as its own pass instead of
parser-time flattening, since a workflow's lowering needs every state and
transition gathered first. `Program.workflows` is kept, not drained,
afterward — `typeck.rs::check_workflow_decl` (the deeper semantic rules:
every non-terminal state has a transition, every `data.<field>`/
`link_<Event>` reference resolves) reads the original syntax from there.

For `workflow KycOnboarding`:

- `enum KycOnboardingEvent { ...one zero-payload variant per distinct
  event name, first-appearance order... }` — the "small zero-payload
  enum" pattern `docs/LANGUAGE.md` §6b already documents as the fix for
  status-string data, applied to workflow events too.
- `struct KycOnboardingData { ...the `data` block's own fields... }` —
  even with zero fields, so `start_*`'s signature never needs a
  conditional shape. This is what makes `data.<field>` a real, typechecked
  binding rather than an opaque JSON blob.
- `struct KycOnboardingLinkToken { value: str }` — only if the workflow
  has at least one `link`-marked transition. A fresh, workflow-scoped
  carrier struct (not the user's own `Text`, if they happen to have one —
  `docs/LANGUAGE.md` §6b's convention name — since that would risk a name
  collision this doesn't need to risk).
- `fn start_kyc_onboarding(identity: Option(VerifiedIdentity), data: KycOnboardingData) -> Result(i64, WorkflowActionError)`
  — creates the durable instance (state = first-declared state), runs
  that state's `on_entry`, returns the new instance id. A real
  `POST /api/start_kyc_onboarding` endpoint, for free. `identity` is
  **optional** — §6 below ("who submitted this"): `Some(_)`'s `subject`
  is durably recorded as `started_by_subject`; `None` is the legitimate
  anonymous-start case this very example (`kyc_onboarding.nir`) uses.
- `fn advance_kyc_onboarding(identity: VerifiedIdentity, instance_id: i64, event: KycOnboardingEvent, payload: json) -> Result(bool, WorkflowActionError)`
  — the ordinary, authenticated transition entry point. `identity` is
  **required** here (§§1-4 above, "state ownership") — checked against
  the *current instance's* live state's `owner`, never statically.
- `fn email_verified_via_link(instance_id: i64, token: KycOnboardingLinkToken, payload: json) -> Result(bool, WorkflowActionError)`
  — one per distinct `link`-marked event name, **unauthenticated** (no
  `requires`, no `VerifiedIdentity` param) — same shape
  `trade_finance.nir:637-689`'s hand-written `decide_approval_via_link`
  already established, and deliberately **not** owner-checked either — a
  consumed, single-use link token is its own authorization.
- `fn list_kyc_onboarding_pending_for_me(identity: VerifiedIdentity) -> Result(json, WorkflowActionError)`
  — §§1-4 above, the queue read side.
- `fn list_kyc_onboarding_submitted_by_me(identity: VerifiedIdentity) -> Result(json, WorkflowActionError)`
  — §6 below, the "My Requests" read side.
- `fn get_kyc_onboarding_history(identity: VerifiedIdentity, instance_id: i64) -> Result(json, WorkflowActionError)`
  — §7 below, the audit-trail read side.

`payload` (`advance_*`/`*_via_link`'s trailing argument) is still not
threaded into `on_entry`/`on_exit` bindings — see "Deliberate non-goals"
below — but §7's audit trail now reads one field out of it
(`{"comment": "..."}`), a narrow, self-contained exception to that gap,
not a change to it.

## Runtime protocol (`interpreter.rs`, `workflow_log.rs`)

1. `__workflow_start`: creates the `workflow_instance` row (durable —
   `workflow_log.rs`, modeled directly on `transact_log.rs`'s open/write
   shape; SQLite-file-backed by default, or a shared Postgres database or
   Raft-replicated SQLite cluster for multi-instance, see that module's
   own doc comment), state = the first-declared state. Runs that
   state's `on_entry` actions.
2. `__workflow_advance`: looks up the instance's current state; a
   missing instance or an event with no matching transition out of that
   state are clean `Err(InstanceNotFound)`/`Err(NoSuchTransition)`, never
   a trap. Runs the *old* state's `on_exit` actions first — a failure
   here (retries exhausted) leaves the instance in the old state, so
   `advance_*` is safe to call again. Then durably records the
   transition (`workflow_history`, append-only — the same "log, don't
   mutate a stage field" shape `trade_finance.nir:28-30`'s own doc
   comment already calls out as the right one for n-eyes). Then runs the
   *new* state's `on_entry` actions — minting any `link`-marked outgoing
   transitions' tokens first, so an `on_entry` action can embed one (e.g.
   in an email's `vars`).
3. `__workflow_link_advance`: looks up the one not-yet-consumed link
   minted for `(instance_id, event)`, compares the presented token
   **constant-time** (`interpreter.rs::constant_time_eq` — the same
   timing-side-channel fix `trade_finance.nir:676`'s own magic-link
   compare already established), then consumes it via a single
   `UPDATE ... WHERE consumed = 0` (TOCTOU-safe: two simultaneous
   requests can't both win). A match with no exhaustion runs the normal
   `advance` path.
4. Every `on_entry`/`on_exit` action call's callee and already-evaluated
   arguments are durably recorded (`WorkflowLog::begin_pending_action`)
   *before* it's dispatched — the same "log intent, then act" ordering
   `transact_log.rs::begin_pending` gives `network`, so a crash between
   that write and the call actually running is resumable, not lost. Each
   call then gets a bounded, backoff retry — the same shape
   `run_transact_write_slot` gives `transact`'s `commit`/`compensate` (5
   attempts, `20ms << attempt` backoff), treating a trap *or* a
   `Result(_, _)`'s `Err` variant as failure. Success marks the row
   `done`. Exhausting the live budget **traps**
   `WorkflowActionPending { instance_id, state, action }` — never a
   guessed outcome, matching `TransactCommitPending`'s own "stuck and
   visible on purpose" precedent — but the row stays durably `pending`,
   picked up by `Interpreter::replay_pending_workflow_actions` (called
   once at `nirdosha serve` startup, right alongside
   `replay_pending_transactions`) the same way a crashed `transact` is.
   Arguments round-trip through JSON via `serve::encode_value`/
   `decode_value` — the same general struct/scalar codec the RPC layer
   already uses, decoded back on replay via the callee's *current*
   declared parameter types (no separate type information is stored) —
   so a callee whose signature changed, or that was removed, between the
   crash and the restart reports `WorkflowReplayOutcome::Stuck` with a
   named reason rather than guessing.

## `send_email`/`send_sms`/`send_push`/`notify`

```
enum Recipient { BySubject(str), ByRole(str) }
fn send_email(conn: db, to: Recipient, template: str, vars: json) -> Result(bool, WorkflowActionError)
fn send_sms  (conn: db, to: Recipient, template: str, vars: json) -> Result(bool, WorkflowActionError)
fn send_push (conn: db, to: Recipient, template: str, vars: json) -> Result(bool, WorkflowActionError)
fn notify(conn: db, mq: Mq, to: Recipient, template: str, vars: json) -> Result(bool, WorkflowActionError)
```

`Recipient`'s `str` payloads are the documented, precedented enum-variant
exemption from the fn-boundary `str` ban (`docs/LANGUAGE.md` §6b: "the check
only inspects a `fn`'s own declared parameter/return type expression" —
these are builtins besides, exempt by construction like every other
builtin).

- `conn: db` is explicit, matching every `db_query`/`db_execute`
  convention in this language — no implicit/global DB handle anywhere.
  `Recipient::ByRole(role)` fans out via `identity_directory`
  (`workflow_log.rs`, upserted on every successful `resolve_identity` in
  `serve.rs` — the one piece that didn't exist anywhere in this codebase
  before this feature: no reverse role→subjects lookup existed).
  `Err(NoRecipientsForRole)` if the role matches nobody.
- The actual transport is a **generic, provider-agnostic authenticated
  HTTPS POST** to an admin-configured endpoint — not SendGrid's/Twilio's/
  FCM's own exact API schema, which can't be verified without live
  accounts. `send_email` (same for `_sms`/`_push`) reads the first
  `active = 1` row of a fixed-name table — `email_provider_config`/
  `sms_provider_config`/`push_provider_config` — via `conn`:
  ```
  struct EmailProviderConfig {
      id: i64,
      active: bool,
      host: str,
      port: i64,
      path: str,
      api_key: str,
      from_address: str,
  }
  ```
  migrated by `nirdosha serve --db` like any other struct (this **is**
  the "communication control" — an ordinary admin-editable CRUD screen,
  not new UI work). POSTs `{"to","from","template","vars"}` as JSON with
  `Authorization: Bearer <api_key>`. No active row → `Err(ProviderNotConfigured)`;
  a non-2xx/connection failure → `Err(ProviderRequestFailed(message))`.
- `notify`'s presence bridge: `identity_presence` (`workflow_log.rs`),
  written only by two new `serve.rs` routes, `POST /api/_presence_connect`/
  `_disconnect` — a trusted external WS gateway (not an end user) reports
  connect/disconnect, authenticated by `--presence-token` (a service
  credential, constant-time compared) rather than a normal identity
  bearer token. **This repository has no WebSocket support and adds
  none** — verified before this feature was built (no `tungstenite`/
  upgrade-handling anywhere, `tiny_http` is strictly synchronous
  request/response); real-time delivery is a Redis pub/sub bridge
  instead: online → `notify` does a Redis `PUBLISH` on
  `nirdosha:push:<subject>` (a sibling to `mq_publish`/`mq_consume`'s
  existing `LPUSH`/`BLPOP`, same connection type — the `mq: Mq` argument
  is the same handle `mq_connect` already produces), which an external WS
  gateway is expected to `SUBSCRIBE` to and relay to that subject's live
  browser connection; offline → falls back to `send_email`. No
  `--presence-token` configured means the two routes 404 and `notify`
  always takes the offline path — a feature not opted into costs nothing,
  the same framing `docs/TRANSACT.md`'s row 6 already establishes.
  **2026-08-28 — the external WS gateway this bridge names now exists:**
  `crates/presence-gateway/` (its own crate — `README.md` there has
  the full protocol/design writeup), a small standalone process that
  terminates real browser WebSocket connections, independently verifies
  each one's identity token (its own `--jwks-file`/`--issuer`/
  `--audience`, deliberately not sharing code with `interpreter.rs`'s
  JWT verifier — see that crate's `src/jwt.rs` doc comment for why),
  calls `_presence_connect`/`_disconnect` as connections open/close
  (correctly ref-counted per subject for multi-tab), and relays each
  `nirdosha:push:<subject>` publish to the right live connection.
  Verified live end-to-end, not just built: a real `nirdosha serve`, a
  real Redis, a real browser-shaped `WebSocket` client, and a real
  `notify()` call round-trip correctly, both as a plain binary and as a
  built Docker image (`docker stop` shuts it down gracefully within
  Docker's default timeout, confirmed by the client actually receiving a
  clean close frame, not a connection reset).

## Deliberate non-goals (disclosed, not silently dropped)

- **This repository does not terminate WebSocket connections and adds no
  new transport.** `notify`'s real-time path is a Redis `PUBLISH` — a
  separate crate does the terminating (`crates/presence-gateway/`, immediately
  above), deliberately kept out of `compiler/` itself; nothing here
  changes.
- **At-least-once notification delivery, never exactly-once**, the same
  honest limit `docs/TRANSACT.md`'s own `network` idempotency-key section
  already discloses for the same underlying reason (no purely local
  mechanism can make a network call exactly-once).
- **No provider-specific API schemas.** One generic authenticated-POST
  transport for every channel; a specific vendor's own exact contract
  (SendGrid's `/v3/mail/send`, Twilio's REST API, FCM's HTTP v1 API) is
  future, dedicated-adapter work, not this version's job.
- **`identity_directory`'s role lookup is a `LIKE` match against raw
  claims JSON, not a real JSON-array membership query** — simple, honest
  about being simple, documented here rather than presented as more
  rigorous than it is.
- **`payload` (the last argument to `advance_*`/`*_via_link`) is accepted
  but not yet threaded into `on_entry`/`on_exit` bindings.** Reserved for
  a future increment; every current binding comes from `data`/`instance_id`/
  `link_<Event>` only.
- **No native codegen.** `workflow`-desugared functions call
  `send_email`/`notify`/`__workflow_*`, none of which are in
  `codegen.rs`'s `PHASE4_BUILTINS`/`PHASE5_BUILTINS`/... allowlists, so
  `nirdosha build`/`emit-llvm` rejects a program using `workflow` the
  same clean, disclosed way it already rejects one using `transact` —
  `check_supported` names the specific unsupported builtin, never a
  silent mis-compile.

## State ownership + a generated queue UI

**2026-08-26, built.** Found working through `scratch/extracted_typed_v1.json`'s
`WF-TRDPAY-001` example against the real, shipped six-eyes/Maker-Checker
implementation (`examples/trade-finance/trade_finance.nir`'s
`submit_approval`/`decide_approval`, which doesn't even use `workflow{}`):
before this, there was **no** way to say, in `.nir` source, who may act
on a `state`, and **no** generated UI anywhere a user could see "the
things currently waiting on me" and act on one — `ast::StateDecl` had no
owner field, `ui_gen.rs` had zero references to `WorkflowDecl` at all,
and the one hand-written approval screen `trade_finance.nir` actually
ships (`screen`-free, naming-convention-only `Approval`) was a read-only
list gated to a single fixed role, with no decide action and no per-row
"is this mine" filtering. The section below was a proposed design
sketch; every numbered piece is now real (`docs/ROADMAP.md` Track A item A13,
`crates/compiler/UI_DSL_TODO.md`) — updated in place to describe what actually
shipped rather than kept as a stale "not built" proposal, and extended
past the original sketch with three more real, shipped pieces this
session added on top of it (§6-§8 below): **who submitted this**
(`Option(VerifiedIdentity)` on `start_<workflow>`, a "My Requests" tab),
an **audit trail** (`get_<workflow>_history`: actor/via-link/comment per
transition), and a **broader enterprise catalog** disclosing what's
still genuinely not built (delegation, SLA/escalation timers, quorum,
bulk actions, an in-app notification inbox — see §9). Two pieces remain
deliberately not built, disclosed in their own sections: **quorum**
("N of these, not just one," six-eyes) is still extraction-schema
metadata only (`required_decisions`), not runtime-enforced; per-viewer
history ACL is a disclosed simplification (any signed-in identity may
view any instance's history today).

### Why this needs new machinery, not just a new field

A state's owner is a **runtime** question, not a static one — unlike an
ordinary `fn`'s `requires(role: ...)`, which is checked once per function
regardless of which instance is involved, `advance_<workflow>` is one
function serving every instance of that workflow; whether the *current
caller* may fire an event out of instance `N`'s *current* state depends on
which state instance `N` happens to be in *right now*. That check has to
happen inside `__workflow_advance` itself (interpreter-level, against the
instance's live row), not at `serve.rs::dispatch`'s static per-function
gate — the same reason row-level ACL (`docs/API_TRUST_MODEL.md` §6) can't be
expressed as an ordinary `requires(...)` either. This is the one genuinely
new piece of runtime logic the whole proposal needs; everything else below
is plumbing around it.

### 1. Grammar: `owner` on a `state`

Reuses the exact visibility-expression grammar `screen`'s `field { view:
role(...) }` already has (`typeck.rs::check_visibility_expr`) — no new
expression syntax:

```
state PendingSixEyes {
    owner: role("six_eyes_reviewer")
    on_entry {
        notify_six_eyes_reviewer(instance_id)
    }
    on Approved -> Approved
    on Rejected -> Rejected
}
```

`owner` is deliberately independent of `on_entry`'s `notify(...)` target —
they typically name the same role, but they answer different questions
("who's told" vs. "who may act") and nothing should force them to agree
syntactically. A state with no `owner` is unrestricted (any authenticated
caller may fire its transitions) — the same "default open unless you say
otherwise" posture `requires(...)` already has on ordinary functions
(`docs/ROADMAP.md` A10), so this needs the *same* typeck-warning treatment A10
shipped: an owner-less, non-terminal state should warn, not silently ship
open.

### 2. Runtime enforcement

`advance_<workflow>` gains a leading `identity: VerifiedIdentity`
parameter (a real, disclosed signature change from today's
`advance_kyc_onboarding(instance_id, event, payload)` — every existing
`workflow` program would need updating, the same class of migration the
str-ban and other breaking changes already went through, `docs/ROADMAP.md`
A4's compatibility-policy gap being exactly why this needs a real policy
before it ships). `__workflow_advance` looks up the instance's current
state's `owner` from the `WorkflowDecl` AST (already in scope — the
interpreter already holds the whole `Program`) and checks `identity`
against it with the same `identity_has_role`/`identity_has_mapped_role`
logic `serve.rs`'s own `requires(role: ...)` enforcement already uses —
no new authorization primitive, just a new call site for the existing one.
A caller who fails this gets a clean `Err(NotStateOwner)` (a new
`WorkflowActionError` variant), never a trap.

### 3. Read side: "what's waiting on me"

No new storage — `workflow_instance` already durably tracks each
instance's current state (`workflow_log.rs`). `workflow_lower.rs`
additionally synthesizes, per workflow:

```
fn list_<workflow_snake>_pending_for_me(identity: VerifiedIdentity) -> Result(json, WorkflowActionError)
```

— queries instances whose current state's `owner` the caller satisfies,
the same query shape `list_approval`/`list_audit_log` already run by hand
today, just generic and auto-generated instead of hand-rolled per app.

### 4. UI: a new screen archetype, plus real navigation

This is the part `ui_gen.rs` has no precedent for at all: every existing
generated screen has a **fixed** action set for the whole table (`list`/
`create`/`update`/`delete`, declared once). A workflow queue needs a
**per-row** action set, because different rows can be in different
states with different outgoing events. Proposed shape:

- One new top-level nav section, **"Workflows"** — one entry per declared
  `workflow`, exactly answering the "a menu of workflows, click one, see
  its entries" question directly. Needs `ui_gen.rs` to read
  `Program.workflows` at all, which it doesn't today.
- Clicking a workflow opens its queue: `list_<workflow>_pending_for_me`'s
  rows, columns from the `data` block (same field→control inference
  `screen`s already have), a status badge for the current state, and —
  per row — a button per outgoing event of *that row's own current
  state*, calling `advance_<workflow>(event, ...)`. A state's own human-
  readable name is currently just its PascalCase identifier
  (`PendingSixEyes`); a `label: "string"` kv-entry on `state` (same shape
  `screen`'s own `title`) would give this a real display string instead
  of splitting the identifier client-side.
- Clicking an instance row opens its detail: full `data`, current state,
  and — real, already-durable data nothing new needs building —
  `workflow_history`'s append-only transition log as an audit trail.

### 5. The thing this still can't express: quorum ("N of these, not just one")

`owner` answers "who may decide," not "how many *distinct* such people
must decide before advancing" — six-eyes needs the latter. The real
shipped system gets this today via `required_eyes: i64` counted against
an `approval_decision` table, a fundamentally different shape than a
single `on Event -> State` transition firing once. Bolting `owner` onto
`state` does not, by itself, correctly model quorum — a state with
`owner: role("six_eyes_reviewer")` alone would let the *first* qualifying
person's decision fire the transition immediately, which is Maker-Checker
semantics (one decider), not six-eyes (several, independent, distinct).
Getting this right needs either a new transition shape (`on N-of-role(X)
event -> target`, real new grammar, not sketched here) or keeping quorum
counting in a hand-rolled table the way it works today and layering
`owner`/the queue UI only on top of that existing mechanism instead of
`workflow{}`'s own transitions. **Not resolved here** — named so a first
implementation doesn't quietly assume `owner` alone covers six-eyes when
it demonstrably doesn't.

### What's parked in the extraction schema for this already

`scratch/prompt_v2.txt`/`extraction_schema.rs` now capture `owner_role`/
`owner_claim`/`label` per extracted `state`, and a `required_decisions`
hint distinguishing a single-decider state from a quorum one — data ready
to feed this design the moment it's built, explicitly *not* implying any
of it is enforced today (`docs/ROADMAP.md` tracks the compiler-side gap
separately from the schema's ability to record the intent).

### 6. "Who submitted this" — built

Surfaced by a direct question against `examples/purchase_approval.nir`'s
own demo: a real approval-chain app needs the *requester* to be able to
track their own submission's status, not just the approvers to see their
own queues — arguably the single most basic expectation of *any*
enterprise approval system (ServiceNow, SAP Business Workflow, Salesforce
Approval Processes, Concur all put "my requests" one click from the
homepage). Nothing in the original design above provided it: `start_*`
had no identity slot at all, so no instance could be traced back to
whoever created it.

- `start_<workflow>` gains a leading `identity: Option(VerifiedIdentity)`
  param — **optional**, unlike `advance_<workflow>`'s required one,
  because starting a workflow is legitimately anonymous in real programs
  today (`kyc_onboarding.nir`'s own public intake: a brand-new applicant
  has no account yet). `serve.rs::dispatch` gained a genuinely new,
  general capability for this, not workflow-specific: a fn param typed
  `Option(VerifiedIdentity)` is injected `Some(id)` when a valid bearer
  token was presented, `None` when absent — **never** a 401 either way,
  unlike a bare `VerifiedIdentity` param. Any program can use this for
  "personalize when signed in, still work when not," not just workflows.
- `workflow_instance` (`workflow_log.rs`) gains a `started_by_subject`
  column, populated from `identity.subject` when `Some(_)`, left `NULL`
  for a legitimate anonymous start.
- A new synthesized read fn, `list_<workflow>_submitted_by_me(identity:
  VerifiedIdentity) -> Result(json, WorkflowActionError)` — `identity`
  here is **required** (unlike `start_*`'s optional one): "show me my
  own requests" is meaningless with nothing to scope it to.
- Generated UI: the "Workflows" queue screen gained a second tab, **"My
  requests"**, next to "Waiting on me" — same row shape, but read-only
  (no action buttons; a requester watches, they don't decide).

### 7. Audit trail — built

The other near-universal enterprise expectation: *who* approved/rejected
*what*, *when*, and *why* (SOX/banking-regulation territory — every
system above has some version of this). `workflow_history` already
existed, durably, from `docs/WORKFLOW.md`'s very first version — it just
wasn't exposed to a caller, and didn't record *who* acted or *why*.

- `workflow_history` gains `actor_subject` (the acting identity's
  `subject`, `NULL` only for the magic-link path — see below),
  `via_link` (whether this transition fired through an unauthenticated
  magic-link click rather than an ordinary authenticated call), and
  `comment` (free text, see next point).
- `advance_<workflow>`'s trailing `payload: json` argument — accepted
  since the very first version of this feature but never threaded
  anywhere (the "deliberate non-goals" section above still correctly
  says it isn't threaded into `on_entry`/`on_exit` bindings) — is now
  read for a `{"comment": "..."}` string and logged to the history row.
  The generated UI prompts for one (optional) when a decision button is
  clicked. This is a narrow, self-contained use of `payload`, not the
  fuller on_entry/on_exit threading that section still discloses as not
  done.
- A new synthesized read fn, `get_<workflow>_history(identity:
  VerifiedIdentity, instance_id: i64) -> Result(json, WorkflowActionError)`
  — the full transition log, oldest first. **Disclosed simplification,
  not a real per-viewer ACL**: any signed-in identity may view any
  instance's history today, the same way `identity` on this fn exists
  only to demand *a* signed-in caller, not to scope what they see. A
  real system would restrict this to participants (any state's owner
  across the workflow's whole lifetime) and the original requester —
  building that needs a join across every state's `owner` plus
  `started_by_subject`, not sketched here.
- Generated UI: every row, in both "Waiting on me" and "My requests",
  gets a "History" button expanding an inline transition log below it.

### 8. Bug found and fixed in passing: `payload: json` never actually worked over HTTP

While wiring §7 above, found `serve.rs::decode_value` had **no `Ty::Json`
arm at all** — any `fn` param typed `json` (which `advance_<workflow>`'s
own `payload` always was, since the very first version of this feature)
unconditionally 400'd over a real `nirdosha serve` request, regardless of
what the caller sent. Nothing in this feature's design depended on that
working before now, so it went unnoticed; fixed with a direct pass-
through arm (`Ty::Json => Ok(Value::Json(Arc::new(json.clone())))`),
unrelated to ownership/audit specifically but blocking §7's comment
capture from working end to end.

### 9. The enterprise catalog: what real systems have that this still doesn't

A direct answer to "what would it take to handle every real-world
enterprise approval pattern" — drawing on the same shape ServiceNow, SAP
Business Workflow, Salesforce Approval Processes, Concur, Camunda/
Flowable, and banking maker-checker/six-eyes systems all converge on.
Each row below is a real, common pattern; "Have?" says whether today's
`workflow`/`ui_gen.rs` can express it, not whether it's a good idea.

| Pattern | Real-world example | Have? | What it would take |
|---|---|---|---|
| Sequential multi-level approval, different role per stage | Purchase orders, expense reports (this doc's own `examples/purchase_approval.nir`) | **Yes** | Already built — this is exactly §§1-7 above. |
| Quorum / N-of-role ("six-eyes") | Large wire transfers, dual-control banking release | **No** | §5 above — needs `on N-of-role(X) event -> target` grammar or a hand-rolled decision-count table; `owner` alone models one decider, not several. |
| Who submitted this / "my requests" | Every system listed above | **Yes** | §6 above. |
| Audit trail (who/when/why) | SOX compliance, banking regulation | **Yes**, with the disclosed per-viewer-ACL gap in §7 | §7 above; a real per-viewer ACL (participants + requester only) is the remaining gap. |
| Delegation / out-of-office reassignment | "I'm on leave, my approvals route to my manager for two weeks" (every system above has this) | **No** | Needs a new admin-editable `WorkflowDelegation`-shaped struct (`from_subject, to_subject, workflow_name, starts_at, ends_at`) and a second check in `identity_satisfies_owner` — real, buildable, same "ordinary struct, free CRUD screen" convention `RoleMapping`/`EmailProviderConfig` already use, just not built. |
| SLA / escalation timers | "If not acted on in 48h, escalate to the owner's manager or notify again" (universal in enterprise workflow engines) | **No, structurally — tracked as `docs/ROADMAP.md` Track A item A15** | `docs/WORKFLOW.md`'s own "Deliberate non-goals" section already discloses this: **there is no scheduling/cron primitive in Nirdosha at all**. The `state`/`on_entry` shape is durable and retry-safe, but nothing inside the language can fire "48 hours from now" on its own — this needs an *external* scheduler calling `advance_<workflow>`/a new escalation fn, same as every other "nightly" workflow already has to work this way. A real, scoped proposed design (`state { sla: "<duration>" }` + a `list_<workflow>_overdue()` read fn for whatever external scheduler polls it) is in `docs/ROADMAP.md` A15, not sketched here. |
| Bulk actions ("approve all 12 selected") | Any queue-shaped enterprise UI | **No** | Pure UI-layer batching over the existing `advance_<workflow>` calls, one request per selected row — no new compiler construct needed, just not built in `ui_gen_template.html` yet. |
| In-app notification inbox (persisted, browsable later) | Almost every enterprise app's bell icon | **Already possible today, not a compiler gap** | `on_entry`/`on_exit` can call *any* function, not just `send_email`/`send_sms`/`notify` — an ordinary user-defined `fn` that `db_execute`s an insert into your own `struct Notification { ... }` (a real, free CRUD screen) gives a persisted, browsable inbox with zero new grammar. What's still missing is only a UI *bell icon* convention (`ui_gen.rs` has no special-cased "notification" struct today — it would just render as an ordinary screen, not a badge in the nav bar). |
| Unified cross-workflow "Approvals" inbox (one queue merging every workflow, not one nav tab per workflow) | Any org with more than one approval process | **No** | Pure UI aggregation over each workflow's own `list_<workflow>_pending_for_me` — no new compiler construct, just not built (today's "Workflows" nav is one entry per `workflow`, not a merged view). |
| Reporting/analytics (cycle time, bottleneck stage, volume by requester) | Every workflow engine's dashboard | **Already possible today, not a gap** | `workflow_history` has everything a `db_query`-driven `stat_*`/`chart_*` fn needs — but `workflow_history`/`workflow_instance` live in `workflow_log.rs`'s own private SQLite store, not the app's `--db` database, so a report can't `db_query` them directly today; the existing dashboard convention (`docs/LANGUAGE.md` §11) would need a new builtin exposing this store to ordinary SQL, or a periodic export, to actually wire up. |
| Real-time push to an open browser tab | Slack-style live badge updates | **No, structurally, by design** | Already disclosed in "Deliberate non-goals" above: `notify()`'s online path assumes an *external* WS gateway relaying its Redis `PUBLISH`; this repository terminates no WebSocket connections itself. |
| Visual drag-and-drop workflow designer | Most enterprise workflow *products'* own admin UI | **No, out of scope by design** | Orthogonal to a compiled DSL — `workflow { ... }` is source code, the same way `struct`/`fn` are; a visual designer is a separate tool that would *generate* `.nir` source, not a compiler feature. |

Two rows are marked **Yes** because this session built them; the rest
are named here specifically so a future session (or reader) doesn't have
to re-derive "does Nirdosha have X" from scratch — each row's "what it
would take" is a real, scoped starting point, not a vague TODO.

**The exact reach of `on_entry`/`on_exit`, worth stating plainly:** they
can call *any* function — not just `send_email`/`send_sms`/`send_push`/
`notify`, but an ordinary user-defined `fn` doing `http_post` (call any
webhook), `db_execute` (persist anything to your own `--db` tables, e.g.
a real notification-inbox row), or both. That closes the notification-
inbox row above for free. What it structurally *cannot* do: fire with no
transition at all (SLA/escalation needs something external calling back
in *after a delay with no human action*, which nothing in this language
provides) or change who is *authorized* to act (`owner` is checked
before `on_entry`/`on_exit` ever run, so delegation needs the owner
check itself to consult a delegation table — notifying a stand-in
doesn't authorize them).
