//! Test utility for PTY integration tests: gives a window's terminal model a
//! known load, so a test can watch how the daemon behaves while the child keeps
//! the model busy.
//!
//! Positional argv, no parser: `screen_load_child <mode> [<text>]`.
//! - `align`: floods `ESC # 8` (fill the screen).
//! - `rep`: floods a character followed by `CSI 65535 b` (repeat it).
//! - `text <line>`: prints `<line>` once at start and stays alive.
//!
//! Every mode stays alive until its stdout closes, so the window under test
//! does not exit while the test is still reading it.

use std::io::{self, Write};

/// Matches the daemon's PTY read size, so one block is the most a read returns.
const BLOCK_BYTES: usize = 8192;

fn flood(unit: &[u8]) -> io::Result<()> {
    let block = unit.repeat(BLOCK_BYTES / unit.len());
    let stdout = io::stdout();
    let mut stdout = stdout.lock();
    loop {
        stdout.write_all(&block)?;
        stdout.flush()?;
    }
}

fn stay_alive() -> io::Result<()> {
    io::copy(&mut io::stdin().lock(), &mut io::sink())?;
    Ok(())
}

fn main() -> io::Result<()> {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(String::as_str) {
        Some("align") => flood(b"\x1b#8"),
        Some("rep") => flood(b"x\x1b[65535b"),
        Some("text") => {
            let line = args.get(2).map(String::as_str).unwrap_or_default();
            let mut stdout = io::stdout();
            stdout.write_all(format!("{line}\r\n").as_bytes())?;
            stdout.flush()?;
            stay_alive()
        }
        _ => {
            eprintln!("usage: screen_load_child align|rep|text <line>");
            std::process::exit(2);
        }
    }
}
