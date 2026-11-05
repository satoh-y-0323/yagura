#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the one-line notice the daemon types into a parent
//! window's input once reports are waiting in its inbox (NT-*). The parent is
//! a `submit_child` fixture whose status the test pins through the
//! structured-source path (`status_event`), so every state a case depends on
//! is set explicitly and observed before the case moves on. Never a real
//! vendor CLI, never a running daemon.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    Harness, SubmitDefinitionInput, VALID_TOKEN, approver_base_config, call_open_window, identify,
    let_every_clock_bound_run_out, open_submit_child_window_with_composer, policy, read_response,
    register_approver, send, text_tail, unique_temp_dir, wait_until_status,
};

use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::TURN_ACTIVE_QUIET;
use yagura_protocol::{ClientMessage, DaemonMessage, ReceivedReport, ReportOutcome, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process::echo_child_command;

/// The parent's `[input] ready_hold_ms`: how long the notice path must see
/// the parent hold a status that accepts input before it writes.
const READY_HOLD_MS: u64 = 200;

/// How far the stopped clock is moved while a parent reported `running` is
/// watched: past every clock-bound step the notice path takes before its
/// first write (the ready hold), yet short of the quiet time after which the
/// detector itself closes the reported turn and the parent may be typed into.
const RUNNING_WATCH: Duration = Duration::from_millis(READY_HOLD_MS * 5);
const _: () = assert!(RUNNING_WATCH.as_millis() < TURN_ACTIVE_QUIET.as_millis());

const PARENT_MARKER: &str = "nt-parent-ready";

/// The notice text, built from the fixed wording rather than taken from the
/// product, so the product cannot move the expectation along with itself.
fn expected_notice(pending: usize) -> String {
    format!("[yagura report] {pending} pending report(s). Call wait_for_report to read them.")
}

fn notice_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["open_window"])),
        ..approver_base_config()
    }
}

/// No playbook, other than letting the scene open its windows: every other
/// gated tool would need a human decision.
fn approval_required_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["open_window"])),
        playbook: None,
        ..approver_base_config()
    }
}

fn parent_input() -> SubmitDefinitionInput {
    SubmitDefinitionInput {
        bracketed_paste: false,
        ready_hold_ms: READY_HOLD_MS,
        submit_delay_ms: 50,
        clear: "",
        delivery_confirm_ms: 0,
        resend_max: 0,
    }
}

/// A parent window (a `submit_child` that prints `SUBMITTED <line>` for every
/// line written into it), a connection identified as that parent, a child
/// window the parent opened (so the parent is the child's opener), a
/// connection identified as the child, and a probe connection for status.
struct Scene {
    h: Harness,
    parent: String,
    parent_conn: Connection,
    child_conn: Connection,
    probe: Connection,
}

async fn scene(tag: &str, config_for: fn(&Path) -> DaemonConfig) -> Scene {
    scene_ready_on(tag, config_for, PARENT_MARKER).await
}

/// [`scene`] whose parent's ready pattern is `marker` (see
/// [`common::ready_pattern_with_redraw`]).
async fn scene_ready_on(tag: &str, config_for: fn(&Path) -> DaemonConfig, marker: &str) -> Scene {
    let dir = unique_temp_dir(tag);
    let h = Harness::start(tag, config_for(&dir)).await;
    let mut probe = h.connect().await;
    let parent = open_submit_child_window_with_composer(
        &h,
        &mut probe,
        &dir,
        "nt-parent",
        marker,
        0,
        0,
        0,
        parent_input(),
        0,
    )
    .await;
    let mut parent_conn = h.connect().await;
    identify(&mut parent_conn, &parent).await;
    let child = call_open_window(&mut parent_conn, echo_child_command()).await;
    let mut child_conn = h.connect().await;
    identify(&mut child_conn, &child).await;
    Scene {
        h,
        parent,
        parent_conn,
        child_conn,
        probe,
    }
}

