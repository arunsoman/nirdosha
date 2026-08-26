# Nirdosha mobile — native iOS/Android app generation (design)

**Status: design only. Nothing in this document is implemented.** No
`mobile_gen.rs`, no `emit-mobile` CLI verb, no generated Swift/Kotlin
anywhere in this repo as of this writing. This doc is the spec Track D
(`ROADMAP.md`) executes against, written *before* the first line of it
lands — the same order `WORKFLOW.md`/`TRANSACT.md` were written in for
their own features, not after.

This is the Nirdosha-native answer to "generate a real iOS/Android app,
not just a phone-sized web view" — grown out of a research pass over
`compiler/examples/*.nir` (see the "why" section below) plus a full
read of `ui_gen.rs`/`ui_gen_template.html`/`serve.rs`, done specifically
to answer whether a native backend could reuse what `emit-ui` already
has, or would need its own struct/fn-introspection pass duplicated.
It can reuse it completely — that's the finding this whole doc is
built on.

## The finding that matters

**This doesn't need a new frontend.** `ui_gen.rs::build_screens` already
walks the typechecked AST and produces a target-agnostic IR — `Screen`/
`FieldSpec`/`Action`/`Metric` — with zero HTML in it. `ui_gen_template.html`
is just the one existing *renderer* of that IR: a fixed set of generic
JS functions keyed off `field.control`/`action.kind` (`buildFieldControl`,
`renderListScreen`, `renderSingularScreen`, `renderDashboard`, `renderLogin`),
never one function per struct. `serve.rs` is the one HTTP backend any
renderer talks to (`POST /api/<fn>`, `POST /_nirdosha/table/<name>`),
and it independently re-checks every `requires(role/claim: ...)` gate
server-side regardless of what a client claims — a native client
inherits that same trust boundary for free, it doesn't need to
re-implement it.

A native backend is therefore **a second renderer of the same IR**, not
a second UI-DSL, a second manifest builder, or a second RPC layer. It
differs from the web renderer in exactly one respect: instead of
substituting `MANIFEST`/`STATS`/`CHARTS` JSON into one HTML file at
generation time and interpreting it client-side at runtime, it emits
real per-screen Swift/Kotlin source at generation time, into a real
Xcode/Gradle project — because branding (app icon, bundle ID), platform
entitlements (Face ID usage string, push capability), and app-store
distribution all require a real native project to exist, not a generic
WebView shell pointed at the same manifest.

## Why (the research this is grounded in)

A survey of every `.nir` file in `compiler/examples/` split into two
groups: files with zero structs bearing `list_`/`create_`/`update_`/
`delete_`/`get_` fns — `wargame_agents.nir`, `observability.nir`,
`sandbox_channels.nir`, `transact.nir`, `transact_cross_process.nir`,
`row12_identity.nir`, `payments_mock.nir` — pure language/runtime
demos or backend stubs, **zero** screens, categorically irrelevant to
any UI target, mobile included; and files that do produce screens,
which split further by how much a *native* renderer specifically would
add over what the web renderer already gives them:

- **`kyc_onboarding.nir`** (KYC/compliance onboarding, a `workflow{}`
  state machine with `on_entry` `send_email`/`notify` hooks) and
  `trade_finance.nir`'s Module 2 (KYC/KYB onboarding) — the strongest
  case. Real-world KYC needs ID/document scanning and a selfie liveness
  check, neither of which a browser can do as well as a native camera
  API, and the workflow already has a real notification vocabulary
  (`send_push`/`notify`) waiting for a real push transport.
- **`trade_finance.nir`**'s Module 1 (Maker-Checker/6-Eyes approval
  governance) — second-best case: reviewers approving high-value
  transactions benefit from push-on-pending-approval and a biometric
  step-up before signing off, rather than a bearer token alone.
- **`store.nir`** (e-commerce CRUD + checkout) and **`ui_todo.nir`**
  — plain CRUD; a responsive web `emit-ui` output already serves this
  well, native mainly buys nicer look-and-feel and app-store presence,
  not a new capability.
