#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the per-window notice queue and its single delivery
//! agent (NQ-*). Covers what no single notice kind's own suite can: the agent
//! retrying a notice the window's input declined. The window declares delivery
//! with no way to wake its CLI, so a notice is typed into it while it waits.
//! Every window is a `submit_child` or `echo_child` fixture, never a real
//! vendor CLI.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    Harness, MARKER, SUBMIT_CHILD_PROBE, SubmitDefinitionInput, VALID_TOKEN, approver_base_config,
    call_open_window, identify, open_submit_child_window_with_composer,
    open_submit_child_window_with_composer_logging_input, policy, read_response, send,
    unique_temp_dir, wait_until_input_log_contains, wait_until_status,
};

use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::echo_child_command;

const NOTICE_TIMEOUT: Duration = Duration::from_secs(10);

/// Longer than the clock-driven retry interval, so a timer retry would show
/// up inside it. It is only waited out, never relied on to succeed.
const NO_RETRY_WINDOW: Duration = Duration::from_secs(4);

const FOREIGN_DRAFT: &str = "NQFOREIGNDRAFT";

/// Free of the daemon's probe byte, so the log can be told apart from it.
const KEYS_TYPED_ON: &str = "MORE";

fn expected_report_notice(pending: usize) -> String {
    format!("[yagura report] {pending} pending report(s). Call wait_for_report to read them.")
}

// --- reading what reached a window's input ------------------------------------

async fn wait_for_flattened(h: &Harness, window_id: &str, needle: &str, timeout: Duration) {
    let deadline = tokio::time::Instant::now() + timeout;
    while !h.flattened_scrollback(window_id).contains(needle) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {needle:?} in {window_id}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn pin_status(probe: &mut Connection, window_id: &str, status: WindowStatus) {
    send(
        probe,
        &ClientMessage::StatusEvent {
            source: "notice-queue-test".to_string(),
            status,
            window_id: Some(window_id.to_string()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(probe, window_id, status, Duration::from_secs(10)).await;
}

// --- A notice declined by a foreign draft waits for the draft to change ---

fn report_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["open_window"])),
        ..approver_base_config()
    }
}

#[tokio::test]
async fn a_notice_declined_by_a_foreign_draft_is_retried_only_when_the_draft_changes() {
    let dir = unique_temp_dir("nq5-retry-pace");
    let h = Harness::start("nq5-retry-pace", report_config(&dir)).await;
    let mut probe = h.connect().await;
    let parent = open_submit_child_window_with_composer(
        &h,
        &mut probe,
        &dir,
        "nq5-parent",
        "nq5-parent-ready",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: false,
            ready_hold_ms: 200,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 0,
            resend_max: 0,
        },
        0,
    )
    .await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent).await;
    let child = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child).await;
    pin_status(&mut probe, &parent, WindowStatus::WaitingInput).await;

    h.write_input(&parent, FOREIGN_DRAFT.as_bytes()).await;
    wait_for_flattened(&h, &parent, FOREIGN_DRAFT, NOTICE_TIMEOUT).await;

    send(
        &mut child_conn,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: "done".to_string(),
            path: None,
        },
    )
    .await;
    match read_response(&mut child_conn, Duration::from_secs(5)).await {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }

    let entry = h.registry.get(&parent).expect("parent window entry");
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while entry.notice_attempts() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the notice was never attempted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    // The draft belongs to someone else and nobody writes while the status
    // stays put: the ledger must not move, so no second attempt may start.
    // An attempt is counted only after its own writes are recorded, so the
    // ledger value read here already includes them.
    let before = entry.notice_attempts();
    let settled = entry.session().writes_recorded();
    let mut writes = entry.session().subscribe_writes_recorded();
    let moved = tokio::time::timeout(NO_RETRY_WINDOW, writes.changed()).await;
    assert!(
        moved.is_err(),
        "a notice declined by a foreign draft must not write into the window again"
    );
    assert_eq!(entry.session().writes_recorded(), settled);
    assert_eq!(entry.notice_attempts(), before);
    assert_eq!(
        h.submitted_count(&parent, ""),
        0,
        "no notice may be typed over the foreign draft"
    );

    h.write_input(&parent, b"\x15").await;
    wait_for_flattened(
        &h,
        &parent,
        &format!("SUBMITTED {}", expected_report_notice(1)),
        NOTICE_TIMEOUT,
    )
    .await;
    assert_eq!(
        entry.notice_attempts(),
        before + 1,
        "clearing the draft must deliver the notice on the very next attempt"
    );
    h.registry.terminate_all().await;
}