/// Sends a hook report for `window_id` and waits until the daemon reports
/// that status (the screen must agree, or the window must have no screen
/// reading the report could be overruled by).
async fn pin_status(probe: &mut Connection, window_id: &str, status: WindowStatus) {
    send(
        probe,
        &ClientMessage::StatusEvent {
            source: "notice-test".to_string(),
            status,
            window_id: Some(window_id.to_string()),
            token: VALID_TOKEN.to_string(),
        },
    )
    .await;
    wait_until_status(probe, window_id, status).await;
}

async fn report(child_conn: &mut Connection, summary: &str, path: Option<&str>) {
    send(
        child_conn,
        &ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: summary.to_string(),
            path: path.map(str::to_string),
        },
    )
    .await;
    match read_response(child_conn).await {
        DaemonMessage::ReportSent { .. } => {}
        other => panic!("expected report_sent, got {other:?}"),
    }
}

async fn drain_inbox(parent_conn: &mut Connection) -> Vec<ReceivedReport> {
    send(
        parent_conn,
        &ClientMessage::WaitForReport { timeout_sec: 0 },
    )
    .await;
    match read_response(parent_conn).await {
        DaemonMessage::Reports {
            reports,
            awaiting: _,
        } => reports,
        other => panic!("expected reports, got {other:?}"),
    }
}

// --- reading what reached the parent's input ------------------------------------

