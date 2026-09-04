//! The literal, runnable version of "install a package via Cargo, and
//! Nirdosha's compiler takes care of the rest"
//! (`docs/ROADMAP.md` Track G, G1 / `docs/ECOSYSTEM.md` §G1's Stage 1
//! proposal). This file is exactly what a project author writes today —
//! `cargo add`-ing a plugin crate is real; there's no CLI
//! auto-discovery yet (disclosed in `docs/ECOSYSTEM.md` §G1), so the
//! project's own small entrypoint is what wires the plugin in and
//! drives the pipeline.
//!
//! Run it against the sample program in this same directory:
//!
//! ```sh
//! cargo run -p nirdosha-plugin-rot13 --example run -- \
//!     crates/plugin-example-rot13/examples/scramble.nir
//! # Aveqbfun
//! ```
//!
//! Or against any other `.nir` file that calls `rot13(s: str) -> str` —
//! nothing here is specific to `scramble.nir`.

use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_rot13::Rot13Plugin;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: run <path/to/program.nir>");
        std::process::exit(2);
    });
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("couldn't read {path}: {e}");
        std::process::exit(2);
    });

    // This one line is the entire "installation" step: any crate
    // implementing `NirdoshaPlugin` -- `Rot13Plugin` here, a real
    // third-party crate in general -- plugs into the same
    // `run_with_plugins` call. Nothing about `run_with_plugins` itself
    // is specific to this plugin.
    let plugins = Rot13Plugin.builtins();

    match nirdosha::run_with_plugins(&src, &plugins) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
