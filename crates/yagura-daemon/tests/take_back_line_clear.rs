#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a take-back must empty a composer whose clear key erases
//! one display line at a time, and must keep doing so when the receiver reads
//! nothing for a while and only then works through what was written to it.
//!
//! Every case drives a real `WindowRegistry` against `submit_child` in
//! composer-draw mode with a body long enough to wrap over several display
//! lines, and a requester that leaves once the body has reached the child, so
//! the window layer takes the body back. The receiver reads its composer back
//! correctly in each case; only what its clear key does differs, so nothing
//! here depends on how the composer is read.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, approver_base_config, let_every_clock_bound_run_out,
    open_submit_child_window_with_clear_scope, read_composer_now, settled_marker_count,
    settled_submitted_count, unique_temp_dir, wait_until_composer_is, wait_until_status,
    write_submit_definition_with_input_toml,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{
    NotPlaced, NotPlacedReason, SendStage, SubmitJudgment, SubmitPlacement, WindowEntry,
};
use yagura_protocol::WindowStatus;
use yagura_test_support::process;

fn detection_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        ..approver_base_config()
    }
}

fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// Must exceed the definition's fixed `confirm_ms` (30).
const SUBMIT_DELAY_MS: u64 = 120;

/// `stall-until|<resume>|line-clear`: a receiver that reads nothing until the
/// test creates `resume`.
fn stalled_line_clear(resume: &Path) -> String {
    format!(
        "stall-until|{}|line-clear",
        resume
            .to_str()
            .expect("the resume path must be valid UTF-8")
    )
}

fn line_clear_input_toml() -> String {
    format!(
        "bracketed_paste = true\n\
         ready_hold_ms = 0\n\
         submit_delay_ms = {SUBMIT_DELAY_MS}\n\
         clear = \"\\u0015\"\n\
         delivery_confirm_ms = 2000\n\
         resend_max = 2\n\
         probe = \"x\"\n"
    )
}

/// 70 characters without a space: three display lines in the child's composer.
fn long_body() -> String {
    "abcdefghij".repeat(7)
}

/// Starts a delivery of `body` whose requester can leave.
fn start_delivery(
    h: &Harness,
    window_id: &str,
    body: &str,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<yagura_daemon::pty::PtyResult<SubmitJudgment>>,
) {
    let (leave, requester) = tokio::sync::watch::channel(false);
    let sender = entry(h, window_id);
    let body = body.to_string();
    let call = tokio::spawn(async move {
        sender
            .submit_message(&body, None, SendStage::BeforeSubmitKey, Some(requester))
            .await
    });
    (leave, call)
}

/// Waits until the receiver has stopped reading with the body begun: the
/// delivery has written its body.
async fn wait_for_the_stall(h: &Harness, window_id: &str) {
    h.wait_for_scrollback(window_id, b"STALL-BEGAN", Duration::from_secs(10))
        .await;
}

fn not_placed(result: &SubmitJudgment, what: &str) -> NotPlaced {
    match &result.placement {
        SubmitPlacement::NotPlaced(not_placed) => *not_placed,
        other => panic!("{what}, got {other:?}"),
    }
}

#[tokio::test]
async fn a_take_back_empties_a_composer_whose_clear_key_erases_one_display_line_at_a_time() {
    let dir = unique_temp_dir("line-clear-empties");
    let h = Harness::start("line-clear-empties", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "empties",
        "empties-marker",
        &line_clear_input_toml(),
        0,
        "held:line-clear",
    )
    .await;
    let body = long_body();

    let (leave, call) = start_delivery(&h, &window_id, &body);
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    leave.send(true).expect("the call holds its receiver");
    let result = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");

    let declined = not_placed(&result, "a body that never showed must not be submitted");
    assert_eq!(declined.reason, NotPlacedReason::NotReflected);
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty,
        "the take-back must leave the receiver's composer empty, however many display \
         lines the body wrapped over"
    );
    assert!(
        !declined.envelope_remains,
        "with the composer empty the sender must be told nothing remains"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0,
        "nothing may reach the receiver as a submission"
    );
}

#[tokio::test]
async fn a_take_back_queued_behind_a_stall_empties_the_composer_once_the_receiver_resumes() {
    let dir = unique_temp_dir("line-clear-stall");
    let resume = dir.join("resume");
    let h = Harness::start("line-clear-stall", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "stall",
        "stall-marker",
        &line_clear_input_toml(),
        0,
        &stalled_line_clear(&resume),
    )
    .await;
    let body = long_body();

    let (leave, call) = start_delivery(&h, &window_id, &body);
    wait_for_the_stall(&h, &window_id).await;
    leave.send(true).expect("the call holds its receiver");
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a take-back the receiver has not read yet is waited on, not given up on by a clock"
    );

    std::fs::write(&resume, b"").expect("let the receiver resume");
    let result = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    // The receiver works through the body, the take-back and the probe in the
    // order they were written: the take-back is long enough to empty what the
    // body painted, so only the probe is left, and the call erases it before
    // it answers.
    let declined = not_placed(&result, "a body still unread must not be submitted");
    assert_eq!(declined.reason, NotPlacedReason::NotReflected);
    assert!(
        !declined.envelope_remains,
        "the take-back was confirmed by the probe showing alone, so nothing remains"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "STALL-ENDED", 1).await,
        1,
        "the receiver must resume"
    );
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Empty,
        Duration::from_secs(30),
    )
    .await;
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0
    );
}