async fn wait_for_submitted(h: &Harness, window_id: &str, text: &str, times: usize) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    loop {
        if h.submitted_count(window_id, text) >= times {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for the parent to accept {text:?} {times} time(s); \
                 accepted lines so far: {}, flattened scrollback tail: {:?}",
                h.submitted_count(window_id, ""),
                text_tail(&h.flattened_scrollback(window_id)),
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// The parent's write count once its delivery agent has finished exactly
/// `attempts` notice attempts: an attempt is counted only after its own
/// writes are, so the count read here holds all of them.
async fn writes_after_attempts(h: &Harness, window_id: &str, attempts: u64) -> u64 {
    let entry = h.registry.get(window_id).expect("the parent is registered");
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while entry.notice_attempts() < attempts {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for notice attempt {attempts}; finished so far: {}",
            entry.notice_attempts()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert_eq!(
        entry.notice_attempts(),
        attempts,
        "the parent's delivery agent made more notice attempts than expected"
    );
    entry.write_seq()
}

/// Lets every clock-bound step the notice path could still take (ready hold,
/// submit delay, a declined attempt's retry) run out, then fails if anything
/// was written into the parent since its count read `writes`, or another
/// notice attempt was made. A write is counted before the clock can move on,
/// so nothing the agent does within the clock's run is missed.
async fn assert_nothing_more_written(
    h: &Harness,
    window_id: &str,
    writes: u64,
    attempts: u64,
    what: &str,
) {
    let_every_clock_bound_run_out().await;
    assert_written_since(h, window_id, writes, attempts, what);
}

fn assert_written_since(h: &Harness, window_id: &str, writes: u64, attempts: u64, what: &str) {
    let entry = h.registry.get(window_id).expect("the parent is registered");
    assert_eq!(
        entry.write_seq(),
        writes,
        "{what}: something was written into the parent; flattened scrollback tail: {:?}",
        text_tail(&h.flattened_scrollback(window_id)),
    );
    assert_eq!(
        entry.notice_attempts(),
        attempts,
        "{what}: the parent's delivery agent made another notice attempt"
    );
}

/// Moves the stopped clock by `span`, so every step due within it runs.
async fn let_the_clock_run_for(span: Duration) {
    tokio::time::pause();
    tokio::time::sleep(span).await;
    tokio::time::resume();
}

// --- One report to a ready parent ----------------------------------------------

#[tokio::test]
async fn a_report_to_a_parent_that_accepts_input_types_the_notice_exactly_once() {
    let mut s = scene("notice-ready-once", notice_config).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    report(&mut s.child_conn, "done", None).await;

    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 1).await;
    // Once the notice is typed nothing is due, so the agent must neither
    // write nor attempt again, whatever the clock does.
    let writes = writes_after_attempts(&s.h, &s.parent, 1).await;
    assert_nothing_more_written(&s.h, &s.parent, writes, 1, "after the first notice").await;
    assert_eq!(s.h.submitted_count(&s.parent, ""), 1);
    s.h.registry.terminate_all().await;
}

// --- A running parent is notified once it accepts input again ------------------

#[tokio::test]
async fn reports_to_a_running_parent_are_announced_once_it_accepts_input_with_the_count_then() {
    let mut s = scene("notice-running-then-ready", notice_config).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::Running).await;
    let before = writes_after_attempts(&s.h, &s.parent, 0).await;

    report(&mut s.child_conn, "first", None).await;
    report(&mut s.child_conn, "second", None).await;
    let_the_clock_run_for(RUNNING_WATCH).await;
    assert_written_since(&s.h, &s.parent, before, 0, "while the parent is running");

    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;
    wait_for_submitted(&s.h, &s.parent, &expected_notice(2), 1).await;
    assert_eq!(s.h.submitted_count(&s.parent, ""), 1);
    let writes = writes_after_attempts(&s.h, &s.parent, 1).await;
    assert_nothing_more_written(&s.h, &s.parent, writes, 1, "after the notice").await;
    s.h.registry.terminate_all().await;
}

// --- An Unknown parent is never typed into -------------------------------------

#[tokio::test]
async fn reports_to_a_parent_in_unknown_are_never_announced() {
    let mut s = scene("notice-unknown", notice_config).await;
    // The fixture's way out of a ready status: filler that pushes the marker
    // out of the detection window, with the input box left empty.
    s.h.write_input(&s.parent, &[0x03]).await;
    wait_until_status(&mut s.probe, &s.parent, WindowStatus::Unknown).await;
    let before = writes_after_attempts(&s.h, &s.parent, 0).await;

    report(&mut s.child_conn, "first", None).await;
    report(&mut s.child_conn, "second", None).await;

    assert_nothing_more_written(&s.h, &s.parent, before, 0, "while the parent is Unknown").await;
    s.h.registry.terminate_all().await;
}

// --- A report handed straight to a waiting parent ------------------------------

#[tokio::test]
async fn a_report_handed_to_a_waiting_wait_for_report_is_never_announced() {
    let mut s = scene("notice-direct-handoff", notice_config).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    send(
        &mut s.parent_conn,
        &ClientMessage::WaitForReport { timeout_sec: 30 },
    )
    .await;
    // Frames on one connection are handled in order, so the pong proves the
    // wait above is already parked before the report is sent.
    send(&mut s.parent_conn, &ClientMessage::Ping).await;
    match read_response(&mut s.parent_conn).await {
        DaemonMessage::Pong => {}
        other => panic!("expected pong, got {other:?}"),
    }
    let before = writes_after_attempts(&s.h, &s.parent, 0).await;

    report(&mut s.child_conn, "handed over", None).await;
    match read_response(&mut s.parent_conn).await {
        DaemonMessage::Reports {
            reports,
            awaiting: _,
        } => {
            assert_eq!(reports.len(), 1);
            assert_eq!(reports[0].summary, "handed over");
        }
        other => panic!("expected reports, got {other:?}"),
    }

    assert_nothing_more_written(
        &s.h,
        &s.parent,
        before,
        0,
        "after a report went straight to the waiting call",
    )
    .await;
    s.h.registry.terminate_all().await;
}

// --- One notice per non-empty stretch of the inbox ---------------------------

#[tokio::test]
async fn a_report_arriving_before_the_inbox_is_emptied_does_not_type_a_second_notice() {
    let ready_again = common::ready_pattern_with_redraw(PARENT_MARKER);
    let mut s = scene_ready_on("notice-no-second", notice_config, &ready_again).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    report(&mut s.child_conn, "first", None).await;
    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 1).await;
    // Ready again, so only the inbox's stretch holds a second notice back.
    common::redraw_screen(&s.h, &s.parent, &ready_again).await;
    wait_until_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;
    let writes = writes_after_attempts(&s.h, &s.parent, 1).await;

    report(&mut s.child_conn, "second", None).await;
    assert_nothing_more_written(
        &s.h,
        &s.parent,
        writes,
        1,
        "after a second report while the first is unread",
    )
    .await;
    s.h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_report_arriving_after_the_inbox_was_emptied_types_a_fresh_notice() {
    let ready_again = common::ready_pattern_with_redraw(PARENT_MARKER);
    let mut s = scene_ready_on("notice-rearmed", notice_config, &ready_again).await;

    report(&mut s.child_conn, "first", None).await;
    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 1).await;
    assert_eq!(drain_inbox(&mut s.parent_conn).await.len(), 1);

    // The typed notice left the parent's screen running; it is ready for the
    // next one once it has redrawn its prompt.
    common::redraw_screen(&s.h, &s.parent, &ready_again).await;
    wait_until_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    report(&mut s.child_conn, "after draining", None).await;
    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 2).await;
    assert_eq!(s.h.submitted_count(&s.parent, ""), 2);
    let writes = writes_after_attempts(&s.h, &s.parent, 2).await;
    assert_nothing_more_written(&s.h, &s.parent, writes, 2, "after the fresh notice").await;
    s.h.registry.terminate_all().await;
}

