#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a CLI that drops its pending tool calls when its turn
//! ends and sends no cancellation.
//!
//! Such a requester's window is the only sign it left: it was `Running` while
//! the call was out and leaves for a state that accepts input (or ends) once
//! the call is dropped. A definition says so with `[requests]
//! abandoned_at_turn_end`; only then does that transition end a request that
//! is still waiting on its answer. Every case drives a real `IpcServer`:
//! the requester is an `echo_child` whose status is set by what the test
//! prints into it (a status report cannot make a window `Interrupted`,
//! `Idle` or `Unknown`, a screen rule can), and the call it leaves out is a
//! `wait_for_report` no clock of the test ends. "Does not end" is shown with
//! the test's clock paused and an hour of it let pass, then by a report ending
//! the same wait; the hang guards only bound a failure.

use crate::common;

use std::path::{Path, PathBuf};
use std::sync::Arc;

use tokio::io::AsyncReadExt;

use common::{
    Harness, SubmitDefinitionInput, approver_base_config, call_list_windows, decide,
    expect_approval_requested, expect_error, identify, let_every_clock_bound_run_out, policy,
    read_approver_reply, read_response, register_approver, send, unique_temp_dir,
    wait_until_input_log_contains, wait_until_status, write_submit_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::window::WindowEntry;
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome, WindowStatus, WithdrawReason};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

use yagura_test_support::HANG_GUARD;

/// What the requester window prints to take each status.
const RUNNING: &str = "REQ-RUNNING";
const WAITING_INPUT: &str = "REQ-WAITING-INPUT";
const WAITING_APPROVAL: &str = "REQ-WAITING-APPROVAL";
const INTERRUPTED: &str = "REQ-INTERRUPTED";
const IDLE: &str = "REQ-IDLE";
/// Matches no rule of the requester's definition.
const NO_RULE_MATCHES: &str = "REQ-NOTHING-KNOWN";

/// The `wait_for_report` a requester leaves out: longer than the hour of the
/// clock a "does not end" case lets pass, so only an event can end it.
const LONG_WAIT_SEC: u64 = 7200;

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
        wait_timeout_max_secs: LONG_WAIT_SEC,
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
            match read_response(&mut self.observer).await {
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
        wait_until_status(&mut self.observer, &self.requester_window, step.status()).await;
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
    wait_until_status(&mut observer, &requester_window, WindowStatus::WaitingInput).await;

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
    let receiver = h.open_delivering(command, Some("receiver")).await;
    wait_until_status(&mut observer, &receiver, WindowStatus::WaitingInput).await;

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

/// The requester opens a child (an approver lets it) and leaves a
/// `wait_for_report` for its reports out, unanswered. Returns the child.
async fn start_wait_for_report(stage: &Stage, requester: &mut Connection) -> String {
    let mut approver = stage.h.connect().await;
    register_approver(&mut approver).await;
    send(
        requester,
        &ClientMessage::OpenWindow {
            command: process::echo_child_command(),
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    decide(&mut approver, &request.request_id, true).await;
    let child = match read_response(requester).await {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected window_opened, got {other:?}"),
    };
    // Frames on one connection are handled in order: the pong proves the wait
    // is registered.
    send(
        requester,
        &ClientMessage::WaitForReport {
            timeout_sec: LONG_WAIT_SEC,
        },
    )
    .await;
    expect_pong(requester).await;
    child
}

/// `child` reports `summary` to its opener.
async fn report(stage: &Stage, child: &str, summary: &str) {
    let mut reporter = stage.h.connect().await;
    identify(&mut reporter, child).await;
    send(
        &mut reporter,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: None,
        },
    )
    .await;
    match read_response(&mut reporter).await {
        DaemonMessage::ReportSent { delivered_to, .. } => {
            assert_eq!(
                delivered_to.as_deref(),
                Some(stage.requester_window.as_str())
            );
        }
        other => panic!("expected report_sent, got {other:?}"),
    }
}

/// The requester's wait ended at its turn's end with nothing taken, and the
/// next request on the same connection is the next frame it gets (no second
/// answer to the first).
async fn expect_ended_empty_and_no_second_answer(requester: &mut Connection) {
    match read_response(requester).await {
        DaemonMessage::Reports { reports, .. } => assert!(reports.is_empty(), "{reports:?}"),
        other => panic!("expected an empty reports answer, got {other:?}"),
    }
    expect_pong(requester).await;
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
        .wait_for_scrollback(&stage.receiver, b"STALL-BEGAN")
        .await;
}

async fn expect_pong(requester: &mut Connection) {
    send(requester, &ClientMessage::Ping).await;
    let pong = read_response(requester).await;
    assert!(
        matches!(pong, DaemonMessage::Pong),
        "the connection must still be served and have nothing else queued, got {pong:?}"
    );
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
/// clock passes with it unanswered, and then a report from `child` ends it
/// (so nothing earlier did).
async fn expect_the_call_outlives(
    mut stage: Stage,
    requester: Connection,
    child: &str,
    steps: &[Step],
) {
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

    report(&stage, child, "OUTLIVED").await;
    let (_requester, answer) = tokio::time::timeout(HANG_GUARD, answer)
        .await
        .expect("the report must end the wait")
        .expect("the reader must not panic");
    match answer {
        DaemonMessage::Reports { reports, .. } => {
            assert_eq!(
                reports
                    .iter()
                    .map(|r| r.summary.as_str())
                    .collect::<Vec<_>>(),
                ["OUTLIVED"],
                "the wait must have ended on the report, not on the requester's states"
            );
        }
        other => panic!("expected the report on the wait still out, got {other:?}"),
    }
}

// =====================================================================
// A requester that abandons its calls at the end of a turn
// =====================================================================

/// A requester (definition true) that was `Running` and is interrupted, goes
/// idle or is closed has dropped its call: the call is answered at once with
/// nothing taken, on the connection that is still open. (`Running` then
/// `WaitingInput` is the `wait_for_report` case below.)
#[tokio::test]
async fn a_wait_for_report_ends_empty_when_its_requester_goes_from_running_to_interrupted() {
    let (mut stage, mut requester) =
        stage("turn-end-interrupted", Some(&abandoned_table(true))).await;
    start_wait_for_report(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::Interrupted]).await;
    expect_ended_empty_and_no_second_answer(&mut requester).await;
}

#[tokio::test]
async fn a_wait_for_report_ends_empty_when_its_requester_goes_from_running_to_idle() {
    let (mut stage, mut requester) = stage("turn-end-idle", Some(&abandoned_table(true))).await;
    start_wait_for_report(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::Idle]).await;
    expect_ended_empty_and_no_second_answer(&mut requester).await;
}

#[tokio::test]
async fn a_wait_for_report_ends_empty_when_its_requester_window_is_closed_after_running() {
    let (mut stage, mut requester) = stage("turn-end-closed", Some(&abandoned_table(true))).await;
    start_wait_for_report(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::Close]).await;
    expect_ended_empty_and_no_second_answer(&mut requester).await;
}

