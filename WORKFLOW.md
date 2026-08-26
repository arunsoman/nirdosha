# Nirdosha `workflow` — durable, notification-driven state machines

This is the Nirdosha-native answer to "we need a real construct for
multi-step, human-in-the-loop processes" (user onboarding, KYC approval,
salary disbursal, and "many more" like them) — see the design
conversation this doc grew out of, which first tried and rejected a
general-purpose async/continuation-based `durable fn` (`PROTOLANG_PORT.md`
row 5/row 4: Nirdosha's concurrency story is already `spawn`+`chan`, and
`transact` already covers durable effect sequencing; neither needed
reopening). The actual gap, once n-eyes approval was built by hand in
`trade_finance.nir`, was narrower: a *named, multi-state* pipeline with
notification actions and external triggers, generalizing the
`status TEXT` + hand-rolled `CREATE TABLE`/append-only-log pattern every
one of those processes was independently re-deriving.

The finding that matters, same shape as `TRANSACT.md`'s own: **this
doesn't need a new runtime.** `workflow` is a compiler-stage desugaring —
every `workflow` block becomes ordinary `fn`/`enum`/`struct` declarations
(`workflow_lower.rs`) that flow through every existing pass (typeck, the
interpreter, `nirdosha serve`'s automatic `POST /api/<fn>` RPC exposure)
unchanged. The only genuinely new runtime pieces are: a durable SQLite
store for instance state (`workflow_log.rs`, modeled directly on
`transact_log.rs`), and four new interpreter builtins
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
pipeline (`LANGUAGE.md` §11). No new `resource { ... }` declaration, no
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
already uses (`LANGUAGE.md` §12), just as its own pass instead of
parser-time flattening, since a workflow's lowering needs every state and
transition gathered first. `Program.workflows` is kept, not drained,
afterward — `typeck.rs::check_workflow_decl` (the deeper semantic rules:
every non-terminal state has a transition, every `data.<field>`/
`link_<Event>` reference resolves) reads the original syntax from there.

For `workflow KycOnboarding`:

- `enum KycOnboardingEvent { ...one zero-payload variant per distinct
  event name, first-appearance order... }` — the "small zero-payload
  enum" pattern `LANGUAGE.md` §6b already documents as the fix for
  status-string data, applied to workflow events too.
- `struct KycOnboardingData { ...the `data` block's own fields... }` —
  even with zero fields, so `start_*`'s signature never needs a
  conditional shape. This is what makes `data.<field>` a real, typechecked
  binding rather than an opaque JSON blob.
- `struct KycOnboardingLinkToken { value: str }` — only if the workflow
  has at least one `link`-marked transition. A fresh, workflow-scoped
  carrier struct (not the user's own `Text`, if they happen to have one —
  `LANGUAGE.md` §6b's convention name — since that would risk a name
  collision this doesn't need to risk).
- `fn start_kyc_onboarding(data: KycOnboardingData) -> Result(i64, WorkflowActionError)`
  — creates the durable instance (state = first-declared state), runs
  that state's `on_entry`, returns the new instance id. A real
  `POST /api/start_kyc_onboarding` endpoint, for free.
- `fn advance_kyc_onboarding(instance_id: i64, event: KycOnboardingEvent, payload: json) -> Result(bool, WorkflowActionError)`
  — the ordinary, authenticated transition entry point.
- `fn email_verified_via_link(instance_id: i64, token: KycOnboardingLinkToken, payload: json) -> Result(bool, WorkflowActionError)`
  — one per distinct `link`-marked event name, **unauthenticated** (no
  `requires`, no `VerifiedIdentity` param) — same shape
  `trade_finance.nir:637-689`'s hand-written `decide_approval_via_link`
  already established.

`payload` is accepted by `advance_*`/`*_via_link` for signature symmetry
with a future increment, but is **not yet threaded into `on_entry`/
`on_exit` bindings** — see "Deliberate non-goals" below.

## Runtime protocol (`interpreter.rs`, `workflow_log.rs`)

1. `__workflow_start`: creates the `workflow_instance` row (durable,
   SQLite — `workflow_log.rs`, modeled directly on `transact_log.rs`'s
   open/write shape), state = the first-declared state. Runs that
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
exemption from the fn-boundary `str` ban (`LANGUAGE.md` §6b: "the check
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
  the same framing `TRANSACT.md`'s row 6 already establishes.

## Deliberate non-goals (disclosed, not silently dropped)

- **This repository does not terminate WebSocket connections and adds no
  new transport.** `notify`'s real-time path is a Redis `PUBLISH` — an
  external WS gateway process is assumed, not built here.
- **At-least-once notification delivery, never exactly-once**, the same
  honest limit `TRANSACT.md`'s own `network` idempotency-key section
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

## Proposed, not built: state ownership + a generated queue UI

**2026-08-26.** Found working through `scratch/extracted_typed_v1.json`'s
`WF-TRDPAY-001` example against the real, shipped six-eyes/Maker-Checker
implementation (`examples/trade-finance/trade_finance.nir`'s
`submit_approval`/`decide_approval`, which doesn't even use `workflow{}`):
today there is **no** way to say, in `.nir` source, who may act on a
`state`, and **no** generated UI anywhere a user could see "the things
currently waiting on me" and act on one. Verified directly, not assumed:
`ast::StateDecl` has no owner field; `ui_gen.rs` has zero references to
`WorkflowDecl` at all; the one hand-written approval screen
`trade_finance.nir` actually ships (`screen`-free, naming-convention-only
`Approval`) is a read-only list gated to a single fixed role, with no
decide action and no per-row "is this mine" filtering. This section is a
proposed design to close that gap — a sketch for review, matching every
other `[OPEN]`/proposed section this file and `API_TRUST_MODEL.md`
already use that convention for, **nothing here is built**.

### Why this needs new machinery, not just a new field

A state's owner is a **runtime** question, not a static one — unlike an
ordinary `fn`'s `requires(role: ...)`, which is checked once per function
regardless of which instance is involved, `advance_<workflow>` is one
function serving every instance of that workflow; whether the *current
caller* may fire an event out of instance `N`'s *current* state depends on
which state instance `N` happens to be in *right now*. That check has to
happen inside `__workflow_advance` itself (interpreter-level, against the
instance's live row), not at `serve.rs::dispatch`'s static per-function
gate — the same reason row-level ACL (`API_TRUST_MODEL.md` §6) can't be
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
(`ROADMAP.md` A10), so this needs the *same* typeck-warning treatment A10
shipped: an owner-less, non-terminal state should warn, not silently ship
open.

### 2. Runtime enforcement

`advance_<workflow>` gains a leading `identity: VerifiedIdentity`
parameter (a real, disclosed signature change from today's
`advance_kyc_onboarding(instance_id, event, payload)` — every existing
`workflow` program would need updating, the same class of migration the
str-ban and other breaking changes already went through, `ROADMAP.md`
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
of it is enforced today (`ROADMAP.md` tracks the compiler-side gap
separately from the schema's ability to record the intent).
