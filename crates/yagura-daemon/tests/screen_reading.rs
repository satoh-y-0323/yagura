#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The daemon's reading of a screen whose model bounds the work a child's
//! output can ask for: what the reading shows after such output.

mod common;

mod work_bound {
    //! One `feed` of a few KB must return quickly even on the largest screen, whatever the
    //! child asks for: a few bytes can ask for a rewrite of the whole screen. "Quickly" is
    //! measured against a baseline feed in the same run, so the machine's speed cancels out.

    use std::time::{Duration, Instant};

    use super::common::CopiedScreen;

    const MAX_COLS: u16 = 1000;
    const MAX_ROWS: u16 = 1000;
    /// How many times the baseline one flood may take. Generous on purpose: a bounded flood
    /// spends at most the same work budget as the baseline, so it takes about as long, while
    /// an unbounded one rewrites the whole screen thousands of times.
    const BASELINE_FACTOR: u32 = 10;

    /// The baseline's edits: whole-screen fills and text that spend nearly all of one feed's
    /// work budget (a row of cells per byte plus four screens) without exceeding it. Fills,
    /// the costliest edit per cell, keep the baseline as slow as a bounded flood.
    const BASELINE_SCREEN_FILLS: usize = 12;
    const BASELINE_TEXT_BYTES: usize = 8150;

    /// How long a flood may take in this run: [`BASELINE_FACTOR`] times one feed on the
    /// largest screen that spends nearly the whole work budget on edits the budget allows.
    fn flood_bound() -> Duration {
        let mut bytes = "\x1b#8".repeat(BASELINE_SCREEN_FILLS).into_bytes();
        bytes.extend(repeated("a", BASELINE_TEXT_BYTES));
        bytes.extend_from_slice(b"\x1b[1;1HX\x1b[2;1HY\x1b[1;1H\x1b[2K");
        let mut screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
        let start = Instant::now();
        screen.feed(&bytes);
        let took = start.elapsed();
        let lines = screen.bottom_lines(usize::from(MAX_ROWS));
        assert!(
            !lines[0].contains('X') && lines[1].starts_with('Y'),
            "the baseline's last edits must still fit the work budget: {:?}",
            &lines[..2]
        );
        took * BASELINE_FACTOR
    }