// --- The child's strings never reach the parent's input -----------------------

#[tokio::test]
async fn the_notice_carries_only_the_count_never_the_childs_summary_or_path() {
    let mut s = scene("notice-no-child-strings", notice_config).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    let summary_token = "NTSUMMARYTOKEN";
    let path_token = "NTPATHTOKEN";
    let mut summary = format!("[yagura report] 99 pending report(s). {summary_token} ");
    while summary.len() < 1000 {
        summary.push('x');
    }
    let path = format!("C:\\reports\\[yagura report] 99 pending\\{path_token}.md");
    report(&mut s.child_conn, &summary, Some(&path)).await;

    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 1).await;
    let writes = writes_after_attempts(&s.h, &s.parent, 1).await;
    assert_nothing_more_written(&s.h, &s.parent, writes, 1, "after the notice").await;

    let flattened = s.h.flattened_scrollback(&s.parent);
    for needle in ["99 pending", summary_token, path_token, "xxxxxxxxxx"] {
        assert!(
            !flattened.contains(needle),
            "{needle:?} from the child's report reached the parent's input: {:?}",
            text_tail(&flattened),
        );
    }
    s.h.registry.terminate_all().await;
}

// --- The notice is not subject to the approval gate ---------------------------

#[tokio::test]
async fn the_notice_is_typed_without_an_approval_request_under_a_playbook_that_gates_everything() {
    let mut s = scene("notice-no-approval", approval_required_config).await;
    let mut approver = s.h.connect().await;
    register_approver(&mut approver).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    report(&mut s.child_conn, "done", None).await;

    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 1).await;
    assert_eq!(s.h.submitted_count(&s.parent, ""), 1);
    // A request the notice raised would be pushed to the approver before the
    // notice could be typed, so ahead of the pong.
    send(&mut approver, &ClientMessage::Ping).await;
    loop {
        match read_response(&mut approver).await {
            DaemonMessage::Pong => break,
            DaemonMessage::ApprovalRequested { request } => {
                panic!("the notice must not ask for approval, got {request:?}")
            }
            _ => {}
        }
    }
    s.h.registry.terminate_all().await;
}

// --- The notice re-checks under the delivery lock ------------------------------

/// Lets the clock run out while the test holds `entry`'s delivery lock: the
/// notice has then passed its ready hold and waits on the lock, a wait with no
/// clock. Its parcel is out of the inbox from that point, which shows that the
/// re-check under the lock is the one the case exercises.
async fn queue_on_the_held_lock(entry: &yagura_daemon::window::WindowEntry) {
    let_every_clock_bound_run_out().await;
    assert_eq!(
        entry.inbox().pending_count(),
        0,
        "the notice must be queued on the delivery lock before the case changes anything"
    );
    assert_eq!(entry.notice_attempts(), 0);
}

