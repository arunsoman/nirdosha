// Compiles `src/runtime_kernels.rs` — the freestanding, data-dependent
// linalg kernels (`det`/`inv`/`solve`/`rank`/`kf_update_state`/
// `kf_update_cov`) — into a static library at `nirdosha`'s own build
// time, once, not per user program. `codegen.rs` embeds the resulting
// `.a` via `include_bytes!` and links it into every native binary
// `nirdosha build` produces, alongside the `.ll` file it generates —
// see runtime_kernels.rs's module doc for why these six builtins go
// through a linked native `call` instead of hand-emitted branchy IR.
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

    let status = Command::new("rustc")
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
        .status()
        .expect(
            "failed to invoke `rustc` to build runtime_kernels.rs -- \
             a Rust toolchain with `rustc` on PATH is required to build the \
             `nirdosha` compiler itself, same as `cargo` already is",
        );

    assert!(status.success(), "rustc failed to compile src/runtime_kernels.rs into a staticlib");
}
