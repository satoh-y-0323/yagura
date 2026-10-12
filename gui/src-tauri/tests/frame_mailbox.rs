#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The Tauri layer's mailbox between the daemon's frames and the single
//! pending "next frames" request of the WebView: one request is held for the
//! whole app, and it returns as soon as any window has an undelivered part.
//!
//! Nothing here waits on a clock. A pending `take` is made visible to the
//! mailbox by running the spawned task to its first suspension on the
//! single-threaded test runtime (`yield_now`); the `timeout` wrappers only
//! keep a broken mailbox from hanging the suite.

use std::sync::Arc;
use std::time::Duration;

use yagura_gui::frame_mailbox::FrameMailbox;
use yagura_protocol::{CursorShape, CursorState, Frame, ViewState};

const HANG_GUARD: Duration = Duration::from_secs(10);

fn frame(seq: u64, reset: bool) -> Frame {
    Frame {
        seq,
        cols: 2,
        rows: 1,
        reset,
        last: true,
        attrs: Vec::new(),
        lines: Vec::new(),
        cursor: CursorState {
            row: 0,
            col: 0,
            visible: true,
            shape: CursorShape::Block,
        },
        view: ViewState {
            display_offset: 0,
            history_rows: 0,
            mouse_reporting: false,
            alt_screen: false,
        },
        overrides: Vec::new(),
    }
}

/// The seqs delivered for one window, in delivery order.
fn seqs_of(delivered: &[(String, Frame)], window_id: &str) -> Vec<u64> {
    delivered
        .iter()
        .filter(|(id, _)| id == window_id)
        .map(|(_, f)| f.seq)
        .collect()
}

async fn finished<T>(task: tokio::task::JoinHandle<T>) -> T {
    tokio::time::timeout(HANG_GUARD, task)
        .await
        .expect("the mailbox call never returned")
        .unwrap()
}

/// Starts a `take` and lets it reach its suspension point.
async fn start_take(mailbox: &Arc<FrameMailbox>) -> tokio::task::JoinHandle<Vec<(String, Frame)>> {
    let mailbox = Arc::clone(mailbox);
    let task = tokio::spawn(async move { mailbox.take().await });
    tokio::task::yield_now().await;
    task
}

#[tokio::test]
async fn a_pending_take_returns_when_any_window_gets_a_frame() {
    let mailbox = Arc::new(FrameMailbox::new());
    let pending = start_take(&mailbox).await;
    assert!(!pending.is_finished());

    mailbox.push("w2", frame(1, true));

    let delivered = finished(pending).await;
    assert_eq!(delivered.len(), 1);
    assert_eq!(delivered[0].0, "w2");
    assert_eq!(delivered[0].1, frame(1, true));
}

#[tokio::test]
async fn take_returns_at_once_when_frames_are_already_waiting() {
    let mailbox = FrameMailbox::new();
    mailbox.push("w1", frame(1, true));

    let delivered = tokio::time::timeout(HANG_GUARD, mailbox.take())
        .await
        .expect("take should not wait when a frame is queued");
    assert_eq!(seqs_of(&delivered, "w1"), vec![1]);
}

#[tokio::test]
async fn frames_of_one_window_keep_their_order_across_windows_mixed_in() {
    let mailbox = FrameMailbox::new();
    mailbox.push("a", frame(1, true));
    mailbox.push("b", frame(10, true));
    mailbox.push("a", frame(2, false));
    mailbox.push("b", frame(11, false));
    mailbox.push("a", frame(3, false));

    let delivered = tokio::time::timeout(HANG_GUARD, mailbox.take())
        .await
        .unwrap();
    assert_eq!(seqs_of(&delivered, "a"), vec![1, 2, 3]);
    assert_eq!(seqs_of(&delivered, "b"), vec![10, 11]);
}

#[tokio::test]
async fn delivered_frames_are_not_delivered_again() {
    let mailbox = Arc::new(FrameMailbox::new());
    mailbox.push("a", frame(1, true));
    let first = tokio::time::timeout(HANG_GUARD, mailbox.take())
        .await
        .unwrap();
    assert_eq!(seqs_of(&first, "a"), vec![1]);

    let pending = start_take(&mailbox).await;
    assert!(!pending.is_finished(), "the delivered part came back");
    mailbox.push("a", frame(2, false));
    let second = finished(pending).await;
    assert_eq!(seqs_of(&second, "a"), vec![2]);
}

#[tokio::test]
async fn a_reset_frame_drops_that_windows_earlier_undelivered_parts_only() {
    let mailbox = FrameMailbox::new();
    mailbox.push("a", frame(1, true));
    mailbox.push("a", frame(2, false));
    mailbox.push("b", frame(10, true));
    mailbox.push("b", frame(11, false));
    mailbox.push("a", frame(3, true));
    mailbox.push("a", frame(4, false));

    let delivered = tokio::time::timeout(HANG_GUARD, mailbox.take())
        .await
        .unwrap();
    assert_eq!(seqs_of(&delivered, "a"), vec![3, 4]);
    assert_eq!(seqs_of(&delivered, "b"), vec![10, 11]);
}

#[tokio::test]
async fn discarding_a_window_drops_its_undelivered_parts_and_keeps_the_others() {
    let mailbox = FrameMailbox::new();
    mailbox.push("a", frame(1, true));
    mailbox.push("b", frame(10, true));
    mailbox.push("a", frame(2, false));

    mailbox.discard_window("a");

    let delivered = tokio::time::timeout(HANG_GUARD, mailbox.take())
        .await
        .unwrap();
    assert!(seqs_of(&delivered, "a").is_empty());
    assert_eq!(seqs_of(&delivered, "b"), vec![10]);
}

#[tokio::test]
async fn a_second_take_returns_the_first_empty_and_takes_over() {
    let mailbox = Arc::new(FrameMailbox::new());
    let first = start_take(&mailbox).await;
    let second = start_take(&mailbox).await;

    let from_first = finished(first).await;
    assert!(from_first.is_empty(), "the superseded take must be empty");
    assert!(!second.is_finished());

    mailbox.push("a", frame(1, true));
    let from_second = finished(second).await;
    assert_eq!(seqs_of(&from_second, "a"), vec![1]);
}
