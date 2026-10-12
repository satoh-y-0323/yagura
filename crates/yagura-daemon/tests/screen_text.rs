#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The text a window shows, as `read_output` and the status summary return it: what a
//! person sees in the window (and could scroll back to), in logical lines.

use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::screen_view::View;
use yagura_protocol::PaletteSlot;

const NO_BYTE_LIMIT: usize = usize::MAX;

fn screen(cols: u16, rows: u16, history: usize, bytes: &[u8]) -> VtScreen {
    let mut screen = VtScreen::with_history(cols, rows, history);
    screen.feed(bytes);
    screen
}

#[test]
fn a_row_the_terminal_redraws_at_its_last_column_is_read_once() {
    // ConPTY's repaint of a wrapped line once the screen scrolls: a line break, then the
    // cursor back on the previous row's last column, which is written again.
    let bytes = b"aaaaaaaaaabbbbbbbbbbcccccccccX\r\n\x1b[2;10HXYZ";
    let mut screen = screen(10, 3, 100, bytes);
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("aaaaaaaaaabbbbbbbbbbcccccccccXYZ".to_string(), false)
    );
}

#[test]
fn a_line_wider_than_the_window_is_one_line() {
    let mut line = "0123456789".repeat(7);
    line.push_str("tail");
    let bytes = format!("{line}\r\nshort\r\n");
    let mut screen = screen(10, 4, 100, bytes.as_bytes());
    assert_eq!(
        screen.text_tail(2, NO_BYTE_LIMIT),
        (format!("{line}\nshort"), false)
    );
    assert_eq!(
        screen.text_tail(1, NO_BYTE_LIMIT),
        ("short".to_string(), true)
    );
}

#[test]
fn lines_that_scrolled_off_the_screen_are_still_read() {
    let bytes: String = (1..=20).map(|n| format!("line {n}\r\n")).collect();
    let mut screen = screen(20, 3, 100, bytes.as_bytes());
    let all: Vec<String> = (1..=20).map(|n| format!("line {n}")).collect();
    assert_eq!(screen.text_tail(20, NO_BYTE_LIMIT), (all.join("\n"), false));
    assert_eq!(
        screen.text_tail(3, NO_BYTE_LIMIT),
        (all[17..].join("\n"), true)
    );
}

#[test]
fn a_line_redrawn_in_place_reads_as_its_last_state() {
    let mut screen = screen(20, 3, 100, b"spin 1\r\x1b[Kspin 2\r\x1b[Kspin 3");
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("spin 3".to_string(), false)
    );
}

#[test]
fn a_full_screen_program_shows_only_its_screen_and_the_history_returns_after_it() {
    let mut screen = screen(
        20,
        3,
        100,
        b"before 1\r\nbefore 2\r\n\x1b[?1049h\x1b[H\x1b[2Jfull screen view",
    );
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("full screen view".to_string(), false)
    );

    screen.feed(b"\x1b[?1049l");
    let (text, _) = screen.text_tail(10, NO_BYTE_LIMIT);
    assert!(
        text.contains("before 1") && text.contains("before 2") && !text.contains("full screen"),
        "leaving the full-screen program must give back the history and nothing of its screen; got {text:?}"
    );
}

#[test]
fn the_byte_limit_keeps_the_end_on_a_character_boundary() {
    let mut screen = screen(20, 3, 100, "first\r\nあいう\r\n".as_bytes());
    assert_eq!(screen.text_tail(10, 7), ("いう".to_string(), true));
}

#[test]
fn a_blank_screen_reads_as_empty_text() {
    let mut screen = screen(20, 3, 100, b"");
    assert_eq!(screen.text_tail(10, NO_BYTE_LIMIT), (String::new(), false));
}

#[test]
fn spaces_painted_after_a_line_are_not_part_of_it() {
    let mut screen = screen(20, 3, 100, b"value  \x1b[5Ckept\r\nend     ");
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("value       kept\nend".to_string(), false)
    );
}

const LONG_LINE_LEN: usize = 70;

fn long_line() -> String {
    let mut line = String::new();
    for n in 0..LONG_LINE_LEN / 10 {
        line.push_str(&format!("https{n:04}x"));
    }
    line
}

fn lines_of(screen: &mut VtScreen) -> Vec<String> {
    screen
        .text_tail(usize::MAX, NO_BYTE_LIMIT)
        .0
        .split('\n')
        .map(str::to_string)
        .collect()
}

#[test]
fn a_long_history_line_stays_one_line_when_the_window_is_resized() {
    let line = long_line();
    let mut bytes = format!("{line}\r\n");
    for n in 1..=10 {
        bytes.push_str(&format!("row {n}\r\n"));
    }
    for width in [5u16, 40, 200, 10] {
        let mut screen = screen(10, 3, 100, bytes.as_bytes());
        screen.resize(width, 3);
        let lines = lines_of(&mut screen);
        assert_eq!(lines.first(), Some(&line), "width {width}: {lines:?}");
        assert_eq!(lines.len(), 11, "width {width}: {lines:?}");
    }
}

