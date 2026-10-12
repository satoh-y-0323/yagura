#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The view's scroll position: the wheel, Shift+PageUp / Shift+PageDown and
//! the `scroll_view` message move it, the frame's rows are then history, and
//! it holds still while new output arrives -- also when the window's history
//! is full and when the output wraps a wide character at the last column.
//! When the program asked for mouse reports, the wheel belongs to the
//! program and the view does not move.

mod common;

use common::Harness;
use common::views::{CATCH_UP, numbered_lines, open_script_window, script_write_and_wait};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    CellPosition, ClientMessage, Key, KeyInput, Modifiers, NamedKey, TerminalSize, WheelInput,
};
use yagura_test_support::view::ViewClient;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";
const AT_CELL: CellPosition = CellPosition { col: 5, row: 5 };

fn bytes_of(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

fn wheel(delta: i32) -> WheelInput {
    WheelInput {
        delta,
        cell: AT_CELL,
        mods: Modifiers::default(),
    }
}

fn shifted(key: NamedKey) -> KeyInput {
    KeyInput {
        key: Key::Named(key),
        mods: Modifiers {
            shift: true,
            ..Modifiers::default()
        },
    }
}

/// A script window that printed `lines` numbered lines, and a view of it.
async fn view_with_history(
    tag: &str,
    config: DaemonConfig,
    lines: usize,
) -> (Harness, String, ViewClient) {
    let h = Harness::start(tag, config).await;
    let window_id = open_script_window(&h).await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, &numbered_lines(1, lines), b"HIST-FILL"]),
        "HIST-FILL",
    )
    .await;
    view.pump_until("the filled screen", CATCH_UP, |grid| {
        grid.contains_text("HIST-FILL")
    })
    .await;
    (h, window_id, view)
}

/// Asserts that `now` shows the rows `before` showed, moved down by `by`
/// rows: the rows that fell off the bottom are gone, the top `by` rows are
/// older ones.
fn assert_shifted_down(before: &[String], now: &[String], by: usize) {
    assert_eq!(before.len(), now.len());
    for row in by..now.len() {
        assert_eq!(
            now[row],
            before[row - by],
            "row {row} should be the row {} of before; now:\n{now:#?}\nbefore:\n{before:#?}",
            row - by
        );
    }
}

#[tokio::test]
async fn the_wheel_moves_the_view_into_history_and_back() {
    let (_h, _window_id, mut view) =
        view_with_history("view-wheel", DaemonConfig::default(), 100).await;
    let live = view.grid.screen_text();
    assert_eq!(view.grid.view.display_offset, 0);
    assert!(view.grid.view.history_rows > 0);

    view.send_wheel(wheel(-3)).await;
    view.pump_until("the view three rows back", CATCH_UP, |grid| {
        grid.view.display_offset == 3
    })
    .await;
    assert_shifted_down(&live, &view.grid.screen_text(), 3);
    assert!(
        !view.grid.contains_text("HIST-FILL"),
        "the newest row is below the view now:\n{}",
        view.grid.dump()
    );

    view.send_wheel(wheel(3)).await;
    view.pump_until("the view back at the bottom", CATCH_UP, |grid| {
        grid.view.display_offset == 0
    })
    .await;
    assert_eq!(view.grid.screen_text(), live);
}

#[tokio::test]
async fn shift_page_up_and_shift_page_down_page_the_view() {
    let (_h, _window_id, mut view) =
        view_with_history("view-page", DaemonConfig::default(), 100).await;
    let live = view.grid.screen_text();

    view.send_key(shifted(NamedKey::PageUp)).await;
    view.pump_until("a page back", CATCH_UP, |grid| grid.view.display_offset > 0)
        .await;
    let offset = view.grid.view.display_offset as usize;
    assert!(
        offset <= usize::from(SIZE.rows),
        "one page is at most the screen height, was {offset}"
    );
    assert_shifted_down(&live, &view.grid.screen_text(), offset);

    view.send_key(shifted(NamedKey::PageDown)).await;
    view.pump_until("back at the bottom", CATCH_UP, |grid| {
        grid.view.display_offset == 0
    })
    .await;
    assert_eq!(view.grid.screen_text(), live);
}

