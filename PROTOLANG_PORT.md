# Porting ProtoLang to Nirdosha

This is the Nirdosha-native answer to `protolang_reference_specification.md`
and `protolang_std_io_specification.md` — two aspirational specs for a
distributed-systems language (effects, guardians, protocol types, type-safe
SQL, durable execution, a whole `std.io` hierarchy of state machines). The
question this document answers is not "how do we implement ProtoLang" —
Nirdosha is a different, narrower project (`goal.md`'s ten rows), and most of
ProtoLang's surface area exists to solve problems (distributed guardians,
session types, SQL-schema-at-compile-time) Nirdosha doesn't have. The
question is: **which of ProtoLang's actual mechanisms are real, motivated
asks against Nirdosha's own stated goals, and what's the smallest native
Nirdosha version of each** — the same exercise `TRANSACT.md` already did for
one slice (§4 of the reference spec, distributed transactions) and
`SANDBOXING.md` did for another (concurrency primitives). This document does
the rest, and does it the same way: study the ProtoLang mechanism, name the
Nirdosha requirement it actually maps to (or admit it doesn't map to one),
and either lock a narrow design or say plainly why it's deferred or rejected.

## Method

Every ProtoLang mechanism below gets one of four verdicts:

- **Already native** — Nirdosha has a narrower mechanism that earns the same
  guarantee today, just built differently. No work; the entry explains the
  mapping so it isn't re-invented later.
- **Port now** — motivated by a real row in `goal.md` or a real gap in
  `LANGUAGE.md`, and buildable without a prerequisite Nirdosha doesn't have.
  Gets a locked design below, in `TRANSACT.md`'s style (grammar, semantics,
  rollout layers).
- **Blocked** — motivated, but genuinely needs a Nirdosha feature that
  doesn't exist yet (almost always: a sum type). Named explicitly so it
  isn't silently forgotten, not designed here because designing it before
  its prerequisite exists would mean guessing at the prerequisite's shape.
- **Rejected** — either not motivated by any of the ten rows, or actively in
  tension with one (usually row 6/7's minimalism, or row 8's "one fixed
  primitive set" discipline).

Two things worth stating up front because they explain almost every verdict
below:

1. **Nirdosha has no structs, enums, tuples, or generics** (`LANGUAGE.md`
   §6, `GRAMMAR.md`'s "Deliberate omissions"). This is the single
   prerequisite ProtoLang leans on constantly and Nirdosha doesn't have:
   `Option<T>`, `Result<T,E>`, `variant IOError`, `record Request`, `protocol
   TcpSocket<State>` — all of these are sum/product types with type
   parameters. Nirdosha's affine handles (`box`, `thread`, `sandbox`, `tcp`,
   `tcp_listener`) already get *some* of what ProtoLang's state-dependent
   protocols buy (§9) without needing the type-former at all — see below —
   but anything that needs a genuine closed-choice type is **blocked**, not
   portable piecemeal.
2. **Nirdosha already chose the concurrency model ProtoLang's rows 2–3 want**
   (`goal.md` §3's "Concurrency" layer: Pony-style, no blocking mutex by
   default, race-freedom by ownership not by locks) — `spawn`/`join`,
   `chan`/`send`/`recv`, `sandbox`/`stop` (`SANDBOXING.md`) are that model,
   built. ProtoLang's actor/guardian machinery (§4, §13.4 of the reference
   spec) is a different, heavier way to get a related guarantee; Nirdosha
   doesn't need a second one.

---

## Classification table

### `protolang_reference_specification.md`

