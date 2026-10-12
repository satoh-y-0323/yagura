#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Selecting and copying: the daemon decides, from the pointer events and the
//! window's mouse mode, whether a pointer drag is a report to the program or
//! a selection; selected cells come back in the frames as a flag, and
//! `copy_selection` answers with the text, wrapped lines joined and rows
//! separated by a line feed. Shift forces a selection while the program
//! tracks the mouse.

mod common;

use std::time::Duration;

use common::Harness;
use common::views::{CATCH_UP, open_script_window, script_write_and_wait};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    CellPosition, ClientMessage, Modifiers, PointerButton, PointerInput, PointerKind, TerminalSize,
};
use yagura_test_support::view::ViewClient;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";

fn bytes_of(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn at(col: u16, row: u16) -> CellPosition {
    CellPosition { col, row }
}

fn pointer(kind: PointerKind, cell: CellPosition, mods: Modifiers, clicks: u8) -> PointerInput {
    PointerInput {
        kind,
        button: PointerButton::Left,
        cell,
        mods,
        clicks,
    }
}

/// Presses at `from`, drags to `to` and releases there.
async fn drag(view: &mut ViewClient, from: CellPosition, to: CellPosition, mods: Modifiers) {
    view.send_pointer(pointer(PointerKind::Down, from, mods, 1))
        .await;
    view.send_pointer(pointer(PointerKind::Move, to, mods, 1))
        .await;
    view.send_pointer(pointer(PointerKind::Up, to, mods, 1))
        .await;
}

/// The rows:
///
/// ```text
/// 0  hello world
/// 1  0123456789 x 8            (80 cells, wraps)
/// 2  ABCDEFGHIJ
/// 3  tail line SEL-END
/// ```
async fn window_with_text(tag: &str, extra: &[u8]) -> (Harness, String, ViewClient) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;
    let wrapped = format!("{}ABCDEFGHIJ\r\n", "0123456789".repeat(8));
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[
            CLEAR,
            b"hello world\r\n",
            wrapped.as_bytes(),
            b"tail line SEL-END",
            extra,
        ]),
        "SEL-END",
    )
    .await;
    view.pump_until("the text", CATCH_UP, |grid| grid.contains_text("SEL-END"))
        .await;
    (h, window_id, view)
}

fn cells_of(row: u16, cols: std::ops::RangeInclusive<u16>) -> Vec<(u16, u16)> {
    cols.map(|col| (row, col)).collect()
}

#[tokio::test]
async fn dragging_inside_a_row_selects_those_cells_and_copies_their_text() {
    let (_h, _window_id, mut view) = window_with_text("sel-row", b"").await;
    assert_eq!(view.grid.row_text(1).len(), 80);
    assert!(view.grid.lines[1].wrapped, "row 1 goes on in row 2");

    drag(&mut view, at(0, 0), at(4, 0), Modifiers::default()).await;
    view.pump_until("the selection", CATCH_UP, |grid| {
        grid.selected_cells().len() == 5
    })
    .await;
    assert_eq!(view.grid.selected_cells(), cells_of(0, 0..=4));
    assert_eq!(view.copy_selection().await.as_deref(), Some("hello"));
}

#[tokio::test]
async fn a_selection_across_a_wrapped_line_copies_it_joined_without_a_line_break() {
    let (_h, _window_id, mut view) = window_with_text("sel-wrapped", b"").await;

    drag(&mut view, at(70, 1), at(4, 2), Modifiers::default()).await;
    view.pump_until("the selection", CATCH_UP, |grid| {
        grid.selected_cells().len() == 15
    })
    .await;
    let mut expected = cells_of(1, 70..=79);
    expected.extend(cells_of(2, 0..=4));
    assert_eq!(view.grid.selected_cells(), expected);
    assert_eq!(
        view.copy_selection().await.as_deref(),
        Some("0123456789ABCDE")
    );
}

