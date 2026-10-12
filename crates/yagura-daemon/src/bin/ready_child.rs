//! Test utility for PTY integration tests: writes the marker line given as
//! its first argument, then echoes stdin to stdout byte for byte like
//! `echo_child`.
//!
//! The marker is what makes a window's readiness observable: a test can wait
//! for that exact line instead of guessing when the child started reading.
//!
//! Positional argv, no parser:
//! `ready_child <marker> [<composer_marker> <separator> <continuation_prefix>
//! <reflect_delay_ms> [<fold_placeholder>]]`.
//! Given the four trailing arguments the fixture runs in composer-draw mode
//! instead: it prints the same marker line, then paints what was written to
//! it as one marked composer region and echoes a line only once a submit key
//! arrives. Without them it behaves exactly as it did before the mode
//! existed, which is what every caller of the one-argument launcher relies on.

use std::io::{self, Write};

#[path = "shared/composer_paint.rs"]
mod composer_paint;
#[path = "shared/echo_modes.rs"]
mod echo_modes;

use composer_paint::ComposerStyle;
use echo_modes::{draw_until_end_of_input, echo_until_end_of_input};

fn main() -> io::Result<()> {
    let marker = std::env::args().nth(1).unwrap_or_default();
    match ComposerStyle::from_args(2) {
        Some((style, reflect)) => {
            draw_until_end_of_input("ready_child", Some(&marker), style, reflect)
        }
        None => {
            let mut stdout = io::stdout();
            writeln!(stdout, "{marker}")?;
            stdout.flush()?;
            echo_until_end_of_input()
        }
    }
}
