# Nirdosha — निर्दोष ("without fault")

[![build](https://github.com/kannamma-labs/nirdosha/actions/workflows/build.yml/badge.svg)](https://github.com/kannamma-labs/nirdosha/actions/workflows/build.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Wiki](https://img.shields.io/badge/docs-wiki-blue)](https://github.com/kannamma-labs/nirdosha/wiki)
[![Contributing](https://img.shields.io/badge/CONTRIBUTING-read-blue)](./CONTRIBUTING.md)
[![Governance](https://img.shields.io/badge/GOVERNANCE-read-blue)](./GOVERNANCE.md)
[![Roadmap](https://img.shields.io/badge/ROADMAP-view-purple)](./docs/PUBLIC_ROADMAP.md)
[![Maintainers](https://img.shields.io/badge/maintainers-5-green)](./MAINTAINERS.md)
[![Sponsor](https://img.shields.io/badge/%E2%9D%A4-Sponsor-ea4aaa)](https://github.com/sponsors/arunsoman)
[![Open in GitHub Codespaces](https://github.com/codespaces/badge.svg)](https://codespaces.new/arunsoman/nirdosha?quickstart=1)

> **A systems language designed for LLMs to write, with a grammar so
> constrained the model can't emit invalid syntax.** No garbage collector,
> no data races, no deadlocks, no integer/buffer overflow — those aren't
> the pitch, they're the proof that a language built for an AI agent to
> write unsupervised can also be trusted to run.

Status: a real, runnable Rust compiler (`crates/compiler/`) under
active development — many safety properties are *proven* today, and
where one is still *aspirational* the [wiki](https://github.com/kannamma-labs/nirdosha/wiki/Honest-Scope-and-Roadmap)
says so plainly. Source files use the `.nir` extension.

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
cd crates/compiler && cargo run -- ../../examples/hello_above_fold.nir
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
really flips a row from `requested` to `approved` in SQLite. Nothing here
is simulated — see the [UI Engine](https://github.com/kannamma-labs/nirdosha/wiki/UI-Engine)
and [Honest Scope](https://github.com/kannamma-labs/nirdosha/wiki/Honest-Scope-and-Roadmap)
wiki pages.

---

## Current focus / how to help

Small team, high-context contributions matter more than volume — see
[`MAINTAINERS.md`](./MAINTAINERS.md) for who has write access and how
active each is. Right now:

- **Track B (full compilation)** — native codegen only covers the
  numeric/control-flow subset; `db`/`json`/`http`/`mq`/identity/
  `transact`/concurrency are interpreter-only *(fully functional
  today — the `vendor_ops.nir` demo above combines all four live;
  Track B is a latency/throughput project, not a capability gap, see
  [ROADMAP](./docs/PUBLIC_ROADMAP.md))*. First gap to close:
  `transact` → `db`/`json`.
- **LLM eval harness** — [`crates/bench/`](./crates/bench) has a real pass@1 +
  self-repair-rate scaffold, and now a real `Model` too:
  `real_model::RealModel` (`--mode real`) talks to any OpenAI-compatible
  `/chat/completions` endpoint (DeepSeek, Kimi/Moonshot, GLM/Zhipu — set
  `NIRDOSHA_BENCH_API_BASE`/`_API_KEY`/`_MODEL`). Its request-building and
  response-parsing are unit-tested, but it hasn't been run against a live
  provider yet — no API key is set in this project's dev/CI environment.
  Running it for real against one or more of the three providers, and
  reporting the resulting pass@1/self-repair numbers, is the actual gap
  left to close.
- **macOS verification** — release binaries link system Z3 because
  `z3-src` doesn't build against current AppleClang ([ADR
  0001](./docs/adr/0001-vendor-z3-except-macos.md), tracked as
  [issue #5](https://github.com/kannamma-labs/nirdosha/issues/5)) — that part
  is still open. What's now closed: macOS gets CI verification on every
  push/PR, not just at release time. `build-macos` in
  [`.github/workflows/build.yml`](./.github/workflows/build.yml) mirrors
  release.yml's proven `brew install z3` + system-Z3 build on a real
  `macos-14` runner. Windows is CI-verified too: `build-windows` in the
  same file builds and runs the `tcp`/`sandbox`/`sandbox_channels`/
  `channels` suite plus the compiled-native-codegen TCP tests on a real
  `windows-latest` runner on every push/PR (see [ROADMAP
  A7](./docs/PUBLIC_ROADMAP.md)).

Full list with status tags: [`docs/PUBLIC_ROADMAP.md`](./docs/PUBLIC_ROADMAP.md).
Issues are labeled `good first issue` / `help wanted` / `compiler` /
`llm` / `infra` / `documentation` (full set: [`.github/labels.yml`](./.github/labels.yml)).
Pick one, comment before starting on anything non-trivial — see
[`CONTRIBUTING.md`](./CONTRIBUTING.md). [`AREAS.md`](./AREAS.md) lists
who owns which subsystem; a cross-cutting or breaking change goes
through the [RFC process](./rfcs/README.md) first — see
[`GOVERNANCE.md`](./GOVERNANCE.md). Your first issue or PR here won't
land in silence: [`welcome.yml`](./.github/workflows/welcome.yml) posts
a real, specific reply (not boilerplate) and
[`label-first-contribution.yml`](./.github/workflows/label-first-contribution.yml)
tags it `first-time contributor` so it's visible at a glance — it
doesn't solve cold-start on its own, but it's a real signal in place of
nothing.

| If you care about | Try | 
|---|---|
| Ownership/concurrency, PL theory | A `Track B` codegen gap, or an SMT/typeck edge case |
| Constrained decoding, agent repair loops | `crates/bench/` harness + real models, `crates/grammar_export/` corpus entries |
| Real backends, CRUD, sandboxing | A new `examples/*.nir` service, or Track A production-readiness items |
| Docs / DX | Error-message clarity, Getting Started walkthroughs, missing examples |

## Why this exists, in one paragraph

Nirdosha targets one specific, currently-unsolved problem: **a backend
service written and maintained by an AI coding agent, with no human
reviewing every line before it runs.** An LL(1) grammar exported to GBNF
lets a sampler force every token an agent emits to stay syntactically
valid; `--format=json` gives a self-repair loop a structured proof
obligation instead of a paragraph to guess at; `sandbox` is a real OS
process and a language primitive, not a bolted-on Docker wrapper; there is
no mutex in the language, so an agent literally cannot generate a
lock-ordering deadlock. It isn't trying to be a better Rust — see the
[wiki](https://github.com/kannamma-labs/nirdosha/wiki) for the full case,
including where the design is still evolving, not a finished product.

## Who this is for — and who it isn't

**This is for you if:**
- You're building an agent (or agent framework) that writes and runs
  backend code with no human reviewing it before it executes, and you
  need the *language* to make bug classes unrepresentable rather than
  catching them in review.
- You want to try the constrained-decoding / self-repair mechanism on a
  real compiler today, not a whitepaper — the GBNF grammar,
  `--format=json` diagnostics, and `crates/bench/` all run now.
- You're fine filing issues against a fast-moving pre-1.0 project, not
  pulling a finished 1.0 into a production stack.

**This isn't for you if:**
- You want a general-purpose systems language for humans to write —
  that's Rust, and Rust is the honest answer (see the FAQ below).
- You need `db`/`json`/`http`/`mq`/concurrency compiled to native code
  today — those run now in the interpreter (see the vendor_ops.nir demo
  above), but aren't native-compiled until Track B lands (see
  [ROADMAP](./docs/PUBLIC_ROADMAP.md)).
- You need something production-ready this quarter — nothing here
  claims that.

Full picture: [Who It's For](https://github.com/kannamma-labs/nirdosha/wiki/Who-Its-For) in the wiki.

## Nirdosha vs. Rust, Go, Mojo — the one-line version

| | **Nirdosha** | **Rust** | **Go** | **Mojo** |
|---|---|---|---|---|
| Target use case | LLM-written backend services, compliance CRUD | General-purpose systems | Cloud-native services | AI/ML-first, Python-compatible |
| Data-race freedom | Static | Static | Dynamic only | Not yet fully guaranteed |
| Deadlock freedom | No mutex primitive exists at all | Possible | Possible | Not a current guarantee |
| LLM writability | LL(1) grammar exported to GBNF for constrained decoding | LLMs default to Python 90–97% of the time | No constrained decoding built in | No published GBNF integration |

Full comparison, plus the honest "why not just use Rust" answer, in the
[wiki](https://github.com/kannamma-labs/nirdosha/wiki/Nirdosha-vs-Alternatives).

## Try it in under a minute

**No local toolchain at all?** Click
[**Open in GitHub Codespaces**](https://codespaces.new/arunsoman/nirdosha?quickstart=1)
— it builds the compiler for you (system Z3, ~1 min) and drops you into a
VS Code shell in the browser with everything already checked out. From
there:

```sh
cd crates/compiler
cargo run -- ../../examples/hello.nir
cargo run -- serve ../../examples/store.nir --port 8080   # open via the Ports tab
```

**Don't want to learn the syntax first?** Paste
[`agent-skills/nirdosha/paste-anywhere-prompt.md`](./agent-skills/nirdosha/paste-anywhere-prompt.md)
into any LLM chat and describe what you want in plain English — it writes
the `.nir` code for you. This prompt has already been used, unmodified, to
generate a working e-commerce store, a food-delivery platform, a telecom
revenue-assurance system, and an online trading platform, each hundreds of
lines, each by an LLM with no prior Nirdosha exposure. See
[LLM Integration](https://github.com/kannamma-labs/nirdosha/wiki/LLM-Integration)
for the full mechanism and evidence.

**Install and run it yourself — no compiler needed, prebuilt binaries are
published on every [release](https://github.com/kannamma-labs/nirdosha/releases):**

```sh
git clone https://github.com/kannamma-labs/nirdosha.git && cd nirdosha

# macOS / Linux — installer script, auto-detects your platform
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/kannamma-labs/nirdosha/main/scripts/install.sh | sh

nirdosha examples/hello.nir
nirdosha serve examples/store.nir --port 8080   # CRUD API from a struct
```

Prefer not to pipe a script into `sh`? Download the binary straight from
the release instead — `.../releases/latest/download/<asset>` always
resolves to the newest release, so this stays correct with no version
number to update. Run this from inside the `nirdosha` clone from above:

```sh
# Linux x86_64
curl -fsSL https://github.com/kannamma-labs/nirdosha/releases/latest/download/nirdosha-x86_64-unknown-linux-gnu.tar.gz | tar xz

# macOS, Apple Silicon
curl -fsSL https://github.com/kannamma-labs/nirdosha/releases/latest/download/nirdosha-aarch64-apple-darwin.tar.gz | tar xz

./nirdosha examples/hello.nir
```

These two targets are what's currently published; Windows and Intel
Mac binaries aren't up yet (build from source below in the meantime) —
the [releases page](https://github.com/kannamma-labs/nirdosha/releases/latest)
has the current full asset list.

Full install (Windows, building from source, toolchain requirements),
scaffolding a new project, and generating a UI: see
[Getting Started](https://github.com/kannamma-labs/nirdosha/wiki/Getting-Started)
in the wiki.

### Before you write your own program

The example above runs as-is, but these four things will trip up your
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
- [Honest Scope & Roadmap](https://github.com/kannamma-labs/nirdosha/wiki/Honest-Scope-and-Roadmap) — shipped vs. interpreter-only vs. aspirational
- [FAQ](https://github.com/kannamma-labs/nirdosha/wiki/FAQ)

## FAQ (short version)

**Is it production-ready?** Not yet — it's under active development,
with real guarantees proven today and the rest tracked openly, not
hand-waved. See [Honest Scope & Roadmap](https://github.com/kannamma-labs/nirdosha/wiki/Honest-Scope-and-Roadmap).

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
