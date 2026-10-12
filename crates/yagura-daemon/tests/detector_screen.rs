#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `VtScreen`, the VT screen model the detector matches patterns against.
//!
//! Only the public wrapper type is driven here; the underlying VT parser crate is an
//! implementation detail and must not appear in this file. Degenerate sizes are clamped
//! rather than panicking because they arrive from untrusted IPC clients, and PTY reads
//! may split escape sequences or multi-byte characters across `feed` calls.

use regex::Regex;
use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::definition::ComposerSection;
use yagura_daemon::detector::screen::ComposerReadout;

const COLS: u16 = 20;
const ROWS: u16 = 4;

fn new_screen() -> VtScreen {
    VtScreen::new(COLS, ROWS)
}

/// A composer-identifying section built directly (not through the TOML
/// loader), for tests that only need the screen-side region/readout
/// algorithm, not definition parsing.
fn composer(marker: &str, separator: &str, continuation_prefix: &str) -> ComposerSection {
    ComposerSection {
        marker: Regex::new(marker).expect("test fixture pattern must compile"),
        separator: separator.to_string(),
        continuation_prefix: continuation_prefix.to_string(),
        blank_lines_in_body: false,
    }
}

/// A screen exactly `rows.len()` rows tall, `cols` wide, with `rows` fed in as
/// display lines joined by CRLF. The cursor is left wherever ordinary output
/// would leave it (the end of the last row fed); callers that need it
/// somewhere else reposition it explicitly with a CUP escape afterward.
fn feed_rows(cols: u16, rows: &[&str]) -> VtScreen {
    let mut screen = VtScreen::new(cols, rows.len() as u16);
    screen.feed(rows.join("\r\n").as_bytes());
    screen
}

fn assert_lines(actual: &[String], expected: &[&str]) {
    let actual: Vec<&str> = actual.iter().map(String::as_str).collect();
    assert_eq!(actual, expected);
}

#[test]
fn size_returns_cols_then_rows() {
    let screen = new_screen();

    assert_eq!(screen.size(), (COLS, ROWS));
}

#[test]
fn fed_lines_are_readable_as_display_lines() {
    let mut screen = new_screen();

    screen.feed(b"line1\r\nline2\r\nline3\r\nline4");

    assert_lines(
        &screen.bottom_lines(4),
        &["line1", "line2", "line3", "line4"],
    );
}

#[test]
fn bottom_lines_returns_only_the_bottom_rows_in_display_order() {
    let mut screen = new_screen();

    screen.feed(b"line1\r\nline2\r\nline3\r\nline4");

    assert_lines(&screen.bottom_lines(2), &["line3", "line4"]);
}

#[test]
fn display_lines_have_trailing_padding_trimmed() {
    let mut screen = new_screen();

    screen.feed(b"ok");

    assert_lines(&screen.bottom_lines(4), &["ok", "", "", ""]);
}

#[test]
fn cursor_positioning_overwrites_an_earlier_line() {
    let mut screen = new_screen();
    screen.feed(b"first\r\nsecond");

    screen.feed(b"\x1b[1;1Hthird");

    assert_lines(&screen.bottom_lines(4), &["third", "second", "", ""]);
}

#[test]
fn carriage_return_redraw_shows_only_the_latest_frame() {
    let mut screen = new_screen();

    screen.feed(b"\x1b[?25lworking -\rworking \\\rworking |");

    assert_lines(&screen.bottom_lines(4), &["working |", "", "", ""]);
}

#[test]
fn erase_display_sequence_removes_previous_content() {
    let mut screen = new_screen();
    screen.feed(b"line1\r\nline2\r\nline3\r\nline4");

    screen.feed(b"\x1b[2J\x1b[Hafter clear");

    assert_lines(&screen.bottom_lines(4), &["after clear", "", "", ""]);
}

