#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a report notice that landed behind an approval dialog is
//! submitted by its own attempt once the dialog is gone, whatever the pending
//! count did meanwhile, and is never cleared, overwritten or submitted over
//! when someone else's text stands behind it.
//!
//! The receiver is a `submit_child` that draws an approval dialog the moment
//! the notice text lands and keeps it until the test removes it, so the
//! attempt waits with its notice unsent but already in the composer, and what
//! happens next is set up by the test.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, SUBMIT_CHILD_PROBE, approver_base_config, call_open_window, composer_section_toml,
    identify, policy, read_composer_now, read_response, send, unique_temp_dir,
    wait_until_composer_satisfies, wait_until_status,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{InputOrigin, WindowEntry};
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::{self, echo_child_command};

const APPROVAL_HEADING: &str = "NOTICE-APPROVAL";

const NOTICE_TIMEOUT: Duration = Duration::from_secs(20);

fn notice_for(pending: usize) -> String {
    format!("[yagura report] {pending} pending report(s). Call wait_for_report to read them.")
}

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
clear = "\u0015"
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

struct Scene {
    h: Harness,
    parent: String,
    child_conn: Connection,
    /// Created by the test to remove the dialog.
    dialog_gone: PathBuf,
}

impl Scene {
    fn entry(&self) -> Arc<WindowEntry> {
        self.h
            .registry
            .get(&self.parent)
            .expect("the receiver must exist")
    }

    fn remove_dialog(&self) {
        std::fs::write(&self.dialog_gone, b"").expect("remove the dialog");
    }

    /// Waits until the notice agent has finished `count` attempts in all.
    async fn wait_for_attempts(&self, count: u64) {
        let entry = self.entry();
        let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
        while entry.notice_attempts() < count {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the notice agent never finished attempt {count}; screen text: {}",
                self.h
                    .flattened_scrollback_keeping_osc_payload(&self.parent)
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }
}

async fn report(child_conn: &mut Connection) {
    send(
        child_conn,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: "done".to_string(),
            path: None,
        },
    )
    .await;
    match read_response(child_conn, Duration::from_secs(5)).await {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }
}

/// A receiver whose first attempt wrote its notice for one pending report and
/// waits behind the dialog, which is still drawn in `style` (`hidden` swallows
/// what is typed; `working` leaves the composer taking input).
async fn scene_with_first_notice_waiting(tag: &str, clear_scope: &str, style: &str) -> Scene {
    let dir = unique_temp_dir(tag);
    let dialog_gone = dir.join("dialog-gone");
    let h = Harness::start(tag, report_config(&dir)).await;
    let mut observer = h.connect().await;
    write_definition(&dir, "parent", "parent-ready");
    let mut command =
        process::submit_child_command_with_clear_scope("parent-ready", 0, 0, 0, 0, clear_scope);
    command.push(format!(
        "body:{style}:until|{}",
        dialog_gone
            .to_str()
            .expect("the dialog path must be valid UTF-8")
    ));
    command.push(APPROVAL_HEADING.to_string());
    let parent = open_delivering(&h, command, "parent").await;
    wait_until_status(
        &mut observer,
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

    report(&mut child_conn).await;
    let scene = Scene {
        h,
        parent,
        child_conn,
        dialog_gone,
    };
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while !scene
        .h
        .flattened_scrollback_keeping_osc_payload(&scene.parent)
        .contains(APPROVAL_HEADING)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the first attempt never met the dialog; screen text: {}",
            scene
                .h
                .flattened_scrollback_keeping_osc_payload(&scene.parent)
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        scene.entry().notice_attempts(),
        0,
        "the first attempt is still waiting behind the dialog, it has not finished"
    );
    scene
}

#[tokio::test]
async fn a_notice_written_before_the_pending_count_moved_is_submitted_as_it_was_written_never_merged_with_a_later_one()
 {
    let mut scene = scene_with_first_notice_waiting("notice-rewrite", "", "hidden").await;
    // The pending count moves while the first attempt's text is still in the
    // composer: the attempt submits the text it wrote, and the later count is
    // another notice's text.
    report(&mut scene.child_conn).await;
    scene.remove_dialog();

    let old = notice_for(1);
    let new = notice_for(2);
    let submitted = format!("SUBMITTED {old}");
    let deadline = tokio::time::Instant::now() + NOTICE_TIMEOUT;
    while !scene
        .h
        .flattened_scrollback_keeping_osc_payload(&scene.parent)
        .contains(&submitted)
    {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the notice the attempt had written never reached the receiver; \
             screen text: {}",
            scene
                .h
                .flattened_scrollback_keeping_osc_payload(&scene.parent)
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let text = scene
        .h
        .flattened_scrollback_keeping_osc_payload(&scene.parent);
    assert!(
        [
            format!("{old} {new}"),
            format!("{new} {old}"),
            format!("{old}{new}"),
            format!("{new}{old}"),
        ]
        .iter()
        .all(|merged| !text.contains(merged.as_str())),
        "the old and the new text must not be sent together"
    );
    scene.h.registry.terminate_all().await;
}

#[tokio::test]
async fn text_typed_behind_a_waiting_notice_is_neither_cleared_overwritten_nor_submitted_over() {
    let scene =
        scene_with_first_notice_waiting("notice-rewrite-foreign", "literal-clear", "working").await;
    let parent = scene.parent.clone();
    let old = notice_for(1);
    // The composer still takes input behind the heading, so what someone types
    // lands right behind the notice: another writer's byte ends the attempt.
    wait_until_composer_satisfies(
        &scene.h,
        &parent,
        "the notice in the composer",
        NOTICE_TIMEOUT,
        |readout| matches!(readout, ComposerReadout::Body(text) if text.starts_with(&old)),
    )
    .await;
    scene
        .entry()
        .write_pty_input(b"USER-TYPED", InputOrigin::Interactive)
        .await
        .expect("write user text");
    // A call that stepped aside returns only once the screen can be written to
    // again, so the dialog goes first.
    scene.remove_dialog();
    scene.wait_for_attempts(1).await;

    scene.wait_for_attempts(2).await;
    let text = scene.h.flattened_scrollback_keeping_osc_payload(&parent);
    // This receiver types the clear key out as `^U`, so the notice's take-back
    // shows where it reached the composer: in front of the other writer's text.
    let composer = match read_composer_now(&scene.h, &parent).await {
        ComposerReadout::Body(composer) => composer,
        other => panic!("the other writer's text must still stand in the composer, got {other:?}"),
    };
    let typed = composer
        .find("USER-TYPED")
        .unwrap_or_else(|| panic!("the other writer's text must be kept; composer: {composer}"));
    assert!(
        composer[..typed].contains("^U") && !composer[typed..].contains("^U"),
        "the notice's take-back must go in front of the other writer's text, never behind it; \
         composer: {composer}"
    );
    assert!(
        !text.contains("SUBMITTED "),
        "nothing may be submitted over someone else's text"
    );
    assert!(
        !text.contains(&notice_for(2)),
        "no second text may be written after what stands in the composer; screen text: {text}"
    );
    scene.h.registry.terminate_all().await;
}
