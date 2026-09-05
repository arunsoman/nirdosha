# Evidence for RFC 0005 §3: native compiled plugin call overhead

Isolates the pure call-dispatch cost of a `nirdosha build`-style compiled
call into a real, separately-compiled native plugin symbol — the same
mechanism `codegen::build_with_native_plugins` now automates (see
`crates/compiler/tests/native_plugin_codegen.rs` for the real, automated
version of this; this spike is the hand-written numbers cited in §3).

```sh
rustc --crate-type staticlib -O plugin_native.rs -o libplugin_native.a
clang -O2 call_plugin.ll libplugin_native.a -o call_plugin
clang -O2 inline_baseline.ll -o inline_baseline
time ./call_plugin      # 500,000,000 calls to the linked extern fn
time ./inline_baseline  # identical math, inlined, no call at all
```

`call_plugin.ll`/`inline_baseline.ll` are exactly what `codegen.rs`
itself would emit for `declare i64 @plugin_scale(i64)` plus a loop
calling it — hand-written here to isolate the number before automating
it, same "spike first, then verify the built mechanism reproduces it"
approach the WASM (Kind C) spike in the parent directory already used.
