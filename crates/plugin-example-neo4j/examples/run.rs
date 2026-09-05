//! ```sh
//! cargo run -p nirdosha-plugin-neo4j --example run -- \
//!     crates/plugin-example-neo4j/examples/query.nir
//! ```

use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_neo4j::Neo4jPlugin;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: run <path/to/program.nir>");
        std::process::exit(2);
    });
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("couldn't read {path}: {e}");
        std::process::exit(2);
    });

    let plugins = Neo4jPlugin.builtins();
    match nirdosha::run_with_plugins(&src, &plugins) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
