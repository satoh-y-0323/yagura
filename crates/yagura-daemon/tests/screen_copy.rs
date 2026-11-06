#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The copy of a window's screen that the daemon reads: carried from the
//! terminal model as a full copy followed by differences, checked before it
//! is applied.

use yagura_protocol::{MAX_ZEROWIDTH_PER_CELL, ScreenCopy, ScreenPush, ScreenRows};
use yagura_term::{PtyHost, VtScreen};

const HOST: PtyHost = PtyHost {
    requests_win32_input: false,
};

const ACUTE: char = '\u{301}';

/// One window's copy as the daemon holds it, fed from one model through one
/// projection, the way a window's link carries it.
struct Carried {
    screen: VtScreen,
    projector: yagura_term::Projector,
    held: yagura_daemon::detector::HeldCopy,
}

impl Carried {
    fn new(cols: u16, rows: u16) -> Self {
        Self {
            screen: VtScreen::new(cols, rows, HOST),
            projector: yagura_term::Projector::new(),
            held: yagura_daemon::detector::HeldCopy::new(),
        }
    }

    /// Takes in one chunk of output the way the feeder does.
    fn feed(&mut self, bytes: &[u8]) {
        self.screen.feed(bytes);
        self.screen.note_output();
    }

    fn next(&mut self) -> (Vec<ScreenRows>, ScreenPush) {
        self.projector.project(self.screen.screen_copy())
    }

    fn carry(&mut self) -> ScreenPush {
        let (rows, push) = self.next();
        if let Err(err) = self.held.apply(rows, push.clone()) {
            panic!("a copy the projection made was refused: {err:?}");
        }
        push
    }

    fn held(&self) -> ScreenCopy {
        match self.held.copy() {
            Some(copy) => copy.clone(),
            None => panic!("no copy is held"),
        }
    }
}

fn line_text(copy: &ScreenCopy, line: usize) -> String {
    copy.lines
        .iter()
        .find(|l| usize::from(l.line) == line)
        .map(|l| l.cells.iter().map(|cell| cell.c).collect())
        .unwrap_or_default()
}

fn cell_columns(copy: &ScreenCopy, line: usize) -> Vec<usize> {
    copy.lines
        .iter()
        .find(|l| usize::from(l.line) == line)
        .map(|l| l.cells.iter().map(|cell| usize::from(cell.col)).collect())
        .unwrap_or_default()
}

enum Step {
    Output(Vec<u8>),
    Resize(u16, u16),
}

fn out(bytes: &[u8]) -> Step {
    Step::Output(bytes.to_vec())
}

#[test]
fn differences_applied_in_order_rebuild_the_models_copy() {
    let mut marks = String::from("e");
    for _ in 0..MAX_ZEROWIDTH_PER_CELL + 3 {
        marks.push(ACUTE);
    }
    let steps = [
        out(b"wrapping past the edge"),
        out(b"\r\n1\r\n2\r\n3\r\n4\r\n5\r\n6\r\nlast"),
        out(b"\x1b[?1049h\x1b[HALT"),
        out(b"\x1b[?1049l"),
        out(format!("\r\n\u{3042}\u{3044}{marks}").as_bytes()),
        out(b"\x1b[?25l"),
        out(b"\x1b[?2026hheld"),
        out(b"\x1b[?1h\x1b[?2004h\x1b[?1000h\x1b[?1006h\x1b[?1004h"),
        Step::Resize(14, 5),
        out(b"\x1b[H\x1b[2Jtop"),
        out(b"\x1b[?2026l\x1b[?25h"),
    ];

    let mut carried = Carried::new(10, 4);
    carried.carry();
    assert_eq!(carried.held(), carried.screen.screen_copy());

    for (index, step) in steps.iter().enumerate() {
        match step {
            Step::Output(bytes) => carried.feed(bytes),
            Step::Resize(cols, rows) => {
                carried.screen.resize(*cols, *rows);
            }
        }
        carried.carry();
        assert_eq!(
            carried.held(),
            carried.screen.screen_copy(),
            "the carried copy parts from the model after step {index}"
        );
        match index {
            0 => {
                assert_eq!(line_text(&carried.held(), 0), "wrapping p");
                assert_eq!(line_text(&carried.held(), 1), "ast the ed");
            }
            2 => assert_eq!(line_text(&carried.held(), 0), "ALT"),
            9 => assert_eq!(line_text(&carried.held(), 0), "top"),
            _ => {}
        }
    }
}

