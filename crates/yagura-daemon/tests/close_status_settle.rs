#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! `close_window` settles the window's confirmed status before its IPC reply returns.
//!
//! The contract lives in the reply timing of `handle_close_window` (`ipc.rs`), so it is
//! measured over a real IPC round trip: `ListWindows` goes out on the same connection
//! immediately after `WindowClosed` arrives, and the status it reports is asserted.

mod common;

use std::time::Duration;

use common::{Harness, policy, read_response, send};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};

/// Far above the measured settle gap, so the wait's timeout never races the assertion.
const INJECTED_SETTLE_MS: u64 = 5000;

fn config_with_settle_ms(close_settle_ms: u64) -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        close_settle_ms,
        ..DaemonConfig::default()
    }
}

/// Opens a real, long-lived child. Only the post-close status is asserted, so no
/// detection definition is configured.
async fn open_a_real_window(h: &Harness) -> String {
    h.open(common::sleepy_command("30"), None).await
}

/// Closes the window, then reads its status from a `ListWindows` sent on the same
/// connection right after the `WindowClosed` ack -- the boundary under test.
async fn close_then_read_status_from_list_windows(
    client: &mut yagura_test_support::ipc::Connection,
    window_id: &str,
    force: Option<bool>,
) -> WindowStatus {
    send(
        client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_string(),
            force,
        },
    )
    .await;
    match read_response(client, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: acked } => {
            assert_eq!(acked, window_id, "WindowClosed must ack the closed window")
        }
        other => panic!("expected window_closed, got {other:?}"),
    }

    send(client, &ClientMessage::ListWindows).await;
    match read_response(client, Duration::from_secs(10)).await {
        DaemonMessage::WindowList { windows } => {
            windows
                .into_iter()
                .find(|w| w.window_id == window_id)
                .unwrap_or_else(|| panic!("closed window {window_id} missing from list_windows"))
                .status
        }
        other => panic!("expected window_list, got {other:?}"),
    }
}

#[tokio::test]
async fn close_window_without_force_reports_exited_in_the_immediately_following_list_windows() {
    let h = Harness::start("settle-graceful", config_with_settle_ms(INJECTED_SETTLE_MS)).await;
    let mut client = h.connect().await;
    let window_id = open_a_real_window(&h).await;

    let status = close_then_read_status_from_list_windows(&mut client, &window_id, None).await;

    assert_eq!(
        status,
        WindowStatus::Exited,
        "the list_windows sent immediately after window_closed must already report Exited \
         (handle_close_window must wait for the confirmed status to settle before replying)"
    );
}

#[tokio::test]
async fn close_window_with_force_reports_exited_in_the_immediately_following_list_windows() {
    let h = Harness::start("settle-forced", config_with_settle_ms(INJECTED_SETTLE_MS)).await;
    let mut client = h.connect().await;
    let window_id = open_a_real_window(&h).await;

    let status =
        close_then_read_status_from_list_windows(&mut client, &window_id, Some(true)).await;

    assert_eq!(
        status,
        WindowStatus::Exited,
        "the force=true path must satisfy the same settle contract as the graceful path -- \
         force only reorders terminate / job shutdown, it must not skip the wait"
    );
}

// Scoped to "does not error": a zero budget does not guarantee the limit is reached
// (a status already Exited before the wait starts never reaches it).
#[tokio::test]
async fn close_window_with_a_zero_settle_budget_still_returns_window_closed_without_erroring() {
    let h = Harness::start("settle-zero-budget", config_with_settle_ms(0)).await;
    let mut client = h.connect().await;
    let window_id = open_a_real_window(&h).await;

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(10)).await {
        DaemonMessage::WindowClosed { window_id: acked } => {
            assert_eq!(acked, window_id, "WindowClosed must ack the closed window")
        }
        other => {
            panic!("a close_settle_ms of 0 must not turn the reply into an error, got {other:?}")
        }
    }
    // No status assertion: only the absence of an error is under test.
}