#[tokio::test]
async fn a_selection_over_several_rows_separates_rows_with_a_line_feed() {
    let (_h, _window_id, mut view) = window_with_text("sel-rows", b"").await;

    drag(&mut view, at(6, 0), at(3, 3), Modifiers::default()).await;
    view.pump_until("the selection", CATCH_UP, |grid| {
        !grid.selected_cells().is_empty() && grid.selected_cells().last() == Some(&(3, 3))
    })
    .await;
    let expected = format!("world\n{}ABCDEFGHIJ\ntail", "0123456789".repeat(8));
    assert_eq!(
        view.copy_selection().await.as_deref(),
        Some(expected.as_str())
    );
}

#[tokio::test]
async fn double_click_selects_a_word_and_triple_click_a_line() {
    let (_h, _window_id, mut view) = window_with_text("sel-clicks", b"").await;

    view.send_pointer(pointer(
        PointerKind::Down,
        at(2, 0),
        Modifiers::default(),
        2,
    ))
    .await;
    view.send_pointer(pointer(PointerKind::Up, at(2, 0), Modifiers::default(), 2))
        .await;
    view.pump_until("the word", CATCH_UP, |grid| {
        grid.selected_cells().len() == 5
    })
    .await;
    assert_eq!(view.grid.selected_cells(), cells_of(0, 0..=4));
    assert_eq!(view.copy_selection().await.as_deref(), Some("hello"));

    view.send_pointer(pointer(
        PointerKind::Down,
        at(7, 0),
        Modifiers::default(),
        3,
    ))
    .await;
    view.send_pointer(pointer(PointerKind::Up, at(7, 0), Modifiers::default(), 3))
        .await;
    view.pump_until("the line", CATCH_UP, |grid| grid.selected_cells().len() > 5)
        .await;
    let copied = view.copy_selection().await.expect("a line is selected");
    assert_eq!(copied.trim_end_matches('\n'), "hello world");
}

#[tokio::test]
async fn nothing_is_selected_until_a_drag_makes_a_selection() {
    let (_h, _window_id, mut view) = window_with_text("sel-none", b"").await;
    assert!(view.grid.selected_cells().is_empty());
    assert_eq!(view.copy_selection().await, None);
}

#[tokio::test]
async fn a_drag_is_a_report_not_a_selection_while_the_program_tracks_the_mouse_and_shift_forces_one()
 {
    let (_h, _window_id, mut view) = window_with_text("sel-shift", b"\x1b[?1000h\x1b[?1006h").await;
    view.pump_until("the mouse-reporting state", CATCH_UP, |grid| {
        grid.view.mouse_reporting
    })
    .await;

    drag(&mut view, at(0, 0), at(4, 0), Modifiers::default()).await;
    view.assert_no_frame(
        "the drag went to the program, which tracks the mouse, and selected nothing",
        Duration::from_millis(300),
    )
    .await;
    assert!(view.grid.selected_cells().is_empty());
    assert_eq!(view.copy_selection().await, None);

    let shift = Modifiers {
        shift: true,
        ..Modifiers::default()
    };
    drag(&mut view, at(0, 0), at(4, 0), shift).await;
    view.pump_until("the forced selection", CATCH_UP, |grid| {
        grid.selected_cells().len() == 5
    })
    .await;
    assert_eq!(view.copy_selection().await.as_deref(), Some("hello"));
}

#[tokio::test]
async fn clearing_the_selection_removes_the_mark_from_the_next_frame_and_empties_the_copy() {
    let (_h, _window_id, mut view) = window_with_text("sel-clear", b"").await;

    drag(&mut view, at(0, 0), at(4, 0), Modifiers::default()).await;
    view.pump_until("the selection", CATCH_UP, |grid| {
        grid.selected_cells().len() == 5
    })
    .await;
    assert_eq!(view.copy_selection().await.as_deref(), Some("hello"));

    view.send(&ClientMessage::ClearSelection).await;
    view.pump_until("the mark gone", CATCH_UP, |grid| {
        grid.selected_cells().is_empty()
    })
    .await;
    assert_eq!(view.copy_selection().await, None);
}

#[tokio::test]
async fn clearing_the_selection_without_an_attachment_is_refused() {
    let h = Harness::start("sel-clear-detached", DaemonConfig::default()).await;
    let mut client = h.connect().await;
    common::send(&mut client, &ClientMessage::ClearSelection).await;
    common::expect_error(
        common::read_response(&mut client, Duration::from_secs(5)).await,
        "not_attached",
    );
}
