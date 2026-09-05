# Evidence for RFC 0005: the Nirdosha↔Rust plugin boundary

Four standalone spikes (not workspace members — see RFC 0005's
own Compatibility section for why), each reproducing one set of numbers
cited in that RFC's Evidence section.

## `plugin_bench/` — dispatch-mechanism micro-benchmark (RFC 0005 §E1)

Isolates `ast::BUILTIN_NAMES.contains`'s linear-scan cost, `plugin.rs`'s
real `PluginFn` dispatch shape, a representative real-builtin `match`
dispatch, and a zero-dispatch floor — all executing the real `rot13`
transform from `crates/plugin-example-rot13`.

```sh
cd plugin_bench && cargo run --release
```

Takes under a minute for the 55-byte-payload cases (1–4). The optional
64 KB repeat (cases 5–6, appended for anyone who wants to verify Kind
A's `Arc::clone`-based argument passing really does stay ~O(1) as
payload size grows) does real per-iteration heap allocation at scale —
expect several minutes even at the reduced 20,000-iteration count in
the committed source; RFC 0005 doesn't cite numbers from these two
cases (Arc's O(1) clone cost is a basic, well-established property of
the type, not something this spike needed to re-derive), so lower the
iteration count further, or comment them out, if you just want 1–4.

## `rot13_wasm_guest/` — the Kind C guest (RFC 0005 §E2 / §2)

```sh
rustup target add wasm32-unknown-unknown   # once
cd rot13_wasm_guest && cargo build --release --target wasm32-unknown-unknown
# output: target/wasm32-unknown-unknown/release/rot13_wasm_guest.wasm
```

A pre-built copy is committed as `rot13_wasm_guest_compiled.wasm` (20
KB) so `rot13_wasm_host` below is runnable without installing the
`wasm32-unknown-unknown` target first.

## `rot13_wasm_host/` — the `wasmtime` embedder (RFC 0005 §E2)

```sh
cd rot13_wasm_host && cargo build --release
./target/release/rot13_wasm_host ../rot13_wasm_guest_compiled.wasm
```

Runs in well under a minute for the 55-byte cases; the 61 KB repeat
(200,000 full-round-trip iterations, 20,000 for the isolating cases)
takes longer since it's doing real O(n) guest-side computation — this
is the case RFC 0005 cites numbers from, so don't skip it if you're
verifying that section specifically.

## `native_plugin_spike/` — compiled native plugin call overhead (RFC 0005 §3)

See its own `README.md`. The *automated, production* version of this
mechanism is `codegen::build_with_native_plugins`, shipped to `main` —
`crates/compiler/tests/native_plugin_codegen.rs` is the real,
end-to-end test (compiles a genuine Rust function, links it, runs the
resulting binary); this spike is only the hand-written numbers §3
cites for the pure call-overhead comparison.
