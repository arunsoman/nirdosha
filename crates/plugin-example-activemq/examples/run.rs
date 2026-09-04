//! ```sh
//! cargo run -p nirdosha-plugin-activemq --example run -- \
//!     crates/plugin-example-activemq/examples/pubsub.nir
//! ```

use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_activemq::ActiveMqPlugin;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: run <path/to/program.nir>");
        std::process::exit(2);
    });
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("couldn't read {path}: {e}");
        std::process::exit(2);
    });

    let plugins = ActiveMqPlugin.builtins();
    match nirdosha::run_with_plugins(&src, &plugins) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
