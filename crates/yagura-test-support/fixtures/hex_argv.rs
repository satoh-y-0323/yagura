//! What the fixture binaries of more than one crate share: hex digits decoded
//! to bytes, and an argv started as a child of the fixture.
//!
//! Included by each fixture with `#[path]` rather than taken from this
//! crate's library: a `[[bin]]` cannot use dev-dependencies, and a normal
//! dependency on this crate would put test support in the daemon's and the
//! holder's builds. Cargo builds nothing from this directory.

use std::process::{Command, Stdio};

/// The bytes the hex digit pairs of `digits` name, in either case; a pair
/// that is not two hex digits, and a lone last digit, give nothing.
pub(crate) fn decode_hex(digits: &[u8]) -> Vec<u8> {
    let (pairs, _) = digits.as_chunks::<2>();
    pairs
        .iter()
        .filter_map(|[high, low]| Some((hex_value(*high)? << 4) | hex_value(*low)?))
        .collect()
}

fn hex_value(digit: u8) -> Option<u8> {
    char::from(digit)
        .to_digit(16)
        .and_then(|value| u8::try_from(value).ok())
}

/// Starts the argv `decoded` (one element per line) as a child of this
/// process with no stdio of its own, reaped in the background.
// Test-only fixture, not a production spawn path: it starts the child its
// input names. Some fixtures include this file only to decode hex.
#[allow(dead_code, clippy::disallowed_methods)]
pub(crate) fn spawn_argv(decoded: &[u8]) {
    let text = String::from_utf8_lossy(decoded);
    let mut argv = text.lines();
    let Some(program) = argv.next() else { return };
    let spawned = Command::new(program)
        .args(argv)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    if let Ok(mut child) = spawned {
        std::thread::spawn(move || child.wait());
    }
}