/// A user typing into their own draft moves the write ledger on every key
/// while the composer stays non-empty, so a ledger move alone must not start
/// another attempt: its probe would land inside the text being typed.
#[tokio::test]
async fn a_notice_is_not_retried_while_the_user_keeps_typing_into_the_draft() {
    let dir = unique_temp_dir("nq5-typing");
    let log = dir.join("nq5-typing-input.log");
    let h = Harness::start("nq5-typing", report_config(&dir)).await;
    let mut probe = h.connect().await;
    let parent = open_submit_child_window_with_composer_logging_input(
        &h,
        &mut probe,
        &dir,
        "nq5-typing-parent",
        "nq5-typing-parent-ready",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: false,
            ready_hold_ms: 200,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 0,
            resend_max: 0,
        },
        0,
        &log,
    )
    .await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent).await;
    let child = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child).await;
    pin_status(&mut probe, &parent, WindowStatus::WaitingInput).await;

    h.write_input(&parent, FOREIGN_DRAFT.as_bytes()).await;
    wait_for_flattened(&h, &parent, FOREIGN_DRAFT, NOTICE_TIMEOUT).await;

    send(
        &mut child_conn,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: "done".to_string(),
            path: None,
        },
    )
    .await;
    match read_response(&mut child_conn, Duration::from_secs(5)).await {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }
    let entry = h.registry.get(&parent).expect("parent window entry");
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while entry.notice_attempts() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the notice was never attempted"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let before = entry.notice_attempts();

    // The user types on: the ledger moves, the composer is still not empty.
    h.write_input(&parent, KEYS_TYPED_ON.as_bytes()).await;
    let read = wait_until_input_log_contains(&log, KEYS_TYPED_ON.as_bytes(), NOTICE_TIMEOUT).await;
    let typed_at = read
        .windows(KEYS_TYPED_ON.len())
        .rposition(|w| w == KEYS_TYPED_ON.as_bytes())
        .expect("the log was returned holding the typed keys")
        + KEYS_TYPED_ON.len();

    // Nothing is observed to happen here; the window only bounds a retry that
    // the ledger move alone would start.
    tokio::time::sleep(NO_RETRY_WINDOW).await;
    h.write_input(&parent, MARKER).await;
    let read = wait_until_input_log_contains(&log, MARKER, NOTICE_TIMEOUT).await;
    let at = read
        .windows(MARKER.len())
        .rposition(|w| w == MARKER)
        .expect("the log was returned holding the marker");
    let after_typing = &read[typed_at..at];
    assert!(
        !after_typing.contains(&SUBMIT_CHILD_PROBE.as_bytes()[0]),
        "no probe may be written behind a draft the user is still typing into; the child read          {after_typing:?} after the keys"
    );
    assert_eq!(entry.notice_attempts(), before);
    assert_eq!(h.submitted_count(&parent, ""), 0);

    h.write_input(&parent, b"\x15").await;
    wait_for_flattened(
        &h,
        &parent,
        &format!("SUBMITTED {}", expected_report_notice(1)),
        NOTICE_TIMEOUT,
    )
    .await;
    assert_eq!(
        entry.notice_attempts(),
        before + 1,
        "emptying the draft must deliver the notice on the very next attempt"
    );
    h.registry.terminate_all().await;
}
