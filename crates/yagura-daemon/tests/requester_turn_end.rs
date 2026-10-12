#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a CLI that drops its pending tool calls when its turn
//! ends and sends no cancellation.
//!
//! Such a requester's window is the only sign it left: it was `Running` while
//! the call was out and leaves for a state that accepts input (or ends) once
//! the call is dropped. A definition says so with `[requests]
//! abandoned_at_turn_end`; only then does that transition end a request that
//! is still waiting on its answer. Every case drives a real `IpcServer`:
//! the receiver is a `submit_child` that stops reading once the body arrives,
//! the requester is an `echo_child` whose status is set by what the test
//! prints into it (a status report cannot make a window `Interrupted`,
//! `Idle` or `Unknown`, a screen rule can). "Does not end" is shown with the
//! test's clock paused and an hour of it let pass, then by another writer's
//! byte ending the same wait; the hang guards only bound a failure.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncReadExt;

use common::{
    Harness, SubmitDefinitionInput, approver_base_config, call_list_windows,
    expect_approval_requested, expect_error, identify, let_every_clock_bound_run_out, policy,
    read_approver_reply, read_response, register_approver, send, unique_temp_dir,
    wait_until_composer_is, wait_until_input_log_contains, wait_until_status,
    write_submit_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{InputOrigin, WindowEntry};
use yagura_protocol::{
    ClientMessage, DaemonMessage, HandoffDelivery, WindowStatus, WithdrawReason,
};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

/// The longest any single step may take before the case is called hung.
const HANG_GUARD: Duration = Duration::from_secs(30);

/// What the requester window prints to take each status.
const RUNNING: &str = "REQ-RUNNING";
const WAITING_INPUT: &str = "REQ-WAITING-INPUT";
const WAITING_APPROVAL: &str = "REQ-WAITING-APPROVAL";
const INTERRUPTED: &str = "REQ-INTERRUPTED";
const IDLE: &str = "REQ-IDLE";
/// Matches no rule of the requester's definition.
const NO_RULE_MATCHES: &str = "REQ-NOTHING-KNOWN";

/// Typed into a window by another writer: it ends a delivery that waits.
const OTHER_WRITER: &[u8] = b"Q";

/// One change of the requester window's state.
#[derive(Clone, Copy, Debug)]
enum Step {
    Running,
    WaitingInput,
    WaitingApproval,
    Interrupted,
    Idle,
    Unknown,
    /// The window is closed (its status becomes `Exited`).
    Close,
}

impl Step {
    fn status(self) -> WindowStatus {
        match self {
            Step::Running => WindowStatus::Running,
            Step::WaitingInput => WindowStatus::WaitingInput,
            Step::WaitingApproval => WindowStatus::WaitingApproval,
            Step::Interrupted => WindowStatus::Interrupted,
            Step::Idle => WindowStatus::Idle,
            Step::Unknown => WindowStatus::Unknown,
            Step::Close => WindowStatus::Exited,
        }
    }

    fn printed(self) -> &'static str {
        match self {
            Step::Running => RUNNING,
            Step::WaitingInput => WAITING_INPUT,
            Step::WaitingApproval => WAITING_APPROVAL,
            Step::Interrupted => INTERRUPTED,
            Step::Idle => IDLE,
            Step::Unknown => NO_RULE_MATCHES,
            Step::Close => panic!("closing a window prints nothing"),
        }
    }
}

fn config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&[
            "send_message",
            "send_keys",
            "send_handoff",
            "close_window",
        ])),
        max_waiters_per_conn: 16,
        ..approver_base_config()
    }
}

