# 0003: Split the compiled-path runtime kernels into their own Cargo-dependency-aware crate

Date: 2026-09-05
Status: accepted

## Context

`codegen.rs`'s "linked native kernel" pattern (`det`/`inv`/`solve`/
`rank`/`kf_update_state`/`kf_update_cov`, later `tcp`/`tcp_listener`,
later `file`) has proven, since Phase 5, to be the right way to give a
compiled `.nir` program real behavior codegen can't reasonably emit as
hand-rolled LLVM IR: write it once in ordinary Rust, compile it to a
staticlib at `nirdosha`'s own build time, `declare` + `call` it from
generated IR, link the `.a` in. Every one of those kernels lived in
one file, `crates/compiler/src/runtime_kernels.rs`, built by
`crates/compiler/build.rs` via a **bare `rustc --crate-type staticlib`
invocation** — deliberately not `cargo build` on a sub-crate, per that
file's own original comment: no crate graph to resolve, no circular-
dependency risk from a sub-crate that might otherwise want to depend
on `nirdosha` itself.

That held up fine as long as every kernel needed nothing but `std`.
It stopped holding the moment `dec128` needed one:
`rfcs/0005-plugin-boundary-safety-and-performance.md`'s own research
(written chasing a completely different question — closing Track B's
compiled-path gaps generally) found that `rust_decimal` — already a
real dependency of the `nirdosha` lib crate itself, used by
`interpreter.rs::Value::Dec128` — was **completely unreachable** from
`runtime_kernels.rs`, because a bare `rustc` invocation has no
dependency resolution at all. `Ty::Dec128` had stayed interpreter-only
long after `tcp`/`file` were compiled specifically because of this,
not because decimal arithmetic itself was hard.

## Decision

`runtime_kernels.rs` moved to `crates/runtime-kernels/`, a real Cargo
package with a `[dependencies]` section (`rust_decimal = "1"`, so far).
`crates/compiler/build.rs` now runs `cargo rustc --release --manifest-
path crates/runtime-kernels/Cargo.toml --target-dir <OUT_DIR>/... --
--print=native-static-libs` instead of a bare `rustc` call — one
command, same as before, that both builds the artifact and captures
the OS-level native libraries it transitively needs (`ws2_32.lib` on
Windows for `std::net`, etc. — the reason the old code needed
`--print=native-static-libs` in the first place is unchanged).

**The one real new risk, and how it's avoided**: `crates/runtime-
kernels/Cargo.toml` declares its own, empty `[workspace]` table,
deliberately making it *not* a member of this repository's root
workspace. A `cargo build`/`cargo rustc` invoked from inside a build
script that is itself running under an outer `cargo build` of the
*same* workspace would contend for that workspace's own `target/`
directory lock — the outer build is already holding it, waiting for
this build script to finish, so a nested call sharing that lock would
deadlock, not just run slowly. A genuinely separate workspace, built
into its own private `--target-dir` under `OUT_DIR`, shares no lock
with the outer build at all. Verified directly: a clean `rm -rf
target && cargo build -p nirdosha` completes normally (no hang), and
the full test suite (921 tests as of this ADR) passes unchanged.

`dec_from_i64`/`dec_to_str`/`+`/`-`/`*`/`/` on `dec128` now compile to
native code (`crates/compiler/tests/codegen.rs`'s `dec128_*` tests,
verified against a real compiled+run binary, output matching the
interpreter byte for byte). `dec_from_str`/`dec_round`/`dec_scale` and
all six comparison operators are **not** wired into codegen yet — the
kernels this ADR's crate split unblocked exist for `dec_from_str`
already (`nir_dec128_from_str`); comparisons and rounding are real
follow-up work, cleanly rejected (not silently wrong) in the meantime.

## Consequences

**Easier now**: any future compiled-path kernel that genuinely needs a
crates.io dependency (a real cryptography library instead of the
from-scratch SHA-256 `runtime-kernels/src/lib.rs` already carries, a
proper JSON/CBOR encoder, a UUID generator) has a place to add it —
`crates/runtime-kernels/Cargo.toml`'s `[dependencies]` — instead of
hitting the same wall `dec128` did.

**Harder, or at least new**: `nirdosha`'s own build now shells out to
`cargo` a second time, recursively, from a build script. This is a
well-known category of build-script hazard (lock contention,
environment-variable bleed-through, toolchain-version mismatches if
`CARGO`/`RUSTC` aren't threaded through correctly) — mitigated here by
the separate-workspace/separate-target-dir design above and by using
`std::env::var("CARGO")` (the exact binary cargo itself set, not a bare
`"cargo"` looked up on `PATH`) rather than avoided outright. Cross-
compilation (building `nirdosha` itself for a target other than the
host) hasn't been exercised against this new path — the nested `cargo
rustc` call doesn't yet forward a `--target` flag, an honest gap if
that scenario is ever hit, not silently assumed to be fine.

**Also true, stated plainly**: this ADR is a build-*architecture*
decision (how `nirdosha` builds itself), not a language-surface change
— no `.nir` grammar, no new `Ty` variant, no `GOVERNANCE.md` RFC gate
applies to it the way one did for `Ty::Handle`
(rfcs/0005-plugin-boundary-safety-and-performance.md §1). Recorded as
an ADR, not an RFC, for exactly that reason (`docs/adr/README.md`'s own
"a judgment call made while implementing something" scope).