| § | Mechanism | Verdict | Why |
|---|---|---|---|
| 2 | Primitive/composite type system | **Rejected** (as a whole) | `Array`/`List`/`Map`/`Set`/`Tuple`/`Record`/`Variant` are exactly the generic/sum/product types row 1 above says Nirdosha doesn't have. `Vector`/`Matrix` are Nirdosha's actual answer for the one composite type that matters today (dense numeric arrays) — see `LANGUAGE.md` §2. Rebuilding ProtoLang's full container hierarchy is a separate, much bigger project than "porting" anything; not attempted here. |
| 2.3 | Refinement types (`where value > 0`) | **Already native** | This is `goal.md` row 4, already shipped and *stronger* than ProtoLang's sketch: two independent provers (`refine.rs` interval analysis, `smt.rs` real Z3) discharge integer-overflow, divide-by-zero, and index-bounds facts at compile time, feeding codegen directly (`LANGUAGE.md` §8, §10). ProtoLang states the goal; Nirdosha has the tiered Proved/Checked/Audited machinery (`goal.md` §4) actually built. |
| 3 | Effect system | **Port now** | Directly named in `goal.md` §3's synthesis table ("Effects — rows 4, 9") and never built. See design below. |
| 4 | Guardians / (top)actions / durable execution | **Already covered** | `TRANSACT.md` — one keyword (`transact`), five slots, no `guardian`/`stable` type-former. Not re-litigated here. |
| 5 | Async without function coloring | **Rejected** | Solves a problem Nirdosha's model doesn't have. ProtoLang needs this because its functions are plain call/return and *also* need to suspend transparently on I/O — so it invents delimited continuations to avoid a `Future<T>` coloring split. Nirdosha's answer to concurrency is already message-passing (`spawn`+`chan`, `SANDBOXING.md`) — a function that wants to overlap I/O with other work spawns a thread and receives on a channel; there is no colored-function problem to solve because there's no implicit-async function color at all. Building continuation-passing state machines to solve a non-problem fails row 6 for no gain. |
| 5.3 | `all` / `race` / `background` | **Partially native** | `spawn`+`join` gets `all`'s "wait for every result" today (spawn N, join each). `race` (first successful result) and `background`-as-fire-and-forget-with-later-await have no direct equivalent, but both are thin sugar over `spawn`/`chan`/`join` a program can already write by hand; not worth new syntax until a real program needs it repeated enough to hurt (row 6's actual test), which no Nirdosha program has yet. |
| 6 | Null safety (`Option<T>`, `?.`, `??`) | **Partially shipped** | `Option(T)` itself is real (`nirdosha_row11_amendment.md`'s prelude, layer 7) — `let x: Option(i64) = Some(5)`/`None()`, matched exhaustively. `?.`/`??` (optional-chaining/null-coalescing *syntax*) aren't built — those are sugar over a `match`, a separate, smaller follow-on with no motivating example yet, not blocked on anything. See "The next prerequisite" below for how this got unblocked. |
| 7 | Linear types & resource management | **Already native** | This is `ownership.rs`, already stronger in one respect: it's a real static move-checker across branches and loop iterations (`LANGUAGE.md` §6, `GRAMMAR.md`'s soundness-bug note), not just "the compiler tracks linearity." `box`/`thread`/`sandbox`/`tcp`/`tcp_listener` are exactly ProtoLang §7's linear resources. Borrowing (`&`) exists; `&mut` doesn't yet (`GRAMMAR.md`), narrower than ProtoLang §7.2 but a real gap already tracked there, not new information from this exercise. |
| 8 | Type-safe database queries | **Rejected** (compile-time-checked queries specifically); **runtime DB connectivity shipped separately** | The *compile-time-checked* version this row actually asks for — a schema-loading compiler stage, `Record`-typed query results validated against that schema at compile time — is still rejected; none of those three prerequisites exist, and Row 11 (structs) doesn't supply the schema-loading or compile-time-SQL-parsing half. What did ship, as `std_io` §7's revisit below explains, is ordinary *runtime* DB connectivity — a real, useful, much smaller thing than this row describes, not a relabeling of it. |
| 9 | State-dependent types (protocols) | **Already native, narrower** | ProtoLang needs a `protocol`/`state`/`transition` type-former because its types (`Connection`, `File`, `TcpSocket`) are otherwise state-blind. Nirdosha's affine handles get the load-bearing half of this for free: `tcp`/`tcp_listener`/`sandbox` are each already "closed then open then closed," and the move-checker already rejects use-after-`stop`/use-after-`join` — which *is* "cannot call `send` on a closed connection," just enforced as an affine-consumption fact instead of a state-transition fact. What Nirdosha's version can't express that ProtoLang's can: a handle with more than two states, or state-specific *methods* (different operations legal in different states) — genuinely needs the general type-former (§9's `protocol`), so multi-state handles beyond open/closed are **blocked** on the same prerequisite as `Option<T>`. |
| 9.4 | Session types (protocol duality) | **Rejected** | No two-party protocol in Nirdosha today has more than the request/response shape `tcp` already gives untyped; formalizing duality-checking is real research-scale work (see `goal.md`'s own honest rating of Idris2/Verona) for a need that hasn't appeared yet. |
| 10 | Configuration as code | **Blocked** on sum types (partially) | A *flat* version (top-level typed named constants, `env("X") ?? "default"`-shaped) doesn't strictly need records — `ServiceConfig`'s single-level fields could be individual `let`-like declarations. But `env()`'s honest return type is `Option<str>` (an env var may not be set), so even the flat version wants `Option` first. Revisit once `Option` lands. |
| 11 | Built-in observability (auto spans/metrics/logs) | **Rejected**, for now | Not one of `goal.md`'s ten rows, and cross-cuts every effectful builtin in the interpreter (`spawn`/`sandbox`/`connect`/`chan`/future `file`) — real engineering cost with no named requirement pulling it in. If it's wanted later, effects (below) are the right hook to hang it from (an effect is already "this call touches the outside world"), so building effects first is the right order regardless. |
| 12 | Exhaustive error handling (`throws`, `?`) | **Blocked** on a propagation primitive only, now | `Result(T, E)` itself exists (layer 7, same as `Option`) — a function can already return `Result(T, E)` and its caller can `match` it exhaustively. What's still missing is (b) alone: non-local control flow to *propagate* an `Err` without writing the `match` out by hand at every call site, which Nirdosha has zero of today (traps abort the whole run; there's no `return`-early-on-error operator at all). A real "phase" of its own, not a same-day follow-on — see `nirdosha_row11_amendment.md`'s own "named follow-on, not designed here" note on `?`. |
| 12.5 | Panics vs. errors | **Already native** | This distinction already exists, just under different names: a Tier-1/2 trap (`abort()`, `LANGUAGE.md` §8) *is* ProtoLang's `panic` — unrecoverable, aborts the run. Nirdosha doesn't yet have the recoverable half (`throws`) to contrast it against — see above — but the panic half was never missing. |
| 13 | Data race freedom | **Already native** | `goal.md` §3's synthesis already committed to this (ownership checker rules out simultaneous mutable aliasing; `SANDBOXING.md`'s affine channel handles rule out a sender touching a value after `send`). ProtoLang's `Send`/`Sync` auto-derivation (§13.2) has no Nirdosha equivalent because Nirdosha has no user-defined aggregate types to derive it *over* yet — moot until row 1 in the type-system table above is revisited. |
| 13.3 | Mutex / atomics | **Rejected**, for now | Nirdosha's concurrency story is deliberately lock-free-by-default (`chan`, matching Pony — `goal.md` §3's "Concurrency" callout: "shared-memory locks are opt-in, gated by a static lock-rank check"). No Nirdosha program has needed a shared-memory lock yet; adding `Mutex`/`AtomicInt64` before a real use case exists is exactly the kind of premature surface `goal.md` §4's escape-valve discipline warns against building speculatively. |
| 13.4 | Actor model | **Already native, different mechanism** | `sandbox` (a real OS process) plus `chan` (cross-process via a Unix domain socket, per the "real cross-process chan IPC" work already shipped) *is* Nirdosha's actor model — isolated state, message-only communication. ProtoLang's `actor`/`message` keywords are sugar over the same guarantee; Nirdosha's version costs zero new keywords, per `SANDBOXING.md`'s own framing ("`spawn`/`chan` cost zero new checker machinery"). |
| 14 | Feature flags & experiments | **Rejected** | Not one of the ten rows, and the "automatic cleanup after an experiment concludes" half (§14.4) needs live code deletion tooling that's its own project. Nothing in Nirdosha today needs an A/B-testing story. |
| 15 | Implicit context propagation | **Rejected**, for now | Needs ambient, automatically-threaded state across `spawn`/`sandbox` boundaries — real design work (how does a `RequestContext` cross a `sandbox` process boundary that's really a re-exec'd binary talking over a socket?) with no motivating row. Revisit only if/when observability (§11 above) is picked back up, since context propagation is really that feature's prerequisite, not a standalone ask. |
| 16–17 | Runtime architecture / compilation pipeline | **N/A** | These describe ProtoLang's own implementation, not a portable language feature. Nirdosha's actual pipeline is `LANGUAGE.md` §1 and `GRAMMAR.md`; nothing to port. |
| 18 | Stdlib module list | **See `std_io` table below** | |

