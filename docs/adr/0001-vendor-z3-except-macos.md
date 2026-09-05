# 0001: Vendor Z3 for release builds, except macOS (system Z3 there)

Date: 2026-08-25
Status: accepted

## Context

`crates/compiler` links Z3 at compile time (`smt.rs`'s Tier-2 bounds
proving). A prebuilt `nirdosha` release binary that requires the end
user to have `libz3` installed separately is a real distribution
tax — most of the "download and run" install story
([`scripts/install.sh`](../../scripts/install.sh)) would otherwise
silently need "...and also `apt install libz3-dev`" first. The `z3`
crate's `z3-src` feature (`dist` feature,
[`crates/compiler/Cargo.toml`](../../crates/compiler/Cargo.toml))
vendors and statically builds Z3 as part of `cargo build`, removing
that runtime dependency — for Linux and Windows.

macOS is the disclosed exception: `z3-src` 416.0.2 (the version the
`z3` 0.20.2 crate pulls) fails to compile against the AppleClang
shipped on GitHub's `macos-13`/`macos-14` runners — a real upstream
`obj_hashtable.h` constructor-strictness incompatibility, confirmed
2026-08-25 on [`release.yml`](../../.github/workflows/release.yml)'s
first real CI run for that leg, not a local config mistake.

## Decision

- Linux and Windows release binaries vendor Z3 (`--features dist`) —
  zero extra system dependency for the end user, `clang` only needed
  at runtime for `nirdosha build`/`emit-llvm` (native codegen), not for
  interpreting/`emit-ast`/`emit-ui`/`serve`.
- macOS release binaries instead link the **system** Z3 (`brew install
  z3` on the CI runner, and required on the end user's machine too) —
  the same dependency building from source would already require.
  This is a real, disclosed asymmetry between platforms, not a silent
  gap: `release.yml`'s own header comment states it, and this ADR
  restates it here so the reasoning doesn't live only in a workflow
  comment.

## Consequences

- Linux/Windows users get the frictionless "download and run" install
  story this ADR exists to protect.
- macOS users carry one extra install step (`brew install z3`) that
  Linux/Windows users don't — a real, if narrow, platform-support gap,
  tracked as [issue #5](https://github.com/kannamma-labs/nirdosha/issues/5)
  ("macOS: z3-src 416.0.2 fails against current AppleClang — vendor Z3
  or document workaround").
- This decision reverses automatically, with no code change needed
  here, once a `z3`/`z3-src` release upstream fixes the AppleClang
  incompatibility — at that point `release.yml`'s macOS legs should
  switch to `--features dist` like the other two, and issue #5 closes.
