#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! When the daemon sends the next frame: not while the last one is
//! unacknowledged (the newest state goes out at the acknowledgement, not the
//! states in between), not while the program holds a synchronized update
//! (`CSI ? 2026 h`) -- and always, at attach and re-attach, the whole screen.
//!
//! Every wait is for an event (the model showing a marker, a frame
//! arriving). That nothing arrives is judged by order, not by a quiet spell:
//! no frame may come before the `pong` to a `ping`, which the daemon queues
//! after the frames for everything the attached window's feeder had received,
//! a synchronized update holding the view included.

use crate::common;
use crate::common::bins;

use common::Harness;
use common::views::{
    grid_differs_from_model, model_screen, numbered_lines, open_script_window,
    open_view_with_history, script_command, script_write, script_write_and_wait,
};
use tokio::io::AsyncWriteExt;
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage, TerminalSize};
use yagura_test_support::frame::Grid;
use yagura_test_support::ipc::{Connection, ReadOutcome, read_daemon_frame, send_client_message};
use yagura_test_support::view::ViewClient;

const SIZE: TerminalSize = TerminalSize { cols: 80, rows: 24 };
const CLEAR: &[u8] = b"\x1b[H\x1b[2J";

fn bytes_of(parts: &[&[u8]]) -> Vec<u8> {
    parts.concat()
}

/// Writes `msg` on a connection the daemon may already have closed.
async fn send_even_if_closed(conn: &mut Connection, msg: &ClientMessage) {
    let frame = yagura_protocol::frame_encode(&yagura_protocol::encode_client_message(msg));
    let _ = conn.write_all(&frame).await;
}

/// Attaches on `conn` and reads the answer strictly in arrival order:
/// `Attached`, the parts of one whole-screen frame, then `StatusChanged`.
/// Returns the grid and the frame's number, unacknowledged.
async fn attach_in_order(conn: &mut Connection, window_id: &str) -> (Grid, u64) {
    send_client_message(
        conn,
        &ClientMessage::AttachView {
            window_id: window_id.to_string(),
            size: None,
        },
    )
    .await;
    let first = read_daemon_frame(conn)
        .await
        .expect_message("the attach answer");
    assert!(
        matches!(first, DaemonMessage::Attached { .. }),
        "the first message after attach_view is `attached`, got {first:?}"
    );
    let mut grid = Grid::new();
    let mut parts = 0usize;
    let seq = loop {
        match read_daemon_frame(conn)
            .await
            .expect_message("the attach frame")
        {
            DaemonMessage::Frame(part) => {
                parts += 1;
                assert!(
                    parts > 1 || part.reset,
                    "the attach frame starts with a part that replaces the whole grid"
                );
                if grid
                    .apply(&part)
                    .expect("the attach frame keeps the wire rules")
                {
                    break part.seq;
                }
            }
            other => panic!("the attach frame's parts follow `attached`, got {other:?}"),
        }
    };
    let status = read_daemon_frame(conn)
        .await
        .expect_message("the status after the attach frame");
    assert!(
        matches!(status, DaemonMessage::StatusChanged { .. }),
        "`status_changed` follows the attach frame, got {status:?}"
    );
    (grid, seq)
}

/// Applies and acknowledges frames on `conn` until `grid` shows `marker`,
/// returning how many frames completed on the way.
async fn read_frames_until_shown(conn: &mut Connection, grid: &mut Grid, marker: &str) -> usize {
    let mut completed = 0;
    while !grid.contains_text(marker) {
        match read_daemon_frame(conn)
            .await
            .expect_message("a frame showing the marker")
        {
            DaemonMessage::Frame(part) => {
                if grid.apply(&part).expect("a frame keeps the wire rules") {
                    completed += 1;
                    send_client_message(conn, &ClientMessage::FrameAck { seq: part.seq }).await;
                }
            }
            DaemonMessage::StatusChanged { .. } => {}
            other => panic!("expected a frame, got {other:?}"),
        }
    }
    completed
}

