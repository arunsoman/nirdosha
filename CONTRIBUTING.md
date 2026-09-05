# Contributing to Nirdosha

Thanks for being here. Nirdosha is an actively developed systems
language and every contribution — code, docs, examples, tests, issue
triage, or design feedback — helps.

## Quick ways to help

- **Try it and report what breaks.** Build from source (below) or grab
  a [prebuilt binary](https://github.com/kannamma-labs/nirdosha/releases/latest),
  run a few `examples/*.nir`
  files, open an issue for anything confusing or wrong.
- **Improve docs.** Typos, unclear explanations, and missing examples
  are all welcome fixes.
- **Add `.nir` examples**, especially ones exercising a feature that
  doesn't have a dedicated example under `examples/` yet.
- **Port a benchmark.** `benchmarks/{c,julia,nirdosha}/` compares
  Nirdosha against C and Julia on a handful of numeric kernels — more
  comparison points are useful.
- **GBNF test cases.** `crates/grammar_export/` validates that
  `crates/compiler/nirdosha.gbnf` (the constrained-decoding grammar) accepts
  and rejects exactly what the real compiler does — corpus entries that
  exercise an edge case are valuable.
- **Pick up an item from the [Public Roadmap](./docs/PUBLIC_ROADMAP.md).**

## Before you contribute

1. Check existing issues and the [Public Roadmap](./docs/PUBLIC_ROADMAP.md)
   so you're not duplicating work already scoped or underway.
2. For anything non-trivial, open an issue first so we can agree on
   direction before you sink time into an implementation. **Exception:**
   an issue already labeled `good first issue` is pre-scoped — just send
   the PR, no need to ask.
3. Keep changes minimal and focused — a bug fix doesn't need drive-by
   refactoring bundled in.

## Development setup

**No local toolchain?** [Open in GitHub Codespaces](https://codespaces.new/kannamma-labs/nirdosha?quickstart=1) —
`.devcontainer/devcontainer.json` installs `clang`/`libz3-dev` and runs
the first build for you, so you land in a ready-to-go shell.

```sh
cd crates/compiler
cargo build          # fast dev build
cargo test           # full suite (unit + crates/compiler/tests/*.rs)
```

System deps the build links against directly:

```sh
# Debian/Ubuntu
sudo apt install clang libz3-dev

# macOS (Homebrew)
brew install llvm z3

# Arch
sudo pacman -S clang z3
```

`clang` is only invoked at runtime by `nirdosha build`/`emit-llvm`
(native codegen) — you don't need it just to interpret a program or run
the test suite. `z3` is linked at compile time and is required to build
the compiler at all.

Read [`AGENTS.md`](./AGENTS.md) first if you're going to touch the
compiler itself — it has the hard gotchas (no `::` token, `str` banned
as a function argument/return type, no statement separators) that will
otherwise cost you real debugging time, plus a router table pointing at
the right design doc for whatever you're changing.

## Pull request process

1. Fork and branch.
2. Run the full test suite: `cargo test` in `crates/compiler/`.
3. Update relevant docs (`docs/LANGUAGE.md`, `docs/GRAMMAR.md`, `docs/ROADMAP.md`,
   `docs/PUBLIC_ROADMAP.md`) in the *same* PR, not a follow-up — this
   project treats docs as load-bearing, not aspirational.
4. Reference the issue your PR addresses: `Closes #123`.
5. Keep commits small and messages descriptive.

## Response time

**Triage SLA: 48 hours** to a first response (a label, a question, or
just "seen, will look") on a new issue or PR. That's not the same as a
full resolution — this is a small team (see
[`MAINTAINERS.md`](./MAINTAINERS.md) for who has write access today and
how active each is), so expect a real answer or merge within about a
week; feel free to ping the thread if you haven't heard back past that.

## Breaking changes

A change to the language surface, grammar, or a public interface (CLI
flags, the manifest format, the plugin ABI) goes through the
[RFC process](./rfcs/README.md) before it lands — a written proposal
and a review window, not a change that just appears in one commit. See
[`GOVERNANCE.md`](./GOVERNANCE.md) for the full policy and
[`docs/adr/0002-ban-str-in-fn-signatures.md`](./docs/adr/0002-ban-str-in-fn-signatures.md)
for the real precedent (a breaking change shipped in one session, no
proposal, no window) that made this a written rule instead of an
assumption.

## Community

- **GitHub Discussions** for long-form questions and design conversations.
- All substantive design decisions happen in public GitHub issues,
  [RFCs](./rfcs/README.md), or Discussions — nothing gets decided in
  private that affects the language or its roadmap.
- See [`GOVERNANCE.md`](./GOVERNANCE.md) for roles (owner/maintainer/
  triager), [`MAINTAINERS.md`](./MAINTAINERS.md) for who holds them
  today, and [`AREAS.md`](./AREAS.md) for who owns which subsystem.

## Code of Conduct

See [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md).