#[tokio::test]
async fn the_scroll_view_message_moves_the_view_and_is_clamped_to_the_history() {
    let (_h, _window_id, mut view) =
        view_with_history("view-scroll-message", DaemonConfig::default(), 100).await;
    let history = view.grid.view.history_rows;

    view.send(&ClientMessage::ScrollView { lines: 5 }).await;
    view.pump_until("five rows back", CATCH_UP, |grid| {
        grid.view.display_offset == 5
    })
    .await;

    view.send(&ClientMessage::ScrollView { lines: i32::MAX })
        .await;
    view.pump_until("the top of the history", CATCH_UP, |grid| {
        grid.view.display_offset == history
    })
    .await;

    view.send(&ClientMessage::ScrollView { lines: i32::MIN })
        .await;
    view.pump_until("the bottom", CATCH_UP, |grid| grid.view.display_offset == 0)
        .await;
}

#[tokio::test]
async fn a_wheel_the_program_asked_to_receive_does_not_move_the_view() {
    let (h, window_id, mut view) =
        view_with_history("view-wheel-reported", DaemonConfig::default(), 100).await;

    script_write_and_wait(
        &h,
        &window_id,
        b"\x1b[?1000h\x1b[?1006hTRACKING",
        "TRACKING",
    )
    .await;
    view.pump_until("the mouse-reporting state", CATCH_UP, |grid| {
        grid.view.mouse_reporting
    })
    .await;

    view.send_wheel(wheel(-3)).await;
    view.assert_no_frame(
        "the wheel was reported to the program, which owns it, and the view did not move",
        std::time::Duration::from_millis(300),
    )
    .await;
    assert_eq!(view.grid.view.display_offset, 0);
}

#[tokio::test]
async fn a_view_scrolled_back_in_a_full_history_keeps_showing_the_same_rows_as_output_arrives() {
    let config = DaemonConfig {
        read_output_max_lines: 60,
        ..DaemonConfig::default()
    };
    let (h, window_id, mut view) = view_with_history("view-full-history", config, 150).await;

    // More output; the history was already full, so it stays the same size.
    let full = view.grid.view.history_rows;
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[&numbered_lines(151, 30), b"HIST-FILL-2"]),
        "HIST-FILL-2",
    )
    .await;
    view.pump_until("the second fill", CATCH_UP, |grid| {
        grid.contains_text("HIST-FILL-2")
    })
    .await;
    assert!(full > 0);
    assert_eq!(
        view.grid.view.history_rows, full,
        "the history is full, so it did not grow"
    );

    view.send(&ClientMessage::ScrollView { lines: 10 }).await;
    view.pump_until("ten rows back", CATCH_UP, |grid| {
        grid.view.display_offset == 10
    })
    .await;
    let before = view.grid.screen_text();

    // A line whose wide last character cannot fit the last column and wraps
    // (the engine scrolls without a line feed), then ordinary lines.
    let wrapping = format!("{}\u{5168}\r\n", "y".repeat(79));
    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[wrapping.as_bytes(), &numbered_lines(181, 5), b"HIST-AFTER"]),
        "HIST-AFTER",
    )
    .await;

    // Ask for one more row of history: the answer is built after the output
    // above is in the model, so it shows where the view really stood.
    view.send(&ClientMessage::ScrollView { lines: 1 }).await;
    let expected_before = before.clone();
    view.pump_until("one more row back", CATCH_UP, |grid| {
        grid.screen_text() != expected_before
    })
    .await;
    assert_shifted_down(&before, &view.grid.screen_text(), 1);
    assert!(
        view.grid.view.display_offset > 10,
        "the position moved with the output, was {}",
        view.grid.view.display_offset
    );
    assert!(
        !view.grid.contains_text("HIST-AFTER"),
        "the view still looks at history, not at the new output:\n{}",
        view.grid.dump()
    );
}
