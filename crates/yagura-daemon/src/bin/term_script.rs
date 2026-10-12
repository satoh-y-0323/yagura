//! Test utility for PTY integration tests: a child that writes exactly the
//! bytes a test tells it to, so a test can drive a window's terminal model
//! through any sequence and then compare what the daemon sends with it.
//!
//! Script mode (no argument): after entering raw console input it prints
//! [`READY_MARKER`], then reads stdin. Bytes between `<` and `>` are
//! lowercase hex digits (other bytes in between are ignored) and are written
//! to stdout decoded; a `<` restarts the command, so a stray terminal report
//! (a mouse report, for instance) cannot make a command out of what it
//! carries. A `!` outside a command makes the child exit with code 0.
//! Anything else is ignored, so keys the test types at the window do not
//! echo.
//!
//! `attr-grid <cols> <rows>`: waits for the first input byte, then paints
//! `cols` x `rows` cells of `x`, every cell with its own pair of the 16 base
//! colors (see [`fg_code`] and [`bg_code`]), at absolute positions so nothing
//! scrolls, and stays alive. Tests state the expected colors on their own.
//!
//! `sync-flood <bytes>`: waits for the first input byte, then opens a
//! synchronized update (`CSI ? 2026 h`), writes about `<bytes>` bytes of text
//! lines (`x` repeated, a line break every 79 characters), and stays alive
//! without ever ending the update.

use std::io::{self, Read, Write};

const READY_MARKER: &[u8] = b"SCRIPT-READY\r\n";

/// The SGR foreground code of cell (`row`, `col`) in `attr-grid`: one of
/// 30-37 / 90-97 (colors 0-15).
fn fg_code(row: usize, col: usize) -> usize {
    let color = (row + 3 * col) % 16;
    if color < 8 {
        30 + color
    } else {
        90 + color - 8
    }
}

/// The SGR background code of cell (`row`, `col`) in `attr-grid`: one of
/// 40-47 / 100-107 (colors 0-15).
fn bg_code(row: usize, col: usize) -> usize {
    let color = (5 * row + col) % 16;
    if color < 8 {
        40 + color
    } else {
        100 + color - 8
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        _ => None,
    }
}

fn run_script(stdout: &mut io::Stdout) -> io::Result<()> {
    stdout.write_all(READY_MARKER)?;
    stdout.flush()?;

    let mut stdin = io::stdin().lock();
    let mut buf = [0u8; 4096];
    let mut in_command = false;
    let mut high: Option<u8> = None;
    let mut decoded: Vec<u8> = Vec::new();
    loop {
        let count = stdin.read(&mut buf)?;
        if count == 0 {
            return Ok(());
        }
        for byte in buf[..count].iter().copied() {
            match byte {
                b'<' => {
                    in_command = true;
                    high = None;
                    decoded.clear();
                }
                b'>' if in_command => {
                    in_command = false;
                    stdout.write_all(&decoded)?;
                    stdout.flush()?;
                    decoded.clear();
                }
                b'!' if !in_command => {
                    stdout.flush()?;
                    return Ok(());
                }
                _ if in_command => {
                    if let Some(value) = hex_value(byte) {
                        match high.take() {
                            Some(h) => decoded.push((h << 4) | value),
                            None => high = Some(value),
                        }
                    }
                }
                _ => {}
            }
        }
    }
}

fn run_attr_grid(stdout: &mut io::Stdout, cols: usize, rows: usize) -> io::Result<()> {
    let mut first = [0u8; 1];
    while io::stdin().lock().read(&mut first)? == 0 {}

    let mut out = Vec::with_capacity(rows * cols * 14);
    out.extend_from_slice(b"\x1b[H\x1b[2J");
    for row in 0..rows {
        out.extend_from_slice(format!("\x1b[{};1H", row + 1).as_bytes());
        for col in 0..cols {
            out.extend_from_slice(
                format!("\x1b[{};{}mx", fg_code(row, col), bg_code(row, col)).as_bytes(),
            );
        }
    }
    out.extend_from_slice(b"\x1b[0m\x1b[1;1H");
    stdout.write_all(&out)?;
    stdout.flush()?;

    stay_alive()
}

/// Reads stdin to its end, so the child lives until the window closes it.
fn stay_alive() -> io::Result<()> {
    io::copy(&mut io::stdin().lock(), &mut io::sink())?;
    Ok(())
}

fn run_sync_flood(stdout: &mut io::Stdout, bytes: usize) -> io::Result<()> {
    let mut first = [0u8; 1];
    while io::stdin().lock().read(&mut first)? == 0 {}

    stdout.write_all(b"\x1b[H\x1b[2J\x1b[?2026h")?;
    let mut line = vec![b'x'; 79];
    line.extend_from_slice(b"\r\n");
    for _ in 0..bytes.div_ceil(line.len()) {
        stdout.write_all(&line)?;
    }
    stdout.flush()?;

    stay_alive()
}

fn main() -> io::Result<()> {
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("term_script: cannot enter raw console input: {error}");
        std::process::exit(2);
    }
    let args: Vec<String> = std::env::args().collect();
    let mut stdout = io::stdout();
    match args.get(1).map(String::as_str) {
        Some("attr-grid") => {
            let cols = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(80);
            let rows = args.get(3).and_then(|n| n.parse().ok()).unwrap_or(24);
            run_attr_grid(&mut stdout, cols, rows)
        }
        Some("sync-flood") => {
            let bytes = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(0);
            run_sync_flood(&mut stdout, bytes)
        }
        _ => run_script(&mut stdout),
    }
}
