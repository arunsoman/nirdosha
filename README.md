# Nirdosha — निर्दोष ("without fault")

[![build](https://github.com/arunsoman/nirdosha/actions/workflows/build.yml/badge.svg)](https://github.com/arunsoman/nirdosha/actions/workflows/build.yml)
[![license: MIT](https://img.shields.io/badge/license-MIT-blue.svg)](./LICENSE)
[![Wiki](https://img.shields.io/badge/docs-wiki-blue)](https://github.com/arunsoman/nirdosha/wiki)
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
*aspirational* (called out honestly in the [wiki](https://github.com/arunsoman/nirdosha/wiki/Honest-Scope-and-Roadmap)).
Source files use the `.nir` extension.

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
really flips a row from `requested` to `approved` in SQLite. Nothing here
is simulated — see the [UI Engine](https://github.com/arunsoman/nirdosha/wiki/UI-Engine)
and [Honest Scope](https://github.com/arunsoman/nirdosha/wiki/Honest-Scope-and-Roadmap)
wiki pages.

---

## Current focus / how to help

Solo-maintained research project — small, high-context contributions
matter more than volume. Right now:

- **Track B (full compilation)** — native codegen only covers the
  numeric/control-flow subset; `db`/`json`/`http`/`mq`/identity/
  `transact`/concurrency are interpreter-only. First gap to close:
  `transact` → `db`/`json`.
- **LLM eval harness** — [`bench/`](./bench) has a real pass@1 +
  self-repair-rate scaffold but ships with mock models; wiring it to
  DeepSeek/Kimi/GLM would make the LLM-writability claims independently
  checkable.
- **Windows / macOS verification** — the compiled `tcp`/`tcp_listener`
  runtime has never run on a real Windows machine; macOS binaries link
  system Z3 because `z3-src` doesn't build against current AppleClang.

Full list with status tags: [`PUBLIC_ROADMAP.md`](./PUBLIC_ROADMAP.md).
Issues are labeled `good first issue` / `help wanted` / `compiler` /
`llm` / `infra` / `docs`. Pick one, comment before starting on anything
non-trivial — see [`CONTRIBUTING.md`](./CONTRIBUTING.md).

| If you care about | Try | 
|---|---|
| Ownership/concurrency, PL theory | A `Track B` codegen gap, or an SMT/typeck edge case |
| Constrained decoding, agent repair loops | `bench/` harness + real models, `grammar_export/` corpus entries |
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
[wiki](https://github.com/arunsoman/nirdosha/wiki) for the full case,
including where the design is still a research bet, not a finished
product.

## Nirdosha vs. Rust, Go, Mojo — the one-line version

| | **Nirdosha** | **Rust** | **Go** | **Mojo** |
|---|---|---|---|---|
| Target use case | LLM-written backend services, compliance CRUD | General-purpose systems | Cloud-native services | AI/ML-first, Python-compatible |
| Data-race freedom | Static | Static | Dynamic only | Not yet fully guaranteed |
| Deadlock freedom | No mutex primitive exists at all | Possible | Possible | Not a current guarantee |
| LLM writability | LL(1) grammar exported to GBNF for constrained decoding | LLMs default to Python 90–97% of the time | No constrained decoding built in | No published GBNF integration |

Full comparison, plus the honest "why not just use Rust" answer, in the
[wiki](https://github.com/arunsoman/nirdosha/wiki/Nirdosha-vs-Alternatives).

## Try it in under a minute

**Don't want to learn the syntax first?** Paste
[`agent-skills/nirdosha/paste-anywhere-prompt.md`](./agent-skills/nirdosha/paste-anywhere-prompt.md)
into any LLM chat and describe what you want in plain English — it writes
the `.nir` code for you. This prompt has already been used, unmodified, to
generate a working e-commerce store, a food-delivery platform, a telecom
revenue-assurance system, and an online trading platform, each hundreds of
lines, each by an LLM with no prior Nirdosha exposure. See
[LLM Integration](https://github.com/arunsoman/nirdosha/wiki/LLM-Integration)
for the full mechanism and evidence.

**Install and run it yourself:**

```sh
# macOS / Linux
curl --proto '=https' --tlsv1.2 -sSf https://raw.githubusercontent.com/arunsoman/nirdosha/main/scripts/install.sh | sh

git clone https://github.com/arunsoman/nirdosha.git && cd nirdosha
nirdosha examples/hello.nir
nirdosha serve examples/store.nir --port 8080   # CRUD API from a struct
```

Full install (Windows, building from source, toolchain requirements),
scaffolding a new project, and generating a UI: see
[Getting Started](https://github.com/arunsoman/nirdosha/wiki/Getting-Started)
in the wiki.

## 📚 Documentation lives in the wiki

This README is the pitch and the five-minute quick start. Everything
else — the full design philosophy, the compiler architecture, the
complete feature and grammar reference, benchmarks with methodology, and
the LLM-integration mechanism with evidence — lives in the
**[Nirdosha Wiki](https://github.com/arunsoman/nirdosha/wiki)**:

- [Design Philosophy](https://github.com/arunsoman/nirdosha/wiki/Design-Philosophy) — the twelve requirements, and the Rice's-theorem constraint that shapes everything
- [Who It's For](https://github.com/arunsoman/nirdosha/wiki/Who-Its-For) — the honest fit
- [Nirdosha vs. Rust, Go, Mojo](https://github.com/arunsoman/nirdosha/wiki/Nirdosha-vs-Alternatives)
- [Architecture](https://github.com/arunsoman/nirdosha/wiki/Architecture) — the real compiler pipeline, the LL(1) grammar, independent cross-checks
- [Language Features](https://github.com/arunsoman/nirdosha/wiki/Language-Features) — the full feature set
- [The UI Engine](https://github.com/arunsoman/nirdosha/wiki/UI-Engine) — zero-syntax CRUD/dashboard generation
- [Benchmarks](https://github.com/arunsoman/nirdosha/wiki/Benchmarks) — compiled-vs-compiled numbers, methodology and caveats included
- [**LLM Integration**](https://github.com/arunsoman/nirdosha/wiki/LLM-Integration) — the flagship page: what each mechanism solves for an agent, and the evidence it's real
- [Getting Started](https://github.com/arunsoman/nirdosha/wiki/Getting-Started) — full install/build/run/scaffold
- [Honest Scope & Roadmap](https://github.com/arunsoman/nirdosha/wiki/Honest-Scope-and-Roadmap) — shipped vs. interpreter-only vs. aspirational
- [FAQ](https://github.com/arunsoman/nirdosha/wiki/FAQ)

## FAQ (short version)

**Is it production-ready?** No — active research prototype. See
[Honest Scope & Roadmap](https://github.com/arunsoman/nirdosha/wiki/Honest-Scope-and-Roadmap).

**Why not just use Rust?** Rust already solves memory safety for teams
that can invest in its learning curve. Nirdosha targets a narrower
problem — AI agents writing backend code unsupervised. See the
[full answer](https://github.com/arunsoman/nirdosha/wiki/Nirdosha-vs-Alternatives).

**Found a bug?** Run `nirdosha <file.nir> --format=json` and paste the
`Diagnostic` JSON into a GitHub issue. Security issue? See
[SECURITY.md](./SECURITY.md) instead.

**Want to contribute?** See [CONTRIBUTING.md](./CONTRIBUTING.md). More in
the [full FAQ](https://github.com/arunsoman/nirdosha/wiki/FAQ).

---

*निर्दोष — designed so that what the compiler accepts is, provably, without
fault.*
