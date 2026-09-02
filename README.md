# Nirdosha — निर्दोष ("without fault")

[![build](https://github.com/arunsoman/nirdosha/actions/workflows/build.yml/badge.svg)](https://github.com/arunsoman/nirdosha/actions/workflows/build.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![docs](https://img.shields.io/badge/docs-LANGUAGE.md-blue)](./LANGUAGE.md)
[![Contributing](https://img.shields.io/badge/CONTRIBUTING-read-blue)](./CONTRIBUTING.md)
[![Roadmap](https://img.shields.io/badge/ROADMAP-view-purple)](./PUBLIC_ROADMAP.md)
[![Sponsor](https://img.shields.io/badge/%E2%9D%A4-Sponsor-ea4aaa)](https://github.com/sponsors/arunsoman)

> **A systems language designed for LLMs to write, with a grammar so
> constrained the model can't emit invalid syntax.** No garbage collector,
> no data races, no deadlocks, no integer/buffer overflow — those aren't
> the pitch, they're the proof that a language built for an AI agent to
> write unsupervised can also be trusted to run.

Status: active research prototype. The compiler is a real, runnable Rust
crate (`compiler/`); many safety properties are *proven* today and some are
*aspirational* (called out honestly below). Source files use the `.nir`
extension.

```nirdosha
fn secret(n: i64) -> i64 requires(role: "admin") {
    return n + 1
}

fn work(b: box i64) -> i64 {
    return *b
}

fn main() {
    print("hello, Nirdosha")
    let h: box i64 = box 21
    let t: thread i64 = spawn work(h)
    print(join t)
}
```

```sh
cd compiler && cargo run -- ../examples/hello_above_fold.nir
# hello, Nirdosha
# 21
```

`box` is single-owner — `spawn` moves `h` into the thread, so `main` can
never touch it again; that's checked at compile time, not by convention.
`secret` is gated by `requires(role: "admin")` and is literally uncallable
without an `acquire`d `RoleView` proof — see
[`examples/privileged_fn.nir`](./examples/privileged_fn.nir) for the full
role-acquisition flow.

![334 lines of Nirdosha producing a themed dashboard with live SQLite data, a sortable/searchable vendor table, and a role-gated payout-approval action — then the same screens under a lower-privileged identity, with a field dropped and an action disabled by the server](./demo.gif)

*334 lines, zero UI code — `examples/vendor_ops.nir`, verified with
`wc -l`. `nirdosha serve examples/vendor_ops.nir --theme
examples/vendor_ops_theme.json` derives a live dashboard, a
sortable/searchable table, and a role-gated `Approve` action from two
`struct`s and a `screen`/`dashboard` block. Signed in as `analyst`, the
exact same screen drops the `risk_score` field and column entirely and
disables `Approve` — both enforced by `serve.rs` on every call, not
hidden by client JS. Signed in as `admin`, that same `Approve` action
really flips a row from `requested` to `approved` in SQLite. Nothing
here is simulated — see §6, §7, §11.*

---

## Table of contents

1. [What the name means](#1-what-the-name-means)
2. [Motivation](#2-motivation)
3. [Who this is for (and who it isn't)](#3-who-this-is-for-and-who-it-isnt)
4. [Why not Rust, Go, or Mojo?](#4-why-not-rust-go-or-mojo)
5. [Grammar](#5-grammar)
6. [Features](#6-features)
7. [The UI engine](#7-the-ui-engine)
8. [Benchmarks](#8-benchmarks)
9. [LLM integration](#9-llm-integration)
10. [Try it out today — code in Nirdosha without learning Nirdosha](#10-try-it-out-today--code-in-nirdosha-without-learning-nirdosha)
11. [Honest scope](#11-honest-scope)

---

## 1. What the name means

**Nirdosha** (निर्दोष) is Sanskrit for *without fault / flawless / innocent*.
The name is a design statement, not a marketing claim: the language is
shaped around the goal of a program that passes the compiler being
**provably free of an enumerated set of faults** — use-after-free, data
races, deadlocks, integer/buffer overflow — rather than "fast and usually
safe." When the checker can't prove a correct program correct, the program
is **rejected**, not silently accepted (see §2). That is the price of the
guarantee, and the name states it up front.

## 2. Motivation

The design is driven by **twelve requirements, treated as one set** —
because none of them stands independently of the others. Roughly half are
things a compiler can *prove* (hard), half are things that can only be
*measured* against how humans and models behave (soft). A design that
satisfies only the provable half is Idris2/ATS: correct and nearly unused.
A design that satisfies only the measured half is Go/Python: usable and
unsafe. Nirdosha's thesis is that **both halves must be designed together
from the start**.

| # | Requirement | Class |
|---|---|---|
| 1 | No GC, no manual `free()` | proof (ownership / linear types) |
| 2 | No data races | proof (type system rules out aliased mutation) |
| 3 | No deadlocks | lock-ordering deadlock: proof-by-construction (no mutex primitive exists to acquire out of order); `recv`/`join` deadlock: real runtime detection (a `join`-cycle or every live thread simultaneously blocked traps with a clear diagnostic instead of hanging — `interpreter::DeadlockRegistry`), not full static prevention |
| 4 | No int / buffer overflow | SMT-discharged refinement types, tiered |
| 5 | Native, hardware-speed codegen | engineering (AOT via LLVM/`clang`) |
| 6 | No steep learning curve | measured (small, orthogonal grammar) |
| 7 | Easy for an LLM to write/reason about | measured + one hard sub-property (decidable grammar) |
| 8 | Logical, composable syntax | compositionality of the semantics |
| 9 | AI as a first-class citizen | measured; agent-facing API is hard-typed |
| 10 | Tamper-evidence — detect "alien" code in the binary | proof (reproducible builds, content-addressed source) — **aspirational** |
| 11 | Closed product types, sum types, generics | proof (decidable) — `struct`/`enum`/`match` + per-instantiation generics |
| 12 | Capability-gated access — "who is allowed to call this" is checked, not trusted | proof (statically checked at the call site) — `requires(role/claim: ...)` + `acquire`d `RoleView`/`ClaimView` proofs |

**The constraint that shapes everything** is Rice's theorem (1953): no
algorithm can decide a non-trivial semantic property (termination, race,
overflow) for *every* program in a Turing-complete language. So Nirdosha's
type system is deliberately **conservative** — like Rust, SPARK Ada, F*,
Pony — accepting a smaller language in exchange for the guarantee
"everything this accepts is safe." The real design question is therefore
*where the conservative boundary sits, and what the programmer — human or
model — does at it.* That boundary matters equally for formal-methods
correctness and for whether an LLM can reliably work around it.

## 3. Who this is for (and who it isn't)

The honest fit, not the marketing one.

**Squarely for:**

- **Backend/network services written or maintained by an AI coding agent,
  with no human reviewing every line before it runs.** This is the one
  problem nothing else on the market is built for from the grammar up: an
  LL(1) grammar exported to GBNF lets a sampler force every token an agent
  emits to stay syntactically valid; `--format=json` gives a self-repair
  loop a structured proof obligation instead of a paragraph to guess at;
  `sandbox` is a real OS process and a language primitive, not a bolted-on
  Docker wrapper around output nobody trusts; and there is no mutex in the
  language, so an agent literally cannot generate a lock-ordering deadlock —
  and if generated code still manages a `recv`/`join` deadlock (still
  possible; async messages don't make that vanish), the runtime traps it
  with a clear diagnostic instead of silently hanging the process. If
  you're building an autonomous coding agent, or letting one operate
  against production, this is the concrete gap Nirdosha targets.
- **Compliance-shaped CRUD systems** — trade finance, KYC/onboarding,
  anything where "who is allowed to call this" is part of the spec, not an
  afterthought. `requires(role: "admin")` / `requires(claim: "department",
  "cardiology")` are type-checked at the call site, not `if user.role ==
  "admin"` sprinkled through handlers — §7's UI engine derives the
  login/role gate automatically from the same annotation.
- **Deterministic simulations and audits**, where "run it twice, get the
  same trace" matters. `rand_seed` resets a from-scratch RNG with no OS
  entropy, so a run is byte-for-byte reproducible from a seed.

**Not (yet) for:**

- OS/kernel or embedded work — no `no_std`, no freestanding target; the
  runtime assumes a real OS underneath it for threads and processes.
- General frontend/UI work — `emit-ui`/`serve` generate CRUD + dashboard
  screens from struct/fn naming, not a general application-UI framework.
- Anything where you need the crate ecosystem, hiring pool, or decade of
  production hardening Rust/Go already have. Nirdosha is two days into
  being public; picking it over Rust today is a research bet, not an
  engineering one — see §11.

## 4. Why not Rust, Go, or Mojo?

Nirdosha isn't trying to be a better Rust; it gives up Rust's full
expressiveness in exchange for a grammar an LLM can be forced to stay
inside, and a concurrency model where whole bug classes are
unrepresentable rather than merely unlikely.

| | **Nirdosha** | **Rust** | **Go** | **Mojo** |
|---|---|---|---|---|
| **Target use case** | Deterministic backend services, compliance CRUD, LLM-written agents | General-purpose systems: kernels, browsers, databases, embedded | Cloud-native services, DevOps tooling | AI/ML-first, Python-compatible kernels for CPU/GPU |
| **Memory management** | Affine ownership (`box`/`&`), single-owner heap, no GC | Ownership + borrowing + lifetimes, no GC in safe code | Tracing GC | Ownership/borrowing (Rust-inspired) + Python dynamic layer |
| **Data-race freedom** | Static — no shared mutable state, no aliasing | Static — borrow checker rules them out | Not statically guaranteed (`go test -race` is dynamic-only) | Not yet fully guaranteed |
| **Deadlock freedom** | Lock-ordering: proof-by-construction (no mutex primitive at all). `recv`/`join`: real runtime detection — a `join`-cycle or every live thread simultaneously blocked traps immediately with a diagnostic, not full static prevention | Possible — `Mutex`/`Condvar`/async can deadlock | Possible — channels + `sync.Mutex` can deadlock; the runtime detects only the case where *every* goroutine is asleep, not a partial deadlock among some of them | Not a current guarantee |
| **LLM writability** | LL(1) grammar exported to GBNF for constrained decoding; structured JSON diagnostics | LLMs default to Python 90–97% of the time; Rust's API churn compounds it | Easy to generate syntactically; no constrained decoding or proof obligations built in | Easy for Python-like snippets; no published GBNF/constrained-decoding integration |
| **Maturity** | 2-day-old public research prototype | Production-ready, decade of hardening | Production-ready, huge ecosystem | Pre-1.0, stabilizing |

If you're wondering "why not just use Rust, Go, or Mojo?" — the honest
answer is that those languages already solve memory safety and
concurrency for teams that can invest in their learning curve. Nirdosha
targets a different, narrower problem: AI agents writing backend code
unsupervised, where the grammar itself has to make invalid syntax and
whole bug classes impossible to emit, not just unlikely. If you love
Rust, keep using it — Nirdosha is a research bet on a different user,
not a replacement.

## 5. Grammar

The parser (`compiler/src/parser.rs`) is **hand-written recursive descent
with strictly one token of lookahead and no backtracking, anywhere** — the
operational definition of **LL(1)**. Binary-operator precedence is handled by
precedence climbing, not left-recursive grammar rules, which is what keeps
the expression grammar LL(1)-parseable without a transformation step.

### Independent cross-checks

Nirdosha is unusual in that its grammar claims are **verified by external
tools**, not just asserted by re-reading the hand-written parser:

- **`grammar_check/`** — feeds the grammar to `lalrpop` (an independent
  LALR(1) generator). This cross-check found something real: because the
  language has **no statement separator** (no semicolons, no significant
  newlines), the grammar as an abstract CFG is ambiguous wherever a token
  could either extend an expression or start a new statement. The parser
  resolves every such case deterministically with one rule:
  **always extend the current expression over ending the statement**
  (shift over reduce, no exception). So `return x` followed on the next
  line by `-y` parses as `return (x - y)`, not as two statements. The
  hand-written parser is unambiguous; the bare CFG is not, without this
  rule stated explicitly — a distinction only visible by running a second
  tool against it.
- **`grammar_export/`** — hand-translates the grammar to **GBNF**
  (`compiler/nirdosha.gbnf`), the constrained-decoding grammar artifact for
  LLM samplers (row 7). Validated two ways: by `llama-cpp-gbnf` (the real
  llama.cpp parser, which caught a real bug during writing), and by running
  a corpus — every shipped `.nir` example plus rejection cases — through
  the GBNF and confirming it accepts/rejects exactly what the real compiler
  does.

### EBNF shape

Top-level: `item ::= fn_decl | struct_decl | enum_decl | screen_decl |
dashboard_decl`. The full EBNF lives in [`GRAMMAR.md`](./GRAMMAR.md).
Notable choices:

- **No statement separators** (see the disambiguation rule above).
- **Keyword-heavy over symbol-heavy** syntax (aids LLM reliability, row 7).
- **Construction is an ordinary call**, not a separate literal form — a
  `struct`'s name is its own positional constructor via `Expr::Call`.
- **Contextual keywords** (`field`, `action`, `tile`, `chart`, `role`,
  `claim`) are matched only in their leading slot, so they stay ordinary
  identifiers everywhere else.

## 6. Features

Mined from the actual implementation (`compiler/src/`), not the design
docs. See [`LANGUAGE.md`](./LANGUAGE.md) for the authoritative reference.

### Types
Signed/unsigned ints (`i8`..`i64`, `u8`..`usize`), `f64` (IEEE-754, saturating),
`bool`, `unit`, `str` (UTF-8, `Arc<str>`-backed), `box T` (single-owner heap),
`&T` (read-only borrow of an identifier), `thread T`, `chan T` (unbounded
MPMC), `sandbox`, `tcp`/`tcp_listener`, `file`, `VerifiedIdentity`/
`RoleView`/`ClaimView` (identity proofs), `Vector(T, N)` and
`Matrix(T, R, C)` (fixed-shape, fixed-length — `Vector(f64,3) ≠
Vector(f64,4)`).

### Ownership & affine types (row 1, 2)
`box T` is single-owner; moving into `spawn`/`send`/`stop` consumes it.
`&T` is a read-only borrow. The ownership checker (`ownership.rs`) enforces
the linear discipline statically — no use-after-free, no aliased mutation,
no GC. Cyclic structures are the known hard case (an arena escape hatch is
the planned answer).

### Concurrency (rows 2, 3)
- `spawn f(args)` — real OS thread, returns `thread T`; `join` consumes it
  once.
- `chan T` — unbounded MPMC channel; the handle is freely copyable, the
  *payload* moves through `send`.
- `sandbox worker(args)` — a **real, separate OS process** (a fresh
  `nirdosha` invocation), affine `sandbox` handle; `stop` terminates it. A
  handle that goes out of scope unstopped still kills its process (no
  zombies) — deterministic cleanup, not discipline-dependent.

There is no mutex in the language, so a *lock-ordering* deadlock is not
expressible at all. Hot paths that want shared-memory locks get re-cast as
messages — but messages have their own deadlock shape (a `recv` nobody
ever `send`s to, two threads mutually `join`ing each other), and that one
*is* expressible. `interpreter::DeadlockRegistry` catches it at runtime
instead: a `join`-cycle is detected precisely (a real wait-for graph over
`join` edges); a `recv` gets a coarser, still-sound fallback (every live
thread simultaneously blocked — the same condition Go's own runtime
deadlock detector checks, generalized to also catch a `join`-cycle
mid-program, which Go's whole-process-only check misses). The one
disclosed gap: a `recv` blocked forever while some *other*, unrelated
thread stays busy on its own work is invisible to the coarse check — that
would need real points-to tracking of channel handles, not attempted.

### Effects (rows 4, 9)
`effect(...)` annotations are **fully inferred by default** (no notation
paid unless load-bearing). A declared annotation is checked against what
the body actually does — an effect performed but not declared is a
compile error, not a silent gap. `effect(pure)`, `effect(io)`, etc.

### Refinement types & SMT (row 4)
Integer/buffer bounds proofs are discharged by an SMT solver (Z3, via
`refine.rs` / `smt.rs`) at compile time, **tiered**: Tier-1 attempts a
static proof; Tier-2 inserts a runtime guard when a fact isn't SMT-decidable
in scope; `audited "justification" { ... }` is the one documented escape
hatch that suppresses guards with a human-language justification (the one
place a human review gate stays mandatory).

### Determinism (row 9)
`rand_seed(seed)` resets a from-scratch SplitMix64 RNG (no OS entropy, no
hidden global state). `rand_f64` / `rand_gaussian` draw from it. A
simulation's random draws are byte-for-byte reproducible from a seed — the
foundation of `bench/`'s `run-deterministic` (run + hash-check) and of
auditable simulations.

### Identity, roles, claims (row 12)
`oidc_validate_token(...)` validates an externally-issued OIDC/JWT ID token
(HMAC-SHA256) and returns a `VerifiedIdentity` — the runtime **never mints
tokens, only consumes them**. `check_role` / `extract_claim` (and their
dotted-path siblings for nested IdP schemas like Keycloak) produce
`RoleView` / `ClaimView` proofs. `requires(role: "admin")` /
`requires(claim: "department", "cardiology")` on a `fn` demands such a
proof at the call site — capability-gated, statically tracked.

### Workflows (row 9)
`workflow Name { data { ... } state ... }` declares a durable, named
state machine — `state`s, `on <Event> -> <Target>` transitions (a `link`
mark makes one an unauthenticated, single-use magic link), and
`on_entry`/`on_exit` actions that can call the notification builtins
(`send_email`/`send_sms`/`send_push`/`notify`). It's **pure desugaring,
not a new runtime primitive**: `workflow_lower.rs` turns the block into
ordinary `fn`/`enum`/`struct` declarations right after parsing, so
`nirdosha serve`'s automatic `POST /api/<fn>` RPC exposure — and every
other later pass — never sees `workflow` syntax itself. `on_entry`/
`on_exit` actions are crash-durable (logged before running, replayed on
restart), the same discipline `transact` already uses. Interpreter-only,
the same way `transact`/`db`/`mq` are (§11): `nirdosha build`/`emit-llvm`
cleanly rejects a program using `workflow`, naming the specific
unsupported builtin, never a silent mis-compile. A `state` can also
declare `owner: role(...)`/`claim(...)` (who may fire its outgoing
events, checked per-instance at runtime, not statically) and
`label: "..."` -- `nirdosha serve`/`emit-ui` render a generated
"Workflows" nav section from these: a per-role "what's waiting on me"
queue, each row's own buttons driven by that row's own current state.
Full grammar and runtime protocol in [`WORKFLOW.md`](./WORKFLOW.md).

### Data types & generics (row 11)
`struct`, `enum`, and `match` (exhaustive, no wildcard/binding patterns in
v1). Type parameters are **concrete-per-instantiation**: `Pair(i64, str)`
and `Pair(f64, bool)` are different, unrelated types — no monomorphizer
pass exists or is needed. `Option(T)` and `Result(T, E)` are ordinary
generic `enum`s injected into every program at parse time. Affinity
propagates through struct/enum fields and through a generic instantiation's
own concrete type arguments, the same way it does through `box`.

### Dense linear algebra (row 5)
`Vector` and `Matrix` with `transpose`, `dot`, `cross` (3-vectors),
`zeros`/`ones`/`identity`, `sum`, `len`, `norm`/`norm1`/`norm_inf`,
`frobenius_norm`, `trace`, matrix×matrix and matrix×vector multiply with
shape checking at typecheck time. `Vector * Vector` is a **type error by
design** (ambiguous inner vs. outer product) — use `dot()`. The whole
linalg feature set is modeled on Julia and now **compiled** (see §8).

### I/O & networking
`print`, `file` handles (`open`/`read`/`write`/`stop`), `tcp` client
(`connect`), `tcp_listener` (`listen`/`accept`/`stop`), JSON, a `db`
builtin (SQLite via rusqlite, or Postgres via `postgres`/
`postgres-native-tls` — picked by `db_connect`'s own connection-string
scheme, see [`PROTOLANG_PORT.md`](./PROTOLANG_PORT.md)'s "Locked design
5: DB"), `transact` semantics (see [`TRANSACT.md`](./TRANSACT.md))
including cross-process transactions, and a `redis`-backed message queue.

### Structured diagnostics (row 9)
Every error — type, ownership, runtime — has **one structured shape**
(`Diagnostic` JSON via `--format=json`), not English prose. This is what
makes the LLM self-repair loop (§9) possible: the model gets a
machine-parseable proof obligation back, not a sentence to guess at.

## 7. The UI engine

Nirdosha ships a **declarative UI DSL** plus a UI generator that derives a
full CRUD + dashboard web application from a program's `struct`
declarations and its function-naming conventions — **no UI syntax needed
for the common case**.

### Zero-syntax inference
`compiler/src/ui_gen.rs` looks for `list_<struct>`, `create_<struct>`,
`update_<struct>`, `delete_<struct>`, `get_<struct>`, and
`stat_<name>` / `chart_<name>` functions and generates a complete
HTML/JS CRUD app + dashboard from them alone.

### Optional `screen` / `dashboard` blocks
For what a naming convention can't express (a friendlier title, a
relabeled field, a custom action), there is an **additive** DSL:

```nirdosha
struct Product {
    id: i64,
    name: str,
    price_cents: i64,
    stock: i64,
}

fn list_product() -> Result(json, str) { ... }
fn create_product(p: Product) -> Result(i64, str) requires(role: "admin") { ... }
fn restock_product(id: i64) -> Result(i64, str) requires(role: "admin") { ... }

screen Product {
    title: "Catalog"
    field name { label: "Product Name" pattern: "^[A-Za-z0-9 ]+$" }
    field stock { min: 0 }
    action "Restock +10" -> restock_product {
        style: "outlined"
        confirm: "Restock this product by 10 units?"
    }
}

dashboard {
    tile "Products" -> stat_product_count
    chart "By Price" -> chart_products_by_price
}
```

- `screen`/`dashboard` are real reserved keywords (top-level items like
  `struct`/`fn`); `field`/`action`/`tile`/`chart` are contextual keywords.
- Typechecked: `screen <Name>` must name a real `struct`; `field`/`action`
  targets must resolve; `view`/`edit` must be `role(...)`/`claim(...)`;
  `pattern` must compile as a regex and only apply to a `str` field;
  `format` must be one of `"email"`/`"phone"`/`"date"`/`"url"`/`"uuid"`;
  `min`/`max` must only apply to a numeric field.
- **Inert to native codegen** — `nirdosha build` compiles a program
  containing `screen`/`dashboard` cleanly (codegen never inspects them).
  They're consumed only by `nirdosha emit-ui` / `nirdosha serve`.
- `view`/`edit` (role/claim visibility) and `pattern`/`format`/`min`/`max`
  (format validation) are enforced for real, both client- and
  server-side — `serve.rs` is the actual security/validation boundary,
  the client-side version is cosmetic convenience only.
- Tracked-but-not-wired (see [`compiler/UI_DSL_TODO.md`](./compiler/UI_DSL_TODO.md)):
  `paginate`, `searchable`/`sortable`.
- Deliberately closed, not gaps: one chart type (inline-SVG bar chart,
  no line/scatter/heatmap/treemap/geo/3D, no Recharts/D3/Victory
  dependency), four fixed built-in animations (`fade-in`/`slide-up`/
  `scale-in`/`pop`, no custom transitions or Framer-Motion-style
  gesture/physics motion), and a fixed seven-kind form-control set
  (text/number/checkbox/select/struct/readonly/date — no rich text
  editor, color picker, drag-drop upload preview, autocomplete,
  calendar/scheduler, or signature pad). See `compiler/UI_DSL_TODO.md`'s
  "Deliberate non-goals" section for the full rationale.

### Design tokens: `--theme`
`nirdosha emit-ui`/`serve --theme theme.json` layers a full design
system on the baked-in Material Design 3 defaults — brand/neutral color
ramps, fonts, radius, shadow, density, real entrance/hover/press
animations, three dark-mode strategies, and CSS-only layout shell
variants (`LANGUAGE.md` §11b). Every section is optional; a program
with no `--theme` renders exactly as before this existed.
`nirdosha serve` re-reads the file on a TTL, so a redeployed theme
takes effect without restarting the server.

### Serving
`nirdosha serve <file.nir>` runs a `tiny_http` server exposing the inferred
functions as a JSON API (`POST /api/<fn>`), with optional OIDC JWKS/issuer/
audience gating — the same identity primitives as §6, applied to HTTP.

## 8. Benchmarks

Full methodology and caveats in [`benchmarks/RESULTS.md`](./benchmarks/RESULTS.md).
Numbers are **compiled-vs-compiled**, best-of-3 wall time, every output
verified bit-identical across all languages before any timing was trusted.
The comparison that matters most is against C, since both are AOT-compiled
to native code on equal footing — the Julia numbers below are informative
but not apples-to-apples (see caveat).

### Group A — scalar / control flow (the credible comparison)
Nirdosha compiled (`-O2`, default) vs. C.

| Benchmark | C (gcc -O2) | Nirdosha (`-O2`) |
|---|---:|---:|
| `fib(35)` | 0.018 s | **0.026 s** |
| `floatloop` (2×10⁸) | 0.443 s | **0.436 s** |

Within **1.4×** of `gcc -O2` on `fib`, and **noise-level tied** with C on
`floatloop` — exactly where a thin LLVM-backed AOT compiler should land.
(For reference: interpreted Nirdosha on `fib(35)` was 16.1 s — 620× slower
than compiled; compiling linalg was the whole point of prioritizing
codegen there.)

### Group B — dense linear algebra (Julia-derived features)
Nirdosha compiled vs. C vs. Julia (JIT). 200,000 iterations.

| Benchmark | C (gcc -O2) | Nirdosha (compiled) | Julia (JIT) | vs. C |
|---|---:|---:|---:|---:|
| `matmul` (4×4) | 0.0102 s | **0.0018 s** | 0.794 s | 5.7× faster |
| `det` (4×4) | 0.0093 s | 0.0272 s | 0.993 s | 2.9× slower |
| `dot` (8-vec) | 0.0023 s | **0.0017 s** | 0.418 s | 1.4× faster |
| `kalman` (4-state) | 0.0798 s | 0.3274 s | 2.735 s | 4.1× slower |

**Caveat on the Julia column:** these numbers include Julia's JIT
compilation overhead, not steady-state execution after warmup — an
AOT-compiled binary vs. a JIT session isn't a fair fight, and the "vs.
Julia" gap is mostly measuring that, not raw execution speed. Treat the
Julia column as directional, not a benchmark claim; the **vs. C** column
is the one worth trusting. On that measure Nirdosha wins `matmul`/`dot`
(fully unrolled at codegen time into straight-line IR) and loses
`det`/`kalman` (a runtime-parameterized native call LLVM can't inline for
`n=4` — a future per-size monomorphization pass would likely close the
gap).

Benchmarks live in [`benchmarks/{c,julia,nirdosha}/`](./benchmarks/).

## 9. LLM integration

Nirdosha is designed so an LLM is a **first-class programmer**: agents emit
**typed AST/IR fragments the compiler validates before splicing**, not raw
text, and compiler errors return **structured proof obligations**, not prose.
This is grounded in capabilities that already exist today:

| Problem today | Nirdosha's answer (shipped/planned) |
|---|---|
| Code looks right but is syntactically invalid | LL(1) grammar + GBNF export → **constrained decoding** |
| Parse errors are prose → guesswork repair | `--format=json` → **structured `Diagnostic`** |
| Type-checks but has subtle safety bugs | `refine.rs` + `smt.rs` → **SMT-discharged bounds proofs** |
| Running LLM code safely = bolted-on Docker | `sandbox`/`stop` → **real OS process isolation, language primitive** |
| Repeated runs give different results | `rand_seed` → **deterministic RNG** |
| Can't improve a generation incrementally | `validate_fragment` → **type-check expression fragments in context** |
| Can't measure domain progress | `bench/` corpus → **pass@1 + self-repair rate** |

### Agent-facing API
[`nirdosha-agent-api.md`](./nirdosha-agent-api.md) specifies a local
HTTP API (`http://localhost:7878`) wrapping these capabilities into
callable endpoints, grouped:

- **A. Code Generation & Validation** — `/v1/generate`, `/v1/validate`,
  `/v1/validate-fragment`, `/v1/repair`, `/v1/splice`
- **B. Execution & Simulation** — `/v1/run`, `/v1/run-sandboxed`,
  `/v1/run-deterministic`, `/v1/build`
- **C. Compiler Introspection** — `/v1/grammar` (the GBNF), `/v1/types`,
  `/v1/builtins`, `/v1/emit-ast`
- **D. Benchmarking & Evaluation** — `/v1/bench/run`, `/v1/bench/repair-rate`
- **E. Provenance & Reproducibility** — `/v1/provenance/hash`,
  `/v1/provenance/verify`, `/v1/provenance/audit` (row 10 — planned)

Every endpoint references something already built or explicitly planned
in [`Nirdosha_Unified_Plan.md`](./Nirdosha_Unified_Plan.md); nothing is
aspirational hand-waving.

### Constrained decoding
`compiler/nirdosha.gbnf` (produced/tested by `grammar_export/`) is a
grammar-constrained-decoding artifact an LLM sampler (llama.cpp etc.) can
load to **guarantee every token emitted stays inside Nirdosha's syntax** —
the same way an LSP keeps an IDE's completions valid. This is the hard
sub-property of row 7, and it's decidable/checkable against the real
compiler's accept/reject behavior.

### Benchmark harness
[`bench/`](./bench/) is a pass@1 + self-repair-rate harness with a
`corpus.json` of 23 tasks spanning the language's shipped features. It
feeds each attempt's structured `Diagnostic` back in as the next attempt's
context — the same re-prompt loop a real self-repair integration would use.
It ships mock models today; wiring a real LLM API is a distinct, separate
piece of work the harness is built to plug into.

### Agent skills — try Nirdosha without learning Nirdosha
You don't need to read `LANGUAGE.md` or learn a new syntax to build
something real in Nirdosha — describe what you want in plain English to
an LLM and let it write the `.nir` code. [`agent-skills/nirdosha/`](./agent-skills/nirdosha/)
packages the rules an LLM needs to get that code right on the first
try — no GBNF sampler required, just a markdown file most agentic
tools already know how to read: a Claude Code Skill, an `AGENTS.md`
(Codex CLI, Amp, and other tools that read that convention), Cursor
rules, GitHub Copilot instructions, Windsurf, Cline, and — for the
true zero-install path — [`paste-anywhere-prompt.md`](./agent-skills/nirdosha/paste-anywhere-prompt.md),
a self-contained prompt you paste into any chat LLM (ChatGPT, Claude.ai,
Gemini, ...) with no file access or tool use needed. Every variant is
the same content verified against the real compiler — install one in
~10 seconds and an agent that has never seen Nirdosha before can write
it correctly.

This isn't a claim taken on faith: the prompt has been used, unmodified,
to generate several full working applications end to end — an
e-commerce store, a food-delivery platform, a telecom revenue-assurance
system, an online trading platform — each hundreds of lines, each
written by an LLM with no prior Nirdosha exposure from nothing but a
plain-English description. Every real compiler error the exercise
turned up (an ownership edge case, a silent JSON-unwrap footgun, a
markdown-fence copy/paste artifact) was folded back into `core.md` and
propagated to all seven derived files, so the next model to use the
prompt doesn't repeat it. The loop — generate, compile, fix, feed the
fix back into the prompt — is how this guide gets better, not a one-time
write-up.

## 10. Try it out today — code in Nirdosha without learning Nirdosha

**Don't want to learn the syntax first?** Paste
[`agent-skills/nirdosha/paste-anywhere-prompt.md`](./agent-skills/nirdosha/paste-anywhere-prompt.md)
into any LLM chat (ChatGPT, Claude.ai, Gemini, ...), describe what you
want in plain English, and it writes the `.nir` code for you — no
install needed for that step. See [§9](#9-llm-integration) for what
that's been used to build. The install below is for actually running
the code it hands you back.

### Install (no Rust, no clang, no z3)

```sh
# macOS / Linux
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/arunsoman/nirdosha/main/scripts/install.sh | sh
```

```powershell
# Windows
irm https://raw.githubusercontent.com/arunsoman/nirdosha/main/scripts/install.ps1 | iex
```

Prebuilt binaries: Linux x86_64 and Windows x86_64 have Z3 statically
vendored — nothing to install first, no linker errors. macOS binaries
link the *system* Z3 instead (`brew install z3` first) — `z3-src`
416.0.2 doesn't compile against the AppleClang on current macOS
toolchains, a real upstream incompatibility, not a packaging choice;
tracked in [`PUBLIC_ROADMAP.md`](./PUBLIC_ROADMAP.md). `clang` is only
needed later, and only on the machine running `nirdosha build`/
`emit-llvm` (native codegen); interpreting, `emit-ui`, and `serve` all
work straight out of the download on every platform. See
[GitHub Releases](https://github.com/arunsoman/nirdosha/releases) to
download a binary directly instead of piping the script.

**Windows is untested.** The compiled `tcp`/`tcp_listener` runtime was
ported to Windows' socket API (`RawSocket` vs. Unix's `RawFd`) but has
not been verified against a real Windows machine — no Windows
environment was available to this project at the time of this release.
Everything else (interpret, `emit-ui`, `serve`, the rest of native
codegen) doesn't touch that code path and should be unaffected. Please
report an issue if something doesn't work on Windows — it's the one
platform here running on belief that the port is correct, not on a
real end-to-end test.

### Get the examples

The commands below (and "build from source") reference `examples/*.nir`
by path — that's this repo, not something the installer above downloads
on its own:

```sh
git clone https://github.com/arunsoman/nirdosha.git
cd nirdosha
```

### Or build from source (for contributors)

```sh
cd compiler
cargo build --release
# binary: compiler/target/release/nirdosha
```

Toolchain: Rust (edition 2024), plus two system libraries the build links
against directly — install these *before* `cargo build` or the build fails
with a linker error, not a friendly message:

```sh
# Debian/Ubuntu
sudo apt install clang libz3-dev

# macOS (Homebrew)
brew install llvm z3

# Arch
sudo pacman -S clang z3
```

`clang` is invoked at runtime by `nirdosha build`/`emit-llvm` (native codegen);
`z3` is linked at compile time for the SMT refinement layer (row 4) and is
required even just to build the compiler, not only to use that feature. (The
prebuilt binaries above sidestep this with `cargo build --features dist`,
which vendors Z3 from source instead — see
`.github/workflows/release.yml`.)

### Run a program

```sh
# Interpret (always works for every construct)
nirdosha examples/hello.nir

# Compile to a native binary (subset — see LANGUAGE.md §10)
nirdosha build examples/factorial.nir -o factorial
./factorial

# Inspect the program
nirdosha emit-llvm examples/factorial.nir   # print LLVM IR
nirdosha emit-ast  examples/matrices.nir    # print AST as JSON
```

### Scaffold a new project

```sh
nirdosha init shop
# writes ./shop/:
#   shop.nir      -- starter source (Email/RoleMapping admin-panel
#                     fixtures by default; --no-email/--no-roles/--sms/
#                     --push to change which ones)
#   nirdosha      -- a copy of this executable, so the folder can be
#                     moved to another machine and run standalone (same
#                     OS/arch only -- no cross-compilation)
#   run.sh        -- launches it: nirdosha serve shop.nir ... (run.bat
#                     on Windows)
#   jwks.json     -- an empty placeholder key set so run.sh works out of
#                     the box; requires(role: ...) routes 401 until real
#                     identity-provider values replace the placeholder
#                     --jwks-file/--issuer/--audience in run.sh

cd shop && ./run.sh
```

`--dest <path>` puts the `shop/` folder under `<path>` instead of the
current directory; `--force` overwrites an existing one. This is tooling
convenience only — Nirdosha has no compiler-level notion of "a project"
beyond the one `.nir` file inside the folder.

### Generate / serve a UI

```sh
nirdosha emit-ui examples/store.nir -o store.html   # self-contained CRUD HTML
nirdosha serve   examples/store.nir --port 8080     # JSON API server
```

### Structured diagnostics (for LLM/agent loops)

```sh
nirdosha examples/broken.nir --format=json   # Diagnostic JSON on failure
```

### Explore the language

- **Full feature reference**: [`LANGUAGE.md`](./LANGUAGE.md)
- **Full grammar**: [`GRAMMAR.md`](./GRAMMAR.md)
- **Every example in one browsable file**: [`all_examples.md`](./all_examples.md) (not in git; generated locally)
- **Design rationale**: [`goal.md`](./goal.md), [`Nirdosha_Unified_Plan.md`](./Nirdosha_Unified_Plan.md)
- **Sandboxing**: [`SANDBOXING.md`](./SANDBOXING.md) · **Transactions**: [`TRANSACT.md`](./TRANSACT.md)
- **Agent API**: [`nirdosha-agent-api.md`](./nirdosha-agent-api.md)

Start small (`hello.nir` → `factorial.nir` → `ownership.nir` →
`borrow.nir`), then concurrency (`threads.nir`, `channels.nir`,
`sandbox.nir`), then the domain-scale examples (`store.nir`,
`transact.nir`, `rev-assurence/`, `trade-finance/`).

## 11. Honest scope

This README states what's real and checkable today vs. what's aspirational,
following the project's own discipline (see `goal.md`'s "Honest correction"
notes):

- **Shipped and checkable**: ownership/affine, concurrency primitives,
  `sandbox`/`stop`, effects inference, `struct`/`enum`/`match`/generics,
  `Option`/`Result` prelude, `Vector`/`Matrix` linalg (compiled), `str`/
  `tcp`/`connect`/`listen`/`accept` (compiled), `box`/`&`/`*` (compiled),
  deterministic RNG, identity/role/claim, structured diagnostics, the UI
  engine (`emit-ui`/`serve`), project scaffolding (`init`), the GBNF
  artifact, the benchmark harness.
- **Interpreter-only (rejected at compile time, not mis-compiled)**:
  `spawn`/`join`/`thread`/`chan`/`send`/`recv`, `sandbox`, and
  `struct`/`enum`/`match` over *affine-containing* payloads, plus every
  `db`/`json`/`http` builtin, identity (`oidc_validate_token`/
  `check_role`/`extract_claim`), `transact`, and `workflow` — none of
  these is in `codegen.rs`'s `PHASE4_BUILTINS`/`PHASE5_BUILTINS`/
  `STR_CRYPTO_BUILTINS`/`RAND_BUILTINS` allowlists, so `nirdosha build`/
  `emit-llvm` names the specific unsupported builtin and stops, rather
  than silently miscompiling it. A program that never actually uses one
  compiles normally.
- **Aspirational, not built**: row 10's full ambition (reproducible builds,
  content-addressed source, capability manifests at the kernel boundary, a
  signed provenance chain) — a future implementation pass extending the
  deterministic-RNG foundation, not a current claim.
- **Open follow-ups**: per-size monomorphization of the `det`/`kalman`
  runtime kernels; `bench/`'s real-LLM integration; UI DSL's `paginate`/
  `searchable`/`sortable` DSL keys (`nirdosha serve --db` already
  provides real sorting/search/pagination unconditionally per struct —
  see §7 — these two keys specifically remain parsed but inert).

For precise syntax and semantics, the compiler sources under
[`compiler/`](./compiler) are the authoritative reference.

---

## FAQ

**Is Nirdosha production-ready?**
Not yet. It's an active research prototype. Many safety properties are
proven today; others are explicitly marked aspirational above. See §11
and [`ROADMAP.md`](./ROADMAP.md) for the full status.

**Why not just use Rust?**
See §4. Short version: Rust already solves memory safety and
concurrency for teams that can invest in its learning curve. Nirdosha
targets a narrower, different problem — AI agents writing backend code
where the grammar itself has to make invalid syntax impossible to
emit, not just unlikely. If Rust already works for you, keep using it.

**What compiles today vs. what only runs in the interpreter?**
Compiled: numerics, `box`/`&`/`*`, `str`, `tcp`, `Vector`/`Matrix`,
non-affine `struct`/`enum`/`match`, deterministic RNG. Interpreter-only:
`spawn`/`chan`, `sandbox`, `db`/`json`/`http`, identity, `transact`,
`workflow`. A program that avoids the interpreter-only features
compiles to a native binary; one that doesn't is rejected at compile
time, not silently mis-compiled. Full list in §11.

**How do I report a bug?**
Run `nirdosha <file.nir> --format=json` and paste the `Diagnostic` JSON
into a GitHub issue. If it's a security issue (a type-checker/ownership
soundness hole, an auth bypass, anything that breaks a safety
guarantee this README claims), see [SECURITY.md](./SECURITY.md)
instead — report it privately, not as a public issue.

**How can I contribute?**
See [CONTRIBUTING.md](./CONTRIBUTING.md). Docs, examples, and `.nir`
test cases are the fastest way to start.

**Where's the roadmap?**
[`PUBLIC_ROADMAP.md`](./PUBLIC_ROADMAP.md) for the scannable version;
[`ROADMAP.md`](./ROADMAP.md) for the full internal tracker with
verification detail.

**What's the license?**
MIT — see [LICENSE](./LICENSE).

---

*निर्दोष — designed so that what the compiler accepts is, provably, without
fault.*