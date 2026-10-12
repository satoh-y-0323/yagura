#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A view's frames read straight off a screen, applied the way a renderer
//! applies them (`yagura_test_support::frame::Grid`): which rows a frame that
//! rebuilds the grid leaves out, that a change outside the rows still sends a
//! frame, the cursor waiting to wrap, and a selection made while the view looks
//! into the history.

use yagura_daemon::detector::VtScreen;
use yagura_daemon::detector::screen_view::{self, View};
use yagura_protocol::{
    CellColor, CellPosition, Modifiers, PointerButton, PointerInput, PointerKind,
};
use yagura_test_support::frame::Grid;

const COLS: u16 = 80;
const ROWS: u16 = 24;

/// A client of one view: the frames it was sent, applied and acknowledged.
struct Client {
    view: View,
    grid: Grid,
}

impl Client {
    /// Attaches to `screen`: the first frame rebuilds the whole grid.
    fn attach(screen: &mut VtScreen) -> Self {
        let mut client = Self {
            view: View::new(),
            grid: Grid::new(),
        };
        assert!(client.take(screen, true), "an attach always sends a frame");
        client
    }

    /// Takes the next frame, if the view sends one, and acknowledges it.
    fn take(&mut self, screen: &mut VtScreen, full: bool) -> bool {
        let Some(capture) = self.view.capture(screen, false, full) else {
            return false;
        };
        for part in capture.into_parts() {
            if self.grid.apply(&part).unwrap() {
                self.view.acknowledge(part.seq);
            }
        }
        true
    }
}

#[test]
fn a_frame_that_rebuilds_the_grid_leaves_out_only_rows_that_are_truly_blank() {
    let mut screen = VtScreen::new(COLS, ROWS);
    let full_row = "x".repeat(usize::from(COLS));
    screen.feed(format!("{full_row}\r\n\x1b[44m\x1b[2K\x1b[0m\r\nend").as_bytes());

    let client = Client::attach(&mut screen);

    assert_eq!(client.grid.row_text(0), full_row, "{}", client.grid.dump());
    for col in 0..COLS {
        assert_eq!(
            client.grid.cell(1, col).attr.bg,
            CellColor::Indexed(4),
            "column {col} of the erased row"
        );
    }
}

#[test]
fn moving_only_the_cursor_sends_a_frame() {
    let mut screen = VtScreen::new(COLS, ROWS);
    screen.feed(b"still");
    let mut client = Client::attach(&mut screen);

    screen.feed(b"\x1b[5;10H");
    assert!(client.take(&mut screen, false));
    assert_eq!((client.grid.cursor.row, client.grid.cursor.col), (4, 9));
}

#[test]
fn turning_any_mouse_tracking_mode_on_or_off_alone_sends_a_frame() {
    let mut screen = VtScreen::new(COLS, ROWS);
    let mut client = Client::attach(&mut screen);

    for mode in [1000, 1002, 1003] {
        screen.feed(format!("\x1b[?{mode}h").as_bytes());
        assert!(client.take(&mut screen, false), "mode {mode} set");
        assert!(client.grid.view.mouse_reporting, "mode {mode} set");

        screen.feed(format!("\x1b[?{mode}l").as_bytes());
        assert!(client.take(&mut screen, false), "mode {mode} reset");
        assert!(!client.grid.view.mouse_reporting, "mode {mode} reset");
    }
}

#[test]
fn a_cursor_waiting_to_wrap_is_one_past_the_last_column() {
    let mut screen = VtScreen::new(COLS, ROWS);
    screen.feed("y".repeat(usize::from(COLS)).as_bytes());

    let client = Client::attach(&mut screen);

    assert_eq!((client.grid.cursor.row, client.grid.cursor.col), (0, COLS));
}

fn press(kind: PointerKind, col: u16, row: u16) -> PointerInput {
    PointerInput {
        kind,
        button: PointerButton::Left,
        cell: CellPosition { col, row },
        mods: Modifiers::default(),
        clicks: 1,
    }
}

#[test]
fn a_selection_made_while_the_view_looks_into_the_history_marks_the_rows_shown() {
    let mut screen = VtScreen::with_history(COLS, ROWS, 100);
    let lines: String = (0..60).map(|n| format!("{n:03} history\r\n")).collect();
    screen.feed(lines.as_bytes());
    screen_view::scroll_view(&mut screen, 5);
    let mut client = Client::attach(&mut screen);
    assert_eq!(client.grid.view.display_offset, 5);
    let shown = client.grid.row_text(0);

    screen_view::select(&mut screen, &press(PointerKind::Down, 0, 0));
    screen_view::select(&mut screen, &press(PointerKind::Move, 8, 0));
    assert!(client.take(&mut screen, false));

    let first_nine: Vec<(u16, u16)> = (0..9).map(|col| (0, col)).collect();
    assert_eq!(client.grid.selected_cells(), first_nine);
    assert_eq!(
        screen_view::selected_text(&mut screen),
        Some(shown[..9].to_string())
    );
}