#[test]
fn sgr_color_sequences_are_not_part_of_display_text() {
    let mut screen = new_screen();

    screen.feed(b"\x1b[1;31mDo you want to\x1b[0m");

    assert_lines(&screen.bottom_lines(4), &["Do you want to", "", "", ""]);
}

#[test]
fn scrolled_out_lines_are_not_reported_as_display_lines() {
    let mut screen = new_screen();

    screen.feed(b"line1\r\nline2\r\nline3\r\nline4\r\nline5");

    assert_lines(
        &screen.bottom_lines(4),
        &["line2", "line3", "line4", "line5"],
    );
}

#[test]
fn bottom_lines_is_capped_at_the_current_row_count() {
    let mut screen = new_screen();
    screen.feed(b"line1\r\nline2\r\nline3\r\nline4");

    let lines = screen.bottom_lines(1000);

    assert_eq!(lines.len(), usize::from(ROWS));
}

#[test]
fn bottom_lines_with_zero_returns_no_lines() {
    let mut screen = new_screen();
    screen.feed(b"line1");

    assert!(screen.bottom_lines(0).is_empty());
}

#[test]
fn resize_updates_the_reported_size() {
    let mut screen = new_screen();

    screen.resize(40, 6);

    assert_eq!(screen.size(), (40, 6));
}

#[test]
fn display_lines_are_readable_after_resize() {
    let mut screen = new_screen();
    screen.feed(b"before resize");

    screen.resize(40, 6);
    screen.feed(b"\x1b[6;1Hafter resize");

    assert_lines(&screen.bottom_lines(1), &["after resize"]);
}

#[test]
fn resize_to_a_smaller_screen_keeps_the_screen_usable() {
    let mut screen = new_screen();
    screen.feed(b"line1\r\nline2\r\nline3\r\nline4");

    screen.resize(4, 1);

    assert_eq!(screen.size(), (4, 1));
    assert_eq!(screen.bottom_lines(4).len(), 1);
}

#[test]
fn degenerate_resize_is_clamped_instead_of_panicking() {
    let mut screen = new_screen();

    screen.resize(0, 0);

    let (cols, rows) = screen.size();
    assert!(cols >= 1, "cols must be clamped to at least 1, got {cols}");
    assert!(rows >= 1, "rows must be clamped to at least 1, got {rows}");
    screen.feed(b"still alive");
    assert_eq!(screen.bottom_lines(1).len(), 1);
}

#[test]
fn a_size_below_the_narrowest_window_is_raised_to_it() {
    let narrowest = yagura_protocol::MIN_WINDOW_COLS;
    let mut screen = VtScreen::new(narrowest - 1, 5);
    assert_eq!(screen.size(), (narrowest, 5));

    screen.resize(COLS, 5);
    screen.resize(narrowest - 1, 5);
    assert_eq!(screen.size(), (narrowest, 5));
}

#[test]
fn malformed_escape_sequences_do_not_panic() {
    let mut screen = new_screen();

    let malformed: &[&[u8]] = &[
        b"\x1b",
        b"\x1b[",
        b"\x1b[?",
        b"\x1b[38;2;",
        b"\x1b[999999999999;999999999999H",
        b"\x1b[-1;-1H",
        b"\x1b]0;incomplete osc title",
        b"\x1bP unterminated dcs",
        b"\x07\x00\x08\x0b\x0c\x7f",
        b"\x1b[999999999999X",
    ];
    for chunk in malformed {
        screen.feed(chunk);
    }

    assert_eq!(screen.bottom_lines(4).len(), usize::from(ROWS));
}

#[test]
fn invalid_utf8_bytes_do_not_panic() {
    let mut screen = new_screen();

    screen.feed(&[0xff, 0xfe, 0x80, 0xc3, 0x28]);

    assert_eq!(screen.bottom_lines(4).len(), usize::from(ROWS));
}