/// A definition whose rules take the status from the last line the window
/// printed. `requests` is the whole `[requests]` table, or `None` to omit it.
fn write_requester_definition(dir: &Path, name: &str, requests: Option<&str>) {
    let requests = requests.unwrap_or_default();
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"schema_version = 1
name = "{name}"
bottom_line_count = 1

[debounce]
confirm_ms = 100

[[rules]]
status = "running"
pattern = '{RUNNING}'
priority = 1
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '{WAITING_INPUT}'
priority = 1
region = "bottom"

[[rules]]
status = "waiting_approval"
pattern = '{WAITING_APPROVAL}'
priority = 1
region = "bottom"

[[rules]]
status = "interrupted"
pattern = '{INTERRUPTED}'
priority = 1
region = "bottom"

[[rules]]
status = "idle"
pattern = '{IDLE}'
priority = 1
region = "bottom"

{requests}
"#
        ),
    )
    .expect("write the requester definition");
}

fn abandoned_table(value: bool) -> String {
    format!("[requests]\nabandoned_at_turn_end = {value}\n")
}

struct Stage {
    h: Harness,
    /// An external connection: watches statuses and closes the requester.
    observer: Connection,
    requester_window: String,
    receiver: String,
    input_log: PathBuf,
    resume: PathBuf,
}

impl Stage {
    fn receiver_entry(&self) -> Arc<WindowEntry> {
        self.h
            .registry
            .get(&self.receiver)
            .expect("the receiver must exist")
    }

    /// Takes the requester window to `step` and waits for the daemon to
    /// confirm it.
    async fn drive(&mut self, step: Step) {
        if let Step::Close = step {
            send(
                &mut self.observer,
                &ClientMessage::CloseWindow {
                    window_id: self.requester_window.clone(),
                    force: Some(true),
                },
            )
            .await;
            match read_response(&mut self.observer, HANG_GUARD).await {
                DaemonMessage::WindowClosed { .. } => {}
                other => panic!("expected window_closed, got {other:?}"),
            }
        } else {
            self.h
                .write_input(
                    &self.requester_window,
                    format!("{}\r\n", step.printed()).as_bytes(),
                )
                .await;
        }
        wait_until_status(
            &mut self.observer,
            &self.requester_window,
            step.status(),
            HANG_GUARD,
        )
        .await;
    }

    async fn drive_all(&mut self, steps: &[Step]) {
        for step in steps {
            self.drive(*step).await;
        }
    }
}

/// Builds the daemon, a requester window whose definition carries `requests`
/// (in `WaitingInput`), a receiver that stops reading once a body arrives,
/// and a connection identified as the requester.
async fn stage(tag: &str, requests: Option<&str>) -> (Stage, Connection) {
    let dir = unique_temp_dir(tag);
    let h = Harness::start(tag, config(&dir)).await;
    let mut observer = h.connect().await;

    write_requester_definition(&dir, "requester", requests);
    let requester_window = h
        .open(process::echo_child_command(), Some("requester"))
        .await;
    h.write_input(&requester_window, format!("{WAITING_INPUT}\r\n").as_bytes())
        .await;
    wait_until_status(
        &mut observer,
        &requester_window,
        WindowStatus::WaitingInput,
        HANG_GUARD,
    )
    .await;

    let input_log = dir.join("receiver-input.log");
    let resume = dir.join("resume");
    write_submit_definition(
        &dir,
        "receiver",
        "receiver-marker",
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear: "\\u0015",
            delivery_confirm_ms: 2000,
            resend_max: 0,
        },
    );
    let mut command = process::submit_child_command_with_composer("receiver-marker", 0, 0, 0, 0);
    command.push(String::new()); // fold_placeholder: none
    command.push(format!(
        "stall-until|{}|",
        resume
            .to_str()
            .expect("the resume path must be valid UTF-8")
    ));
    command.push(String::new()); // overlay: none
    command.push(String::new()); // overlay_heading
    command.push(
        input_log
            .to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    let receiver = h.open(command, Some("receiver")).await;
    wait_until_status(
        &mut observer,
        &receiver,
        WindowStatus::WaitingInput,
        HANG_GUARD,
    )
    .await;

    let mut requester = h.connect().await;
    identify(&mut requester, &requester_window).await;
    (
        Stage {
            h,
            observer,
            requester_window,
            receiver,
            input_log,
            resume,
        },
        requester,
    )
}

