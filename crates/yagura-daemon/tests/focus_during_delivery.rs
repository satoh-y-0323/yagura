#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a delivery into a window whose view has lost focus is
//! typed while the child is told it has focus, and the child is told it lost
//! focus again only after the submit key.
//!
//! The child is `submit_child` in `focus-reports` mode: it hides its cursor on
//! a focus-out report and shows it on a focus-in report, the way GitHub
//! Copilot CLI does, so a screen it is not focused on never reads as finished.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    HANG_GUARD, Harness, SubmitDefinitionInput, approver_base_config, attach, policy,
    read_response, send, unique_temp_dir, wait_until_frame_satisfies,
    wait_until_input_log_contains, wait_until_status, write_submit_definition,
};
use tracing_test::traced_test;
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};
use yagura_test_support::process;

const FOCUS_IN: &[u8] = b"\x1b[I";
const FOCUS_OUT: &[u8] = b"\x1b[O";
const BODY: &str = "focus-body";

fn position(haystack: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    haystack
        .get(from..)?
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|at| at + from)
}

/// Waits until the child has read a focus-out report after `from` in its
/// input log, and returns the log.
async fn wait_for_focus_out_after(log: &Path, from: usize) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let read = std::fs::read(log).unwrap_or_default();
        if position(&read, FOCUS_OUT, from).is_some() {
            return read;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child was never told it lost focus after the submit key; it read {:?}",
            String::from_utf8_lossy(&read)
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
#[traced_test]
async fn a_delivery_to_a_window_whose_view_lost_focus_is_typed_with_focus_and_submitted() {
    let dir = unique_temp_dir("focus-during-delivery");
    let h = Harness::start(
        "focus-during-delivery",
        DaemonConfig {
            detection_dir: Some(dir.clone()),
            approval_policy: Some(policy(&["open_window", "send_message"])),
            ..approver_base_config()
        },
    )
    .await;
    let mut client = h.connect().await;

    let name = "focus-receiver";
    let marker = "focus-receiver-marker";
    write_submit_definition(
        &dir,
        name,
        marker,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 60,
            clear: "\\u0015",
            delivery_confirm_ms: 0,
            resend_max: 0,
        },
    );
    let log = dir.join("focus-receiver-input.log");
    let output_now = dir.join("output-now");
    let mut command = process::submit_child_command_with_composer(marker, 0, 0, 0, 0);
    command.push("-".to_string()); // fold_placeholder
    command.push("-".to_string()); // clear_scope
    command.push("-".to_string()); // overlay
    command.push("-".to_string()); // overlay_heading
    command.push(log.to_str().expect("UTF-8 log path").to_string());
    command.push("0".to_string()); // paste_enter_window_ms
    command.push(format!("focus-reports;output-on|{}", output_now.display()));
    let window_id = h.open_delivering(command, Some(name)).await;
    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;

    let mut view = h.connect().await;
    attach(&mut view, &window_id).await;
    send(&mut view, &ClientMessage::Focus { focused: false }).await;
    wait_until_input_log_contains(&log, FOCUS_OUT, HANG_GUARD).await;
    wait_until_frame_satisfies(
        &h,
        &window_id,
        "the cursor hidden after losing focus",
        HANG_GUARD,
        |_, unfinished| unfinished,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: format!("{BODY}\r"),
        },
    )
    .await;
    let answer = tokio::time::timeout(HANG_GUARD, read_response(&mut client, HANG_GUARD * 2)).await;
    let Ok(answer) = answer else {
        panic!(
            "the delivery was never answered; waited on an unfinished screen: {}; the child read {:?}",
            logs_contain("waiting for the screen to finish painting"),
            String::from_utf8_lossy(&std::fs::read(&log).unwrap_or_default())
        );
    };
    match answer {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected text_sent, got {other:?}"),
    }
    h.wait_for_scrollback(
        &window_id,
        format!("SUBMITTED {BODY}").as_bytes(),
        HANG_GUARD,
    )
    .await;

    std::fs::write(&output_now, b"").expect("create the output trigger");
    let read = std::fs::read(&log).unwrap_or_default();
    let view_report_end =
        position(&read, FOCUS_OUT, 0).expect("the view's report was read") + FOCUS_OUT.len();
    let body_at = position(&read, BODY.as_bytes(), view_report_end)
        .unwrap_or_else(|| panic!("the body was never read: {read:?}"));
    let focus_in_at = position(&read, FOCUS_IN, view_report_end);
    assert!(
        focus_in_at.is_some_and(|at| at < body_at),
        "the child must be told it has focus before the body; it read {:?}",
        String::from_utf8_lossy(&read)
    );
    let submit_at = position(&read, b"\r", body_at + BODY.len())
        .unwrap_or_else(|| panic!("the submit key was never read: {read:?}"));
    wait_for_focus_out_after(&log, submit_at + 1).await;
}