- **`rev_assurance.nir`** (analyst dashboard/case queue) — server/
  desktop-shaped; native adds little beyond a push alert on a new
  discrepancy.

This ranking is what "Standard" vs. "Rich" profile (below) is built
around: Standard serves every one of these equally (it's the same
fidelity as web, just native chrome), Rich's individual capabilities
are worth the extra server-side primitive they each require in
proportion to how many real examples above would actually use them.

## What's already reusable, unchanged

| Reused as-is | Source | Native renderer's job |
|---|---|---|
| `Screen`/`FieldSpec`/`Action`/`Metric` IR | `ui_gen.rs::build_screens`/`manifest_json` | Consume directly — same call, same struct shapes, zero duplication of struct/fn introspection |
| `screen X { field/action/... }` override surface | `ast::ScreenDecl`, `ui_gen.rs::apply_field_overrides` | Same overrides apply to both renderers; native adds new override keys only for Rich-profile hooks (see below), it doesn't reinterpret the existing ones |
| `POST /api/<fn>`, `POST /_nirdosha/table/<name>` | `serve.rs::dispatch`/`dispatch_table_query` | Native networking layer calls the identical routes the web JS's `callFn`/`fetchTablePage` already call — no new server routes for Standard profile |
| Server-side `requires(role/claim)` re-check, view/edit field redaction | `serve.rs::resolve_identity`/`redact_gated_fields`/`check_edit_gates` | Inherited automatically — a native client attempting to bypass its own client-side gating hits the same 401/403/redaction the web client does |
| Value encoding (`Option`→`null`, zero-payload enum→bare string, struct→object, `Result`→`{ok}`/`{err}`) | `serve.rs::encode_value`/`decode_value` | Native JSON decoder implements the identical mapping, one-to-one with `ui_gen.rs::build_field`'s control derivation |
| `Theme` (4 color roles × 2 modes, 3 radii, 1 font) | `ui_gen.rs::Theme`/`theme_override_css` | Same JSON file maps onto SwiftUI `Color`/`Font` and Compose `MaterialTheme.colorScheme` — third consumer of one theme file, not a new theming system |
| `effect_badges` (`network`/`io`/`concurrent`) | `ui_gen.rs::effect_badges` | Same display-only chips render as native UI hints (e.g. a small network glyph on a button) — still never a gate, same as web |

Nothing in this table is new work. It's the argument for why Standard
profile (below) is comparatively small.

## Architecture

New module `compiler/src/mobile_gen.rs`, with two emit functions
(`generate_ios`/`generate_android`) taking the exact same `(&Program,
&HashMap<String, FnEffects>, Option<&str> /* identity_base */, bool /*
server_table_api */, Option<&Theme>)` signature `ui_gen::generate`
already takes — same inputs, different output shape: instead of one
`String` (HTML), a `Vec<(PathBuf, String)>` (project-relative path →
file contents) written out under `-o <dir>`.

New CLI verb, mirroring `emit-ui`'s own flags:

```
nirdosha emit-mobile <file.nir> --target ios|android [--profile standard|rich]
                      [-o <dir>] [--identity-base URL] [--theme theme.json]
```

Defaults: `--profile standard`, `-o` defaults to `./ios-app`/`./android-app`.
`--target` is required (no default — unlike web, "which native platform"
is never inferable). `server_table_api` is always `true` for a mobile
target: an installed app is never a one-shot static artifact the way
`emit-ui`'s HTML file can be (opened directly with `file://`, no server
running) — it only makes sense pointed at a live `nirdosha serve
--db <path>` instance, so the generated project's networking layer
always expects the paginated table route to exist, and errors clearly
at request time (not generation time) if it 404s against a `serve`
instance started without `--db`.

