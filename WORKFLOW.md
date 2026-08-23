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
