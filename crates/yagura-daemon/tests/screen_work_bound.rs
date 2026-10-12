#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! One `feed` of a few KB must return quickly even on the largest screen, whatever the
//! child asks for: a few bytes can ask for a rewrite of the whole screen.

use std::time::{Duration, Instant};

use yagura_daemon::detector::VtScreen;

const MAX_COLS: u16 = 1000;
const MAX_ROWS: u16 = 1000;
/// Generous on purpose: an unbounded feed takes tens of seconds in a debug build.
const LIMIT: Duration = Duration::from_secs(3);

fn timed_feed(screen: &mut VtScreen, bytes: &[u8]) {
    let start = Instant::now();
    screen.feed(bytes);
    let took = start.elapsed();
    assert!(took < LIMIT, "a {} byte feed took {took:?}", bytes.len());
}

fn repeated(unit: &str, total_bytes: usize) -> Vec<u8> {
    unit.repeat(total_bytes / unit.len()).into_bytes()
}

#[test]
fn a_flood_of_screen_alignment_fills_returns_quickly_and_the_first_one_is_applied() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
    timed_feed(&mut screen, &repeated("\x1b#8", 8 * 1024));
    let lines = screen.bottom_lines(usize::from(MAX_ROWS));
    assert!(lines[0].starts_with("EEEE"), "{:?}", &lines[0][..8]);
}

#[test]
fn a_flood_of_screen_erases_returns_quickly() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
    screen.feed(b"kept?");
    timed_feed(&mut screen, &repeated("\x1b[2J", 8 * 1024));
    assert!(screen.bottom_lines(1).iter().all(|l| !l.contains("kept")));
}

#[test]
fn a_flood_of_line_erases_and_character_edits_returns_quickly() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
    for unit in ["\x1b[2K", "\x1b[1024@", "\x1b[1024P", "\x1b[1024X"] {
        timed_feed(&mut screen, &repeated(unit, 8 * 1024));
    }
}

#[test]
fn a_flood_of_line_inserts_deletes_and_scrolls_returns_quickly() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
    for unit in ["\x1b[512L", "\x1b[512M", "\x1b[512S", "\x1b[512T"] {
        timed_feed(&mut screen, &repeated(unit, 8 * 1024));
    }
}

#[test]
fn a_flood_of_reverse_index_and_insert_mode_text_returns_quickly() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
    timed_feed(&mut screen, &repeated("\x1bM", 8 * 1024));
    let mut bytes = b"\x1b[4h".to_vec();
    bytes.extend(repeated("a", 8 * 1024));
    timed_feed(&mut screen, &bytes);
}

#[test]
fn a_flood_of_screen_swaps_returns_quickly() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
    timed_feed(&mut screen, &repeated("\x1b[?1049h\x1b[?1049l", 8 * 1024));
    timed_feed(&mut screen, &repeated("\x1b[?3h", 8 * 1024));
}

