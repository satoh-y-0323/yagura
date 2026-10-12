#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A view scrolled back into the history returns to the bottom when the user
//! types or pastes into the window (key, committed text, paste), the way a
//! terminal brings the live screen back on user input. New output, focus
//! changes, pointer events, the wheel and the view's own paging keys leave the
//! position where it is.

mod common;

use common::Harness;
use common::views::{
    CATCH_UP, assert_converges_to_model, numbered_lines, open_view_with_history,
    script_write_and_wait,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    CellPosition, ClientMessage, Key, KeyInput, Modifiers, NamedKey, PointerButton, PointerInput,
    PointerKind, TerminalSize, WheelInput,
};
use yagura_test_support::view::ViewClient;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const AT_CELL: CellPosition = CellPosition { col: 5, row: 5 };

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

fn pointer(kind: PointerKind, button: PointerButton) -> ClientMessage {
    ClientMessage::Pointer(PointerInput {
        kind,
        button,
        cell: AT_CELL,
        mods: Modifiers::default(),
        clicks: 1,
    })
}

/// How the view is moved back into the history before the input.
#[derive(Debug, Clone, Copy)]
enum ScrollBack {
    Wheel,
    Message,
}

async fn scroll_back(view: &mut ViewClient, how: ScrollBack) {
    match how {
        ScrollBack::Wheel => view.send_wheel(wheel(-5)).await,
        ScrollBack::Message => view.send(&ClientMessage::ScrollView { lines: 5 }).await,
    }
    view.pump_until("the view five rows back", CATCH_UP, |grid| {
        grid.view.display_offset == 5
    })
    .await;
    assert!(
        !view.grid.contains_text("HIST-FILL"),
        "the newest row is below the view now:\n{}",
        view.grid.dump()
    );
}

/// Scrolls the view back by `how`, sends `input`, and expects the next frames
/// to bring the view to the bottom, showing the model's screen.
async fn assert_input_brings_the_view_to_the_bottom(
    tag: &str,
    how: ScrollBack,
    input: ClientMessage,
) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let (window_id, mut view) = open_view_with_history(&h, SIZE, 100).await;
    scroll_back(&mut view, how).await;

    view.send(&input).await;
    let what = format!("the view at the bottom after {input:?} (scrolled back by {how:?})");
    view.pump_until(&what, CATCH_UP, |grid| grid.view.display_offset == 0)
        .await;
    assert_converges_to_model(&mut view, &h, &window_id, &what, CATCH_UP).await;
    assert!(
        view.grid.contains_text("HIST-FILL"),
        "the live screen is shown again:\n{}",
        view.grid.dump()
    );
}

#[tokio::test]
async fn a_key_brings_a_view_scrolled_back_by_the_wheel_to_the_bottom() {
    assert_input_brings_the_view_to_the_bottom(
        "view-input-key",
        ScrollBack::Wheel,
        ClientMessage::Key(KeyInput {
            key: Key::Char('k'),
            mods: Modifiers::default(),
        }),
    )
    .await;
}

#[tokio::test]
async fn committed_text_brings_a_view_scrolled_back_by_the_message_to_the_bottom() {
    assert_input_brings_the_view_to_the_bottom(
        "view-input-text",
        ScrollBack::Message,
        ClientMessage::Text {
            text: "typed".to_string(),
        },
    )
    .await;
}

#[tokio::test]
async fn a_paste_brings_a_view_scrolled_back_by_the_wheel_to_the_bottom() {
    assert_input_brings_the_view_to_the_bottom(
        "view-input-paste",
        ScrollBack::Wheel,
        ClientMessage::Paste {
            text: "pasted".to_string(),
        },
    )
    .await;
}

#[tokio::test]
async fn output_alone_keeps_a_view_scrolled_back_in_a_history_that_is_not_full() {
    let h = Harness::start("view-input-output-only", DaemonConfig::default()).await;
    let (window_id, mut view) = open_view_with_history(&h, SIZE, 100).await;
    scroll_back(&mut view, ScrollBack::Message).await;
    let before = view.grid.screen_text();

    let mut more = numbered_lines(101, 5);
    more.extend_from_slice(b"MORE-OUT");
    script_write_and_wait(&h, &window_id, &more, "MORE-OUT").await;

    // The answer to one more row back is built after the output is in the
    // model, so it shows where the view stood once the output arrived.
    view.send(&ClientMessage::ScrollView { lines: 1 }).await;
    let expected_before = before.clone();
    view.pump_until("one more row back", CATCH_UP, |grid| {
        grid.screen_text() != expected_before
    })
    .await;
    let now = view.grid.screen_text();
    for row in 1..now.len() {
        assert_eq!(
            now[row],
            before[row - 1],
            "row {row} should be the row {} of before; now:\n{now:#?}\nbefore:\n{before:#?}",
            row - 1
        );
    }
    assert!(
        !view.grid.contains_text("MORE-OUT"),
        "the view still looks at history, not at the new output:\n{}",
        view.grid.dump()
    );
}

#[tokio::test]
async fn focus_pointer_wheel_and_paging_keys_keep_a_view_scrolled_back() {
    let h = Harness::start("view-input-non-typing", DaemonConfig::default()).await;
    let (_window_id, mut view) = open_view_with_history(&h, SIZE, 100).await;

    view.send_key(shifted(NamedKey::PageUp)).await;
    view.pump_until("a page back", CATCH_UP, |grid| grid.view.display_offset > 0)
        .await;
    let page = view.grid.view.display_offset;
    view.send_key(shifted(NamedKey::PageDown)).await;
    view.pump_until("back at the bottom", CATCH_UP, |grid| {
        grid.view.display_offset == 0
    })
    .await;

    view.send(&ClientMessage::ScrollView { lines: 5 }).await;
    view.pump_until("five rows back", CATCH_UP, |grid| {
        grid.view.display_offset == 5
    })
    .await;

    view.send(&ClientMessage::Focus { focused: false }).await;
    view.send(&ClientMessage::Focus { focused: true }).await;
    view.send(&pointer(PointerKind::Move, PointerButton::None))
        .await;
    view.send(&pointer(PointerKind::Down, PointerButton::Left))
        .await;
    view.send(&pointer(PointerKind::Up, PointerButton::Left))
        .await;
    // Had anything above returned the view to the bottom, the wheel would end
    // two rows back instead of seven.
    view.send_wheel(wheel(-2)).await;
    view.pump_until(
        "seven rows back after focus, pointer and wheel",
        CATCH_UP,
        |grid| grid.view.display_offset == 7,
    )
    .await;

    view.send_key(shifted(NamedKey::PageUp)).await;
    view.pump_until("a page further back", CATCH_UP, |grid| {
        grid.view.display_offset == 7 + page
    })
    .await;
}