#[test]
fn output_that_leaves_the_screen_as_it_was_still_advances_outputs_fed() {
    let mut carried = Carried::new(10, 4);
    carried.feed(b"x");
    let before = carried.carry();
    let screen_before = carried.held();

    carried.feed(b"\x1b[0m");
    let after = carried.carry();

    assert!(
        after.outputs_fed > before.outputs_fed,
        "the copy after an unchanged chunk keeps outputs_fed at {}",
        before.outputs_fed
    );
    assert!(carried.held().outputs_fed > screen_before.outputs_fed);
}

#[test]
fn a_written_blank_is_carried_and_unwritten_cells_and_spacers_are_not() {
    let mut carried = Carried::new(20, 3);
    carried.feed("a b\x1b[3Cc\r\n\u{3042}x".as_bytes());
    carried.carry();
    let copy = carried.held();

    assert_eq!(cell_columns(&copy, 0), vec![0, 1, 2, 6]);
    assert_eq!(line_text(&copy, 0), "a bc");
    assert_eq!(cell_columns(&copy, 1), vec![0, 2]);
    assert_eq!(cell_columns(&copy, 2), Vec::<usize>::new());
}

/// A carried window with one copy applied, and the next copy the projection
/// made after `then` was fed, not yet applied.
fn held_then_next(then: &[u8]) -> (Carried, ScreenCopy, Vec<ScreenRows>, ScreenPush) {
    let mut carried = Carried::new(20, 4);
    carried.feed(b"first");
    carried.carry();
    let held = carried.held();
    carried.feed(then);
    let (rows, push) = carried.next();
    (carried, held, rows, push)
}

fn assert_refused(
    carried: &mut Carried,
    held: &ScreenCopy,
    rows: Vec<ScreenRows>,
    push: ScreenPush,
    what: &str,
) {
    assert!(
        carried.held.apply(rows, push).is_err(),
        "a copy with {what} was applied"
    );
    assert_eq!(
        &carried.held(),
        held,
        "a refused copy with {what} changed the held copy"
    );
}

#[test]
fn a_copy_sized_past_the_limit_is_refused_and_the_held_copy_stays() {
    let (mut carried, held, rows, mut push) = held_then_next(b"\r\nsecond");
    push.cols = yagura_protocol::MAX_WINDOW_DIMENSION + 1;
    assert_refused(&mut carried, &held, rows, push, "a size past the limit");
}

#[test]
fn a_line_outside_the_size_is_refused_and_the_held_copy_stays() {
    let (mut carried, held, mut rows, push) = held_then_next(b"\r\nsecond");
    let Some(line) = rows.iter_mut().flat_map(|r| r.lines.iter_mut()).next() else {
        panic!("the copy carries no line");
    };
    line.line = 999;
    assert_refused(&mut carried, &held, rows, push, "a line outside the size");
}

#[test]
fn a_cell_outside_the_size_is_refused_and_the_held_copy_stays() {
    let (mut carried, held, mut rows, push) = held_then_next(b"\r\nsecond");
    let Some(cell) = rows
        .iter_mut()
        .flat_map(|r| r.lines.iter_mut())
        .flat_map(|l| l.cells.iter_mut())
        .next()
    else {
        panic!("the copy carries no cell");
    };
    cell.col = 999;
    assert_refused(&mut carried, &held, rows, push, "a cell outside the size");
}

#[test]
fn a_cell_past_the_combining_limit_is_refused_and_the_held_copy_stays() {
    let mut marks = String::from("\r\ne");
    for _ in 0..MAX_ZEROWIDTH_PER_CELL {
        marks.push(ACUTE);
    }
    let (mut carried, held, mut rows, push) = held_then_next(marks.as_bytes());
    let Some(cell) = rows
        .iter_mut()
        .flat_map(|r| r.lines.iter_mut())
        .flat_map(|l| l.cells.iter_mut())
        .find(|cell| cell.c == 'e')
    else {
        panic!("the copy carries no marked cell");
    };
    cell.zerowidth.push(ACUTE);
    assert_refused(&mut carried, &held, rows, push, "too many combining marks");
}

#[test]
fn a_copy_that_skips_a_number_is_refused_and_the_held_copy_stays() {
    let (mut carried, held, _skipped_rows, _skipped_push) = held_then_next(b"\r\nsecond");
    carried.feed(b"\r\nthird");
    let (rows, push) = carried.next();
    assert_refused(&mut carried, &held, rows, push, "a skipped number");
}
