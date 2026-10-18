#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a notice whose text has landed behind an approval dialog
//! is finished by the same attempt once the dialog is gone, submitted once and
//! never written a second time.
//!
//! The receiver is a `submit_child` that draws an approval dialog the moment
//! the notice text lands and keeps it until the test removes it, so the
//! attempt waits with the notice unsent but already in the composer.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    Harness, SUBMIT_CHILD_PROBE, approver_base_config, call_open_window, composer_section_toml,
    identify, policy, read_response, send, unique_temp_dir, wait_until_status,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome, WindowStatus};
use yagura_test_support::process::{self, echo_child_command};

const APPROVAL_HEADING: &str = "NOTICE-APPROVAL";

const NOTICE_TIMEOUT: Duration = Duration::from_secs(15);

/// Opens `command` as a window whose profile declares delivery with no way to wake its
/// CLI: a notice reaches it typed while it waits. A window opened from a raw argv takes
/// no part in notices.
async fn open_delivering(h: &Harness, command: Vec<String>, detection: &str) -> String {
    h.registry
        .open_with_extra_env(
            yagura_daemon::OpenWindowRequest {
                command,
                cwd: None,
                name: None,
                vendor: None,
                detection: Some(detection.to_string()),
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                delivery: Some(yagura_daemon::profile_delivery::ProfileDelivery {
                    output: yagura_daemon::inbox::OutputForm::Plain,
                    max_chars: yagura_daemon::inbox::DEFAULT_MAX_CHARS,
                    wake: yagura_daemon::profile_delivery::WakeMode::None,
                    wake_stream: yagura_protocol::DeliveryStream::Stderr,
                    wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
                    launch_prompt: None,
                    shim_guidance: None,
                }),
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("registry.open_with_extra_env should spawn the child")
        .window_id
}

fn report_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["open_window"])),
        ..approver_base_config()
    }
}

fn write_definition(dir: &Path, name: &str, marker: &str) {
    let toml = format!(
        r#"schema_version = 1
name = "{name}"
bottom_line_count = 5

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_approval"
pattern = '{APPROVAL_HEADING}'
priority = 30
region = "bottom"

[[rules]]
status = "running"
pattern = 'SUBMITTED'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '{marker}'
priority = 10
region = "bottom"

[input]
bracketed_paste = true
ready_hold_ms = 0
submit_delay_ms = 120
clear = ""
delivery_confirm_ms = 0
resend_max = 0
probe = "{SUBMIT_CHILD_PROBE}"
"#
    );
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("{toml}{}", composer_section_toml()),
    )
    .expect("write detection definition file");
}

fn flattened(h: &Harness, window_id: &str) -> String {
    let raw = String::from_utf8_lossy(&h.scrollback(window_id)).into_owned();
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars();
    while let Some(c) = chars.next() {
        match c {
            '\u{1b}' => {
                if chars.next() == Some('[') {
                    for c in chars.by_ref() {
                        if ('\u{40}'..='\u{7e}').contains(&c) {
                            break;
                        }
                    }
                }
            }
            '\r' | '\n' => {}
            other => out.push(other),
        }
    }
    out
}

#[tokio::test]
async fn a_notice_written_behind_a_dialog_is_submitted_by_the_same_attempt_once_the_dialog_is_gone()
{
    let dir = unique_temp_dir("notice-residue");
    let dialog_gone = dir.join("dialog-gone");
    let h = Harness::start("notice-residue", report_config(&dir)).await;
    let mut probe = h.connect().await;
    write_definition(&dir, "parent", "parent-ready");
    let mut command =
        process::submit_child_command_with_clear_scope("parent-ready", 0, 0, 0, 0, "");
    command.push(format!(
        "body:hidden:until|{}",
        dialog_gone
            .to_str()
            .expect("the dialog path must be valid UTF-8")
    ));
    command.push(APPROVAL_HEADING.to_string());
    let parent = open_delivering(&h, command, "parent").await;
    wait_until_status(
        &mut probe,
        &parent,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;

    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent).await;
    let child = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child).await;

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

    let entry = h.registry.get(&parent).expect("the receiver must exist");
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while !flattened(&h, &parent).contains(APPROVAL_HEADING) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the notice never landed behind the dialog; screen text: {}",
            flattened(&h, &parent)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        entry.notice_attempts(),
        0,
        "the attempt is still waiting behind the dialog, it has not finished"
    );
    assert!(
        !flattened(&h, &parent).contains("SUBMITTED "),
        "nothing may be submitted while the dialog is up"
    );
    std::fs::write(&dialog_gone, b"").expect("remove the dialog");

    let notice = "[yagura report] 1 pending report(s). Call wait_for_report to read them.";
    let submitted = format!("SUBMITTED {notice}");
    while !flattened(&h, &parent).contains(&submitted) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the notice never reached the receiver: an attempt that waited out a dialog \
             must finish its own notice; screen text: {}",
            flattened(&h, &parent)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let text = flattened(&h, &parent);
    assert_eq!(
        text.matches("SUBMITTED ").count(),
        1,
        "the notice must be submitted exactly once"
    );
    assert!(
        !text.contains(&format!("SUBMITTED {notice}{notice}")),
        "the notice text must not have been written twice into the composer"
    );
    h.registry.terminate_all().await;
}