### `protolang_std_io_specification.md`

| § | Mechanism | Verdict | Why |
|---|---|---|---|
| 1–2 | I/O effect hierarchy, `Resource`/`Reader`/`Writer`/`Closer`/`Seeker` protocols | **Port now** (the hierarchy) / **already native** (linearity) | The *effect* hierarchy (`io` ⊃ `file`/`network`/`db`) is exactly what the effects design below needs anyway — one system, not two. The `Resource`/`Closer` protocols are just "affine handle with a consuming close," which `box`/`tcp`/`sandbox` already are; no type-former needed to get the guarantee, per the reference-spec §9 entry above. |
| 3 | File I/O | **Port now** | The one genuine, motivated gap: Nirdosha has `tcp`/`sandbox` but no file handle at all. See design below. |
| 3.3 | Memory-mapped files | **Rejected**, for now | No Nirdosha program has needed to avoid `read()` syscall overhead yet; `mmap`'s `&mut bytes` return is also a raw-pointer-shaped escape hatch Nirdosha's ownership model has no story for at all (no `bytes` type, no unsafe pointer arithmetic anywhere in the language). |
| 3.4 | Directories | **Deferred**, not rejected | Real and eventually wanted (an LLM-facing language will want to list a directory), but strictly after §3's plain `file` lands — same one-slice-at-a-time discipline as `TRANSACT.md`'s layering. |
| 3.6 | Temp files | **Deferred**, same reason as directories | Sugar over `open` + "delete on close," which only makes sense once `open`/`close` exist. |
| 4 | TCP / UDP / Unix sockets | **Mostly already native** | TCP client+server (`connect`/`listen`/`accept`/`stop`, `send`/`recv` reused) already exists (`LANGUAGE.md` §7). UDP and Unix domain sockets as a *user-facing* type don't exist (Unix sockets are already used *internally* for sandboxed `chan` IPC, just not exposed) — **deferred**, real but no example program has asked for either yet. |
| 5 | HTTP / HTTPS | **Both shipped (plain client + TLS client)** | See "Locked design 4: HTTP" below. The original blocker (a real `str`-manipulation story) turned out to be avoidable for a first cut: `http_get`/`http_post`/`https_get`/`https_post` are Rust-native builtins over the same `tcp` substrate, not a Nirdosha-source-level parser built from string primitives — so no `str` concatenation/slicing was actually needed. HTTPS uses `native-tls` (a real, considered dependency decision — bundling the platform's own TLS via OpenSSL/Schannel/SecureTransport rather than a pure-Rust reimplementation), doing the actual security-critical work (certificate-chain and hostname verification) via its own vetted defaults, exactly the "vetted library binding, not hand-rolled" stance this document already committed to. |
| 6 | WebSockets | **Rejected**, for now | Same substrate argument as HTTP, one layer further out; blocked on HTTP being worth doing first. |
| 7 | Database / SQL as I/O | **Shipped, narrower than either verdict above anticipated** | See "Locked design 5: DB" below. `db_connect`/`db_query`/`db_execute` treat a database exactly as I/O after all — no compile-time schema access, no query validation, `sql` is an opaque `str` checked only at runtime (by the database engine itself, the same way a malformed HTTP request or unparseable JSON is only ever caught at runtime, never statically). What makes this "just I/O" honest, where the reference-spec §8 entry above says it can't be: results come back as `Ty::Json`, wrapped in `Result` (Row 11 layer 7) — no `Record`/schema type needed, the same move JSON's own design already made for the same reason. |
| 8.1 | Standard streams (`stdin`/`stdout`/`stderr`) | **Port now**, small | `print` already covers `stdout` for every practical Nirdosha program today. A `read_line()` builtin (stdin) is a two-line addition once `file`'s `recv`-reuse pattern exists — folded into the file I/O design below rather than given its own section, since it's the same mechanism (a pre-opened, un-closeable `file`-shaped handle). |
| 8.2 | Child process I/O (stdin/stdout/stderr streams) | **Already native, narrower** | `sandbox` already gives a real child-process handle with `stop` returning its exit code (`LANGUAGE.md` §7). Piping the child's stdout back as a `Reader` isn't built — `chan`-over-sandbox already gives typed cross-process communication for programs *written in Nirdosha itself*; piping an arbitrary external command's stdout is a different, deferred ask (no example program launches a non-Nirdosha binary today). |
| 8.3 | Pipes | **Rejected** | `chan` already is Nirdosha's in-process pipe (typed, safer than raw bytes); an OS-level anonymous pipe has no motivating use case `chan` doesn't already cover. |
| 9 | Streaming & backpressure (`Stream<T>`, `.map`/`.filter`/…) | **Blocked** on generics | `Stream<T>` and its combinators are exactly the higher-order, generic machinery `LANGUAGE.md` §6 says doesn't exist ("no closures/lambdas, no first-class functions... no structs, enums, tuples, or generics"). Not attempted piecemeal — needs its own planning pass, same as the unified plan already scopes for real generics (`goal.md`'s Phase references). |
| 10 | Serialization I/O (JSON, protobuf, CSV) | **JSON shipped (read-only navigation); protobuf/CSV still rejected** | See "Locked design 3: JSON" below. Not the design this row originally sketched (`encode<T>`/`decode<T>` generic over an arbitrary Nirdosha type, `Json` as a Nirdosha-level recursive `enum`) — that would additionally need a growable/variable-length collection type, which still doesn't exist (`Vector`/`Matrix` are fixed-size). What shipped instead: `Ty::Json`, a handle over an already-parsed `serde_json::Value` tree, with concrete per-target-type accessors (`json_get_str`/`_i64`/`_f64`/`_bool`) — narrower, but real, and it composes with `Result` (Row 11 layer 7) for the first time. protobuf/CSV are unrelated formats with their own encoders; nothing here generalizes to them. |
| 11 | Resource pools | **Rejected** | Solves connection-reuse cost for `tcp`/DB connections at a concurrency scale (thousands of short-lived connections) no Nirdosha program is anywhere near yet. A `Pool<T>` is also generic over `T: Resource`, same generics blocker as streaming. |
| 12 | TLS/SSL | **Rejected**, for now | Depends on HTTP being worth doing first (§5's verdict), and TLS itself is the kind of security-critical protocol implementation that should be a vetted C library binding, not hand-rolled — out of scope for "port ProtoLang's design," which is a language-feature exercise, not a crypto-implementation one. |
| 13 | I/O observability | **Rejected**, for now | Same verdict and same reasoning as reference-spec §11. |
| 14 | I/O error model (`variant IOError`) | **No longer blocked, not yet done, partially already native** | Nirdosha's actual I/O error reporting today is a single reused `ErrorKind::ChannelIoError { message: String }` across `chan`/`tcp`/`sandbox` (see `interpreter.rs`) — a deliberately flat, stringly-typed error, not ProtoLang's ~20-variant hierarchy. That flatness is a real, current limitation (a caller can't `match` on "connection refused" vs "timed out"). `enum` (Row 11) now exists, so a real fix no longer needs a new type-system prerequisite — but this file's own error-reporting path (a Rust-level `RuntimeError`/`ErrorKind`, not a Nirdosha-level value at all) hasn't been migrated to surface as a real Nirdosha `Result(T, IoError)`-shaped value; that migration is real, un-started work, not merely unblocked. |
| 15 | Complete examples (HTTP proxy, WS chat) | **N/A** | Composites of §5–7 above; nothing to port independently of them. |

---

## Locked design 1: effects

**Status: shipped (21 Aug 2026).** `effect(...)` parses, and enforcement
(declared ⊇ inferred, `TypeErrorKind::EffectNotDeclared`) is real and
tested (`compiler/tests/effects.rs`, `compiler/src/effects.rs`) —
`compiler/examples/effects.nir` is the worked example. Everything in
"Rollout layers" below shipped as one slice rather than layer-by-layer
(the design was small enough, and each layer needed the next to be
testable at all): parsing alone has nothing to verify without inference,
and inference alone has nothing to verify without enforcement. `mask`
and higher-order effect propagation remain out of scope, as designed
below — no motivating case, no first-class functions to propagate through.

### What it brings to the table

`goal.md` §3 already named this as a required synthesis layer ("Koka-style:
function signatures declare what they touch... This is what keeps ownership
and refinement tractable *and* what makes agent-facing diagnostics
structured instead of prose") and it has never been built. Three things
Nirdosha gets that it doesn't have today:

1. **A machine-checkable answer to "does this function touch the outside
   world"** — today, nothing stops a function declared for pure computation
   from quietly calling `sandbox` or `connect`. Row 9 (`goal.md`) wants
   agent-facing diagnostics to be structured; "you declared `pure`, line 12
   calls `spawn`" is exactly that kind of structured, actionable fact.
2. **Zero notational cost until it's used** — matches row 6/7's actual
   requirement (`goal.md` §3's "surface syntax" callout: "the notational
   cost... is paid once, by the compiler, not on every line"). An
   undeclared function is fully inferred, same as today.
3. **A hook for later work** (§11's observability, §15's context
   propagation, both rejected above *for now*) without having built any of
   it yet — an effect is already "this call crosses a boundary worth
   watching," the correct place to attach a span later.

### Design, adapted to what Nirdosha actually has

ProtoLang's lattice (`pure < mutates < io < network`, §3.3) is a *total
order* — a `pure` function can be used anywhere a `mutates` one is expected.
That doesn't fit Nirdosha's real effectful operations, which don't nest:
`network` (tcp) isn't a superset of `concurrent` (spawn/chan/sandbox), and
neither is a superset of the other. `goal.md` itself calls this layer
"Koka-style" — Koka's algebraic effects are row-polymorphic **sets**, not a
single chain, which is the right shape here:

```
effect ::= "pure" | "rng" | "io" | "concurrent" | "network"
```

- `pure` — calls only other `pure` functions and pure builtins (arithmetic,
  comparisons, every dense-linear-algebra builtin in `LANGUAGE.md` §5 —
  `dot`, `transpose`, `det`, `zeros`, etc. are all pure; they read their
  arguments and return a value, nothing else).
- `rng` — calls `rand_seed`/`rand_f64`/`rand_gaussian`. Broken out from
  `io` on purpose: these are deterministic given a seed (`LANGUAGE.md` §9)
  but stateful (mutate the interpreter's own RNG stream) — a different fact
  from "talks to the outside world," worth keeping separately taggable.
- `io` — `print`, and the proposed `file` operations below.
- `concurrent` — `spawn`/`join`, `chan`/`send`/`recv` (channel form),
  `sandbox`/`stop`.
- `network` — `connect`/`listen`/`accept`, `send`/`recv`/`stop` (tcp form).

`pure` is the only ordering fact: `∅ ⊆ every other set`, i.e. a `pure`
function may be called from anywhere. Every other pair is unordered — a
`network` function isn't automatically assumed `concurrent` or vice versa,
because in Nirdosha's builtin set today, neither actually implies the other.

```ebnf
fn_decl  ::= "fn" ident "(" params ")" ("->" ty)? effect_ann? block
effect_ann ::= "effect" "(" effect ("," effect)* ")"
```

- **Omitted entirely** (today's grammar, unchanged) — fully inferred, no
  declaration, no checking against a bound. This is what every existing
  Nirdosha program keeps doing; the feature is additive.
- **Declared** — the compiler still infers the function's real effect set
  (bottom-up over the call graph, the same declaration-order-independent
  pass that already resolves function signatures for arity/type checking,
  per `LANGUAGE.md` §6's "functions are looked up by name in a table") and
  checks it's a **subset** of what was declared. A declared effect the body
  never actually uses is not an error — same generosity ProtoLang's own
  subsumption rule gives (§17.2's "Effect Subsumption": `E ⊆ E'` is fine).
  An inferred effect *not* in the declared set is `TypeErrorKind::
  EffectNotDeclared { fn_name: String, missing: Effect }`, in the same
  family as the existing `ArityMismatch`/`TypeMismatch` kinds.

```nir
fn dot_normalized(a: Vector(f64, 3), b: Vector(f64, 3)) -> f64 effect(pure) {
    return dot(a, b) / (norm(a) * norm(b))
}

fn log_request(msg: str) -> unit effect(io) {
    print(msg)
}

fn broken() -> unit effect(pure) {
    print("oops")   // compile error: EffectNotDeclared { missing: io }
}
```

Two things ProtoLang has that this design deliberately drops:

- **`mask`** (§3.4, calling an effectful function from a pure context and
  asserting the effect doesn't escape) — no Nirdosha program has a
  motivating case for this yet (ProtoLang's own example is a cached config
  loader, and Nirdosha has no config subsystem — see the reference-spec §10
  entry above). Add it only against a real need, per `goal.md` §4's own
  discipline about escape hatches: visible and costed when they exist, not
  spec'd speculatively.
- **§3.5's higher-order effect propagation** — not applicable. Nirdosha has
  no first-class functions (`LANGUAGE.md` §6); there is no `f: function(T):
  U` parameter whose effect could propagate into a caller. Revisit only if
  first-class functions ever land, which is its own, much bigger, separate
  design question.

### Rollout layers

Same discipline as `TRANSACT.md` and `SANDBOXING.md` — ship and test each
layer before the next:

1. **Parse `effect(...)` on `fn`, store it, don't check it.** Proves the
   grammar addition is unambiguous (still LL(1) — `effect` only appears in
   one fixed position, right where `throws` would in a fuller ProtoLang
   port, so this doesn't collide with anything reserved for that later).
2. **Infer the real effect set for every function**, unconditionally,
   whether or not one was declared. Exposed via `emit-ast` (`LANGUAGE.md`
   §1) so it's immediately useful for row 9's agent-facing tooling even
   before enforcement exists.
3. **Enforce declared ⊇ inferred.** The actual feature; everything above is
   scaffolding for this one check.
4. **Codegen consumption: none needed.** Every builtin in the compiled
   subset (`LANGUAGE.md` §10 — scalars, `Vector`/`Matrix`) is `pure` except
   `print` (`io`), so effects are a typeck-only concern with zero codegen
   change, the same "part of the type, not the runtime" property ProtoLang
   itself claims (§3.1).

---

## Locked design 2: file I/O

**Status: Layer 1 shipped (21 Aug 2026).** `open`/`send`/`recv`/`stop` on
`file` are real, tested (`compiler/tests/file_io.rs`), and interpreter-only
per the layering below — `"r"`/`"w"`/`"a"` modes, no `mmap`, no
directories, no `read_line()` yet. `compiler/examples/file_io.nir` is the
worked example. Everything from "Rollout layers" layer 2 onward is still
future work.

### What it brings to the table

Nirdosha's affine-handle story already covers threads, processes, and TCP
(`box`/`thread`/`sandbox`/`tcp`/`tcp_listener`) but not the single most
ordinary form of I/O: reading and writing a file. `std_io` §3 is the
motivated gap; this design is the smallest Nirdosha-shaped version of it,
built by extending the exact pattern `tcp` already established rather than
introducing ProtoLang's `protocol File { state Open {...} state Closed
... }` type-former (blocked — see the reference-spec §9 entry above).

### Design

A new affine type, `file`, alongside `box`/`thread`/`sandbox`/`tcp`/
`tcp_listener` in `Ty::is_affine()` (`ast.rs`). No new keywords: `open`
joins the ranks of `connect`/`listen` as a dedicated `Expr` node (needed
because, like `Expr::Connect`, its result type isn't inferable from a
generic builtin-call shape), and — the same reuse `tcp` already gets from
`send`/`recv`/`stop` — file I/O adds itself as a third arm to those same
three polymorphic verbs instead of inventing `read`/`write`/`close`:

```ebnf
expr ::= ... | "open" "(" expr "," expr ")"      // path: str, mode: str
```

- `open(path, mode) -> file` — `mode` is a `str` literal tag (`"r"`, `"w"`,
  `"a"`), the same pragmatic substitute `TRANSACT.md` chose for `timeout`
  over inventing a duration literal: ProtoLang's `FileMode` enum (§3.1) is a
  closed-choice type Nirdosha doesn't have yet, and a string tag is the
  narrow stand-in, not a permanent design (revisit once `variant` types
  exist and this can become real). Unrecognized mode strings are a runtime
  `ErrorKind::ChannelIoError` (the same reused error kind `tcp`/`chan`
  already report through — see the `std_io` §14 entry above; a faithful
  fix needs sum types, so this doesn't invent a parallel taxonomy in the
  meantime), not a new error variant.
- `send(f, s)` — write `s: str` to `f`, reusing `Expr::Send`'s existing
  `Ty::Tcp => check value against Str` arm's twin: add `Ty::File => { check
  value against Str; Ty::Unit }`. Same justification `tcp` already
  documents: Nirdosha has no `bytes` type, so `str` is the only payload
  type there is (`LANGUAGE.md` §2).
- `recv(f)` — read all currently-available bytes from `f` as `str`,
  reusing `Expr::Recv`'s `Ty::Tcp => Ty::Str` arm's twin: `Ty::File =>
  Ty::Str`. An empty string signals EOF — the same convention a `tcp`
  peer disconnecting already produces from `read_tcp`, not a new
  end-of-stream protocol.
- `stop(f) -> unit` — consuming close, reusing `Expr::StopSandbox`'s
  existing `Ty::Tcp => Ty::Unit` arm's twin: add `Ty::TcpListener`'s
  neighbor, `Ty::File => Ty::Unit`.

```nir
fn read_whole_file(path: str) -> str {
    let f: file = open(path, "r")
    let content: str = recv(f)
    stop(f)
    return content
}

fn write_greeting(path: str) -> unit {
    let f: file = open(path, "w")
    send(f, "hello from nirdosha\n")
    stop(f)
}
```

Zero new `TypeErrorKind` variants beyond one twin of an existing one:
`ExpectedChannelType`/`ExpectedSandboxType` (`typeck.rs`) already carry a
`found: Ty` field generically enough that a `file`-typed mismatch reports
through them unchanged — no `ExpectedFileType` needed.

### `stdin`/`stdout` (`std_io` §8.1), folded in rather than separated

`print` already is `stdout`, and covers every existing Nirdosha program.
The one addition worth making alongside `file`: a `read_line() -> str`
builtin, reading one line from the process's real stdin — implemented as a
pre-opened `file`-shaped handle the runtime constructs once (not
`open`-able, not `stop`-able by the program, the same "already open, cannot
close" shape ProtoLang gives its own `stdin` in §8.1) rather than a new
type. Deferred until `file` itself is proven, per the layering below —
named here so it isn't lost.

### Rollout layers

1. **`open`/`send`/`recv`/`stop` on `file`, `"r"`/`"w"`/`"a"` modes only,
   local filesystem, no `mmap`/directories/temp files.** Parser (`open`'s
   `Expr` node, mirroring `Connect`), typeck (the three polymorphic-verb
   arms above, all following an existing pattern — no new checker
   machinery, same observation `SANDBOXING.md` already made about
   `spawn`/`chan`), ownership (`file` just joins the affine list — nothing
   new for `ownership.rs` to reason about), interpreter (real
   `std::fs::File` underneath, `read_to_string`/`write_all`, matching
   `read_tcp`/`write_tcp`'s existing shape).
2. **`read_line()` on real stdin.**
3. **Append mode's actual semantics, `truncate`/`sync`** (`std_io` §3.1's
   `metadata`/`setPermissions`/`truncate`/`sync`) — deferred past layer 1
   on purpose; most programs need read-a-file/write-a-file long before they
   need `fsync` control.
4. **Directories, temp files** (`std_io` §3.4, §3.6) — their own layer,
   after plain files are proven, same discipline as everything above.
5. **Compiled backend is out of scope until the interpreter version is
   proven** — `file` joins `box`/`thread`/`chan`/`sandbox`/`tcp` on the
   "interpreter-only, rejected not mis-compiled" list (`LANGUAGE.md` §10),
   not an exception to it.

---

## Locked design 3: JSON

**Status: shipped (21 Aug 2026).** `Ty::Json`, `json_parse`, and the
per-target-type accessors (`json_get`/`json_get_str`/`_i64`/`_f64`/
`_bool`, `json_array_len`/`json_array_get`) are real, tested
(`compiler/tests/json.rs`, `compiler/examples/json.nir`),
interpreter-only per the same discipline every other affine-handle
feature here already follows.

### What it brings to the table, and what changed from `std_io` §10's original sketch

The original design (`encode<T>`/`decode<T>` generic over an arbitrary
Nirdosha type, `Json` as a Nirdosha-level recursive `enum` with `Array`/
`Object` variants holding a Nirdosha-level list) needed two things Row 11
still doesn't provide: reflection/derive machinery to make `encode<T>`
generic over *any* struct, and a growable, variable-length collection
type (`Vector`/`Matrix` are fixed-size — the dimension is part of the
type). Neither showed up as newly available just because generics
landed, and neither was worth inventing speculatively to unblock this.

What shipped instead is narrower and more honest: `Ty::Json` is a handle
over an **already-parsed** `serde_json::Value` tree (`serde_json` was
already a dependency, for AST export — `lib.rs::Diagnostic`), navigated
through concrete, per-target-type builtins rather than one generic
`json_get<T>`. This was a deliberate design choice, not a fallback: there
is no mechanism today for a plain builtin call's return type to be
resolved generically the way struct/variant *construction* now is (Row
11 layer 6's `want`-based resolution is specific to construction), so a
family of concrete functions is the same "narrow, concrete, cheap to
extend later" discipline every builtin in this language already follows,
not an oversight to fix later.

A true zero-copy design — cursors into the *original source text*,
materializing nothing until a field is actually read, no parse tree at
all — was considered and is a real, valid future optimization (this is
what simdjson's On-Demand API and serde's zero-copy `Deserialize` do).
It was set aside for this first cut for two concrete reasons: it needs
real byte-range string slicing, which doesn't exist (see §5's own updated
verdict for why that turned out to matter less for HTTP than expected,
but JSON's *read* side genuinely does need it for that specific
design); and hand-rolling a fully correct JSON scanner (escape
sequences, UTF-8 boundaries, number formats) duplicates real, existing
correctness work `serde_json` already has, for no concrete performance
problem any Nirdosha program has hit yet — the same "don't build the
escape valve speculatively" discipline `goal.md` §4 already asks for
everywhere else. If a real program hits a real cost from full-tree
parsing, the fix is entirely internal to `json_parse`'s Rust
implementation — the Nirdosha-facing API (`Ty::Json`, the accessor
functions, `Result(_, str)` on failure) doesn't have to change at all.

Every fallible accessor returns `Result(_, str)` (Row 11 layer 7) — a
missing key, a JSON value of the wrong shape, or malformed input to
`json_parse` are all a real Nirdosha value a program can `match` on, not
a `RuntimeError` trap. This is the first real consumer of `Result`
outside the language's own prelude machinery, and it's what motivated
one small fix to `ownership.rs` along the way: `match`ing directly on a
call's result (`match json_parse(s) { .. }`, the natural idiom) needs the
scrutinee's concrete type arguments resolved precisely, which the
existing Ident-only resolution couldn't do for a bare call — see
`ownership.rs::builtin_return_ty`.

### Rollout layers

1. **Shipped.** `json_parse`, `json_get`, and the four typed leaf
   accessors, plus array navigation (`json_array_len`/`json_array_get`) —
   object/array navigation over an eagerly-parsed tree.
2. **Not designed, not scheduled.** True zero-copy/lazy navigation
   (cursors into the original text, no parse tree) — a real future
   optimization, not a current gap; see above for why it's deferred, not
   forgotten.
3. **Not designed, not scheduled.** JSON *construction*/serialization
   (a Nirdosha value → JSON text) — no motivating example yet; every
   shipped use case so far is reading a response, not building a request
   body (`http_post`'s body is a plain, already-formed `str`).
4. **Compiled backend is out of scope until the interpreter version is
   proven** — `json` joins the existing interpreter-only list, not an
   exception to it.

---

## Locked design 4: HTTP

**Status: shipped (21 Aug 2026), both plain and TLS, client-only.**
`http_get`/`http_post`/`https_get`/`https_post` are real, tested
(`compiler/tests/http.rs`, `compiler/tests/https.rs`,
`compiler/examples/http.nir`, `compiler/examples/https.nir`),
interpreter-only.

### What it brings to the table, and what changed from §5's original verdict

§5's original "Rejected, for now" reasoned that an HTTP layer needs real
`str` concatenation/slicing first, since it assumed HTTP would be a
Nirdosha-*source*-level library built out of string primitives the way a
user might eventually hand-roll one. That assumption turned out to be
avoidable: `http_get`/`http_post` are Rust-native builtins — the request
is built and the response is parsed entirely inside the builtin's own
Rust implementation, over the same `std::net::TcpStream` `connect`
already uses, the same "complex operation, thin Nirdosha-facing surface"
treatment `det`/`inv`/`solve` (dense linear algebra) already get. No
`str` concatenation or slicing was needed at the language level at all.

The other original blocker — "a substantial parser + state machine
(chunked encoding, redirects, keep-alive)" — is narrowed by a real design
choice, not solved in general: every request sends `Connection: close`
and reads the response to EOF. The peer closing the socket *is* the
end-of-body signal, so there's no `Content-Length`/chunked-transfer-
encoding parsing to get right for a first cut. Redirects and keep-alive
are both real, named gaps, not attempted — a caller that needs to follow
a redirect reads `HttpResponse.status` and issues a new `http_get`
itself; nothing here does it automatically.

`http_get(host, port, path)`/`http_post(host, port, path, body)` both
return `Result(HttpResponse, str)`, reusing Row 11 layer 7's real
`Result` the same way JSON's accessors do — a connection failure, a
malformed status line, or a non-UTF-8 body are all `Err(message)`, never
a trap. `HttpResponse { status: i64, body: str }` is a real, ordinary
`struct`, injected into every program's prelude exactly the way
`Option`/`Result` are (`ast::prelude_structs`) — the response body reads
as a plain field access (`resp.body`) and composes directly with
`json_parse` for a JSON API, which is the shape every test in
`compiler/tests/http.rs` actually exercises.

**HTTPS is shipped**, via `native-tls` (chosen deliberately over
`rustls`: it binds the platform's own TLS — OpenSSL on Linux, Schannel
on Windows, Secure Transport on macOS — rather than a pure-Rust
reimplementation, matching this document's own "vetted library binding,
not hand-rolled" stance in the most literal way available). `https_get`/
`https_post` share `http_get`/`http_post`'s exact request-building and
response-parsing code (`interpreter.rs`'s `build_http_request`/
`parse_http_response`), generic over the transport
(`send_and_receive<S: Read + Write + HalfCloseWrite>`) — a plain
`TcpStream` for one, a `native_tls::TlsStream` wrapping one for the
other. `TlsConnector::new()`'s untouched defaults do the actual
security-critical work (certificate-chain and hostname verification
against the platform's trust store); nothing here second-guesses or
weakens them. Verified two ways: `compiler/tests/https.rs`'s own
self-contained suite generates a throwaway self-signed certificate (via
the system `openssl` CLI — this crate already requires the OpenSSL
system *library* to build on Linux, so depending on its CLI too, for
tests only, is a small addition on an already-required dependency, not a
new one) and proves the handshake genuinely rejects it — if certificate
verification were ever accidentally disabled, that test would start
silently *passing* the connection instead of erroring, so it's a real
regression guard. A genuine successful round trip needs a certificate
signed by a real, trusted CA, which no offline test can produce — that
was checked by hand instead, against a real server
(`https_get("example.com", 443, "/")` → `Ok(HttpResponse { status: 200,
.. })`, and `https_get("example.com", 80, "/")` — plaintext HTTP on the
TLS call — → a clean `Err("TLS handshake failed: ...")`, not a hang or a
panic).

One real design wrinkle TLS introduces that plain TCP doesn't:
`http_request`'s existing half-close of the write side (a courtesy for
servers that wait for EOF before responding) has no safe equivalent over
TLS — `TlsStream::shutdown` sends a `close_notify` and tears down the
*whole* session, which would prevent ever reading the response about to
be requested. `HalfCloseWrite` (a small trait, one real impl for
`TcpStream`, one deliberate no-op impl for `TlsStream`) names this
directly rather than papering over it — the request already tells the
server exactly how much to expect (`Content-Length`, or the blank line
ending a bodyless GET's headers), so a well-behaved server needs no
half-close signal at all, over either transport.

### Rollout layers

1. **Shipped.** `http_get`/`http_post`, plain HTTP/1.1, `Connection:
   close` + read-to-EOF, status code + body only (no header access).
2. **Shipped.** `https_get`/`https_post`, same request/response handling,
   over `native-tls`.
3. **Not shipped, named explicitly.** Custom request headers (today's
   fixed set — `Host`, `Connection`, `User-Agent`, `Accept`, and
   `Content-Type`/`Content-Length` for the `_post` variants — covers
   every test case that exists so far); response header access on
   `HttpResponse`.
4. **Not shipped, named explicitly.** An HTTP *server* (`listen`/`accept`
   already exist as the substrate — SANDBOXING.md/the unified plan's
   §4.3.3 — but constructing and writing a response is new work; nothing
   here builds it). An HTTPS server would additionally need a real
   certificate to present, a separate provisioning question this doesn't
   touch.
5. **Compiled backend is out of scope until the interpreter version is
   proven** — `http_get`/`http_post`/`https_get`/`https_post` join the
   existing interpreter-only list, not an exception to it.

---

## Locked design 5: DB

**Status: shipped (21 Aug 2026), layer 1 (SQLite only).**
`db_connect`/`db_query`/`db_execute` are real, tested
(`compiler/tests/db.rs`, `compiler/examples/db.nir`), interpreter-only.

### What it brings to the table, and why "one driver per vendor, one uniform surface" instead of "one driver for everything"

There's no single wire protocol or query language spanning relational,
document, and graph databases — Postgres, MySQL, MongoDB, and Neo4j each
speak their own protocol and their own query language, and even ODBC/
JDBC (the closest real precedent for "one driver, many databases") only
unify the relational subset. So the design here isn't "one driver for
every kind of database" — that isn't achievable without inventing a
protocol none of these vendors actually speak — it's one **uniform
Nirdosha-facing surface** (`db_connect`/`db_query`/`db_execute`/`stop`,
results always `Ty::Json`) over a real, vetted native driver crate per
backend, the same "vetted library binding, not hand-rolled" stance this
document already committed to for TLS. Layer 1 wires up exactly one
backend, SQLite, via `rusqlite` ("bundled": SQLite compiled from source
and statically linked, no system `libsqlite3` dependency — the same
"fully self-contained, no external service" property this project's own
test discipline already requires everywhere else). Adding Postgres later
(named as layer 2, not attempted here) means wiring up `tokio-postgres`
behind the same four function names — a small, isolated addition, never
a rearchitecture, because the Nirdosha-facing shape doesn't change.

`db_query` is for row-returning statements (`SELECT`); `db_execute` is
for everything else (`INSERT`/`UPDATE`/`DELETE`/DDL), returning the
affected-row count. Both return `Result(_, str)` (Row 11 layer 7) — a
connection failure, a SQL syntax error, a constraint violation, are all
`Err(message)`, never a trap; the database engine's own error message is
passed straight through, not re-interpreted. `db_query`'s result reuses
`Ty::Json` rather than inventing a `Ty::Row`/table type — the same move
JSON's own design already made: there's no growable Nirdosha-level
collection type to hold a variable-length row set in otherwise (`Vector`/
`Matrix` are fixed-size), so a query's row count, a genuine runtime
value, has nowhere else to live. A caller navigates a result set with
the exact same `json_array_len`/`json_array_get`/`json_get_*` builtins
any other JSON document uses — `db` and `json` compose for free, not
through any special-casing.

`Ty::Db` is affine, the same as `Ty::Tcp`/`Ty::File` — `stop` (reused a
fourth time) is the one-time consuming close. This surfaced one real,
general gap while building it: `db_query`/`db_execute` are ordinary
builtin calls, not the dedicated `Expr` nodes `tcp`/`file`'s `send`/
`recv` are (Row 11's newer "concrete builtin, not new grammar" pattern —
`Ty::Json`'s own doc comment), so `ownership.rs`'s generic "every call
argument is consumed" rule would otherwise have made a connection usable
exactly once. Fixed with a small, named exception (mirroring the "read,
don't move" treatment `Expr::Accept`'s listener operand already gets) —
the first builtin-call-shaped case that needed one, not a general
mechanism. A **separate, wider gap this doesn't fix**: passing the same
connection on to more than one *other* function still moves it away from
the caller, since there's no `&db`-aware borrowing story for `db_query`/
`db_execute` yet — every query against one connection has to live in a
single function today. That's a real, general limitation shared by every
affine handle in this language (`box`/`thread`/`sandbox`/`tcp`/`file`/
`db` alike), not something new this feature introduced, and not
attempted here.

### Rollout layers

1. **Shipped.** `db_connect`/`db_query`/`db_execute`, SQLite only
   (`rusqlite`, "bundled"), results always `Ty::Json`.
2. **Not shipped, named explicitly.** Postgres (`tokio-postgres`) — the
   most common real backend for services, but its own tests would need a
   real running server (Docker or similar), a real gap in this project's
   self-contained-test discipline that SQLite's embedded nature happens
   to sidestep for free.
3. **Not shipped, named explicitly.** Prepared-statement parameter
   binding (`db_query(conn, "SELECT * FROM t WHERE id = ?", [id])`-
   shaped) — today's `sql` is one opaque string per call, so a caller has
   to format values into it themselves (a real SQL-injection footgun for
   any untrusted input, worth naming honestly rather than glossing over).
4. **Not shipped, named explicitly.** Document/graph backends (MongoDB,
   Neo4j) — each would be its own real driver-crate integration and its
   own connection-string scheme, not a generalization of the relational
   layer above.
5. **Compiled backend is out of scope until the interpreter version is
   proven** — `db_connect`/`db_query`/`db_execute` join the existing
   interpreter-only list, not an exception to it.

---

## The next prerequisite, named plainly

Three separate **Blocked** verdicts above — null safety (reference-spec
§6), exhaustive error handling (§12), and the I/O error hierarchy
(`std_io` §14) — all cite the same missing piece: Nirdosha has no sum type.
That's not a coincidence, and it's worth stating as the single highest-value
next core-language addition once effects (above) ship: a minimal, built-in
`Option<T>`/`Result<T,E>` pair (not general user-defined `variant` — that's
the bigger, `record`-adjacent generics project the type-system-table entries
above already deferred) would unlock all three at once.

**Status: shipped, all the way through (21 Aug 2026).**
`nirdosha_row11_amendment.md` answered the open design question below —
it's a real `enum` grammar production, and every layer of its own §3.6
rollout is now built except layer 5 (extending `refine.rs`/`smt.rs`'s
static-proof boundary set — the Tier-1 bonus prover, not required for
correctness): `struct`/`enum`/`match` (layers 1–4), generics on a
declaration with real structural-per-instantiation type identity, no
monomorphizer pass (layer 6), and `Option(T)`/`Result(T, E)` themselves,
as ordinary generic `enum`s injected into every program (layer 7) — see
that document's §3.6 for the exact layer breakdown,
`compiler/tests/structs_enums.rs`/`compiler/tests/generics.rs` for what's
actually tested. A Nirdosha program can write `enum Option(T) { Some(T),
None }`-shaped types today, including the real prelude ones, without
declaring them. The paragraph below is kept for history; it predates any
of Row 11 shipping.

~~Not designed here, on purpose: what shape that minimal sum type takes —
a real `enum` grammar production, or a narrower pair of builtin generic
types with no user-extensibility — is a genuine open design question this
document's method (start from a concrete ProtoLang mechanism, narrow it)
can't resolve on its own, the same way `TRANSACT.md` didn't guess at
`goal.md` row 10's `ledger.rs` shape before it existed. Flagged so it's the
next thing picked up, not re-discovered from scratch.~~
