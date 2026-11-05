#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a notice whose text has landed behind an approval dialog
//! is finished by the same attempt once the dialog is gone, submitted once and
//! never written a second time, and a probe character a dialog swallowed is
//! neither left behind in the composer nor waited for.
//!
//! The first receiver is a `submit_child` that draws an approval dialog the
//! moment the notice text lands and keeps it until the test removes it, so the
//! attempt waits with the notice unsent but already in the composer. The
//! second draws the dialog at the first probe character typed behind the
//! notice text, which the dialog takes as its own input.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    HANG_GUARD, Harness, SUBMIT_CHILD_PROBE, approver_base_config, call_open_window,
    composer_section_toml, identify, policy, read_response, send, unique_temp_dir,
    wait_until_status,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::definition::parse_definition;
use yagura_daemon::window::WindowEntry;
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{self, echo_child_command};

const APPROVAL_HEADING: &str = "NOTICE-APPROVAL";

/// The second character of a two-character probe: not the notice's last
/// character (`.`) and not the first probe character.
const SECOND_PROBE_CHAR: char = 'z';

const NOTICE: &str = "[yagura report] 1 pending report(s). Call wait_for_report to read them.";

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
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                delivery: Some(yagura_daemon::profile_delivery::ProfileDelivery {
                    output: yagura_daemon::inbox::DoorOutputs::all(
                        &yagura_daemon::inbox::OutputForm::Plain,
                    ),
                    max_chars: yagura_daemon::inbox::DEFAULT_MAX_CHARS,
                    wake: yagura_daemon::profile_delivery::WakeMode::None,
                    wake_stream: yagura_protocol::DeliveryStream::Stderr,
                    wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
                    launch_prompt: None,
                    launch_context: None,
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

fn write_definition(dir: &Path, name: &str, marker: &str, probe: &str) {
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
probe = "{probe}"
"#
    );
    let source = format!("{toml}{}", composer_section_toml());
    // A definition the daemon rejects leaves the window undetected, which a test
    // would only notice by waiting out its upper bound.
    if let Err(error) = parse_definition(&source) {
        panic!("the test's detection definition (probe {probe:?}) must load: {error}");
    }
    std::fs::write(dir.join(format!("{name}.toml")), source)
        .expect("write detection definition file");
}

struct Receiver {
    h: Harness,
    parent: String,
    observer: Connection,
    entry: Arc<WindowEntry>,
    /// Created by the test to remove the dialog.
    dialog_gone: PathBuf,
}

impl Receiver {
    fn screen(&self) -> String {
        self.h
            .flattened_scrollback_keeping_osc_payload(&self.parent)
    }

    /// Waits until `needle` shows on the receiver's screen.
    async fn wait_for_screen(&self, needle: &str, what: &str) {
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while !self.screen().contains(needle) {
            assert!(
                tokio::time::Instant::now() < deadline,
                "{what}; screen text: {}",
                self.screen()
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    fn remove_dialog(&self) {
        std::fs::write(&self.dialog_gone, b"").expect("remove the dialog");
    }

    /// After the dialog is gone: the notice is submitted once, no probe character
    /// stands behind it, and the attempt that wrote it was the only one.
    async fn assert_submitted_once_and_clean(&self, probe: &str) {
        let submitted = format!("SUBMITTED {NOTICE}");
        self.wait_for_screen(
            &submitted,
            "the notice never reached the receiver: an attempt that waited out a dialog \
             must finish its own notice",
        )
        .await;
        let text = self.screen();
        assert_eq!(
            text.matches("SUBMITTED ").count(),
            1,
            "the notice must be submitted exactly once"
        );
        assert!(
            !text.contains(&format!("SUBMITTED {NOTICE} {NOTICE}"))
                && !text.contains(&format!("SUBMITTED {NOTICE}{NOTICE}")),
            "the notice text must not have been written twice into the composer"
        );
        for character in probe.chars() {
            assert!(
                !text.contains(&format!("{submitted}{character}")),
                "probe character {character:?} was left behind the notice in the composer"
            );
        }
        // The attempt is counted before its push ends, so once the inbox lets go of
        // the notice the count is final.
        let deadline = tokio::time::Instant::now() + HANG_GUARD;
        while self.entry.inbox().holds("report") {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the notice never left the inbox; screen text: {text}"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            self.entry.notice_attempts(),
            1,
            "one attempt delivers the notice"
        );
    }
}

/// A receiver whose dialog (`overlay_kind` is its trigger and style; it ends when
/// the file the test creates exists) comes up while the first notice is typed, with a
/// definition declaring `probe`; the child has already sent its report.
async fn start_receiver(tag: &str, probe: &str, overlay_kind: &str) -> Receiver {
    let dir = unique_temp_dir(tag);
    let dialog_gone = dir.join("dialog-gone");
    let h = Harness::start(tag, report_config(&dir)).await;
    let mut observer = h.connect().await;
    write_definition(&dir, "parent", "parent-ready", probe);
    let mut command =
        process::submit_child_command_with_clear_scope("parent-ready", 0, 0, 0, 0, "");
    command.push(format!(
        "{overlay_kind}:until|{}",
        dialog_gone
            .to_str()
            .expect("the dialog path must be valid UTF-8")
    ));
    command.push(APPROVAL_HEADING.to_string());
    let parent = open_delivering(&h, command, "parent").await;
    wait_until_status(&mut observer, &parent, WindowStatus::WaitingInput).await;

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
    match read_response(&mut child_conn).await {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }

    let entry = h.registry.get(&parent).expect("the receiver must exist");
    Receiver {
        h,
        parent,
        observer,
        entry,
        dialog_gone,
    }
}

#[tokio::test]
async fn a_notice_written_behind_a_dialog_is_submitted_by_the_same_attempt_once_the_dialog_is_gone()
{
    let receiver = start_receiver("notice-residue", SUBMIT_CHILD_PROBE, "body:hidden").await;
    receiver
        .wait_for_screen(
            APPROVAL_HEADING,
            "the notice never landed behind the dialog",
        )
        .await;
    assert!(
        !receiver.screen().contains("SUBMITTED "),
        "nothing may be submitted while the dialog is up"
    );
    receiver.remove_dialog();

    receiver
        .assert_submitted_once_and_clean(SUBMIT_CHILD_PROBE)
        .await;
    receiver.h.registry.terminate_all().await;
}

/// The dialog comes up after the notice text, at the first probe character typed
/// behind it, and swallows that character: the attempt must not leave it, or a
/// later one, behind the notice, and must submit the notice once.
#[tokio::test]
async fn a_probe_character_a_dialog_swallowed_does_not_stop_the_notice_from_being_submitted_once() {
    let probe = format!("{SUBMIT_CHILD_PROBE}{SECOND_PROBE_CHAR}");
    let first_byte = SUBMIT_CHILD_PROBE.as_bytes()[0];
    let mut receiver = start_receiver(
        "notice-swallowed-probe",
        &probe,
        &format!("paste-byte={first_byte:02x}:hidden"),
    )
    .await;
    receiver
        .wait_for_screen(
            APPROVAL_HEADING,
            "the dialog never came up behind the notice text",
        )
        .await;
    receiver
        .wait_for_screen(
            "DIALOG-INPUT",
            "the dialog never took the first probe character",
        )
        .await;
    let parent = receiver.parent.clone();
    wait_until_status(
        &mut receiver.observer,
        &parent,
        WindowStatus::WaitingApproval,
    )
    .await;
    receiver.remove_dialog();

    receiver.assert_submitted_once_and_clean(&probe).await;
    receiver.h.registry.terminate_all().await;
}
