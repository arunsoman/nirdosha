# Contributing to Nirdosha

Thanks for being here. Nirdosha is a research-stage systems language and
every contribution — code, docs, examples, tests, issue triage, or
design feedback — helps.

## Quick ways to help

- **Try it and report what breaks.** Build from source (below) or use
  a prebuilt binary once one's published, run a few `examples/*.nir`
  files, open an issue for anything confusing or wrong.
- **Improve docs.** Typos, unclear explanations, and missing examples
  are all welcome fixes.
- **Add `.nir` examples**, especially ones exercising a feature that
  doesn't have a dedicated example under `examples/` yet.
- **Port a benchmark.** `benchmarks/{c,julia,nirdosha}/` compares
  Nirdosha against C and Julia on a handful of numeric kernels — more
  comparison points are useful.
- **GBNF test cases.** `grammar_export/` validates that
  `compiler/nirdosha.gbnf` (the constrained-decoding grammar) accepts
  and rejects exactly what the real compiler does — corpus entries that
  exercise an edge case are valuable.
- **Pick up an item from the [Public Roadmap](./PUBLIC_ROADMAP.md).**

## Before you contribute

1. Check existing issues and the [Public Roadmap](./PUBLIC_ROADMAP.md)
   so you're not duplicating work already scoped or underway.
2. For anything non-trivial, open an issue first so we can agree on
   direction before you sink time into an implementation.
3. Keep changes minimal and focused — a bug fix doesn't need drive-by
   refactoring bundled in.

## Development setup

```sh
cd compiler
cargo build          # fast dev build
cargo test           # full suite (unit + compiler/tests/*.rs)
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
2. Run the full test suite: `cargo test` in `compiler/`.
3. Update relevant docs (`LANGUAGE.md`, `GRAMMAR.md`, `ROADMAP.md`,
   `PUBLIC_ROADMAP.md`) in the *same* PR, not a follow-up — this
   project treats docs as load-bearing, not aspirational.
4. Reference the issue your PR addresses: `Closes #123`.
5. Keep commits small and messages descriptive.

## Response time

This is presently a solo-maintained research project. Expect a response
within about a week; feel free to ping the thread if you haven't heard
back.

## Community

- **GitHub Discussions** for long-form questions and design conversations.
- All substantive design decisions happen in public GitHub issues —
  nothing gets decided in private that affects the language or its
  roadmap.

## Code of Conduct

See [CODE_OF_CONDUCT.md](./CODE_OF_CONDUCT.md).