#[test]
fn a_wrapped_row_on_screen_stays_one_line_after_a_resize() {
    let line = "abcdefghij".repeat(2) + "klmno";
    for width in [7u16, 30, 10] {
        let mut screen = screen(10, 5, 100, line.as_bytes());
        screen.resize(width, 5);
        assert_eq!(
            screen.text_tail(10, NO_BYTE_LIMIT),
            (line.clone(), false),
            "width {width}"
        );
    }
}

#[test]
fn resizing_to_the_same_size_leaves_the_text_unchanged() {
    let bytes = format!("{}\r\nshort\r\n{}", long_line(), "z".repeat(25));
    let mut screen = screen(10, 4, 100, bytes.as_bytes());
    let before = screen.text_tail(usize::MAX, NO_BYTE_LIMIT);
    screen.resize(10, 4);
    assert_eq!(screen.text_tail(usize::MAX, NO_BYTE_LIMIT), before);
}

#[test]
fn shrinking_the_rows_keeps_the_prompt_at_the_cursor_and_moves_the_rest_to_history() {
    let mut screen = screen(10, 5, 100, b"l1\r\nl2\r\nl3\r\nl4\r\nprompt$ ");
    screen.resize(10, 2);
    assert_eq!(
        screen.text_tail(usize::MAX, NO_BYTE_LIMIT),
        ("l1\nl2\nl3\nl4\nprompt$".to_string(), false)
    );
}

fn count_char(text: &str, c: char) -> usize {
    text.chars().filter(|&x| x == c).count()
}

#[test]
fn a_repeat_request_adds_no_more_cells_than_the_bytes_and_the_screen_allow() {
    let (cols, rows) = (10usize, 3usize);
    for prefix in ["", "\x1b[4h"] {
        let bytes = format!("{prefix}a\x1b[65535b");
        let mut screen = screen(10, 3, 100, bytes.as_bytes());
        let (text, _) = screen.text_tail(usize::MAX, NO_BYTE_LIMIT);
        let budget = bytes.len() + 2 * cols * rows;
        assert!(
            count_char(&text, 'a') <= budget,
            "prefix {prefix:?}: {} cells for a budget of {budget}",
            count_char(&text, 'a')
        );
        assert!(count_char(&text, 'a') >= 1, "prefix {prefix:?}");
    }
}

#[test]
fn ordinary_text_is_not_dropped_by_the_repeat_budget() {
    let bytes: String = (1..=30).map(|n| format!("line {n}\r\n")).collect();
    let mut screen = screen(10, 3, 100, bytes.as_bytes());
    let lines = lines_of(&mut screen);
    let expected: Vec<String> = (1..=30).map(|n| format!("line {n}")).collect();
    assert_eq!(lines, expected);
}

#[test]
fn combining_characters_on_one_cell_are_capped_at_sixteen() {
    let mut bytes = "e".to_string();
    bytes.push_str(&"\u{301}".repeat(1000));
    let mut screen = screen(20, 3, 100, bytes.as_bytes());
    let (text, _) = screen.text_tail(10, NO_BYTE_LIMIT);
    let marks = count_char(&text, '\u{301}');
    assert!(text.starts_with('e'), "{text:?}");
    assert_eq!(marks, 16, "combining marks kept");
}

#[test]
fn a_combining_mark_on_a_cell_nobody_wrote_is_read() {
    let mut screen = screen(20, 3, 0, "a\x1b[3G\u{301}".as_bytes());
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("a \u{301}".to_string(), false)
    );
}

#[test]
fn a_terminal_reset_clears_what_was_shown() {
    let mut screen = screen(20, 3, 0, b"before\x1bcafter");
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("after".to_string(), false)
    );
}

/// The background color the program set, as a frame of the window's view shows it.
fn background_set(screen: &mut VtScreen) -> Option<[u8; 3]> {
    let frame = View::new()
        .capture(screen, false, true)
        .expect("a full capture always reads a frame")
        .into_parts()
        .pop()
        .expect("a frame has at least one part");
    frame
        .overrides
        .iter()
        .find(|color| color.slot == PaletteSlot::Background)
        .map(|color| color.rgb)
}

/// OSC 10 with `filler` bytes as its first argument (a foreground color that does
/// not parse) and red as its second, which sets the background: the background
/// changes only if the whole string reached the parser.
fn osc_with_filler(filler: usize) -> Vec<u8> {
    let mut bytes = b"\x1b]10;".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', filler));
    bytes.extend_from_slice(b";rgb:ff/00/00\x07after");
    bytes
}