/// A script window with its screen cleared, and a view attached at 80x24 that
/// has applied and acknowledged everything so far.
async fn settled_view(tag: &str) -> (Harness, String, ViewClient) {
    let h = Harness::start(tag, DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;
    script_write(&h, &window_id, CLEAR).await;
    view.pump_until("the cleared screen", |grid| {
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
    common::views::wait_until_copy_shows(&h, &window_id, b"BURST-3").await;
    view.assert_no_frame("the first frame is still unacknowledged")
        .await;

    view.ack(first).await;
    let parts = view.next_complete_frame().await;
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
    view.assert_no_frame("everything has been sent and acknowledged")
        .await;
}

#[tokio::test]
async fn a_synchronized_update_sends_no_frame_until_it_ends() {
    let (h, window_id, mut view) = settled_view("view-sync-end").await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026hHELD-ONE", "HELD-ONE").await;
    view.assert_no_frame("a synchronized update is open").await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026l END-S", "END-S").await;
    view.pump_until("the frame after the update ended", |grid| {
        grid.contains_text("HELD-ONE END-S")
    })
    .await;
}

#[tokio::test]
async fn input_to_the_window_ends_a_synchronized_update() {
    let (h, window_id, mut view) = settled_view("view-sync-input").await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026hHELD-TWO", "HELD-TWO").await;
    view.assert_no_frame("a synchronized update is open").await;

    view.send_text("x").await;
    view.pump_until("the frame after the input", |grid| {
        grid.contains_text("HELD-TWO")
    })
    .await;
}

#[tokio::test]
async fn the_childs_own_exit_ends_a_synchronized_update_that_no_input_followed() {
    let (h, window_id, mut view) = settled_view("view-sync-own-exit").await;

    script_write_and_wait(&h, &window_id, b"\x1b[?2026hHELD-FIVE", "HELD-FIVE").await;
    view.assert_no_frame("a synchronized update is open").await;

    // One write: the child reopens the update after this write was counted,
    // prints and exits, so no later write can be what releases the hold.
    let mut input = script_command(b"\x1b[?2026l\x1b[?2026h HELD-SIX");
    input.push(b'!');
    h.write_input(&window_id, &input).await;
    view.pump_until("the frame after the child's own exit", |grid| {
        grid.contains_text("HELD-SIX")
    })
    .await;
}

#[tokio::test]
async fn a_superseded_connection_is_told_then_closed_with_no_frame_after_the_notice_and_the_new_one_gets_attached_the_screen_then_the_status()
 {
    let h = Harness::start("view-kick-order", DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let ViewClient { conn: mut old, .. } =
        ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;

    // The old connection acknowledges every frame it gets, so frames would
    // keep coming to it if it stayed open after the notice.
    let old_reader = tokio::spawn(async move {
        let mut frames_after_notice = Vec::new();
        let mut noticed = false;
        loop {
            match read_daemon_frame(&mut old).await {
                ReadOutcome::Closed => return (noticed, frames_after_notice),
                ReadOutcome::Message(DaemonMessage::Error { code, .. }) if code == "superseded" => {
                    noticed = true;
                }
                ReadOutcome::Message(DaemonMessage::Frame(part)) => {
                    if noticed {
                        frames_after_notice.push(part.seq);
                    }
                    if part.last {
                        send_even_if_closed(&mut old, &ClientMessage::FrameAck { seq: part.seq })
                            .await;
                    }
                }
                ReadOutcome::Message(_) => {}
            }
        }
    });

    script_write_and_wait(
        &h,
        &window_id,
        &bytes_of(&[CLEAR, b"BEFORE-KICK"]),
        "BEFORE-KICK",
    )
    .await;
    let mut new = h.connect().await;
    let (mut grid, seq) = attach_in_order(&mut new, &window_id).await;
    assert!(grid.contains_text("BEFORE-KICK"), "{}", grid.dump());

    script_write_and_wait(&h, &window_id, b"\r\nAFTER-KICK", "AFTER-KICK").await;
    send_client_message(&mut new, &ClientMessage::FrameAck { seq }).await;
    read_frames_until_shown(&mut new, &mut grid, "AFTER-KICK").await;

    let (noticed, frames_after_notice) = old_reader.await.expect("the old connection's reader");
    assert!(noticed, "the old connection was told it was superseded");
    assert!(
        frames_after_notice.is_empty(),
        "frames {frames_after_notice:?} reached the old connection after the notice"
    );
}

#[tokio::test]
async fn an_acknowledgement_written_on_a_superseded_connection_brings_the_new_connection_no_frame()
{
    let h = Harness::start("view-kick-ack", DaemonConfig::default()).await;
    let window_id = open_script_window(&h).await;
    let ViewClient { conn: mut old, .. } =
        ViewClient::attach_unacknowledged(h.connect().await, &window_id, Some(SIZE)).await;

    let mut new = ViewClient::attach_unacknowledged(h.connect().await, &window_id, None).await;
    let seq = new.grid.last_seq.expect("the attach frame completed");
    script_write_and_wait(&h, &window_id, &bytes_of(&[CLEAR, b"ACK-MARK"]), "ACK-MARK").await;

    // The old connection writes the number the new view awaits, then reads
    // until its `pong` or its close. The daemon closes a superseded connection
    // once it sends the notice, and whether it read this first is not
    // controlled here: what is shown is only that writing it brings the new
    // connection no frame, not how a superseded acknowledgement is weighed.
    send_even_if_closed(&mut old, &ClientMessage::FrameAck { seq }).await;
    send_even_if_closed(&mut old, &ClientMessage::Ping).await;
    loop {
        match read_daemon_frame(&mut old).await {
            ReadOutcome::Closed | ReadOutcome::Message(DaemonMessage::Pong) => break,
            ReadOutcome::Message(_) => {}
        }
    }

    new.assert_no_frame("the new connection has acknowledged nothing")
        .await;
    new.ack(seq).await;
    let acks = 1usize;
    let frames = 1 + read_frames_until_shown(&mut new.conn, &mut new.grid, "ACK-MARK").await;
    assert!(
        frames <= acks + 1,
        "{frames} frames reached the new connection for {acks} acknowledgements"
    );
}

#[tokio::test]
async fn a_synchronized_update_that_grows_past_the_byte_bound_is_shown_without_ending() {
    let h = Harness::start("view-sync-bound", DaemonConfig::default()).await;
    let window_id = h
        .open(
            vec![
                bins::term_script_path().to_str().unwrap().to_string(),
                "sync-flood".to_string(),
                (1024 * 1024).to_string(),
            ],
            None,
        )
        .await;
    let mut view = ViewClient::attach(h.connect().await, &window_id, Some(SIZE)).await;

    // The flood starts at the first input byte and never writes `CSI ? 2026 l`.
    view.send_text("g").await;
    view.pump_until("a frame while the update is still open", |grid| {
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
    view.assert_no_frame("the first frame is unacknowledged")
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
    h.wait_until_child_exited(&window_id).await;

    let view = ViewClient::attach_unacknowledged(h.connect().await, &window_id, None).await;
    assert!(view.child_exited, "the answer says the child has exited");
    assert_eq!(view.exit_code, Some(0));
    assert!(
        view.grid.contains_text("BYE-TEXT"),
        "the last screen is still sent:\n{}",
        view.grid.dump()
    );
}

#[tokio::test]
async fn after_the_child_exits_an_attach_sends_the_whole_screen_and_a_scroll_after_its_ack_one_more_frame()
 {
    let h = Harness::start("view-exited-scroll", DaemonConfig::default()).await;
    let (window_id, first) = open_view_with_history(&h, SIZE, 100).await;
    drop(first);
    h.write_input(&window_id, b"!").await;
    h.wait_until_child_exited(&window_id).await;

    let mut view = ViewClient::attach(h.connect().await, &window_id, None).await;
    assert!(view.child_exited, "the answer says the child has exited");
    assert!(
        view.grid.contains_text("HIST-FILL"),
        "the attach frame shows the last screen:\n{}",
        view.grid.dump()
    );
    assert_eq!(view.grid.view.display_offset, 0);

    view.send(&ClientMessage::ScrollView { lines: 5 }).await;
    view.pump_until("the scrolled view", |grid| grid.view.display_offset == 5)
        .await;
    assert_eq!(
        view.completed.len(),
        2,
        "the attach frame, then the one frame the scroll brought"
    );
}