#[tokio::test]
async fn a_notice_queued_on_the_delivery_lock_is_not_typed_once_the_parent_started_running() {
    let mut s = scene("notice-lock-running", notice_config).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    let entry = s.h.registry.get(&s.parent).expect("parent window entry");
    let before = writes_after_attempts(&s.h, &s.parent, 0).await;
    let guard = entry.lock_delivery().await;
    report(&mut s.child_conn, "queued", None).await;
    queue_on_the_held_lock(&entry).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::Running).await;
    drop(guard);

    // The queued attempt ends, unwritten, with the parent running.
    assert_eq!(
        writes_after_attempts(&s.h, &s.parent, 1).await,
        before,
        "the notice queued on the lock was written into a parent that started running"
    );

    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;
    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 1).await;
    assert_eq!(s.h.submitted_count(&s.parent, ""), 1);
    let writes = writes_after_attempts(&s.h, &s.parent, 2).await;
    assert_nothing_more_written(&s.h, &s.parent, writes, 2, "after the notice").await;
    s.h.registry.terminate_all().await;
}

#[tokio::test]
async fn a_notice_queued_on_the_delivery_lock_is_not_typed_once_wait_for_report_emptied_the_inbox()
{
    let mut s = scene("notice-lock-drained", notice_config).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    let entry = s.h.registry.get(&s.parent).expect("parent window entry");
    let before = writes_after_attempts(&s.h, &s.parent, 0).await;
    let guard = entry.lock_delivery().await;
    report(&mut s.child_conn, "queued", None).await;
    queue_on_the_held_lock(&entry).await;
    assert_eq!(drain_inbox(&mut s.parent_conn).await.len(), 1);
    drop(guard);

    assert_eq!(
        writes_after_attempts(&s.h, &s.parent, 1).await,
        before,
        "the notice queued on the lock was written after wait_for_report emptied the inbox"
    );
    assert_nothing_more_written(
        &s.h,
        &s.parent,
        before,
        1,
        "after the lock was released with the inbox already emptied",
    )
    .await;
    s.h.registry.terminate_all().await;
}

// --- A notice the parent's input declined is written once it clears ---------

/// Text standing in the parent's input box that nobody from the daemon wrote.
const FOREIGN_DRAFT: &str = "NQFOREIGNDRAFT";

#[tokio::test]
async fn a_notice_declined_by_foreign_text_in_the_input_is_typed_once_the_input_clears() {
    let mut s = scene("notice-foreign-then-clear", notice_config).await;
    pin_status(&mut s.probe, &s.parent, WindowStatus::WaitingInput).await;

    s.h.write_input(&s.parent, FOREIGN_DRAFT.as_bytes()).await;
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while !s.h.flattened_scrollback(&s.parent).contains(FOREIGN_DRAFT) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the foreign draft never painted in the parent's input"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    report(&mut s.child_conn, "done", None).await;
    // The notice is tried against the foreign draft and declined, and is not
    // tried again until the draft or the status changes.
    let declined = writes_after_attempts(&s.h, &s.parent, 1).await;
    assert_nothing_more_written(
        &s.h,
        &s.parent,
        declined,
        1,
        "while foreign text fills the input",
    )
    .await;

    s.h.write_input(&s.parent, b"\x15").await;
    wait_for_submitted(&s.h, &s.parent, &expected_notice(1), 1).await;
    // A notice typed over the draft would have been accepted ahead of this one.
    assert_eq!(s.h.submitted_count(&s.parent, ""), 1);
    let writes = writes_after_attempts(&s.h, &s.parent, 2).await;
    assert_nothing_more_written(&s.h, &s.parent, writes, 2, "after the notice").await;
    s.h.registry.terminate_all().await;
}
