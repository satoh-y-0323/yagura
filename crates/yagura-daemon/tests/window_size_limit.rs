#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! A window's terminal is at most 1000 columns by 1000 rows. A request for more
//! is `invalid_argument` and changes nothing; exactly that size is accepted.
//!
//! Window sizes enter the daemon through `Resize` (IPC) and
//! `PtySession::resize`; `open_window` carries no size.

mod common;

use std::time::Duration;

use common::{Harness, read_response, send};
use yagura_daemon::{DaemonConfig, PtySession};
use yagura_protocol::{ClientMessage, DaemonMessage};

const MAX_COLS: u16 = 1000;
const MAX_ROWS: u16 = 1000;

/// Sizes just past either limit, and far past both.
const TOO_LARGE: [(u16, u16); 4] = [
    (MAX_COLS + 1, 24),
    (80, MAX_ROWS + 1),
    (MAX_COLS + 1, MAX_ROWS + 1),
    (u16::MAX, u16::MAX),
];

async fn attached_echo_window(h: &Harness) -> (yagura_test_support::ipc::Connection, String) {
    let window_id = h
        .open(yagura_test_support::process::echo_child_command(), None)
        .await;
    let mut client = h.connect().await;
    send(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let reply = read_response(&mut client, Duration::from_secs(5)).await;
    assert!(matches!(reply, DaemonMessage::Attached { .. }), "{reply:?}");
    (client, window_id)
}

#[tokio::test]
async fn resize_over_ipc_beyond_the_limit_is_invalid_argument_and_changes_nothing() {
    let h = Harness::start("size-limit-ipc-over", DaemonConfig::default()).await;
    let (mut client, window_id) = attached_echo_window(&h).await;
    let session = h.registry.get(&window_id).unwrap().session().clone();
    let before = session.size().await.unwrap();

    for (cols, rows) in TOO_LARGE {
        send(&mut client, &ClientMessage::Resize { cols, rows }).await;
        match read_response(&mut client, Duration::from_secs(5)).await {
            DaemonMessage::Error { code, .. } => assert_eq!(code, "invalid_argument"),
            other => panic!("resize({cols}, {rows}) should be invalid_argument, got {other:?}"),
        }
    }
    assert_eq!(
        session.size().await.unwrap(),
        before,
        "a rejected resize must change nothing"
    );
}

#[tokio::test]
async fn resize_over_ipc_to_exactly_the_limit_is_accepted() {
    let h = Harness::start("size-limit-ipc-exact", DaemonConfig::default()).await;
    let (mut client, window_id) = attached_echo_window(&h).await;
    let session = h.registry.get(&window_id).unwrap().session().clone();

    send(
        &mut client,
        &ClientMessage::Resize {
            cols: MAX_COLS,
            rows: MAX_ROWS,
        },
    )
    .await;
    send(&mut client, &ClientMessage::Ping).await;
    let reply = read_response(&mut client, Duration::from_secs(5)).await;
    assert!(
        matches!(reply, DaemonMessage::Pong),
        "the largest size must be accepted, got {reply:?}"
    );
    assert_eq!(session.size().await.unwrap(), (MAX_COLS, MAX_ROWS));
}

#[tokio::test]
async fn pty_session_resize_beyond_the_limit_fails_and_changes_nothing() {
    let config = DaemonConfig::default();
    let child = common::echo_child_path();
    let session = PtySession::spawn_with_command(&[child.to_str().unwrap()], &config)
        .await
        .expect("spawn echo_child");
    let before = session.size().await.unwrap();

    for (cols, rows) in TOO_LARGE {
        assert!(
            session.resize(cols, rows).await.is_err(),
            "resize({cols}, {rows}) must be refused"
        );
        assert_eq!(session.size().await.unwrap(), before);
    }
}

#[tokio::test]
async fn pty_session_resize_to_exactly_the_limit_succeeds() {
    let config = DaemonConfig::default();
    let child = common::echo_child_path();
    let session = PtySession::spawn_with_command(&[child.to_str().unwrap()], &config)
        .await
        .expect("spawn echo_child");

    session
        .resize(MAX_COLS, MAX_ROWS)
        .await
        .expect("the largest size must be accepted");
    assert_eq!(session.size().await.unwrap(), (MAX_COLS, MAX_ROWS));
}