#[test]
fn screen_stays_usable_after_malformed_input() {
    let mut screen = new_screen();
    screen.feed(b"\x1b[38;2;");
    screen.feed(&[0xff, 0xfe]);

    screen.feed(b"\x1b[2J\x1b[Hrecovered");

    assert_lines(&screen.bottom_lines(4), &["recovered", "", "", ""]);
}

#[test]
fn multi_byte_character_split_across_feeds_is_reassembled() {
    let mut screen = new_screen();
    let text = "実行中".as_bytes();
    let (head, tail) = text.split_at(2);

    screen.feed(head);
    screen.feed(tail);

    assert_lines(&screen.bottom_lines(4), &["実行中", "", "", ""]);
}

#[test]
fn escape_sequence_split_across_feeds_is_interpreted() {
    let mut screen = new_screen();
    screen.feed(b"first\r\nsecond");

    screen.feed(b"\x1b[1;");
    screen.feed(b"1Hthird");

    assert_lines(&screen.bottom_lines(4), &["third", "second", "", ""]);
}

// =====================================================================
// bottom_lines window definition: the window's tail is the last line
// holding a visible (non-whitespace) character, not merely a non-empty
// string. A line a full-screen TUI painted with nothing but whitespace --
// the way such programs clear-and-redraw their bottom padding -- must not
// push the window past the content a human actually sees there.
// =====================================================================

fn has_visible_character(line: &str) -> bool {
    line.chars().any(|c| !c.is_whitespace())
}

#[test]
fn bottom_lines_tail_is_the_last_line_with_a_visible_character_when_space_only_lines_follow() {
    let screen = feed_rows(10, &["", "", "hello", "   ", "", "  "]);

    assert_eq!(
        screen.bottom_lines(3).last().map(String::as_str),
        Some("hello"),
        "space-only lines below the last visible content must not become the window's tail"
    );
}

#[test]
fn bottom_lines_treats_a_no_break_space_only_line_as_blank_like_a_plain_space() {
    let screen = feed_rows(
        10,
        &["", "", "hello", "\u{a0}\u{a0}\u{a0}", "", "\u{a0}\u{a0}"],
    );

    assert_eq!(
        screen.bottom_lines(3).last().map(String::as_str),
        Some("hello")
    );
}

#[test]
fn bottom_lines_treats_an_ideographic_space_only_line_as_blank_like_a_plain_space() {
    let screen = feed_rows(10, &["", "", "hello", "\u{3000}\u{3000}", "", "\u{3000}"]);

    assert_eq!(
        screen.bottom_lines(3).last().map(String::as_str),
        Some("hello")
    );
}

#[test]
fn a_tab_only_line_never_reaches_the_cells_and_so_never_becomes_the_tail() {
    // A tab moves the cursor without writing any cell, so this line stays
    // empty rather than becoming a line full of tab characters.
    let screen = feed_rows(10, &["", "", "hello", "\t\t\t", "", "\t\t"]);

    assert_eq!(
        screen.bottom_lines(3).last().map(String::as_str),
        Some("hello")
    );
}

#[test]
fn bottom_lines_does_not_alter_a_blank_line_inside_the_window_or_trailing_whitespace_on_content() {
    let screen = feed_rows(10, &["hello   ", "   ", "world"]);

    assert_lines(&screen.bottom_lines(3), &["hello   ", "   ", "world"]);
}

#[test]
fn bottom_lines_on_a_fully_blank_screen_is_capped_at_the_current_row_count() {
    let screen = feed_rows(5, &["  ", "  ", "  ", "  "]);

    assert_eq!(screen.bottom_lines(6).len(), 4);
}

#[test]
fn bottom_lines_on_a_fully_blank_screen_never_reports_a_visible_character() {
    let screen = feed_rows(5, &["  ", "  ", "  ", "  "]);

    let lines = screen.bottom_lines(2);

    assert_eq!(lines.len(), 2);
    assert!(
        lines.iter().all(|line| !has_visible_character(line)),
        "a screen with nothing but blank rows must not report a visible character: {lines:?}"
    );
}