    /// Feeds `bytes` on another thread so an unbounded feed fails the test at `bound`
    /// instead of hanging the run.
    fn feed_within(
        mut screen: CopiedScreen,
        bytes: Vec<u8>,
        bound: Duration,
    ) -> Result<CopiedScreen, ()> {
        let (done_tx, done_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = screen.feed(&bytes);
            let _ = done_tx.send(screen);
        });
        done_rx.recv_timeout(bound).map_err(|_| ())
    }

    fn timed_feed(screen: CopiedScreen, bytes: &[u8], bound: Duration) -> CopiedScreen {
        feed_within(screen, bytes.to_vec(), bound).unwrap_or_else(|()| {
            panic!(
                "a {} byte feed took over {bound:?}, {BASELINE_FACTOR} times the baseline",
                bytes.len()
            )
        })
    }

    fn repeated(unit: &str, total_bytes: usize) -> Vec<u8> {
        unit.repeat(total_bytes / unit.len()).into_bytes()
    }

    #[test]
    fn a_flood_of_screen_alignment_fills_returns_quickly_and_the_first_one_is_applied() {
        let bound = flood_bound();
        let screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
        let screen = timed_feed(screen, &repeated("\x1b#8", 8 * 1024), bound);
        let lines = screen.bottom_lines(usize::from(MAX_ROWS));
        assert!(lines[0].starts_with("EEEE"), "{:?}", &lines[0][..8]);
    }

    #[test]
    fn a_flood_of_screen_erases_returns_quickly() {
        let bound = flood_bound();
        let mut screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
        screen.feed(b"kept?");
        let screen = timed_feed(screen, &repeated("\x1b[2J", 8 * 1024), bound);
        assert!(screen.bottom_lines(1).iter().all(|l| !l.contains("kept")));
    }

    #[test]
    fn a_flood_of_line_erases_and_character_edits_returns_quickly() {
        let bound = flood_bound();
        let mut screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
        for unit in ["\x1b[2K", "\x1b[1024@", "\x1b[1024P", "\x1b[1024X"] {
            screen = timed_feed(screen, &repeated(unit, 8 * 1024), bound);
        }
    }

    #[test]
    fn a_flood_of_line_inserts_deletes_and_scrolls_returns_quickly() {
        let bound = flood_bound();
        let mut screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
        for unit in ["\x1b[512L", "\x1b[512M", "\x1b[512S", "\x1b[512T"] {
            screen = timed_feed(screen, &repeated(unit, 8 * 1024), bound);
        }
    }

    #[test]
    fn a_flood_of_reverse_index_and_insert_mode_text_returns_quickly() {
        let bound = flood_bound();
        let screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
        let screen = timed_feed(screen, &repeated("\x1bM", 8 * 1024), bound);
        let mut bytes = b"\x1b[4h".to_vec();
        bytes.extend(repeated("a", 8 * 1024));
        timed_feed(screen, &bytes, bound);
    }

    #[test]
    fn a_flood_of_screen_swaps_returns_quickly() {
        let bound = flood_bound();
        let screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
        let screen = timed_feed(screen, &repeated("\x1b[?1049h\x1b[?1049l", 8 * 1024), bound);
        timed_feed(screen, &repeated("\x1b[?3h", 8 * 1024), bound);
    }

    #[test]
    fn a_recording_of_full_redraws_is_applied_in_full() {
        let mut screen = CopiedScreen::new(80, 24);
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

    #[test]
    fn a_flood_of_every_csi_final_with_the_largest_count_returns_quickly_without_tab_stops() {
        // The cursor sits in the middle row at the last column with no tab stop on the
        // line, where a count-driven loop in the engine has the most to walk.
        let setup = format!("\x1b[3g\x1b[{};{}H", MAX_ROWS / 2, MAX_COLS);
        let bound = flood_bound();
        let mut slow = Vec::new();
        for final_byte in 0x40u8..=0x7e {
            let mut screen = CopiedScreen::new(MAX_COLS, MAX_ROWS);
            let _ = screen.feed(setup.as_bytes());
            let unit = format!("\x1b[65535{}", char::from(final_byte));
            if feed_within(screen, repeated(&unit, 8 * 1024), bound).is_err() {
                slow.push(format!("{:?}", char::from(final_byte)));
            }
        }
        assert!(
            slow.is_empty(),
            "feeds over {bound:?} ({BASELINE_FACTOR} times the baseline): {slow:?}"
        );
    }

    #[test]
    fn once_the_work_budget_is_spent_row_edits_are_dropped_and_cursor_moves_still_apply() {
        let mut screen = CopiedScreen::new(10, 10);
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

        let mut screen = CopiedScreen::new(10, 10);
        screen.feed(&bytes);
        assert_eq!(screen.bottom_lines(10)[0], "aEEEEEEEEE");
    }

    #[test]
    fn one_feed_may_write_its_bytes_plus_two_screens_of_characters() {
        let mut screen = CopiedScreen::new(10, 10);
        // A repeat of nearly two screens from a few bytes, just within the allowance.
        screen.feed(b"a\x1b[188bZ");
        assert_eq!(screen.bottom_lines(1)[0], "aaaaaaaaaZ");
    }

    #[test]
    fn one_feed_may_rewrite_four_screens_beyond_a_row_per_byte() {
        let mut screen = CopiedScreen::new(10, 10);
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
}

mod counted_reach {
    //! The cap the terminal model puts on every count must not change what a
    //! large count does: every CSI final with a large count is replayed on a bare
    //! engine (no cap) and on the model read through its copy, and the screen
    //! text and cursor must agree.

    use super::common::CopiedScreen;
    use alacritty_terminal::event::VoidListener;
    use alacritty_terminal::grid::Dimensions;
    use alacritty_terminal::index::{Column, Line};
    use alacritty_terminal::term::{Config, Term};
    use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};

    const COLS: u16 = 20;
    const ROWS: u16 = 10;

    /// The CSI final whose count is not capped at a reach: every repeat writes a
    /// character, so the feed's character budget bounds it instead.
    const REPEAT_FINAL: u8 = b'b';

    struct Size;

    impl Dimensions for Size {
        fn total_lines(&self) -> usize {
            usize::from(ROWS)
        }
        fn screen_lines(&self) -> usize {
            usize::from(ROWS)
        }
        fn columns(&self) -> usize {
            usize::from(COLS)
        }
    }

    /// Rows of the bare engine's screen, trailing blanks and blank rows removed.
    fn reference_rows(term: &Term<VoidListener>) -> Vec<String> {
        let mut rows: Vec<String> = (0..term.screen_lines())
            .map(|row| {
                let line = &term.grid()[Line(i32::try_from(row).unwrap())];
                let text: String = (0..term.columns()).map(|col| line[Column(col)].c).collect();
                text.trim_end().to_owned()
            })
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows
    }

    fn screen_rows(screen: &CopiedScreen) -> Vec<String> {
        let mut rows: Vec<String> = screen
            .bottom_lines(usize::from(ROWS))
            .into_iter()
            .map(|line| line.trim_end().to_owned())
            .collect();
        while rows.last().is_some_and(String::is_empty) {
            rows.pop();
        }
        rows
    }

    /// A distinct label on every row so scrolls and line edits show.
    fn labelled_rows() -> String {
        let mut out = String::new();
        for row in 0..ROWS {
            let letter = char::from(b'a' + u8::try_from(row).unwrap());
            out.push_str(&format!("\x1b[{};1H", row + 1));
            out.extend(std::iter::repeat_n(letter, usize::from(COLS) - 1));
        }
        out
    }

    struct Setup {
        origin: bool,
        region: Option<&'static str>,
        tabs_cleared: bool,
        start: (u16, u16),
    }

    fn script(setup: &Setup, op: &str) -> Vec<u8> {
        let mut text = labelled_rows();
        if setup.tabs_cleared {
            text.push_str("\x1b[3g");
        }
        if let Some(region) = setup.region {
            text.push_str(&format!("\x1b[{region}r"));
        }
        if setup.origin {
            text.push_str("\x1b[?6h");
        }
        text.push_str(&format!("\x1b[{};{}H", setup.start.0, setup.start.1));
        text.push_str(op);
        // Where the next character lands shows the cursor column on both sides.
        text.push('Z');
        text.into_bytes()
    }

    fn run_both(bytes: &[u8]) -> (Vec<String>, u16, Vec<String>, i32) {
        let mut screen = CopiedScreen::new(COLS, ROWS);
        screen.feed(bytes);

        let mut term = Term::new(Config::default(), &Size, VoidListener);
        let mut processor = Processor::<StdSyncHandler>::new();
        processor.advance(&mut term, bytes);

        (
            screen_rows(&screen),
            screen.cursor_row(),
            reference_rows(&term),
            term.grid().cursor.point.line.0,
        )
    }

    #[test]
    fn a_huge_count_leaves_the_same_screen_and_cursor_as_the_uncapped_engine() {
        let regions = [None, Some("4;8"), Some("1;5"), Some("6;10")];
        let starts = [(1, 1), (5, 10), (10, 20), (8, 3), (2, 19)];
        // Every CSI final, so each counted method is reached by whichever final
        // dispatches to it; a bare HT stands for `put_tab`, which has no CSI form.
        let mut ops: Vec<String> = (0x40u8..=0x7e)
            .filter(|f| *f != REPEAT_FINAL)
            .map(char::from)
            .flat_map(|f| {
                [
                    format!("\x1b[65535{f}"),
                    format!("\x1b[{}{f}", 2 * ROWS + 1),
                ]
            })
            .collect();
        ops.push("\t".to_owned());
        let mut cases = 0;
        let mut mismatches = Vec::new();
        for op in &ops {
            for origin in [false, true] {
                for region in regions {
                    for tabs_cleared in [false, true] {
                        for start in starts {
                            let setup = Setup {
                                origin,
                                region,
                                tabs_cleared,
                                start,
                            };
                            let bytes = script(&setup, op);
                            let (rows, row, want_rows, want_row) = run_both(&bytes);
                            cases += 1;
                            if rows != want_rows || i32::from(row) != want_row {
                                mismatches.push(format!(
                                    "{op:?} origin={origin} region={region:?} \
                                     tabs_cleared={tabs_cleared} start={start:?}: \
                                     got {rows:?} row {row}, want {want_rows:?} row {want_row}"
                                ));
                            }
                        }
                    }
                }
            }
        }
        assert!(cases > 1000, "the matrix must be exercised: {cases}");
        assert!(
            mismatches.is_empty(),
            "{} of {cases} differ, first: {:?}",
            mismatches.len(),
            mismatches.first()
        );
    }
}
