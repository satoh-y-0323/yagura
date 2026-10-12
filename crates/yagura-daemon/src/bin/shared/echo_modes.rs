//! The two modes `echo_child` and `ready_child` share: echo every byte read,
//! or paint the input as a composer and echo a line per submit key.
//!
//! Included with `#[path]` next to `composer_paint`, for the reason given
//! there.

use std::io::{self, Read, Write};
use std::sync::Arc;

use crate::composer_paint::{ComposerStyle, Output, Reflect, Scan, accept_byte, spawn_reflector};

/// Writes every byte read straight back out, flushing each read so a line
/// still being typed shows at once.
pub(crate) fn echo_until_end_of_input() -> io::Result<()> {
    let stdin = io::stdin();
    let mut stdout = io::stdout();
    let mut buf = vec![0u8; 4096];

    loop {
        let n = stdin.lock().read(&mut buf)?;
        if n == 0 {
            break;
        }
        stdout.write_all(&buf[0..n])?;
        stdout.flush()?;
    }

    Ok(())
}

/// Accumulates input in a painted composer region and echoes it as one line
/// when a submit key arrives. `first_line`, when given, is written above the
/// composer before any input is read.
pub(crate) fn draw_until_end_of_input(
    fixture: &str,
    first_line: Option<&str>,
    style: ComposerStyle,
    reflect: Reflect,
) -> io::Result<()> {
    // A real vendor CLI raw-modes its own terminal on start; without that, the
    // platform's default line discipline echoes and reshapes the bytes written
    // to this process before the composer ever sees them.
    if let Err(error) = yagura_os::enter_raw_console_input() {
        eprintln!("{fixture}: cannot enter raw console input: {error}");
        std::process::exit(2);
    }

    let output = Arc::new(Output::new(Some(style)));
    let reflector = spawn_reflector(Arc::clone(&output), reflect);
    match first_line {
        Some(line) => output.line(line)?,
        None => output.repaint()?,
    }

    let stdin = io::stdin();
    let mut chunk = vec![0u8; 4096];
    let mut buffer: Vec<u8> = Vec::new();
    let mut scan = Scan::Text;

    loop {
        let n = stdin.lock().read(&mut chunk)?;
        if n == 0 {
            break;
        }
        for &byte in &chunk[..n] {
            if let Some(submitted) = accept_byte(&mut buffer, &mut scan, byte) {
                // The buffer is already empty here (accept_byte took it), so
                // this paints the accepted line and the emptied composer in
                // the same frame instead of leaving the composer to a later,
                // separately timed repaint.
                output.set_body(&String::from_utf8_lossy(&buffer));
                output.line(&String::from_utf8_lossy(&submitted))?;
            }
        }
        output.set_body(&String::from_utf8_lossy(&buffer));
        let _ = reflector.send(());
    }

    Ok(())
}
