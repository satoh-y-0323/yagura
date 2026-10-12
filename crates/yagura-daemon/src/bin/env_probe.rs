//! Test utility for PTY integration tests: reports one named environment
//! variable back to the parent as a single sentinel-wrapped line, so a test
//! observes child-process environment inheritance without a shell in the way.
//!
//! The caller is responsible for not naming a variable that may hold a
//! secret, since this fixture prints the value verbatim.

use std::env;
use std::io::{self, Write};

fn main() -> io::Result<()> {
    let args: Vec<String> = env::args().collect();
    let name = match args.get(1) {
        // Further arguments are ignored so a launch profile may append its own.
        Some(name) => name,
        _ => {
            eprintln!("usage: env_probe <NAME> [ignored...]");
            std::process::exit(2);
        }
    };

    let line = match env::var_os(name) {
        Some(value) => format!("<<{name}={}>>\n", value.to_string_lossy()),
        None => format!("<<{name} unset>>\n"),
    };

    let mut stdout = io::stdout();
    stdout.write_all(line.as_bytes())?;
    stdout.flush()
}
