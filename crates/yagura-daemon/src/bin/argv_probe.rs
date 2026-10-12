//! Test utility for PTY integration tests: reports every argument it was
//! started with back to the parent, one sentinel-wrapped line per element
//! (`<<argv[N]=value>>`, `N` counted from 1), so a test observes the exact
//! argv a launch appended without a shell in the way.
//!
//! The lines are assembled first and written in one call: a console host
//! renders its buffer to the PTY asynchronously, so a process that writes
//! line by line and exits at once can have its later lines dropped.

use std::env;
use std::io::{self, Write};

fn main() -> io::Result<()> {
    let report: String = env::args_os()
        .enumerate()
        .skip(1)
        .map(|(index, value)| format!("<<argv[{index}]={}>>\n", value.to_string_lossy()))
        .collect();

    let mut stdout = io::stdout();
    stdout.write_all(report.as_bytes())?;
    stdout.flush()
}