#[test]
fn an_osc_is_applied_whole_within_the_length_limit_and_cut_past_it() {
    const RED: Option<[u8; 3]> = Some([0xff, 0, 0]);
    let within = osc_with_filler(60 * 1024);
    let past = osc_with_filler(70 * 1024);
    let prefixed = |prefix: &[u8]| [prefix, past.as_slice()].concat();
    let cases = [
        ("within, one chunk", within.clone(), 1, RED),
        ("within, three chunks", within, 3, RED),
        ("past, one chunk", past.clone(), 1, None),
        ("past, three chunks", past.clone(), 3, None),
        ("past, after a doubled ESC", prefixed(b"\x1b"), 1, None),
        (
            "past, after an OSC ended by a bare ESC",
            prefixed(b"\x1b]0;t"),
            1,
            None,
        ),
    ];
    for (name, bytes, chunks, expected) in cases {
        let mut screen = VtScreen::with_history(20, 3, 100);
        for chunk in bytes.chunks(bytes.len().div_ceil(chunks)) {
            screen.feed(chunk);
        }
        assert_eq!(background_set(&mut screen), expected, "{name}");
        assert_eq!(
            screen.text_tail(10, NO_BYTE_LIMIT),
            ("after".to_string(), false),
            "{name}"
        );
    }
}

#[test]
fn a_very_long_osc_shows_nothing_and_the_text_after_its_terminator_is_read() {
    let mut bytes = b"\x1b]0;".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 200 * 1024));
    bytes.push(0x07);
    bytes.extend_from_slice(b"after");
    let mut screen = screen(20, 3, 100, &bytes);
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("after".to_string(), false)
    );
}

#[test]
fn the_older_alternate_screen_modes_show_only_their_own_screen() {
    for mode in ["47", "1047"] {
        let mut screen = screen(20, 3, 100, b"before");
        screen.feed(format!("\x1b[?{mode}h\x1b[Hother").as_bytes());
        assert_eq!(
            screen.text_tail(10, NO_BYTE_LIMIT),
            ("other".to_string(), false),
            "mode {mode} entered"
        );
        screen.feed(format!("\x1b[?{mode}l").as_bytes());
        assert_eq!(
            screen.text_tail(10, NO_BYTE_LIMIT),
            ("before".to_string(), false),
            "mode {mode} left"
        );
    }
}

#[test]
fn output_inside_an_unclosed_synchronized_update_is_readable_at_once() {
    let mut screen = screen(20, 3, 100, b"\x1b[?2026hinside");
    assert_eq!(
        screen.text_tail(10, NO_BYTE_LIMIT),
        ("inside".to_string(), false)
    );
}

#[test]
fn a_synchronized_update_holds_the_view_up_to_sixty_four_bytes_per_cell() {
    let (cols, rows) = (80u16, 24u16);
    let bound = usize::from(cols) * usize::from(rows) * 64;
    let open = b"\x1b[?2026h";
    let mut screen = VtScreen::new(cols, rows);

    screen.feed(open);
    assert!(screen.frame_held(), "just opened");
    screen.feed(&vec![b'x'; bound - open.len()]);
    assert!(screen.frame_held(), "at the bound");
    screen.feed(b"x");
    assert!(!screen.frame_held(), "one byte past the bound");
}

#[test]
fn a_full_history_marks_the_text_truncated_even_when_all_of_it_is_read() {
    let bytes: String = (1..=20).map(|n| format!("line {n}\r\n")).collect();
    let mut screen = screen(20, 3, 5, bytes.as_bytes());
    let (text, truncated) = screen.text_tail(usize::MAX, NO_BYTE_LIMIT);
    let kept: Vec<String> = (14..=20).map(|n| format!("line {n}")).collect();
    assert_eq!(text, kept.join("\n"));
    assert!(truncated, "lines 1 to 13 fell out of the full history");
}

#[test]
fn a_history_of_blank_rows_reports_the_rows_it_dropped() {
    let scrolled = [b"hello".as_slice(), &b"\r\n".repeat(20)].concat();

    let mut full = screen(20, 3, 5, &scrolled);
    assert_eq!(
        full.text_tail(10, NO_BYTE_LIMIT),
        (String::new(), true),
        "`hello` scrolled out of a history that is full"
    );

    let mut roomy = screen(20, 3, 100, &scrolled);
    assert_eq!(
        roomy.text_tail(10, NO_BYTE_LIMIT),
        ("hello".to_string(), false)
    );
}

#[test]
fn the_byte_limit_keeps_exactly_the_last_bytes_of_ascii_text() {
    let mut screen = screen(20, 3, 100, b"0123456789");
    assert_eq!(screen.text_tail(10, 4), ("6789".to_string(), true));
}

#[test]
fn blanks_painted_at_the_end_of_a_line_do_not_split_it_under_the_byte_limit() {
    let mut bytes = b"hello".to_vec();
    bytes.extend(std::iter::repeat_n(b' ', 3000));
    let mut screen = screen(100, 30, 1000, &bytes);
    assert_eq!(
        screen.text_tail(usize::MAX, 2000),
        ("hello".to_string(), false)
    );
}

#[test]
fn a_line_longer_than_the_byte_limit_is_one_line_cut_at_the_limit() {
    let mut screen = screen(10, 5, 100, b"01234567890123456789012345");
    assert_eq!(
        screen.text_tail(usize::MAX, 12),
        ("456789012345".to_string(), true)
    );
}
