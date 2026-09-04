# Areas

Repo-level mirror of [`crates/compiler/src/INDEX.md`](./crates/compiler/src/INDEX.md)'s
approach: a durable name for each subsystem and who owns it, not a
claim about exact file boundaries (those drift; consult the actual
`crates/*/Cargo.toml` layout or `INDEX.md` for that). Consumed by
[`.github/CODEOWNERS`](./.github/CODEOWNERS) for automatic review
routing, and by [`MAINTAINERS.md`](./MAINTAINERS.md) for the "which
area" column.

**Owner column is honest, not aspirational** — see
[`MAINTAINERS.md`](./MAINTAINERS.md)'s note on activation. `arunsoman`
is listed as owner of everything below because that's who has actually
touched it; "help wanted" areas are open for a maintainer candidate to
claim per `MAINTAINERS.md`'s "Becoming a maintainer" steps, not areas
nobody may touch.

| Area | Path(s) | Owner | Notes |
|---|---|---|---|
| Compiler core (parser, typeck, ownership, SMT/refine) | `crates/compiler/src/{parser,typeck,ast,ownership,smt,refine}.rs` | `arunsoman` | Static-checking pipeline. See `crates/compiler/src/INDEX.md` for the per-function map. |
| Interpreter | `crates/compiler/src/interpreter.rs` | `arunsoman` | The reference execution semantics — anything the native backend doesn't yet cover falls back here. |
| Native codegen (LLVM) | `crates/compiler/src/codegen.rs`, `runtime_kernels.rs` | `arunsoman` | Narrower than the interpreter by design (`check_supported`'s explicit reject list); help wanted closing Track B gaps, `docs/ROADMAP.md`. |
| UI/screen DSL | `crates/compiler/src/{ui_gen,serve}.rs` | `arunsoman` | Manifest derivation + the server that enforces its gates; see `docs/LANGUAGE.md` §11–13. |
| Plugin/extension system | `crates/compiler/src/plugin.rs`, `crates/plugin-example-rot13/` | `arunsoman` | New, Track G1 Kind A (native builtin extension via Cargo) — actively in progress; see `rfcs/0001-package-manifest-format.md`. |
| Grammar / GBNF / constrained decoding | `crates/grammar_check/`, `crates/grammar_export/`, `crates/compiler/nirdosha.gbnf` | `arunsoman` | LALR(1) cross-check against the hand-written LL(1) parser; corpus entries are a **help wanted** / good-first-issue source (see open issue #3). |
| LLM eval harness | `crates/bench/` | `arunsoman` | Real pass@1/self-repair scaffold, mock models only today — **help wanted** wiring a real provider (open issue #2, `docs/ECOSYSTEM.md` §G3). |
| Presence gateway | `crates/presence-gateway/` | `arunsoman` | `docs/ROADMAP.md` Track A5. |
| Deployment (Docker, Helm, Kustomize) | `Dockerfile`, `deploy/`, `.github/workflows/docker.yml` | `arunsoman` (Helm chart maintainers: `arunsoman`, `lekshmideepu`, `maheshmindlabs` per `deploy/helm/nirdosha/Chart.yaml`) | Chart-level maintainer listing predates GitHub write access; both are now real for `lekshmideepu`/`maheshmindlabs` (see `MAINTAINERS.md`). |
| Release/CI | `.github/workflows/{build,release,docker}.yml` | `arunsoman` | See `GOVERNANCE.md`#releases for the signing/OIDC policy. |
| Docs (design specs, language reference) | `docs/*.md` | `arunsoman` | Treated as load-bearing — `CONTRIBUTING.md`'s PR process requires doc updates in the same PR as the code change they describe. |
| Examples | `examples/*.nir` | `arunsoman` | **help wanted / good first issue** source — missing coverage tracked as individual issues (e.g. #7, #8). |
| Governance / process | `GOVERNANCE.md`, `MAINTAINERS.md`, `AREAS.md`, `rfcs/`, `docs/adr/`, `.github/labels.yml` | `arunsoman` | This document tree. |
