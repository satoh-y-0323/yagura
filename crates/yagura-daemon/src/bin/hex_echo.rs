//! Test utility for PTY integration tests: prints a readiness marker, then
//! writes every byte read from stdin as two lowercase hexadecimal characters.
//!
//! Hex output keeps control sequences printable, so a test can observe the
//! exact bytes received by the child without asking the VT renderer to retain
//! an escape sequence as text.
//!
//! Optional modes, chosen by the first argument:
//! - `--emit <hex>...`: after the marker, writes each hex-encoded argument to
//!   stdout as raw bytes, prints [`EMITTED_MARKER`], then echoes stdin as usual.
//!   A terminal's replies to queries emitted this way show up as hex.
//! - `--flood <hex> <count>`: after the marker, writes the hex-encoded argument
//!   `count` times without reading stdin, prints [`FLOODED_MARKER`], then echoes
//!   stdin as usual.
//! - `--emit-on-input <hex>...`: echoes stdin as usual; right after echoing the
//!   first input it reads, writes each hex-encoded argument to stdout as raw
//!   bytes and prints [`EMITTED_MARKER`], once. A test can change the child's
//!   terminal modes at a moment it chooses.

use std::io::{self, Read, Write};

const READY_MARKER: &[u8] = b"CTRL-READY\r\n";
const EMITTED_MARKER: &[u8] = b"\r\nQUERIES-EMITTED\r\n";
const FLOODED_MARKER: &[u8] = b"\r\nQUERIES-FLOODED\r\n";
const HEX: &[u8; 16] = b"0123456789abcdef";

fn main() -> io::Result<()> {
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("hex_echo: cannot enter raw console input: {error}");
        std::process::exit(2);
    }

    let args: Vec<String> = std::env::args().skip(1).collect();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let mut stdout = io::stdout();
    stdout.write_all(READY_MARKER)?;
    stdout.flush()?;

    match args.first().map(String::as_str) {
        Some("--emit") => {
            for query in &args[1..] {
                stdout.write_all(&decode_hex(query))?;
            }
            stdout.write_all(EMITTED_MARKER)?;
            stdout.flush()?;
        }
        Some("--flood") => {
            let query = decode_hex(args.get(1).map_or("", String::as_str));
            let count: usize = args.get(2).and_then(|n| n.parse().ok()).unwrap_or(0);
            for _ in 0..count {
                stdout.write_all(&query)?;
            }
            stdout.write_all(FLOODED_MARKER)?;
            stdout.flush()?;
        }
        _ => {}
    }
    let mut emit_on_input: Option<Vec<Vec<u8>>> = match args.first().map(String::as_str) {
        Some("--emit-on-input") => Some(args[1..].iter().map(|arg| decode_hex(arg)).collect()),
        _ => None,
    };

    let mut input_bytes = [0u8; 4096];
    let mut encoded = [0u8; 8192];
    loop {
        let count = input.read(&mut input_bytes)?;
        if count == 0 {
            break;
        }
        for (index, byte) in input_bytes[..count].iter().copied().enumerate() {
            encoded[index * 2] = HEX[(byte >> 4) as usize];
            encoded[index * 2 + 1] = HEX[(byte & 0x0f) as usize];
        }
        stdout.write_all(&encoded[..count * 2])?;
        if let Some(sequences) = emit_on_input.take() {
            for sequence in &sequences {
                stdout.write_all(sequence)?;
            }
            stdout.write_all(EMITTED_MARKER)?;
        }
        stdout.flush()?;
    }

    Ok(())
}

fn decode_hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks(2)
        .filter_map(|pair| {
            std::str::from_utf8(pair)
                .ok()
                .and_then(|digits| u8::from_str_radix(digits, 16).ok())
        })
        .collect()
}
