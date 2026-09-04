// Compiles `src/runtime_kernels.rs` — the freestanding, data-dependent
// linalg kernels (`det`/`inv`/`solve`/`rank`/`kf_update_state`/
// `kf_update_cov`), plus the `tcp`/`tcp_listener` native socket kernels —
// into a static library at `nirdosha`'s own build time, once, not per
// user program. `codegen.rs` embeds the resulting `.a` via
// `include_bytes!` and links it into every native binary `nirdosha
// build` produces, alongside the `.ll` file it generates — see
// runtime_kernels.rs's module doc for why these builtins go through a
// linked native `call` instead of hand-emitted branchy IR.
//
// A plain `rustc` invocation, not `cargo build` on a sub-crate: the file
// is intentionally self-contained (no dependency on the `nirdosha` lib
// crate — see runtime_kernels.rs's own doc comment), so there's no crate
// graph to resolve and no circular-dependency risk from a sub-crate that
// would otherwise want to depend on `nirdosha` itself.
use std::path::PathBuf;
use std::process::Command;

fn main() {
    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("cargo always sets OUT_DIR"));
    let src = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("src/runtime_kernels.rs");
    let out_lib = out_dir.join("libnirdosha_runtime.a");

    println!("cargo::rerun-if-changed={}", src.display());

    // `--print=native-static-libs` doesn't change what gets built — it
    // makes rustc additionally emit (to stderr) the OS-level system
    // libraries this staticlib's own code transitively needs at final
    // link time (e.g. `-lm -lpthread ...` on Unix, `ws2_32.lib
    // userenv.lib ...` on Windows-MSVC). A `staticlib` crate type bundles
    // only *Rust* code into the `.a` — it does not, and cannot, bundle
    // the OS socket/threading libraries `std::net`/`std::thread`
    // themselves call into. Normally `rustc` supplies this list itself
    // when it drives the final link of a binary; `codegen.rs` bypasses
    // that (it links this `.a` with a bare `clang` invocation, not
    // `rustc`), so it has to be captured here, at the one point `rustc`
    // is actually invoked, and threaded through explicitly. Found via a
    // real Windows CI failure (`docs/ROADMAP.md` A7): the TCP kernels
    // (`nir_tcp_*`) added after this file was Unix-only-tested pulled in
    // `std::net`, which needs `ws2_32.lib` on Windows — link failed with
    // "linker command failed with exit code 1120" (unresolved externals)
    // until this was threaded through. Captured on every platform (not
    // just Windows) so this stays correct automatically if the Unix list
    // ever changes too, instead of only being right for whichever
    // platform someone happened to be testing on.
    let output = Command::new("rustc")
        .arg("--print=native-static-libs")
        .arg("--edition")
        .arg("2024")
        .arg("--crate-type")
        .arg("staticlib")
        .arg("-C")
        .arg("panic=abort")
        .arg("-O")
        .arg(&src)
        .arg("-o")
        .arg(&out_lib)
        .output()
        .expect(
            "failed to invoke `rustc` to build runtime_kernels.rs -- \
             a Rust toolchain with `rustc` on PATH is required to build the \
             `nirdosha` compiler itself, same as `cargo` already is",
        );

    assert!(
        output.status.success(),
        "rustc failed to compile src/runtime_kernels.rs into a staticlib:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );

    // rustc prefixes every note with `note: ` (`note: native-static-libs:
    // ...`), so this looks for the marker as a substring rather than
    // requiring it to start the line.
    let stderr = String::from_utf8_lossy(&output.stderr);
    const MARKER: &str = "native-static-libs: ";
    let native_libs = stderr
        .lines()
        .find_map(|line| line.find(MARKER).map(|i| &line[i + MARKER.len()..]))
        .unwrap_or_else(|| {
            panic!(
                "expected a `native-static-libs:` note in rustc's stderr, found none:\n{stderr}"
            )
        });
    std::fs::write(out_dir.join("native_static_libs.txt"), native_libs)
        .expect("writing native_static_libs.txt into OUT_DIR");
}
