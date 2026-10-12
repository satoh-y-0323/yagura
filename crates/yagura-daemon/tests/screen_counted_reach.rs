#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The cap `VtScreen` puts on every count must not change what a large count
//! does: every CSI final with a large count is replayed on a bare engine (no
//! cap) and on `VtScreen`, and the screen text and cursor must agree.

use alacritty_terminal::event::VoidListener;
use alacritty_terminal::grid::Dimensions;
use alacritty_terminal::index::{Column, Line};
use alacritty_terminal::term::{Config, Term};
use alacritty_terminal::vte::ansi::{Processor, StdSyncHandler};
use yagura_daemon::detector::VtScreen;

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

fn screen_rows(screen: &VtScreen) -> Vec<String> {
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
    let mut screen = VtScreen::new(COLS, ROWS);
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