/// The requester asks for `BODY` to be sent to the receiver and the receiver
/// has stopped reading in the middle of it: the call is out, unanswered.
async fn start_send_message(stage: &Stage, requester: &mut Connection) {
    send(
        requester,
        &ClientMessage::SendText {
            window_id: stage.receiver.clone(),
            text: "BODY\r".to_string(),
        },
    )
    .await;
    stage
        .h
        .wait_for_scrollback(&stage.receiver, b"STALL-BEGAN", HANG_GUARD)
        .await;
}

/// The same, as a window-addressed handoff.
async fn start_handoff(stage: &Stage, requester: &mut Connection) {
    send(
        requester,
        &ClientMessage::SendHandoff {
            window_id: Some(stage.receiver.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/lib.rs".to_string(),
            note: "please look".to_string(),
        },
    )
    .await;
    stage
        .h
        .wait_for_scrollback(&stage.receiver, b"STALL-BEGAN", HANG_GUARD)
        .await;
}

async fn expect_pong(requester: &mut Connection) {
    send(requester, &ClientMessage::Ping).await;
    let pong = read_response(requester, HANG_GUARD).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "the connection must still be served and have nothing else queued, got {pong:?}"
    );
}

/// What follows once the daemon has answered a request whose requester left:
/// the receiver reads on, the body it was sent is taken back, no submit key
/// is ever written, and the next request on the same connection is the next
/// frame that connection gets (no second answer to the first).
async fn expect_withdrawn_without_a_submit_key_and_no_second_answer(
    stage: &Stage,
    requester: &mut Connection,
) {
    std::fs::write(&stage.resume, b"").expect("let the receiver read on");
    wait_until_input_log_contains(&stage.input_log, b"\x15", HANG_GUARD).await;
    wait_until_composer_is(
        &stage.h,
        &stage.receiver,
        &ComposerReadout::Empty,
        HANG_GUARD,
    )
    .await;

    // Granted only once the first request's delivery has let go of the window.
    send(
        requester,
        &ClientMessage::SendText {
            window_id: stage.receiver.clone(),
            text: "MARK".to_string(),
        },
    )
    .await;
    match read_response(requester, HANG_GUARD).await {
        DaemonMessage::TextSent { window_id } => assert_eq!(window_id, stage.receiver),
        other => panic!("the next frame must answer the next request, got {other:?}"),
    }
    let read = wait_until_input_log_contains(&stage.input_log, b"MARK", HANG_GUARD).await;
    assert!(
        !read.contains(&b'\r'),
        "no submit key may be written for a request whose requester left: {read:?}"
    );
    wait_until_composer_is(
        &stage.h,
        &stage.receiver,
        &ComposerReadout::Body("MARK".to_string()),
        HANG_GUARD,
    )
    .await;
}

/// Reads the next response without any clock, so that it can wait while the
/// test's clock is let run out.
async fn read_response_untimed(client: &mut Connection) -> DaemonMessage {
    loop {
        let mut len = [0u8; 4];
        client
            .read_exact(&mut len)
            .await
            .expect("the connection must stay open until the request is answered");
        let mut body = vec![0u8; u32::from_le_bytes(len) as usize];
        client
            .read_exact(&mut body)
            .await
            .expect("the connection must stay open while a frame is read");
        match yagura_protocol::decode_daemon_message(&body)
            .expect("the daemon must only emit well-formed frames")
        {
            DaemonMessage::Frame(_) | DaemonMessage::StatusChanged { .. } => continue,
            other => return other,
        }
    }
}