#[test]
fn only_line_breaks_are_not_dropped_by_the_work_budget() {
    let mut screen = VtScreen::with_history(MAX_COLS, MAX_ROWS, 20_000);
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
fn a_recording_of_full_redraws_is_applied_in_full() {
    let mut screen = VtScreen::new(80, 24);
    let mut bytes = Vec::new();
    for frame in 0..200 {
        bytes.extend_from_slice(b"\x1b[2J\x1b[H");
        for row in 0..23 {
            bytes.extend_from_slice(format!("frame {frame} row {row:02}\x1b[K\r\n").as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[2K\x1b[L\x1b[M\x1b[24;1H");
    }
    screen.feed(&bytes);
    let lines = screen.bottom_lines(24);
    assert_eq!(lines[0], "frame 199 row 00");
    assert_eq!(lines[22], "frame 199 row 22");
}

/// Feeds `bytes` on another thread so an unbounded feed fails the test at the limit
/// instead of hanging the run.
fn feed_within_limit(mut screen: VtScreen, bytes: Vec<u8>) -> Result<VtScreen, Duration> {
    let (done_tx, done_rx) = std::sync::mpsc::channel();
    let start = Instant::now();
    std::thread::spawn(move || {
        let _ = screen.feed(&bytes);
        let _ = done_tx.send(screen);
    });
    done_rx.recv_timeout(LIMIT).map_err(|_| start.elapsed())
}

#[test]
fn a_flood_of_every_csi_final_with_the_largest_count_returns_quickly_without_tab_stops() {
    // The cursor sits in the middle row at the last column with no tab stop on the
    // line, where a count-driven loop in the engine has the most to walk.
    let setup = format!("\x1b[3g\x1b[{};{}H", MAX_ROWS / 2, MAX_COLS);
    let mut slow = Vec::new();
    for final_byte in 0x40u8..=0x7e {
        let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
        let _ = screen.feed(setup.as_bytes());
        let unit = format!("\x1b[65535{}", char::from(final_byte));
        if let Err(took) = feed_within_limit(screen, repeated(&unit, 8 * 1024)) {
            slow.push(format!("{:?} ({took:?}+)", char::from(final_byte)));
        }
    }
    assert!(slow.is_empty(), "feeds over {LIMIT:?}: {slow:?}");
}

#[test]
fn switching_a_mode_that_leaves_the_screen_alone_costs_no_work() {
    let mut screen = VtScreen::new(MAX_COLS, MAX_ROWS);
    let mut bytes = "\x1b[?1h\x1b[?1l".repeat(8).into_bytes();
    bytes.extend_from_slice(b"\x1b[?25l");
    screen.feed(&bytes);
    assert!(screen.cursor_hidden());
}

#[test]
fn once_the_work_budget_is_spent_row_edits_are_dropped_and_cursor_moves_still_apply() {
    let mut screen = VtScreen::new(10, 10);
    let mut bytes = "\x1b#8".repeat(50).into_bytes();
    bytes.extend_from_slice(b"\x1b[1;1H\x1b[P\x1b[4B\x1b[2C");
    screen.feed(&bytes);
    screen.feed(b"X");
    let lines = screen.bottom_lines(10);
    assert_eq!(lines[0], "EEEEEEEEEE", "the delete was dropped");
    assert_eq!(lines[4], "EEXEEEEEEE", "the moves applied");
}

#[test]
fn a_row_edit_is_dropped_when_less_than_a_row_of_work_is_left() {
    let (cols, rows) = (10usize, 10usize);
    let fills = 7;
    let mut bytes = "\x1b#8".repeat(fills).into_bytes();
    bytes.extend_from_slice(b"\x1b[Ha\x1b[H\x1b[P");
    // The work one feed may do (see `Model::feed`), less the fills and the character.
    let left = bytes.len() * cols + 4 * cols * rows - fills * cols * rows - 1;
    assert!((1..cols).contains(&left), "{left} cells of work left");

    let mut screen = VtScreen::new(10, 10);
    screen.feed(&bytes);
    assert_eq!(screen.bottom_lines(10)[0], "aEEEEEEEEE");
}

#[test]
fn one_feed_may_write_its_bytes_plus_two_screens_of_characters() {
    let mut screen = VtScreen::new(10, 10);
    // A repeat of nearly two screens from a few bytes, just within the allowance.
    screen.feed(b"a\x1b[188bZ");
    assert_eq!(screen.bottom_lines(1)[0], "aaaaaaaaaZ");
}

#[test]
fn one_feed_may_rewrite_four_screens_beyond_a_row_per_byte() {
    let mut screen = VtScreen::new(10, 10);
    screen.feed(b"\x1b[2J\x1b[2J\x1b[2J\x1b#8");
    assert!(
        screen
            .bottom_lines(10)
            .iter()
            .all(|line| line == "EEEEEEEEEE"),
        "{:?}",
        screen.bottom_lines(10)
    );
}
