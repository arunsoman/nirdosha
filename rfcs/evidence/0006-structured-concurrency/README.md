# Evidence for RFC 0006: structured concurrency (Pillars 1-4)

`concurrency_proto/` is a standalone Rust crate (its own `[workspace]`,
not a member of the repo's root workspace — same reasoning as
`crates/runtime-kernels/Cargo.toml`) that implements the runtime
mechanics of `nirdosha_concurrency_spec.md`'s Pillars 1-4
(capability-typed boxes, non-blocking send, blocking receive/select,
structured concurrency) in plain Rust, and runs the adversarial test
list from the companion "Thread/Channel/Sandbox" brief against it for
real. It does **not** touch Nirdosha's actual grammar/typechecker —
the point is runtime-semantics evidence before committing to a
language-surface RFC, the same evidence-first approach
`rfcs/0005-plugin-boundary-safety-and-performance.md` used for the
WASM/native-plugin spikes.

```sh
cd concurrency_proto
cargo test --release      # 19/19 adversarial tests, real classifications
cargo run --release --bin bench   # Phase 8 timing numbers
rustc counterexamples/verify_double_send.rs    # must fail: E0382
rustc counterexamples/verify_dangling_ref.rs   # must fail: E0597
```

See `rfcs/0006-structured-concurrency.md` for the full writeup, findings,
and recommendation.
