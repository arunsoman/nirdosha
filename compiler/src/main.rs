use std::process::ExitCode;

use nirdosha::interpreter::Value;

fn main() -> ExitCode {
    let mut args = std::env::args();
    let _bin = args.next();
    let Some(path) = args.next() else {
        eprintln!("usage: nirdosha <file.nir>");
        return ExitCode::FAILURE;
    };

    let src = match std::fs::read_to_string(&path) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("error reading {path}: {e}");
            return ExitCode::FAILURE;
        }
    };

    match nirdosha::run(&src) {
        Ok(Value::Int(n)) => {
            println!("=> {n}");
            ExitCode::SUCCESS
        }
        Ok(Value::Bool(b)) => {
            println!("=> {b}");
            ExitCode::SUCCESS
        }
        Ok(Value::Unit) => ExitCode::SUCCESS,
        Ok(v @ (Value::Boxed(_) | Value::Ref(_))) => {
            println!("=> {v:?}");
            ExitCode::SUCCESS
        }
        Err(msg) => {
            eprintln!("{msg}");
            ExitCode::FAILURE
        }
    }
}
