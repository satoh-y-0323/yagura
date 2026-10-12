//! Test utility for PTY integration tests: echoes stdin to stdout byte for
//! byte, so binary data and UTF-8 sequences survive the round trip unchanged.
//!
//! Positional argv, no parser (same convention as `ready_child`):
//! `echo_child [<composer_marker> <separator> <continuation_prefix> <reflect_delay_ms>
//! [<fold_placeholder>]]`.
//! Given those four arguments the fixture runs in composer-draw mode instead:
//! it paints what was written to it as one marked composer region and echoes
//! a line only once a submit key arrives. Without them it behaves exactly as
//! it did before the mode existed, which is what every caller of the
//! argument-less launcher relies on.

use std::io;

#[path = "shared/composer_paint.rs"]
mod composer_paint;
#[path = "shared/echo_modes.rs"]
mod echo_modes;

use composer_paint::ComposerStyle;
use echo_modes::{draw_until_end_of_input, echo_until_end_of_input};

fn main() -> io::Result<()> {
    match ComposerStyle::from_args(1) {
        Some((style, reflect)) => draw_until_end_of_input("echo_child", None, style, reflect),
        None => echo_until_end_of_input(),
    }
}