**Split between generated and checked-in code**, the same "code
proportional to the rules, not to the number of structs" property
`ui_gen_template.html` already has for web: a small **runtime library**
(`compiler/src/mobile_runtime/ios/*.swift`, `.../android/*.kt` —
embedded into the `nirdosha` binary via `include_str!`, the same trick
`codegen.rs`'s `RUNTIME_KERNELS_LIB` uses for the LLVM backend's static
lib) does the actual rendering: one generic field-control view per
`control` kind (Text/Number/Checkbox/Date/Select/StructGroup/ReadOnly),
one generic list-screen view (table + create form + pager + per-row
actions), one generic singular-screen view, one dashboard view, one
login view, a networking client (`callFn`/`fetchTablePage` equivalents),
a Keychain/Keystore-backed identity store, and a theme mapper. This is
copied unmodified into every generated project. **Generated, per-app
code** is thin: one Swift `struct`/Kotlin `data class` per `Screen`
declaring its own fields/actions/metrics as typed data (not logic) that
the runtime library's generic views render — real, compilable,
per-struct source (satisfying "leverage native features via real
codegen," not a bundled interpreter reading a JSON blob at runtime) but
sized like a manifest entry, not like a hand-written screen.

Also generated per app: `Info.plist`/`AndroidManifest.xml` (app name,
bundle ID/package name, icon slot, permission strings for whichever
Rich-profile capabilities are actually used — e.g. `NSFaceIDUsageDescription`
only emitted if any action anywhere declares `step_up: biometric`),
and a minimal buildable project wrapper (Swift Package / bare
`.xcodeproj`; Gradle module) so the output opens directly in Xcode /
Android Studio without hand-assembly.

### Per-target screen/dashboard exclusion (`target:` key)

Not every screen belongs on every renderer — a dense admin table is a
web-shaped screen, a future camera-capture flow (blocked on `D3`,
below) will be a mobile-only one. Rather than a parallel declaration
syntax, this is one more optional `kv_entry` on `screen_decl` and on
`dashboard`'s `tile`/`chart` entries: `target: "web"` / `"mobile"` /
`"all"` (default `"all"` — an existing `.nir` file with no `target` key
anywhere behaves exactly as it does today, on both renderers). It needs
**no grammar change at all**: `screen_item`/`dashboard_item` already
reduce to `kv_entry ::= ident ":" expr` (`GRAMMAR.md`), the same
generic production `title`/`list`/`create` already go through — only
`typeck.rs` (a new `TypeErrorKind` if `target`'s value isn't one of the
fixed three strings) and `ui_gen.rs` need real work.

Concretely: `Screen`/`Metric` (`ui_gen.rs`) each gain a `target: Target`
field (`enum Target { Web, Mobile, All }`, defaulting to `All` when the
key is absent — same "absent key = today's behavior" pattern every
other optional `screen`/`dashboard` key already follows). `manifest_json`
filters `Screen`/`Metric` lists per consumer: `ui_gen::generate`
(`emit-ui`/`serve`) keeps only `Web`/`All`, `mobile_gen::generate_ios`/
`generate_android` (once built) keep only `Mobile`/`All`. Both
renderers read the one already-filtered manifest — neither one carries
its own exclusion logic, the same "one IR, thin renderers" property the
rest of this doc is built on.

**This is a `ui_gen.rs` change today, not something deferred until
mobile exists.** Since `Screen`/`Metric` are the one shared IR both
renderers consume, and web is the only renderer that exists right now,
`target: "mobile"` has to make a screen disappear from `emit-ui`'s own
output *before* `mobile_gen.rs` is written — otherwise a `.nir` author
declaring a mobile-only screen today would see it wrongly rendered on
web with no native counterpart to fall back to. That's why this lands
as part of `D1`'s own scope (`ROADMAP.md` Track D), not a `D6` bolted
on after: `D1` is the first point both the IR shape and web's filtering
behavior need to be right.

**Interacts with `module` grouping** (LANGUAGE.md §12): a `target`-
excluded screen is also excluded from whichever `module` nav section it
would have grouped under on the renderer it's excluded from — an empty
module (every member screen excluded on this target) simply emits no
nav entry, rather than an empty group.

## Standard profile — ships first, zero new server primitives

Same fidelity as the web renderer, native chrome: SwiftUI `List`/`Form`/
`NavigationStack` and Jetpack Compose `LazyColumn`/`Scaffold` in place of
`ui_gen_template.html`'s DOM, same MD3-derived `Theme` tokens, same nav
grouping (`module "Name" { ... }` sections), same dashboard tiles/bar
charts, same login screen shape (`identity_base` real-POST or
localStorage-stub mode, unchanged). Every one of the "already reusable"
rows above is the entire feature set. No new `ScreenDecl` grammar, no
new builtins, no new `serve.rs` routes.

This is the only profile Track D's first milestone (`D1`) needs to
ship. It's real, deployable, brandable native apps for every example in
the "why" section above — it just doesn't yet touch a camera, a
fingerprint sensor, or a push notification tray.

## Rich profile — one capability per new server-side primitive

Each of these is independent (no ordering constraint between them),
each gated behind its own `ROADMAP.md` Track D item, and each is
**named, not designed in depth, here** — the same "disclosed gap, not
silently dropped" treatment `WORKFLOW.md`'s presence bridge and
`nirdosha_row12_functions_identity.md`'s unimplemented OIDC/PKCE
sections get. A full design for any one of these should get its own
doc-update pass the day it's actually built, per this repo's own "docs
land with the phase they document" rule — this section is a pointer to
what that pass will need to decide, not a substitute for it.

**Biometric-gated step-up on sensitive actions** (`D2`). Nothing in
`nirdosha_row12_functions_identity.md`'s identity model is shaped for
this today: `VerifiedIdentity`/`TokenReference` are server-side cache
slots, `ApplicationSession` is explicitly an HTTP-only browser cookie
(§10 of that doc), none of them are a credential a native app could
hold in Keychain/Keystore. Needs a new device-bound artifact — likely
built on `RefreshTokenHandle`'s existing shape, exchanged once after a
normal login and then unlocked locally via Face ID/Touch ID/BiometricPrompt
before each use, never replacing the server's own `requires(role/claim)`
re-check, only gating whether the native app is willing to *present*
the stored credential. New `ScreenDecl` surface to name which actions
require it: `action ... { step_up: biometric }` (or a screen-level
default), extending `ActionDecl`'s existing `style`/`confirm` entries
the same way.

**Camera/document capture on upload-shaped fields** (`D3`). Blocked on
something more fundamental than mobile itself: **no file/blob/attachment
type exists anywhere in Nirdosha** — confirmed absent, not merely
undocumented (`trade-finance/todo.md` names this explicitly: every
"document" field in that 9-module app is a `str` a human types the
metadata of by hand). A camera-capture UI has nowhere to put its output
until this lands — a new type/builtin (e.g. `struct Attachment { url:
str, content_type: str, size: i64 }` populated by a new upload endpoint,
or an affine `blob` handle following the `box`/`db`/`file` precedent)
is a real product decision, not a mobile-specific one, and belongs in
its own design pass, likely its own `.md` file, before `mobile_gen.rs`
can render anything richer than the current `"readonly"` fallback for
an upload-shaped field.

**Push notifications tied to workflow transitions** (`D4`).
`send_push`/`notify` already exist (`WORKFLOW.md`) and already read an
admin-editable `push_provider_config` row — but the transport behind
them today is the same generic authenticated-HTTPS-POST adapter every
channel shares, not a real APNs/FCM integration (`WORKFLOW.md`'s own
"Deliberate non-goals": "No provider-specific API schemas... FCM's HTTP
v1 API... is future, dedicated-adapter work"). Needs: a concrete FCM/
APNs adapter behind that same `push_provider_config` pattern, and a new
mechanism for a native app to register its device token against a
subject (doesn't exist in any form today). Note this sidesteps
`ROADMAP.md` Track A6's presence-gateway gap entirely — that gap is
about routing `notify()` to a *live WebSocket* for a connected browser;
a native app's push path never needs a live connection or the
`identity_presence`/`--presence-token` machinery at all, it just needs
a registered device token and a real provider adapter.

**Offline action queue with replay-safe retry** (`D5`). `txn_id`
(`TRANSACT.md`) is a genuine, shipped idempotency mechanism, but it's
scoped to a `transact` block's own `network` slot — generated
server-side, at execution time, for that one call. It is not exposed as
a client-suppliable idempotency key on the ordinary `POST /api/<fn>`
RPC layer `serve.rs` exposes, which is what a mobile app queuing
actions taken while offline and replaying them on reconnect would need
to dedupe against. New work at the RPC dispatch layer: accept an
optional client-generated idempotency key on `/api/<fn>`, and a small
durable "seen keys" table (`serve.rs`-owned, `migrate.rs`-managed like
everything else) to reject/short-circuit a replayed key rather than
re-executing the underlying fn. Same "at-least-once, never
exactly-once" honesty limit `TRANSACT.md`/`WORKFLOW.md` already
disclose applies here too — this makes replay *safe*, not
*exactly-once* end-to-end (the fn body itself still needs to be
idempotent, or wrapped in its own `transact` block, for a true
exactly-once guarantee).

## Deliberate non-goals (disclosed, not silently dropped)

- **No business logic on-device, in either profile.** The server stays
  the sole place `.nir` fn bodies execute — same trust and security
  model as web, deliberately not a KMP-style/transpiled-fn-bodies
  approach. A native app that has no network path to a `nirdosha serve`
  instance can do nothing beyond render its last-known cached data
  (Standard profile) or replay its offline queue once reconnected (Rich
  profile's `D5`, once built) — it never falls back to running Nirdosha
  logic locally.
- **No new UI-DSL.** `screen`/`dashboard`/`field`/`action` stay the one
  surface authors write against; mobile-specific behavior only ever
  adds new *keys* — inside `action { ... }`/`field { ... }` bodies (e.g.
  `step_up: biometric`), or, for `target:` (above), directly on
  `screen`/`dashboard`/`tile`/`chart` themselves, since `kv_entry` is
  already generic there too — never a parallel mobile-specific
  declaration syntax, and never a new grammar production either way.
- **No parity beyond what the manifest already models.** A struct/fn
  shape the web renderer can't turn into a screen (an affine-handle
  param, a payload-carrying-enum field, anything `"readonly"` today)
  stays exactly as unrenderable on native — this doc doesn't expand
  `build_field`'s control-kind coverage, a native-specific field type
  would need its own change to `ui_gen.rs` first, shared by both
  renderers, not forked. That coverage is itself closed by design, not
  a native-vs-web gap: the fixed seven-kind form-control set, the one
  inline-SVG bar chart type, and the fixed four-animation vocabulary
  (`compiler/UI_DSL_TODO.md`'s own "Deliberate non-goals" section) apply
  identically to whatever a native renderer eventually builds — a Rich
  profile adding a native calendar picker or a line chart would be
  widening `ui_gen.rs`'s own closed sets, not a mobile-specific
  addition.
- **No vendor/cert/provisioning decisions made here.** Which push
  provider, whether to self-host FCM/APNs credentials or take an admin
  config row per deployment, code-signing/provisioning-profile
  automation for App Store/Play Store submission — all real, all
  deferred to whoever builds `D2`–`D5`, not pre-decided by this doc.
- **No native codegen of `.nir` itself.** `mobile_gen.rs` emits Swift/
  Kotlin *UI* source only, the same category of output `ui_gen.rs`
  already produces (HTML/JS) — it is not an addition to `codegen.rs`'s
  LLVM backend and has no interaction with `check_supported`/Track B's
  "full compilation" effort.
