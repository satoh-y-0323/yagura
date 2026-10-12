#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! How a window's screen model follows a resize: the cursor stays in view and
//! rows move between the history and the screen, as on a Unix pseudo terminal.
//! The bundled ConPTY sends nothing downstream on a resize, so every system
//! follows this one rule.

use yagura_daemon::detector::VtScreen;

#[test]
fn rows_are_pulled_back_from_history_when_rows_are_added() {
    let mut screen = VtScreen::with_history(10, 3, 100);
    screen.feed(b"a\r\nb\r\nc\r\nd\r\ne");
    screen.resize(10, 5);
    screen.feed(b"\x1b[HX");
    assert_eq!(screen.text_tail(usize::MAX, usize::MAX).0, "X\nb\nc\nd\ne");
}

#[test]
fn the_top_row_is_pushed_into_history_when_columns_shrink() {
    let mut screen = VtScreen::with_history(10, 6, 100);
    screen.feed(b"0123456789ABCDE\r\nx");
    screen.resize(5, 6);
    screen.feed(b"\x1b[HZ");
    assert_eq!(
        screen.text_tail(usize::MAX, usize::MAX).0,
        "01234Z6789ABCDE\nx"
    );
}