/// The requester's call is not ended by its window's `steps`: an hour of the
/// clock passes with it unanswered, and then another writer's byte ends it
/// (so nothing earlier did).
async fn expect_the_call_outlives(mut stage: Stage, requester: Connection, steps: &[Step]) {
    let answer = tokio::spawn(async move {
        let mut requester = requester;
        let answer = read_response_untimed(&mut requester).await;
        (requester, answer)
    });
    stage.drive_all(steps).await;
    let_every_clock_bound_run_out().await;
    assert!(
        !answer.is_finished(),
        "the request must still be waiting after {steps:?}"
    );

    stage
        .receiver_entry()
        .write_pty_input(OTHER_WRITER, InputOrigin::Interactive)
        .await
        .expect("another writer's byte must not fail");
    let (_requester, answer) = tokio::time::timeout(HANG_GUARD, answer)
        .await
        .expect("another writer's byte must end the wait")
        .expect("the reader must not panic");
    let message = expect_error(answer, "not_written");
    assert!(
        message.contains("another write reached the window"),
        "the wait must have ended on the other writer, not on the requester's states: {message}"
    );
}

// =====================================================================
// A requester that abandons its calls at the end of a turn
// =====================================================================

/// A requester (definition true) that was `Running` and is back to
/// `WaitingInput` is answered at once with `not_written`, on the connection
/// that is still open; its body is taken back and its submit key never typed.
#[tokio::test]
async fn a_send_message_is_answered_not_written_when_its_requester_goes_from_running_to_waiting_input()
 {
    let (mut stage, mut requester) = stage("turn-end-send", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    expect_error(
        read_response(&mut requester, HANG_GUARD).await,
        "not_written",
    );
    expect_pong(&mut requester).await;
    expect_withdrawn_without_a_submit_key_and_no_second_answer(&stage, &mut requester).await;
}

#[tokio::test]
async fn a_send_message_is_answered_not_written_when_its_requester_goes_from_running_to_interrupted()
 {
    let (mut stage, mut requester) =
        stage("turn-end-interrupted", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::Interrupted]).await;
    expect_error(
        read_response(&mut requester, HANG_GUARD).await,
        "not_written",
    );
    expect_pong(&mut requester).await;
    expect_withdrawn_without_a_submit_key_and_no_second_answer(&stage, &mut requester).await;
}

#[tokio::test]
async fn a_send_message_is_answered_not_written_when_its_requester_goes_from_running_to_idle() {
    let (mut stage, mut requester) = stage("turn-end-idle", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::Idle]).await;
    expect_error(
        read_response(&mut requester, HANG_GUARD).await,
        "not_written",
    );
    expect_pong(&mut requester).await;
    expect_withdrawn_without_a_submit_key_and_no_second_answer(&stage, &mut requester).await;
}

#[tokio::test]
async fn a_send_message_is_answered_not_written_when_its_requester_window_is_closed_after_running()
{
    let (mut stage, mut requester) = stage("turn-end-closed", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::Close]).await;
    expect_error(
        read_response(&mut requester, HANG_GUARD).await,
        "not_written",
    );
    expect_pong(&mut requester).await;
    expect_withdrawn_without_a_submit_key_and_no_second_answer(&stage, &mut requester).await;
}