/// A handoff the receiver's CLI is already carrying is not taken back when its
/// requester's turn ends: the typing finishes, so the body is submitted and
/// never cleared from the composer, and the handoff is answered once. The call
/// started no attempt of its own (the receiver wakes by none), so the answer
/// says the envelope waits: never that it was not delivered.
#[tokio::test]
async fn a_carried_handoff_is_not_taken_back_when_its_requester_goes_from_running_to_waiting_input()
{
    let (mut stage, mut requester) = stage("turn-end-handoff", Some(&abandoned_table(true))).await;
    start_handoff(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    std::fs::write(&stage.resume, b"").expect("let the receiver read on");
    let answer = read_response(&mut requester).await;
    match &answer {
        DaemonMessage::HandoffSent {
            window_id, opened, ..
        } => {
            assert_eq!(*window_id, stage.receiver);
            assert!(!opened, "the handoff was addressed to an open window");
        }
        other => panic!("expected handoff_sent with the carrier's result, got {other:?}"),
    }
    assert_eq!(
        crate::send_message_lifecycle::state_of(&answer).as_deref(),
        Some("queued"),
        "a carried handoff is never answered undelivered: {answer:?}"
    );
    let read = wait_until_input_log_contains(&stage.input_log, b"\r").await;
    assert!(
        !read.contains(&0x15),
        "a carried handoff must not be cleared from the composer: {read:?}"
    );
    assert_eq!(stage.receiver_entry().inbox().pending_count(), 0);
    expect_pong(&mut requester).await;
}

/// A `wait_for_status` the requester dropped at the end of its turn ends then,
/// as if its time had run out, instead of holding the connection until it does.
#[tokio::test]
async fn a_wait_for_status_ends_as_timed_out_when_its_requester_goes_from_running_to_waiting_input()
{
    let (mut stage, mut requester) =
        stage("turn-end-wait-status", Some(&abandoned_table(true))).await;
    send(
        &mut requester,
        &ClientMessage::WaitForStatus {
            window_id: stage.receiver.clone(),
            target_status: WindowStatus::Exited,
            timeout_sec: 300,
        },
    )
    .await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    match read_response(&mut requester).await {
        DaemonMessage::WaitResult {
            window_id, outcome, ..
        } => {
            assert_eq!(window_id, stage.receiver);
            assert_eq!(outcome, "timeout");
        }
        other => panic!("expected a timed-out wait_result, got {other:?}"),
    }
    expect_pong(&mut requester).await;
}

/// A `wait_for_report` the requester dropped at the end of its turn ends then
/// with nothing taken, so a report arriving later waits for the next call
/// instead of going to a call no one reads.
#[tokio::test]
async fn a_wait_for_report_ends_empty_when_its_requester_goes_from_running_to_waiting_input() {
    let (mut stage, mut requester) =
        stage("turn-end-wait-report", Some(&abandoned_table(true))).await;
    let child = start_wait_for_report(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    expect_ended_empty_and_no_second_answer(&mut requester).await;

    report(&stage, &child, "after the turn").await;
    send(
        &mut requester,
        &ClientMessage::WaitForReport { timeout_sec: 0 },
    )
    .await;
    match read_response(&mut requester).await {
        DaemonMessage::Reports { reports, .. } => {
            assert_eq!(reports.len(), 1, "{reports:?}");
            assert_eq!(reports[0].summary, "after the turn");
        }
        other => panic!("expected the later report on the next call, got {other:?}"),
    }
}

/// After the answer, the requester's window may do anything: nothing is
/// answered again, and the connection carries on.
#[tokio::test]
async fn a_requester_changing_state_after_its_wait_for_report_was_answered_is_answered_only_once() {
    let (mut stage, mut requester) = stage("turn-end-once", Some(&abandoned_table(true))).await;
    start_wait_for_report(&stage, &mut requester).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    match read_response(&mut requester).await {
        DaemonMessage::Reports { reports, .. } => assert!(reports.is_empty(), "{reports:?}"),
        other => panic!("expected an empty reports answer, got {other:?}"),
    }
    stage
        .drive_all(&[Step::Running, Step::Interrupted, Step::Running, Step::Idle])
        .await;
    expect_pong(&mut requester).await;
}

// =====================================================================
// What does not end a request
// =====================================================================

/// A definition that says nothing, or says `false`, keeps the call alive
/// through the same states: Claude Code returns to `WaitingInput` while its
/// call lives on, so the state alone must never end it.
#[tokio::test]
async fn a_requester_whose_definition_says_false_keeps_its_wait_for_report_through_running_and_waiting_input()
 {
    let (stage, mut requester) = stage("turn-end-false", Some(&abandoned_table(false))).await;
    let child = start_wait_for_report(&stage, &mut requester).await;

    expect_the_call_outlives(
        stage,
        requester,
        &child,
        &[Step::Running, Step::WaitingInput],
    )
    .await;
}

#[tokio::test]
async fn a_requester_whose_definition_has_no_requests_table_keeps_its_wait_for_report_through_running_and_waiting_input()
 {
    let (stage, mut requester) = stage("turn-end-omitted", None).await;
    let child = start_wait_for_report(&stage, &mut requester).await;

    expect_the_call_outlives(
        stage,
        requester,
        &child,
        &[Step::Running, Step::WaitingInput],
    )
    .await;
}

/// The approval dialog before a call is `WaitingApproval`, and the state after
/// it is `WaitingInput` once the call is back: without a `Running` in between
/// there was no turn to end.
#[tokio::test]
async fn a_wait_for_report_is_kept_when_its_requester_goes_from_waiting_approval_to_waiting_input_without_running()
 {
    let (stage, mut requester) =
        stage("turn-end-approval-first", Some(&abandoned_table(true))).await;
    let child = start_wait_for_report(&stage, &mut requester).await;

    expect_the_call_outlives(
        stage,
        requester,
        &child,
        &[Step::WaitingApproval, Step::WaitingInput],
    )
    .await;
}

/// A window that cannot be read (`Unknown`) says nothing about its turn: the
/// fail-safe state never ends a request.
#[tokio::test]
async fn a_wait_for_report_is_kept_when_its_requester_goes_from_running_to_unknown() {
    let (stage, mut requester) = stage("turn-end-unknown", Some(&abandoned_table(true))).await;
    let child = start_wait_for_report(&stage, &mut requester).await;

    expect_the_call_outlives(stage, requester, &child, &[Step::Running, Step::Unknown]).await;
}

#[tokio::test]
async fn a_wait_for_report_is_kept_when_its_requester_goes_from_running_to_waiting_approval() {
    let (stage, mut requester) =
        stage("turn-end-approval-after", Some(&abandoned_table(true))).await;
    let child = start_wait_for_report(&stage, &mut requester).await;

    expect_the_call_outlives(
        stage,
        requester,
        &child,
        &[Step::Running, Step::WaitingApproval],
    )
    .await;
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
            on_name_taken: Default::default(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    stage.drive_all(&[Step::Running, Step::WaitingInput]).await;
    expect_error(read_response(&mut requester).await, "approval_timeout");
    match read_approver_reply(&mut approver).await {
        DaemonMessage::ApprovalWithdrawn { request_id, reason } => {
            assert_eq!(request_id, request.request_id);
            assert_eq!(reason, WithdrawReason::RequesterDisconnected);
        }
        other => panic!("expected approval_withdrawn, got {other:?}"),
    }
    expect_pong(&mut requester).await;
    assert_eq!(call_list_windows(&mut stage.observer).await.len(), before);
}
