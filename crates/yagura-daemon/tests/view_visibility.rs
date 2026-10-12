#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A view the client reported as not on screen gets no frames while the window keeps
//! producing output, and gets the whole screen the moment it is reported on screen again.
//! The window's model, its status and `read_output` carry on regardless.
//!
//! The waits are for events; the assertions that nothing arrives ask the daemon a `ping`
//! first, so everything it had ready has been sent, and then watch a short grace period
//! that can only let a wrong daemon be caught.

mod common;

use std::time::Duration;

use common::Harness;
use common::views::{
    CATCH_UP, assert_converges_to_model, numbered_lines, open_script_window, script_write,
    script_write_and_wait,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, TerminalSize};
use yagura_test_support::view::ViewClient;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";
const GRACE: Duration = Duration::from_millis(300);

async fn settled_view(tag: &str) -> (Harness, String, ViewClient) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;
    script_write(&h, &window_id, CLEAR).await;
    view.pump_until("the cleared screen", CATCH_UP, |grid| {
        !grid.contains_text("SCRIPT-READY")
    })
    .await;
    (h, window_id, view)
}

/// Tells the daemon the view is hidden and waits until it has taken that in.
async fn hide(view: &mut ViewClient) {
    view.send(&ClientMessage::SetVisible { visible: false })
        .await;
    view.sync().await;
}

#[tokio::test]
async fn a_hidden_view_gets_no_frame_while_output_continues() {
    let (h, window_id, mut view) = settled_view("visibility-hidden").await;
    hide(&mut view).await;

    for burst in 1..=3usize {
        let mut bytes = numbered_lines(burst * 30, 30);
        bytes.extend_from_slice(format!("HIDDEN-{burst}").as_bytes());
        script_write_and_wait(&h, &window_id, &bytes, &format!("HIDDEN-{burst}")).await;
        view.assert_no_frame("the view is hidden", GRACE).await;
    }
}

#[tokio::test]
async fn a_hidden_view_gets_no_frame_for_a_change_of_the_cursor_or_the_screen_mode() {
    let (h, window_id, mut view) = settled_view("visibility-hidden-modes").await;
    hide(&mut view).await;

    script_write_and_wait(
        &h,
        &window_id,
        b"\x1b[?25l\x1b[?1049hALT-SHOWN",
        "ALT-SHOWN",
    )
    .await;
    view.assert_no_frame("the view is hidden", GRACE).await;
}

#[tokio::test]
async fn the_model_keeps_following_the_child_while_the_view_is_hidden() {
    let (h, window_id, mut view) = settled_view("visibility-model").await;
    hide(&mut view).await;

    script_write_and_wait(
        &h,
        &window_id,
        b"MODEL-STILL-FOLLOWS",
        "MODEL-STILL-FOLLOWS",
    )
    .await;
    let session = h
        .registry
        .get(&window_id)
        .expect("window")
        .session()
        .clone();
    let (text, _) = session
        .text_tail(24, 64 * 1024)
        .await
        .expect("the window's text is readable");
    assert!(text.contains("MODEL-STILL-FOLLOWS"), "read_output: {text}");
}

#[tokio::test]
async fn showing_the_view_again_sends_every_row_with_what_happened_meanwhile() {
    let (h, window_id, mut view) = settled_view("visibility-shown").await;
    hide(&mut view).await;
    let mut bytes = numbered_lines(1, 40);
    bytes.extend_from_slice(b"WHILE-HIDDEN");
    script_write_and_wait(&h, &window_id, &bytes, "WHILE-HIDDEN").await;
    view.assert_no_frame("the view is hidden", GRACE).await;

    view.send(&ClientMessage::SetVisible { visible: true })
        .await;
    let parts = view.next_complete_frame(CATCH_UP).await;
    let carried: usize = parts.iter().map(|part| part.lines.len()).sum();
    assert_eq!(carried, usize::from(SIZE.rows), "every row is sent");
    assert!(view.grid.contains_text("WHILE-HIDDEN"));

    let last = parts.last().expect("a frame has a part").seq;
    view.ack(last).await;
    assert_converges_to_model(
        &mut view,
        &h,
        &window_id,
        "after showing the view",
        CATCH_UP,
    )
    .await;
    view.assert_no_frame("everything has been sent and acknowledged", GRACE)
        .await;
}

#[tokio::test]
async fn a_shown_view_follows_output_again_after_it_was_hidden() {
    let (h, window_id, mut view) = settled_view("visibility-follows").await;
    hide(&mut view).await;
    script_write_and_wait(&h, &window_id, b"MID-HIDDEN", "MID-HIDDEN").await;
    view.send(&ClientMessage::SetVisible { visible: true })
        .await;
    view.pump_until("the screen after showing", CATCH_UP, |grid| {
        grid.contains_text("MID-HIDDEN")
    })
    .await;

    script_write_and_wait(&h, &window_id, b" AFTER-SHOWN", "AFTER-SHOWN").await;
    view.pump_until("output after showing", CATCH_UP, |grid| {
        grid.contains_text("MID-HIDDEN AFTER-SHOWN")
    })
    .await;
}