#[test]
fn bottom_lines_returns_the_top_n_lines_when_content_is_shorter_than_the_window() {
    let screen = feed_rows(10, &["a", "b", "", "", ""]);

    assert_lines(&screen.bottom_lines(4), &["a", "b", "", ""]);
}

// =====================================================================
// cursor_row: the accessor the composer region-finder
// walks upward from, exposed from `VtScreen::cursor_row`.
// =====================================================================

#[test]
fn cursor_row_starts_at_the_top_of_a_fresh_screen() {
    let screen = new_screen();
    assert_eq!(screen.cursor_row(), 0);
}

#[test]
fn cursor_row_reports_the_row_ordinary_output_ends_on() {
    let mut screen = new_screen();
    screen.feed(b"line1\r\nline2");

    assert_eq!(screen.cursor_row(), 1);
}

#[test]
fn cursor_row_moves_with_explicit_cursor_positioning() {
    let mut screen = new_screen();
    screen.feed(b"line1\r\nline2\r\nline3\r\nline4");

    screen.feed(b"\x1b[1;1H");

    assert_eq!(
        screen.cursor_row(),
        0,
        "a CUP escape moving the cursor to row 1 (1-based) must report row 0 (0-based)"
    );
}

// =====================================================================
// read_composer / ComposerReadout -- region determination. The
// three cases below are the ones the rejected "continuation-prefix-until-it-
// stops-matching" design gets wrong; each pins a property the cursor-bound
// design must still satisfy.
// =====================================================================

#[test]
fn composer_region_does_not_extend_past_the_cursor_row_when_the_continuation_prefix_is_empty() {
    // (i): an empty continuation prefix trivially matches every row, so a
    // design that keeps extending the region "while the prefix matches"
    // never terminates before the bottom of the screen. The region must stop
    // at the cursor row regardless.
    let section = composer(r"\$", " ", "");
    let mut screen = feed_rows(
        40,
        &["prompt$ zzShortDraft", "unwanted trailing content below"],
    );
    // The cursor sits on the composer's own row, right after the typed body
    // -- not wherever the last byte fed happened to land it.
    screen.feed(b"\x1b[1;21H");

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "zzShortDraft"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn composer_region_terminates_at_the_cursor_row_with_no_border_line_below() {
    // (ii): Codex and a plain shell have no border line below the composer at
    // all. The decoy row below also happens to start with the
    // continuation prefix, so a design relying on "the prefix stops matching"
    // to find the end would wrongly absorb it.
    let section = composer(r"^>", " ", "  ");
    let mut screen = feed_rows(
        40,
        &["> zzHEADroot", "  BCDEFTAIL", "  indented decoy line"],
    );
    screen.feed(b"\x1b[2;12H");

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "zzHEADrootBCDEFTAIL"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn composer_region_picks_the_marker_row_nearest_the_cursor_over_an_older_one_further_up() {
    // (iii): an older marker line (a previous, already-submitted prompt) sits
    // above the current one. Walking upward must stop at the nearest marker,
    // not continue past it to the stale one further up.
    let section = composer(r"^>", " ", "  ");
    let mut screen = feed_rows(
        40,
        &[
            "> oldDraftFromEarlier",
            "some unrelated command output",
            "> newDraftHEAD",
            "  newDraftTAIL",
        ],
    );
    screen.feed(b"\x1b[4;15H");

    match screen.read_composer(&section) {
        ComposerReadout::Body(text) => assert_eq!(text, "newDraftHEADnewDraftTAIL"),
        other => panic!("expected Body, got {other:?}"),
    }
}

#[test]
fn note_output_counts_each_chunk_taken_in() {
    let mut screen = new_screen();

    assert_eq!(screen.note_output(), 1);
    assert_eq!(screen.note_output(), 2);
}