#[tokio::test]
async fn a_second_attempt_after_a_stalled_take_back_writes_the_body_only_once() {
    let dir = unique_temp_dir("line-clear-retry");
    let resume = dir.join("resume");
    let h = Harness::start("line-clear-retry", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "retry",
        "retry-marker",
        &line_clear_input_toml(),
        0,
        &stalled_line_clear(&resume),
    )
    .await;
    let body = long_body();

    let (leave, call) = start_delivery(&h, &window_id, &body);
    wait_for_the_stall(&h, &window_id).await;
    leave.send(true).expect("the call holds its receiver");
    std::fs::write(&resume, b"").expect("let the receiver resume");
    let first = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    assert_eq!(
        settled_marker_count(&h, &window_id, "STALL-ENDED", 1).await,
        1,
        "the receiver must resume"
    );
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Empty,
        Duration::from_secs(30),
    )
    .await;

    let second = entry(&h, &window_id)
        .submit_message(
            &body,
            first
                .residue_write_seq
                .map(|write_seq| yagura_daemon::window::ComposerResidue {
                    body: &body,
                    write_seq,
                }),
            SendStage::BeforeSubmitKey,
            None,
        )
        .await
        .expect("write must not fail at the PTY level");

    match second.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!(
            "the first attempt took its body back and left the composer empty, so the second \
             attempt must write and submit the body, got {other:?}"
        ),
    }
    assert_eq!(
        // Ends at `\r`, the start of the line end on every platform (a Unix
        // PTY turns the child's `\r\n` into `\r\r\n`), so a body written
        // twice in a row still fails to match.
        settled_submitted_count(&h, &window_id, &body, 1).await,
        1,
        "the receiver must be handed the body exactly once, not twice in a row"
    );
}

/// A receiver that reads nothing until the test lets it, then ignores the
/// clear key entirely, and whose composer folds a long body into a
/// placeholder: nothing on screen ever shows the take-back took effect, so
/// the sender must be told something may remain.
#[tokio::test]
async fn a_clear_key_the_receiver_ignores_leaves_the_sender_told_something_may_remain() {
    let dir = unique_temp_dir("line-clear-ignored");
    let resume = dir.join("resume");
    let h = Harness::start("line-clear-ignored", detection_config(&dir)).await;
    let mut client = h.connect().await;
    write_submit_definition_with_input_toml(
        &dir,
        "ignored",
        "ignored-marker",
        &line_clear_input_toml(),
    );
    let mut command = process::submit_child_command_with_folded_composer(
        "ignored-marker",
        0,
        0,
        0,
        0,
        "[folded]",
    );
    command.push(format!(
        "stall-until|{}|ignore-clear",
        resume
            .to_str()
            .expect("the resume path must be valid UTF-8")
    ));
    let window_id = h.open(command, Some("ignored")).await;
    wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    let body = long_body();

    let (leave, call) = start_delivery(&h, &window_id, &body);
    wait_for_the_stall(&h, &window_id).await;
    leave.send(true).expect("the call holds its receiver");
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a take-back the receiver has not read yet is waited on, not given up on by a clock"
    );
    std::fs::write(&resume, b"").expect("let the receiver resume");
    let result = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");

    let declined = not_placed(
        &result,
        "a take-back that leaves something unreadable behind must not submit",
    );
    assert_eq!(declined.reason, NotPlacedReason::NotReflected);
    assert!(
        declined.envelope_remains,
        "the composer still holds something after the take-back, so the sender must be told \
         it may remain"
    );
    assert_eq!(result.residue_write_seq, None);
    assert_eq!(
        settled_marker_count(&h, &window_id, "STALL-ENDED", 1).await,
        1
    );

    let write_seq_after_first = entry(&h, &window_id).write_seq();
    let retry = tokio::time::timeout(
        Duration::from_secs(20),
        entry(&h, &window_id).submit_message(&body, None, SendStage::BeforeSubmitKey, None),
    )
    .await
    .expect("a retry over what the take-back could not remove must answer, not wait")
    .expect("write must not fail at the PTY level");
    assert!(
        matches!(retry.placement, SubmitPlacement::NotPlaced(_)),
        "a retry over what the take-back could not remove must not submit"
    );
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        write_seq_after_first,
        "the body must not be written a second time on top of what may remain"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0
    );
}
