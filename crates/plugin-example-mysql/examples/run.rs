//! Run against the sample program in this directory (needs a real MySQL
//! reachable at `$MYSQL_DSN`, e.g. `mysql://root:nirdosha@127.0.0.1:3306/nirdosha_test`):
//!
//! ```sh
//! MYSQL_DSN=mysql://root:nirdosha@127.0.0.1:3306/nirdosha_test \
//!     cargo run -p nirdosha-plugin-mysql --example run -- \
//!     crates/plugin-example-mysql/examples/query.nir
//! ```

use nirdosha::plugin::NirdoshaPlugin;
use nirdosha_plugin_mysql::MysqlPlugin;

fn main() {
    let path = std::env::args().nth(1).unwrap_or_else(|| {
        eprintln!("usage: run <path/to/program.nir>  (reads $MYSQL_DSN)");
        std::process::exit(2);
    });
    let src = std::fs::read_to_string(&path).unwrap_or_else(|e| {
        eprintln!("couldn't read {path}: {e}");
        std::process::exit(2);
    });

    let plugins = MysqlPlugin.builtins();
    match nirdosha::run_with_plugins(&src, &plugins) {
        Ok(_) => {}
        Err(e) => {
            eprintln!("{e}");
            std::process::exit(1);
        }
    }
}
