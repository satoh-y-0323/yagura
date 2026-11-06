#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! What one `feed` on the largest screen keeps within its work budget, read back as the
//! screen's text and modes.

use yagura_term::{PtyHost, VtScreen};

/// No window here is behind a PTY host that asks for Win32 Input Mode; no test feeds 9001.
const HOST: PtyHost = PtyHost {
    requests_win32_input: false,
};

const MAX_COLS: u16 = 1000;
const MAX_ROWS: u16 = 1000;

fn repeated(unit: &str, total_bytes: usize) -> Vec<u8> {
    unit.repeat(total_bytes / unit.len()).into_bytes()
}

#[test]
fn only_line_breaks_are_not_dropped_by_the_work_budget() {
    let mut screen = VtScreen::with_history(MAX_COLS, MAX_ROWS, 20_000, HOST);
    let mut bytes = b"top".to_vec();
    bytes.extend(repeated("\n", 8 * 1024));
    bytes.extend_from_slice(b"bottom");
    screen.feed(&bytes);
    let (text, _) = screen.text_tail(usize::MAX, usize::MAX);
    assert!(text.starts_with("top"), "{:?}", &text[..10.min(text.len())]);
    assert!(text.ends_with("bottom"));
    assert_eq!(text.matches('\n').count(), 8 * 1024);
}

#[test]
fn switching_a_mode_that_leaves_the_screen_alone_costs_no_work() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS, HOST);
    let mut bytes = "\x1b[?1h\x1b[?1l".repeat(8).into_bytes();
    bytes.extend_from_slice(b"\x1b[?25l");
    screen.feed(&bytes);
    assert!(screen.cursor_hidden());
}
