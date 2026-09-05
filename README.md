# Nirdosha — निर्दोष ("without fault")

[![build](https://github.com/kannamma-labs/nirdosha/actions/workflows/build.yml/badge.svg)](https://github.com/kannamma-labs/nirdosha/actions/workflows/build.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Wiki](https://img.shields.io/badge/docs-wiki-blue)](https://github.com/kannamma-labs/nirdosha/wiki)
[![Contributing](https://img.shields.io/badge/CONTRIBUTING-read-blue)](./CONTRIBUTING.md)
[![Governance](https://img.shields.io/badge/GOVERNANCE-read-blue)](./GOVERNANCE.md)
[![Roadmap](https://img.shields.io/badge/ROADMAP-view-purple)](./docs/PUBLIC_ROADMAP.md)
[![Maintainers](https://img.shields.io/badge/maintainers-5-green)](./MAINTAINERS.md)
[![Sponsor](https://img.shields.io/badge/%E2%9D%A4-Sponsor-ea4aaa)](https://github.com/sponsors/arunsoman)
[![Open in GitHub Codespaces](https://github.com/codespaces/badge.svg)](https://codespaces.new/kannamma-labs/nirdosha?quickstart=1)

> **A systems language built so an AI agent can write and run backend
> code with no human reviewing every line first.** No garbage
> collector, no data races, no deadlocks, no integer/buffer overflow —
> proven in the compiler today, not promised for later. Linux, macOS,
> and Windows binaries ship on every release, verified by CI on all
> three on every push.

Real, working software today: a full compiler and interpreter
(`crates/compiler/`), an LL(1) grammar exported to GBNF for
constrained decoding, SMT-backed overflow/bounds proofs, a durable
`transact`/`workflow` engine, identity/RBAC, and a declarative UI layer
that turns a `struct` into a working, role-gated web app with zero UI
code. 32 shipped capabilities, transparently tracked, on the
[Public Roadmap](./docs/PUBLIC_ROADMAP.md).

## Two ways in

**Don't write code? You don't need to.** Paste
[`agent-skills/nirdosha/paste-anywhere-prompt.md`](./agent-skills/nirdosha/paste-anywhere-prompt.md)
into any LLM chat (ChatGPT, Claude, Gemini) and describe what you want
in plain English — it writes working `.nir` code for you. This exact
prompt has already produced a working e-commerce store, a food-delivery
platform, a telecom revenue-assurance system, and an online trading
platform, each hundreds of lines, each from an LLM with zero prior
Nirdosha exposure. See [LLM Integration](https://github.com/kannamma-labs/nirdosha/wiki/LLM-Integration)
for the full mechanism and evidence.

**Write code? Start with the language itself.**
[`examples/syntax/`](./examples/syntax/) is a progressive walkthrough —
`hello_nir.nir` to a 500-line multi-module enterprise app — one level
at a time. [`examples/features/`](./examples/features/) is the
complete reference: 47 independently-runnable files, one per language
feature, from scalar types through `sandbox`/`transact`/`workflow` to
the declarative UI layer. Or skip both and jump straight into
[**GitHub Codespaces**](https://codespaces.new/kannamma-labs/nirdosha?quickstart=1) —
zero local setup, building in about a minute.

## What Nirdosha code looks like

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

`box` is single-owner — `spawn` moves `h` into the thread, so `main`
can never touch it again; that's checked at compile time, not by
convention. `secret` is gated by `requires(role: "admin")` and is
literally uncallable without an `acquire`d `RoleView` proof — see
[`examples/features/33_privileged_functions.nir`](./examples/features/33_privileged_functions.nir)
for the full role-acquisition flow.

Try something real right now:

```sh
git clone https://github.com/kannamma-labs/nirdosha.git && cd nirdosha
cd crates/compiler && cargo run -- ../../examples/syntax/hello_nir.nir
# 8
# hello, nirdosha
```

## From two `struct`s to a live, role-gated dashboard

![A themed dashboard with live SQLite data, a sortable/searchable table, and a role-gated approval action — the same screen under a lower-privileged identity, with a field dropped and an action disabled by the server](./demo.gif)

`nirdosha serve <file.nir> --port 8080` turns a `struct` plus a
`screen`/`dashboard` block into a live web app: a sortable/searchable
table, a themed dashboard, and role-gated actions — derived, not
hand-written. Field-level RBAC and format validation are enforced
server-side on every request, not hidden in client JS: sign in with a
lower-privileged role and a gated field or action disappears from the
response itself. See it end to end in
[`examples/syntax/enterprise_app.nir`](./examples/syntax/enterprise_app.nir)
(517 lines — vendor management, a purchase-order approval workflow, a
durable payment disbursement, and the dashboard on top):

```sh
cd crates/compiler
cargo run -- serve ../../examples/syntax/enterprise_app.nir --port 8080
# nirdosha serve: listening on http://127.0.0.1:8080
```

Nothing here is simulated — see the
[UI Engine](https://github.com/kannamma-labs/nirdosha/wiki/UI-Engine) wiki page.

## A real plugin ecosystem, not just built-ins

`db_connect`/`mq_connect_via` dispatch by URL scheme to a plugin at
runtime, so any backend a Rust crate exists for is reachable from plain
Nirdosha source with **no new syntax**:

```nirdosha
db_connect("mysql://user:pass@host/db")     // -> a MySQL-backed `db` handle
mq_connect_via("activemq://host:61613")     // -> an ActiveMQ-backed `mq` handle
```

Five real reference plugins ship today, each a genuine Rust crate
wrapping a real client library — MySQL, ActiveMQ, Cassandra, Neo4j,
HBase — reviewed and listed in [`TRUSTED_PLUGINS.md`](./TRUSTED_PLUGINS.md).
SQLite, Postgres, and Redis are built in with no plugin needed.

## Why this exists, in one paragraph

Nirdosha targets one specific problem: **a backend service written and
maintained by an AI coding agent, with no human reviewing every line
before it runs.** An LL(1) grammar exported to GBNF lets a sampler
force every token an agent emits to stay syntactically valid;
`--format=json` gives a self-repair loop a structured proof obligation
instead of a paragraph to guess at; `sandbox` is a real OS process and
a language primitive, not a bolted-on Docker wrapper; there is no
mutex in the language, so an agent literally cannot generate a
lock-ordering deadlock. It isn't trying to be a better Rust — see the
[wiki](https://github.com/kannamma-labs/nirdosha/wiki) for the full case.

## Nirdosha vs. Rust, Go, Mojo — the one-line version

| | **Nirdosha** | **Rust** | **Go** | **Mojo** |
|---|---|---|---|---|
| Target use case | LLM-written backend services, compliance CRUD | General-purpose systems | Cloud-native services | AI/ML-first, Python-compatible |
| Data-race freedom | Static | Static | Dynamic only | Not yet fully guaranteed |
| Deadlock freedom | No mutex primitive exists at all | Possible | Possible | Not a current guarantee |
| LLM writability | LL(1) grammar exported to GBNF for constrained decoding | LLMs default to Python 90–97% of the time | No constrained decoding built in | No published GBNF integration |

Full comparison in the
[wiki](https://github.com/kannamma-labs/nirdosha/wiki/Nirdosha-vs-Alternatives).

## Install

No compiler needed — prebuilt binaries are published for **Linux,
Windows, and Apple Silicon macOS** on every
[release](https://github.com/kannamma-labs/nirdosha/releases):

```sh
# macOS / Linux — installer script, auto-detects your platform
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/scripts/install.sh | sh
```

```powershell
# Windows — PowerShell
irm https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/scripts/install.ps1 | iex
```

Prefer not to pipe a script? Download the binary straight from the
release instead — `.../releases/latest/download/<asset>` always
resolves to the newest release:

```sh
# Linux x86_64
curl -fsSL https://github.com/kannamma-labs/nirdosha/releases/latest/download/nirdosha-x86_64-unknown-linux-gnu.tar.gz | tar xz

# macOS, Apple Silicon
curl -fsSL https://github.com/kannamma-labs/nirdosha/releases/latest/download/nirdosha-aarch64-apple-darwin.tar.gz | tar xz
```

(Intel Mac: build from source for now — see below.)

Building from source needs `clang` and `z3` (`apt install clang
libz3-dev` / `brew install llvm z3` / `pacman -S clang z3`) — or skip
the setup entirely with [Codespaces](https://codespaces.new/kannamma-labs/nirdosha?quickstart=1).
Full install matrix, scaffolding a new project, and generating a UI:
see [Getting Started](https://github.com/kannamma-labs/nirdosha/wiki/Getting-Started)
in the wiki.

### Before you write your own program

The examples above run as-is, but these four things will trip up your
*first original line* — they're parse/type errors, not style nits:

- **Enum variants are calls, always with `()`.** `Some(5)`, `None()`,
  `Circle(r)` — a zero-payload variant still needs the parens.
  `Color::Red` also works (optional disambiguation sugar), but a bare
  variant name never takes the place of a call.
- **`str` can't be a function's parameter or return type.** Use an
  `enum` for categorical data, or `struct Text { value: str }` to pass
  free text.
- **No string concatenation or formatting.** A `str` value only ever
  comes from a source literal or a builtin (`json_get_str`,
  `db_query`, ...) — there's no `+` or format string to build one at
  runtime.
- **No statement separators.** The parser always extends the current
  expression across a newline — `return x` then `-y` on the next line
  parses as `return (x - y)`, not two statements.

Full rationale and the complete list: [`AGENTS.md`](./AGENTS.md).

## What's shipped

32 capabilities are proven and running today — the highlights:

- **Language core** — LL(1) grammar cross-verified against an
  independent LALR(1) generator, a static type checker,
  ownership/affine types (`box`/`&`, no GC, no manual `free`),
  `spawn`/`thread`/`chan` with no mutex in the language, generics,
  `Option`/`Result`, SMT-backed (Z3) integer/buffer-overflow proofs,
  and `validate { pre:/post: }` Hoare contracts.
- **Native codegen** — LLVM `-O2` compilation for the numeric/
  control-flow subset, within 1.4× of `gcc -O2`.
- **Backend services** — `db` (SQLite/Postgres, plus MySQL/Cassandra/
  Neo4j/HBase via plugin), `json`, `http`/`https`, `mq` (Redis, plus
  ActiveMQ via plugin), identity (OIDC/JWT, roles, claims), durable
  `transact` (WAL, crash replay, retry/timeout, idempotency), and
  `workflow` state machines with notification actions — running today
  through the interpreter, with native compilation expanding under
  [Track B](./docs/PUBLIC_ROADMAP.md).
- **UI engine** — zero-syntax CRUD/dashboard inference, a `screen`/
  `dashboard`/`workspace` DSL for the rest, field-level RBAC and
  format validation enforced server-side, design-token theming with
  live reload.
- **Cross-platform CI** — Linux, macOS, and Windows all build and run
  their full test suite on every push, not just at release time.

Full list with status tags, including what's next:
[`docs/PUBLIC_ROADMAP.md`](./docs/PUBLIC_ROADMAP.md).

## How to help

Small team, high-context contributions matter more than volume — see
[`MAINTAINERS.md`](./MAINTAINERS.md) for who has write access and how
active each is. Issues are labeled `good first issue` / `help wanted` /
`compiler` / `llm` / `infra` / `documentation` (full set:
[`.github/labels.yml`](./.github/labels.yml)) — `good first issue`
tickets don't need a "may I?" comment first, just send the PR. Your
first issue or PR here won't land in silence:
[`welcome.yml`](./.github/workflows/welcome.yml) posts a real, specific
reply, not boilerplate.

| If you care about | Try |
|---|---|
| Ownership/concurrency, PL theory | A `Track B` codegen gap, or an SMT/typeck edge case |
| Constrained decoding, agent repair loops | `crates/bench/`'s pass@1 harness — the scaffold's real, it just hasn't been pointed at a live model yet |
| Real backends, CRUD, sandboxing | A new `examples/*.nir` service, or a sixth plugin |
| Docs / DX | Error-message clarity, Getting Started walkthroughs, missing examples |

[`AREAS.md`](./AREAS.md) lists who owns which subsystem; a cross-cutting
or breaking change goes through the [RFC process](./rfcs/README.md)
first — see [`GOVERNANCE.md`](./GOVERNANCE.md) and
[`CONTRIBUTING.md`](./CONTRIBUTING.md).

## 📚 Documentation lives in the wiki

This README is the pitch and the five-minute quick start. Everything
else — the full design philosophy, the compiler architecture, the
complete feature and grammar reference, benchmarks with methodology, and
the LLM-integration mechanism with evidence — lives in the
**[Nirdosha Wiki](https://github.com/kannamma-labs/nirdosha/wiki)**:

- [Design Philosophy](https://github.com/kannamma-labs/nirdosha/wiki/Design-Philosophy) — the twelve requirements, and the Rice's-theorem constraint that shapes everything
- [Who It's For](https://github.com/kannamma-labs/nirdosha/wiki/Who-Its-For) — the honest fit
- [Nirdosha vs. Rust, Go, Mojo](https://github.com/kannamma-labs/nirdosha/wiki/Nirdosha-vs-Alternatives)
- [Architecture](https://github.com/kannamma-labs/nirdosha/wiki/Architecture) — the real compiler pipeline, the LL(1) grammar, independent cross-checks
- [Language Features](https://github.com/kannamma-labs/nirdosha/wiki/Language-Features) — the full feature set
- [The UI Engine](https://github.com/kannamma-labs/nirdosha/wiki/UI-Engine) — zero-syntax CRUD/dashboard generation
- [Benchmarks](https://github.com/kannamma-labs/nirdosha/wiki/Benchmarks) — compiled-vs-compiled numbers, methodology and caveats included
- [**LLM Integration**](https://github.com/kannamma-labs/nirdosha/wiki/LLM-Integration) — the flagship page: what each mechanism solves for an agent, and the evidence it's real
- [Getting Started](https://github.com/kannamma-labs/nirdosha/wiki/Getting-Started) — full install/build/run/scaffold
- [Honest Scope & Roadmap](https://github.com/kannamma-labs/nirdosha/wiki/Honest-Scope-and-Roadmap) — shipped vs. interpreter-only vs. next
- [FAQ](https://github.com/kannamma-labs/nirdosha/wiki/FAQ)

## FAQ (short version)

**Is it production-ready?** It's pre-1.0 and moving fast, with 32 real
capabilities shipped and verified — see
[what's shipped](#whats-shipped) above and the full
[Public Roadmap](./docs/PUBLIC_ROADMAP.md) for what's next.

**Why not just use Rust?** Rust already solves memory safety for teams
that can invest in its learning curve. Nirdosha targets a narrower
problem — AI agents writing backend code unsupervised. See the
[full answer](https://github.com/kannamma-labs/nirdosha/wiki/Nirdosha-vs-Alternatives).

**Found a bug?** Run `nirdosha <file.nir> --format=json` and paste the
`Diagnostic` JSON into a GitHub issue. Security issue? See
[SECURITY.md](./SECURITY.md) instead.

**Want to contribute?** See [CONTRIBUTING.md](./CONTRIBUTING.md). More in
the [full FAQ](https://github.com/kannamma-labs/nirdosha/wiki/FAQ).

---

*निर्दोष — designed so that what the compiler accepts is, provably, without
fault.*