/// A window-addressed handoff is ended the same way. It answers
/// `handoff_sent` with `delivery: not_written`, the answer a handoff gives
/// whenever its envelope was never submitted.
#[tokio::test]
async fn a_handoff_is_answered_not_written_when_its_requester_goes_from_running_to_waiting_input() {
    let (mut stage, mut requester) = stage("turn-end-handoff", Some(&abandoned_table(true))).await;
    start_handoff(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    match read_response(&mut requester, HANG_GUARD).await {
        DaemonMessage::HandoffSent {
            window_id,
            opened,
            delivery,
        } => {
            assert_eq!(window_id, stage.receiver);
            assert!(!opened, "the handoff was addressed to an open window");
            assert_eq!(delivery, HandoffDelivery::NotWritten);
        }
        other => panic!("expected handoff_sent with delivery not_written, got {other:?}"),
    }
    expect_pong(&mut requester).await;
    expect_withdrawn_without_a_submit_key_and_no_second_answer(&stage, &mut requester).await;
}

/// After the answer, the requester's window may do anything: nothing is
/// answered again, and the connection carries on.
#[tokio::test]
async fn a_requester_changing_state_after_its_send_message_was_answered_is_answered_only_once() {
    let (mut stage, mut requester) = stage("turn-end-once", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    expect_error(
        read_response(&mut requester, HANG_GUARD).await,
        "not_written",
    );
    stage
        .drive_all(&[Step::Running, Step::Interrupted, Step::Running, Step::Idle])
        .await;
    expect_pong(&mut requester).await;
    expect_withdrawn_without_a_submit_key_and_no_second_answer(&stage, &mut requester).await;
}

// =====================================================================
// What does not end a request
// =====================================================================

/// A definition that says nothing, or says `false`, keeps the call alive
/// through the same states: Claude Code returns to `WaitingInput` while its
/// call lives on, so the state alone must never end it.
#[tokio::test]
async fn a_requester_whose_definition_says_false_keeps_its_send_message_through_running_and_waiting_input()
 {
    let (stage, mut requester) = stage("turn-end-false", Some(&abandoned_table(false))).await;
    start_send_message(&stage, &mut requester).await;

    expect_the_call_outlives(stage, requester, &[Step::Running, Step::WaitingInput]).await;
}

#[tokio::test]
async fn a_requester_whose_definition_has_no_requests_table_keeps_its_send_message_through_running_and_waiting_input()
 {
    let (stage, mut requester) = stage("turn-end-omitted", None).await;
    start_send_message(&stage, &mut requester).await;

    expect_the_call_outlives(stage, requester, &[Step::Running, Step::WaitingInput]).await;
}

/// The approval dialog before a call is `WaitingApproval`, and the state after
/// it is `WaitingInput` once the call is back: without a `Running` in between
/// there was no turn to end.
#[tokio::test]
async fn a_send_message_is_kept_when_its_requester_goes_from_waiting_approval_to_waiting_input_without_running()
 {
    let (stage, mut requester) =
        stage("turn-end-approval-first", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    expect_the_call_outlives(
        stage,
        requester,
        &[Step::WaitingApproval, Step::WaitingInput],
    )
    .await;
}

/// A window that cannot be read (`Unknown`) says nothing about its turn: the
/// fail-safe state never ends a request.
#[tokio::test]
async fn a_send_message_is_kept_when_its_requester_goes_from_running_to_unknown() {
    let (stage, mut requester) = stage("turn-end-unknown", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    expect_the_call_outlives(stage, requester, &[Step::Running, Step::Unknown]).await;
}

#[tokio::test]
async fn a_send_message_is_kept_when_its_requester_goes_from_running_to_waiting_approval() {
    let (stage, mut requester) =
        stage("turn-end-approval-after", Some(&abandoned_table(true))).await;
    start_send_message(&stage, &mut requester).await;

    expect_the_call_outlives(stage, requester, &[Step::Running, Step::WaitingApproval]).await;
}

/// A request to open a window is still waiting for an approver when its
/// requester's CLI drops the call: the approval is withdrawn, the call is
/// answered at once on the open connection, and nothing is opened even though
/// the dialog it left behind could still have been answered.
#[tokio::test]
async fn an_open_window_waiting_for_an_approver_is_withdrawn_when_its_requester_ends_its_turn() {
    let (mut stage, mut requester) = stage("turn-end-open", Some(&abandoned_table(true))).await;
    let mut approver = stage.h.connect().await;
    register_approver(&mut approver).await;
    let before = call_list_windows(&mut stage.observer).await.len();

    send(
        &mut requester,
        &ClientMessage::OpenWindow {
            command: process::echo_child_command(),
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    expect_error(
        read_response(&mut requester, HANG_GUARD).await,
        "approval_timeout",
    );
    match read_approver_reply(&mut approver, HANG_GUARD).await {
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            assert_eq!(request_id, request.request_id);
            assert_eq!(reason, WithdrawReason::RequesterDisconnected);
        }
        other => panic!("expected approval_withdrawn, got {other:?}"),
    }
    expect_pong(&mut requester).await;
    assert_eq!(call_list_windows(&mut stage.observer).await.len(), before);
}
