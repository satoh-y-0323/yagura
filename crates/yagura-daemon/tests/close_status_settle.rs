#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Once `close_window` has replied for a window whose exit the daemon observed, every
//! later read reports the window `exited`.
//!
//! Measured end to end over a real IPC round trip on a window whose detection driver
//! has died, so no detector is left to publish the exit: `ListWindows` goes out on the
//! same connection right after `WindowClosed` arrives, and the status it reports is
//! asserted.

use crate::common;

use common::{HANG_GUARD, Harness, policy, read_response, send};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage, DetectedStatus, UnknownReason, WindowStatus};

#[tokio::test]
async fn closing_a_window_whose_detection_stopped_reports_exited_in_the_next_list_windows() {
    // The product's grace bound is the hang guard too, so reaching it fails the case
    // instead of letting a fixed budget decide it.
    let config = DaemonConfig {
        approval_policy: Some(policy(&["close_window"])),
        close_grace_ms: HANG_GUARD.as_millis() as u64,
        ..DaemonConfig::default()
    };
    let h = Harness::start("close-exit-detector-stopped", config).await;
    let mut client = h.connect().await;
    let window_id = h.open(common::sleepy_command("30"), None).await;

    let entry = h
        .registry
        .get(&window_id)
        .expect("the opened window must be registered");
    let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
    let mut statuses = entry.watch_status();
    entry.inject_detector_driver_panic();
    let reached = tokio::time::timeout(HANG_GUARD, async {
        while statuses.borrow_and_update().status != stopped {
            if statuses.changed().await.is_err() {
                return false;
            }
        }
        true
    })
    .await;
    assert_eq!(
        reached,
        Ok(true),
        "the window must report Unknown(DetectorStopped) once its driver dies, got {:?}",
        entry.current_detected()
    );

    send(
        &mut client,
        &ClientMessage::CloseWindow {
            window_id: window_id.clone(),
            force: None,
        },
    )
    .await;
    match read_response(&mut client).await {
        DaemonMessage::WindowClosed { window_id: acked } => {
            assert_eq!(acked, window_id, "WindowClosed must ack the closed window")
        }
        other => panic!("expected window_closed, got {other:?}"),
    }

    send(&mut client, &ClientMessage::ListWindows).await;
    let status = match read_response(&mut client).await {
        DaemonMessage::WindowList { windows } => {
            windows
                .into_iter()
                .find(|w| w.window_id == window_id)
                .unwrap_or_else(|| panic!("closed window {window_id} missing from list_windows"))
                .status
        }
        other => panic!("expected window_list, got {other:?}"),
    };

    assert_eq!(
        status,
        WindowStatus::Exited,
        "the list_windows sent right after window_closed must report the closed window \
         exited, even though no detector is left to publish its exit"
    );
}
