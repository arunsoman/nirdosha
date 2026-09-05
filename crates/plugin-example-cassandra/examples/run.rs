//! ```sh
//! cargo run -p nirdosha-plugin-cassandra --example run -- \
//!     crates/plugin-example-cassandra/examples/query.nir
//! ```

use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_cassandra::CassandraPlugin;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: run <path/to/program.nir>");
        std::process::exit(2);
    });
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("couldn't read {path}: {e}");
        std::process::exit(2);
    });

    let plugins = CassandraPlugin.builtins();
    match nirdosha::run_with_plugins(&src, &plugins) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
