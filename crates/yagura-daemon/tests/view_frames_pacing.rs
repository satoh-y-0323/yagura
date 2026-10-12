#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! When the daemon sends the next frame: not while the last one is
//! unacknowledged (the newest state goes out at the acknowledgement, not the
//! states in between), not while the program holds a synchronized update
//! (`CSI ? 2026 h`) -- and always, at attach and re-attach, the whole screen.
//!
//! Every wait is for an event (the model showing a marker, a frame
//! arriving). The assertions that nothing arrives ask the daemon a `ping` and
//! then watch a short grace period; a daemon that is right never sends, so
//! the grace period can only let a wrong one be caught, never fail a right one.

mod common;

use std::time::Duration;

use common::Harness;
use common::views::{
    CATCH_UP, grid_differs_from_model, model_screen, numbered_lines, open_script_window,
    script_write, script_write_and_wait,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::TerminalSize;
use yagura_test_support::view::ViewClient;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";
const GRACE: Duration = Duration::from_millis(300);

fn bytes_of(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// A script window with its screen cleared, and a view attached at 80x24 that
/// has applied and acknowledged everything so far.
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

#[tokio::test]
async fn output_while_a_frame_is_unacknowledged_goes_out_as_one_latest_state_at_the_acknowledgement()
 {
    let h = Harness::start("view-ack", DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let mut view =
        ViewClient::attach_unacknowledged(h.connect().await, &window_id, Some(SIZE)).await;
    let first = view.grid.last_seq.expect("the first frame completed");

    script_write(&h, &window_id, CLEAR).await;
    for burst in 1..=3usize {
        let mut bytes = numbered_lines(burst * 60, 60);
        bytes.extend_from_slice(format!("BURST-{burst}").as_bytes());
        script_write(&h, &window_id, &bytes).await;
    }
    common::wait_until_model_shows(&h, &window_id, b"BURST-3", CATCH_UP).await;
    view.assert_no_frame("the first frame is still unacknowledged", GRACE)
        .await;

    view.ack(first).await;
    let parts = view.next_complete_frame(CATCH_UP).await;
    let model = model_screen(&h, &window_id).await;
    assert_eq!(
        grid_differs_from_model(&view.grid, &model),
        None,
        "the frame sent at the acknowledgement is the newest state, not one in between"
    );
    assert!(view.grid.contains_text("BURST-3"));
    assert!(
        !view.grid.contains_text("BURST-1"),
        "the states in between were not sent:\n{}",
        view.grid.dump()
    );
    assert_eq!(
        view.completed.len(),
        2,
        "the first frame and one more, nothing in between"
    );

    let last = parts.last().expect("a frame has a part").seq;
    view.ack(last).await;
    view.assert_no_frame("everything has been sent and acknowledged", GRACE)
        .await;
}

#[tokio::test]
async fn a_synchronized_update_sends_no_frame_until_it_ends() {
    let (h, window_id, mut view) = settled_view("view-sync-end").await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026hHELD-ONE", "HELD-ONE").await;
    view.assert_no_frame("a synchronized update is open", GRACE)
        .await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026l END-S", "END-S").await;
    view.pump_until("the frame after the update ended", CATCH_UP, |grid| {
        grid.contains_text("HELD-ONE END-S")
    })
    .await;
}

#[tokio::test]
async fn input_to_the_window_ends_a_synchronized_update() {
    let (h, window_id, mut view) = settled_view("view-sync-input").await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026hHELD-TWO", "HELD-TWO").await;
    view.assert_no_frame("a synchronized update is open", GRACE)
        .await;

    view.send_text("x").await;
    view.pump_until("the frame after the input", CATCH_UP, |grid| {
        grid.contains_text("HELD-TWO")
    })
    .await;
}

#[tokio::test]
async fn the_childs_exit_ends_a_synchronized_update() {
    let (h, window_id, mut view) = settled_view("view-sync-exit").await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026hHELD-THREE", "HELD-THREE").await;
    view.assert_no_frame("a synchronized update is open", GRACE)
        .await;

    h.write_input(&window_id, b"!").await;
    view.pump_until("the frame after the exit", CATCH_UP, |grid| {
        grid.contains_text("HELD-THREE")
    })
    .await;
}

#[tokio::test]
async fn a_synchronized_update_that_grows_past_the_byte_bound_is_shown_without_ending() {
    let h = Harness::start("view-sync-bound", DaemonConfig::default()).await;
    let window_id = h
        .open(
            vec![
                yagura_test_support::process::term_script_path()
                    .to_str()
                    .unwrap()
                    .to_string(),
                "sync-flood".to_string(),
                (1024 * 1024).to_string(),
            ],
            None,
        )
        .await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;

    // The flood starts at the first input byte and never writes `CSI ? 2026 l`.
    view.send_text("g").await;
    view.pump_until("a frame while the update is still open", CATCH_UP, |grid| {
        grid.contains_text("xxxxxxxxxx")
    })
    .await;

    // The frame came from the byte bound, not from the child ending the update.
    view.reattach_unacknowledged(&window_id, None).await;
    assert!(!view.child_exited, "the flooding child is still alive");
}

#[tokio::test]
async fn attach_sends_the_whole_screen_while_a_frame_is_unacknowledged_and_an_update_is_held() {
    let h = Harness::start("view-attach-held", DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    // The first connection never acknowledges its first frame.
    let _unacknowledged =
        ViewClient::attach_unacknowledged(h.connect().await, &window_id, Some(SIZE)).await;

    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, b"\x1b[?2026hHELD-FOUR"]),
        "HELD-FOUR",
    )
    .await;

    let second = ViewClient::attach_unacknowledged(h.connect().await, &window_id, None).await;
    let model = model_screen(&h, &window_id).await;
    assert_eq!(
        grid_differs_from_model(&second.grid, &model),
        None,
        "the attach frame is the model's whole screen, held update included"
    );
    assert!(second.grid.contains_text("HELD-FOUR"));
}

#[tokio::test]
async fn re_attaching_on_the_same_connection_sends_the_whole_screen_again() {
    let h = Harness::start("view-reattach", DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let mut view =
        ViewClient::attach_unacknowledged(h.connect().await, &window_id, Some(SIZE)).await;

    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, b"AFTER-ONE"]),
        "AFTER-ONE",
    )
    .await;
    view.assert_no_frame("the first frame is unacknowledged", GRACE)
        .await;

    view.reattach_unacknowledged(&window_id, None).await;
    assert!(
        view.grid.contains_text("AFTER-ONE"),
        "the attach frame shows the current screen:\n{}",
        view.grid.dump()
    );
    let model = model_screen(&h, &window_id).await;
    assert_eq!(grid_differs_from_model(&view.grid, &model), None);
}

#[tokio::test]
async fn attaching_to_an_exited_child_reports_it_and_still_sends_the_screen() {
    let h = Harness::start("view-exited", DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    script_write_and_wait(&h, &window_id, &bytes_of(&[CLEAR, b"BYE-TEXT"]), "BYE-TEXT").await;
    h.write_input(&window_id, b"!").await;
    h.wait_until_child_exited(&window_id, CATCH_UP).await;

    let view = ViewClient::attach_unacknowledged(h.connect().await, &window_id, None).await;
    assert!(view.child_exited, "the answer says the child has exited");
    assert_eq!(view.exit_code, Some(0));
    assert!(
        view.grid.contains_text("BYE-TEXT"),
        "the last screen is still sent:\n{}",
        view.grid.dump()
    );
}
