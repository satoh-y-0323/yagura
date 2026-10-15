#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon never writes into a window whose screen, at
//! the very instant the daemon decides to write, matches an approval rule of
//! the window's detection definition -- not the body, not the clear
//! sequence, not the submit key, and not a `send_message` body it writes
//! through unjudged.
//!
//! The decision may not wait for the confirmed status: that one trails the
//! screen by `confirm_ms`, and a submit key landing on an approval prompt in
//! that gap picks the prompt's default option. So the detection engine
//! answers a composer query together with what its rules alone say about the
//! same screen, and the window layer declines on that answer.
//!
//! The window-level cases drive a real `WindowRegistry` (+ `IpcServer` where
//! the wire protocol is the subject) against `submit_child`, whose overlay
//! mode draws an approval dialog over its composer. What is asserted is what
//! actually reached the child: a submit key the dialog took prints
//! `APPROVED`, any other byte it took prints `DIALOG-INPUT`, and a line the
//! composer accepted prints `SUBMITTED <body>`.

mod common;

use common::ScreenFeed;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    APPROVAL_HEADING, CLEAR, ERASE, GateWindow, HANG_GUARD, Harness, PASTE_END, SUBMIT_CHILD_PROBE,
    WORKING_HEADING, approver_base_config, count_marker, input_log_read_so_far,
    let_every_clock_bound_run_out, new_engine, open_gate_window, policy, read_composer_now,
    read_response, repaint, send, settled_marker_count, submit_child_composer_section,
    tick_after_ms, unique_temp_dir, wait_until_composer_is, wait_until_composer_satisfies,
    wait_until_input_log_contains,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::engine::{DetectionEngine, DetectorHandle};
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{
    ComposerResidue, InputOrigin, NotPlacedReason, SubmitJudgment, SubmitPlacement, WindowEntry,
};
use yagura_protocol::{ClientMessage, DaemonMessage, DetectedStatus, UnknownReason, WindowStatus};
use yagura_test_support::ipc::Connection;

// =====================================================================
// The detection engine answers a composer query together with what its
// rules alone say about the same screen.
// =====================================================================

/// An approval rule, a rule declaring `unknown`, and an input rule that
/// shares its marker with the composer section.
const ENGINE_DEF: &str = r#"
schema_version = 1
name = "test-screen-rule"

[debounce]
confirm_ms = 300

[[rules]]
status = "waiting_approval"
pattern = 'Do you want to proceed\?'
priority = 30
region = "bottom"

[[rules]]
status = "unknown"
pattern = 'esc to cancel'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = 'QPROMPT>'
priority = 10
region = "bottom"

[composer]
marker = 'QPROMPT>'
separator = " "
continuation_prefix = ""
"#;

/// [`ENGINE_DEF`]'s `[debounce] confirm_ms`.
const ENGINE_CONFIRM_MS: u64 = 300;

fn prompt_screen(body: &str) -> Vec<u8> {
    repaint(&format!("QPROMPT> {body}"))
}

fn approval_screen() -> Vec<u8> {
    repaint("Do you want to proceed?\r\n> 1. Yes\r\n  2. No")
}

/// Drives a fresh [`ENGINE_DEF`] engine to a confirmed `WaitingInput`.
async fn engine_confirmed_waiting_input(config: &DaemonConfig) -> DetectionEngine {
    let mut engine = new_engine(ENGINE_DEF, config);
    engine.feed(&prompt_screen(""));
    tick_after_ms(&mut engine, ENGINE_CONFIRM_MS).await;
    assert_eq!(
        engine.status(),
        WindowStatus::WaitingInput,
        "the starting state must be a confirmed WaitingInput"
    );
    engine
}

#[tokio::test]
async fn the_screen_rule_reports_an_approval_prompt_before_the_confirmed_status_catches_up() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = engine_confirmed_waiting_input(&config).await;

    engine.feed(&approval_screen());
    let observation = engine.query_composer();

    assert_eq!(
        observation.screen_rule,
        Some(DetectedStatus::WaitingApproval),
        "the answer must carry what the rules say about the screen as it stands now"
    );
    assert_eq!(
        engine.status(),
        WindowStatus::WaitingInput,
        "confirm_ms has not elapsed, so the confirmed status still trails the screen"
    );
}

#[tokio::test]
async fn the_screen_rule_ignores_an_open_turn_a_report_declares() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(ENGINE_DEF, &config);

    engine.feed(&repaint("nothing any rule names"));
    engine.on_status_event(WindowStatus::Running);
    tick_after_ms(&mut engine, ENGINE_CONFIRM_MS).await;
    assert_eq!(
        engine.status(),
        WindowStatus::Running,
        "the open turn decides the status for a screen the rules cannot classify"
    );

    assert_eq!(
        engine.query_composer().screen_rule,
        None,
        "the screen rule is the rules' answer alone, whatever a report declares"
    );
}

#[tokio::test]
async fn the_screen_rule_follows_the_screen_and_the_readout_is_unchanged() {
    tokio::time::pause();
    let config = DaemonConfig::default();
    let mut engine = new_engine(ENGINE_DEF, &config);

    engine.feed(&prompt_screen("draft"));
    let observation = engine.query_composer();
    assert_eq!(
        observation.readout,
        ComposerReadout::Body("draft".to_string())
    );
    assert_eq!(
        observation.screen_rule,
        Some(DetectedStatus::WaitingInput),
        "a composer screen matches the input rule, not the approval one"
    );

    engine.feed(&repaint("nothing any rule names"));
    assert_eq!(
        engine.query_composer().screen_rule,
        None,
        "a screen no rule matches has no screen rule"
    );

    engine.feed(&repaint("working... esc to cancel"));
    assert_eq!(
        engine.query_composer().screen_rule,
        Some(DetectedStatus::Unknown(UnknownReason::DeclaredUnknown)),
        "a rule that declares unknown is reported as that declaration"
    );
}

#[tokio::test]
async fn the_detector_handle_answers_with_the_screen_rule_of_the_output_fed_before_the_query() {
    let config = DaemonConfig::default();
    let engine = new_engine(ENGINE_DEF, &config);
    let screen = engine.screen().clone();
    let (handle, _statuses) = DetectorHandle::spawn(engine);

    common::feed_handle(&screen, &handle, &approval_screen());
    let observation = handle.query_composer().await;

    assert_eq!(
        observation.screen_rule,
        Some(DetectedStatus::WaitingApproval),
        "output fed before the query is part of the screen the query is answered from"
    );
}

#[tokio::test]
async fn an_engine_that_cannot_answer_resolves_to_a_screen_rule_that_closes_the_gate() {
    let config = DaemonConfig::default();
    let engine = new_engine(ENGINE_DEF, &config);
    let (handle, statuses) = DetectorHandle::spawn(engine);
    // With nobody left to receive a confirmed status, the driver task ends at
    // its next publish, which `Exited` triggers at once.
    drop(statuses);
    handle.notify_exited();

    let observation = tokio::time::timeout(Duration::from_secs(5), handle.query_composer())
        .await
        .expect("the query must resolve on its own once the engine task is gone");

    assert_eq!(observation.readout, ComposerReadout::NotObserved);
    assert!(
        matches!(
            observation.screen_rule,
            Some(DetectedStatus::WaitingApproval)
                | Some(DetectedStatus::Unknown(UnknownReason::DeclaredUnknown))
        ),
        "an unanswered query must not let a write through, got {:?}",
        observation.screen_rule
    );
}

// =====================================================================
// The window layer declines to write while the screen matches an
// approval rule.
// =====================================================================

/// The fixture's `key` trigger bytes: draw the overlay, remove it.
const OVERLAY_SHOW: u8 = 0x01;
const OVERLAY_HIDE: u8 = 0x02;

fn detection_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["send_message"])),
        ..approver_base_config()
    }
}

fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// What the child's screen shows, read off the window's model, independent of
/// the daemon's own detector.
struct ScreenRead {
    composer: ComposerReadout,
    lines: Vec<String>,
}

async fn rebuilt_screen(h: &Harness, window_id: &str) -> ScreenRead {
    let session = entry(h, window_id).session().clone();
    session
        .read_screen(|screen| {
            let (_, rows) = screen.size();
            ScreenRead {
                composer: screen.read_composer(&submit_child_composer_section()),
                lines: screen.bottom_lines(usize::from(rows)),
            }
        })
        .await
        .unwrap()
}

fn composer_of(screen: &ScreenRead) -> ComposerReadout {
    screen.composer.clone()
}

fn shows(screen: &ScreenRead, needle: &str) -> bool {
    screen.lines.iter().any(|line| line.contains(needle))
}

/// Polls the rebuilt screen until `ready` holds, panicking with `what` after
/// ten seconds.
async fn wait_for_screen(
    h: &Harness,
    window_id: &str,
    what: &str,
    mut ready: impl FnMut(&ScreenRead) -> bool,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while !ready(&rebuilt_screen(h, window_id).await) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

async fn press(h: &Harness, window_id: &str, byte: u8) {
    entry(h, window_id)
        .write_pty_input(&[byte], InputOrigin::Message)
        .await
        .expect("fixture key write must not fail");
}

async fn show_overlay(h: &Harness, window_id: &str, heading: &str) {
    press(h, window_id, OVERLAY_SHOW).await;
    wait_for_screen(h, window_id, "the overlay to be drawn", |screen| {
        shows(screen, heading)
    })
    .await;
}

async fn hide_overlay(h: &Harness, window_id: &str, heading: &str) {
    press(h, window_id, OVERLAY_HIDE).await;
    wait_for_screen(h, window_id, "the overlay to be removed", |screen| {
        !shows(screen, heading)
    })
    .await;
}

/// Leaves `body` in the composer the way an earlier attempt would have, and
/// waits for it to be drawn.
async fn seed_composer(h: &Harness, window_id: &str, body: &str) {
    entry(h, window_id)
        .write_pty_input(body.as_bytes(), InputOrigin::Message)
        .await
        .expect("seed write must not fail");
    let expected = ComposerReadout::Body(body.to_string());
    wait_for_screen(h, window_id, "the seeded body to be drawn", |screen| {
        composer_of(screen) == expected
    })
    .await;
}

fn is_not_accepting(reason: NotPlacedReason) -> bool {
    matches!(reason, NotPlacedReason::NotAccepting)
}

/// Asserts `judgment` declined because the screen was not accepting input,
/// with `envelope_remains` as expected.
fn assert_declined_as_not_accepting(judgment: &SubmitJudgment, envelope_remains: bool) {
    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert!(
                is_not_accepting(not_placed.reason),
                "the decline must name the screen that was not accepting input, got {:?}",
                not_placed.reason
            );
            assert_eq!(not_placed.envelope_remains, envelope_remains);
        }
        other => panic!("nothing may be placed while an approval dialog is up, got {other:?}"),
    }
}

/// Every byte the child read, in order, from the log a [`GateWindow`] with an
/// `input_log` fills.
async fn child_input(h: &Harness, window_id: &str, log: &Path) -> Vec<u8> {
    input_log_read_so_far(&entry(h, window_id), log, Duration::from_secs(10)).await
}

fn count_subslice(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

/// Asserts that, of everything the child read, nothing was a clear sequence, a
/// submit key or a probe erase: the bytes that would act on a dialog.
fn assert_no_acting_byte_reached_the_child(received: &[u8]) {
    assert_no_acting_byte_in(received);
}

/// What the child read after the body's paste ended: the bytes that could act
/// on a dialog the body raised. A probe confirmed and erased before the body
/// was written is not one of them.
fn after_the_body(received: &[u8]) -> &[u8] {
    const PASTE_END: &[u8] = b"\x1b[201~";
    let end = received
        .windows(PASTE_END.len())
        .position(|window| window == PASTE_END)
        .unwrap_or_else(|| panic!("the body's paste must have reached the child: {received:?}"));
    &received[end + PASTE_END.len()..]
}

/// Where another writer's last `foreign` byte stands in `received`, and how
/// many clear bytes the daemon put right in front of it.
fn take_back_in_front_of(received: &[u8], foreign: u8) -> (usize, usize) {
    let at = received
        .iter()
        .rposition(|&byte| byte == foreign)
        .unwrap_or_else(|| panic!("the other writer's byte must have been read: {received:?}"));
    let run = received[..at]
        .iter()
        .rev()
        .take_while(|&&byte| byte == 0x15)
        .count();
    assert!(
        run > 0,
        "the take-back must reach the child right in front of the other writer's byte; it read \
         {received:?}"
    );
    (at, run)
}

/// [`assert_no_acting_byte_reached_the_child`] for what the child read after
/// the body, except for the take-back the daemon puts right in front of
/// another writer's `foreign` byte: a clear on a dialog changes nothing, a
/// submit key would answer it.
fn assert_only_the_take_back_reached_the_child_after_the_body(received: &[u8], foreign: u8) {
    let after = after_the_body(received);
    let (at, run) = take_back_in_front_of(after, foreign);
    assert_no_acting_byte_in(&after[..at - run]);
    assert_no_acting_byte_in(&after[at + 1..]);
}

fn assert_no_acting_byte_in(received: &[u8]) {
    for (byte, what) in [
        (0x15u8, "clear sequence"),
        (b'\r', "submit key"),
        (0x7f, "erase"),
    ] {
        assert!(
            !received.contains(&byte),
            "the child must not have read a {what} (0x{byte:02x}); it read {received:?}"
        );
    }
}

/// Asserts the child took none of the daemon's bytes: no submit key and no
/// other byte reached its dialog, and nothing was submitted.
async fn assert_child_took_nothing(h: &Harness, window_id: &str) {
    assert_eq!(
        settled_marker_count(h, window_id, "APPROVED", 0).await,
        0,
        "a submit key reached the approval dialog"
    );
    assert_eq!(
        settled_marker_count(h, window_id, "DIALOG-INPUT", 0).await,
        0,
        "a byte reached the approval dialog"
    );
    assert_eq!(count_marker(h, window_id, "SUBMITTED"), 0);
}

// --- what the fixture's dialog reports ------------------------------------

/// The cases below catch a submit key landing on a dialog by `APPROVED`
/// alone, so the fixture must report exactly that for it.
#[tokio::test]
async fn a_submit_key_that_reaches_the_fixtures_dialog_is_reported_as_approved() {
    let dir = unique_temp_dir("screen-gate-dialog-report");
    let h = Harness::start("screen-gate-dialog-report", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("report", "report-marker", "key:hidden", APPROVAL_HEADING),
    )
    .await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;

    press(&h, &window_id, b'\r').await;

    h.wait_for_scrollback_occurrences(&window_id, "APPROVED", 1, Duration::from_secs(10))
        .await;
}

// --- a key-only resend ------------------------------------------------

#[tokio::test]
async fn a_key_only_resend_is_not_written_while_an_approval_dialog_hides_the_composer() {
    let dir = unique_temp_dir("screen-gate-resend-hidden");
    let h = Harness::start("screen-gate-resend-hidden", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("hidden", "hidden-marker", "key:hidden", APPROVAL_HEADING),
    )
    .await;
    seed_composer(&h, &window_id, "ENV").await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::NotObserved,
        "the dialog must leave the composer unreadable for this case to mean anything"
    );

    let seq = entry(&h, &window_id).write_seq();
    let judgment = entry(&h, &window_id)
        .submit_message(
            "ENV",
            Some(ComposerResidue {
                body: "ENV",
                write_seq: seq,
            }),
            None,
        )
        .await
        .expect("the call must not fail at the PTY level");

    assert_declined_as_not_accepting(&judgment, true);
    assert_eq!(
        judgment.residue_write_seq,
        Some(seq),
        "the baseline handed on must be one no submit key was written after"
    );
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq,
        "nothing was written"
    );
    assert_child_took_nothing(&h, &window_id).await;
}

#[tokio::test]
async fn a_key_only_resend_is_not_written_when_the_dialogs_option_row_reads_as_a_composer() {
    let dir = unique_temp_dir("screen-gate-resend-option");
    let h = Harness::start("screen-gate-resend-option", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("option", "option-marker", "key:option", APPROVAL_HEADING),
    )
    .await;
    seed_composer(&h, &window_id, "ENV").await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Body("1. Yes".to_string()),
        "the dialog's option row must read as a composer holding other text"
    );

    let seq = entry(&h, &window_id).write_seq();
    let judgment = entry(&h, &window_id)
        .submit_message(
            "ENV",
            Some(ComposerResidue {
                body: "ENV",
                write_seq: seq,
            }),
            None,
        )
        .await
        .expect("the call must not fail at the PTY level");

    assert_declined_as_not_accepting(&judgment, true);
    assert_eq!(judgment.residue_write_seq, Some(seq));
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq,
        "nothing was written"
    );
    assert_child_took_nothing(&h, &window_id).await;
}

#[tokio::test]
async fn a_key_only_resend_is_still_written_once_the_dialog_is_gone() {
    let dir = unique_temp_dir("screen-gate-resend-after");
    let h = Harness::start("screen-gate-resend-after", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("after", "after-marker", "key:hidden", APPROVAL_HEADING),
    )
    .await;
    seed_composer(&h, &window_id, "ENV").await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;
    hide_overlay(&h, &window_id, APPROVAL_HEADING).await;
    wait_for_screen(
        &h,
        &window_id,
        "the seeded body to be drawn again",
        |screen| composer_of(screen) == ComposerReadout::Body("ENV".to_string()),
    )
    .await;

    let seq = entry(&h, &window_id).write_seq();
    let judgment = entry(&h, &window_id)
        .submit_message(
            "ENV",
            Some(ComposerResidue {
                body: "ENV",
                write_seq: seq,
            }),
            None,
        )
        .await
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("an input screen must take the key-only resend, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED ENV", 1).await,
        1,
        "the submit key alone must submit the envelope already in the composer"
    );
    assert_eq!(count_marker(&h, &window_id, "APPROVED"), 0);
}

#[tokio::test]
async fn a_key_only_resend_is_still_written_while_the_window_shows_it_is_working() {
    let dir = unique_temp_dir("screen-gate-resend-working");
    let h = Harness::start("screen-gate-resend-working", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new(
            "rworking",
            "rworking-marker",
            "key:working",
            WORKING_HEADING,
        ),
    )
    .await;
    seed_composer(&h, &window_id, "ENV").await;
    show_overlay(&h, &window_id, WORKING_HEADING).await;

    let seq = entry(&h, &window_id).write_seq();
    let judgment = entry(&h, &window_id)
        .submit_message(
            "ENV",
            Some(ComposerResidue {
                body: "ENV",
                write_seq: seq,
            }),
            None,
        )
        .await
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a working screen must take the key-only resend, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED ENV", 1).await,
        1
    );
}

// --- after the body is written ------------------------------------------

/// A dialog that replaces the composer once the body is in: the screen does
/// not accept a write, and nothing the daemon decides on its own to write (a
/// clear, the probe's erase, the submit key) may reach it, however long that
/// takes. Another writer's byte gets the body's take-back, which goes to the
/// dialog right in front of it, but the call is not over: what the take-back
/// did is checked on a screen that accepts a write, so it is answered only
/// when a second writer's byte, not the dialog's staying, ends the check.
#[tokio::test]
async fn a_dialog_that_replaces_the_written_body_gets_the_take_back_but_never_the_submit_key() {
    let dir = unique_temp_dir("screen-gate-after-body");
    let log = dir.join("child-input.log");
    let h = Harness::start("screen-gate-after-body", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new(
                "afterbody",
                "afterbody-marker",
                "body:hidden",
                APPROVAL_HEADING,
            )
        },
    )
    .await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    wait_for_screen(&h, &window_id, "the dialog to be drawn", |screen| {
        shows(screen, APPROVAL_HEADING)
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a screen that does not accept a write is waited on, not given up on by a clock"
    );
    press(&h, &window_id, b'Q').await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "the dialog still stands, so the take-back cannot be checked and the call is not over"
    );
    press(&h, &window_id, b'R').await;
    let judgment = call
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
            assert!(
                not_placed.envelope_remains,
                "the body is still in the composer"
            );
        }
        other => panic!("a second writer's byte must end the check, got {other:?}"),
    }
    assert_only_the_take_back_reached_the_child_after_the_body(
        &child_input(&h, &window_id, &log).await,
        b'Q',
    );
    assert_eq!(
        count_marker(&h, &window_id, "APPROVED"),
        0,
        "a submit key reached the approval dialog"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

#[tokio::test]
async fn a_dialog_that_flashes_after_the_body_is_written_still_lets_the_submit_key_through() {
    let dir = unique_temp_dir("screen-gate-flash");
    let dialog_gone = dir.join("dialog-gone");
    let h = Harness::start("screen-gate-flash", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = dialog_until("hidden", &dialog_gone);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("flash", "flash-marker", &overlay, APPROVAL_HEADING),
    )
    .await;

    let sender = entry(&h, &window_id);
    let submit = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    wait_for_screen(&h, &window_id, "the dialog to be drawn", |screen| {
        shows(screen, APPROVAL_HEADING)
    })
    .await;
    std::fs::write(&dialog_gone, b"").expect("remove the dialog");
    let judgment = submit
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");

    assert!(
        count_marker(&h, &window_id, APPROVAL_HEADING) > 0,
        "the dialog must have been drawn for this case to mean anything"
    );
    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!(
            "a dialog gone again while the body waits to show must not decline the write, got \
             {other:?}"
        ),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "APPROVED", 0).await,
        0,
        "the submit key must wait until the dialog is gone"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED hello", 1).await,
        1
    );
}

/// A `body`-triggered dialog in `style` that stays drawn until the file
/// `gone` exists.
fn dialog_until(style: &str, gone: &Path) -> String {
    format!(
        "body:{style}:until|{}",
        gone.to_str().expect("the dialog path must be valid UTF-8")
    )
}

/// The written body never paints, so the composer keeps reading empty -- while
/// the dialog drawn beside it matches an approval rule, and then goes away. The
/// clear sequence that would take the write back is a byte for that dialog
/// too, and the body may still land later, so neither a clear of the daemon's
/// own choosing nor a second copy of the body is written while the daemon
/// waits; only another writer's byte ends the wait, with the body's take-back
/// right in front of it.
#[tokio::test]
async fn a_body_that_stays_unread_beside_an_approval_dialog_is_not_rewritten_and_is_taken_back_in_front_of_another_writer()
 {
    let dir = unique_temp_dir("screen-gate-clear");
    let log = dir.join("child-input.log");
    let dialog_gone = dir.join("dialog-gone");
    let h = Harness::start("screen-gate-clear", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = dialog_until("beside", &dialog_gone);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            clear_scope: "frozen:ignore-clear",
            input_log: Some(&log),
            ..GateWindow::new("clear", "clear-marker", &overlay, APPROVAL_HEADING)
        },
    )
    .await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    wait_for_screen(&h, &window_id, "the dialog to be drawn", |screen| {
        shows(screen, APPROVAL_HEADING)
    })
    .await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty,
        "the composer must read empty for this case to reach the clear step"
    );
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "an unread body beside a dialog is waited on, not given up on by a clock"
    );

    std::fs::write(&dialog_gone, b"").expect("remove the dialog");
    wait_for_screen(&h, &window_id, "the dialog to be removed", |screen| {
        !shows(screen, APPROVAL_HEADING)
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a screen that accepts a write again does not end the wait for a body that is still unread"
    );

    press(&h, &window_id, b'Q').await;
    let judgment = call
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");
    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
            assert!(not_placed.envelope_remains);
        }
        other => panic!("another writer's byte must end the wait, got {other:?}"),
    }

    let received = child_input(&h, &window_id, &log).await;
    assert_only_the_take_back_reached_the_child_after_the_body(&received, b'Q');
    assert_eq!(
        count_subslice(&received, b"\x1b[200~hello\x1b[201~"),
        1,
        "the body must have been written exactly once; the child read {received:?}"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
    assert_eq!(count_marker(&h, &window_id, "APPROVED"), 0);
}

// --- before the body is written -----------------------------------------

#[tokio::test]
async fn the_body_is_not_written_when_an_approval_rule_matches_beside_an_empty_composer() {
    let dir = unique_temp_dir("screen-gate-before-body");
    let h = Harness::start("screen-gate-before-body", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new(
            "beforebody",
            "beforebody-marker",
            "key:beside",
            APPROVAL_HEADING,
        ),
    )
    .await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty,
        "the composer must read empty for this case to mean anything"
    );

    let seq = entry(&h, &window_id).write_seq();
    let judgment = entry(&h, &window_id)
        .submit_message("hello", None, None)
        .await
        .expect("the call must not fail at the PTY level");

    assert_declined_as_not_accepting(&judgment, false);
    assert_eq!(judgment.residue_write_seq, None);
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq,
        "nothing was written"
    );
    assert_child_took_nothing(&h, &window_id).await;
}

#[tokio::test]
async fn a_body_is_still_written_into_a_window_that_shows_it_is_working() {
    let dir = unique_temp_dir("screen-gate-working");
    let h = Harness::start("screen-gate-working", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("working", "working-marker", "key:working", WORKING_HEADING),
    )
    .await;
    show_overlay(&h, &window_id, WORKING_HEADING).await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Empty,
        Duration::from_secs(10),
    )
    .await;

    let judgment = entry(&h, &window_id)
        .submit_message("hello", None, None)
        .await
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a working window must still be written to, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED hello", 1).await,
        1
    );
}

// --- an approval dialog raised by the daemon's own bytes ----------------

/// A dialog in `style`, drawn right after the first read carrying `trigger`
/// (`clear` or `byte=<hex>`), that stays until the file `gone` exists.
fn dialog_raised_by(trigger: &str, style: &str, gone: &Path) -> String {
    format!(
        "{trigger}:{style}:until|{}",
        gone.to_str().expect("the dialog path must be valid UTF-8")
    )
}

/// A composer an approval dialog hides for as long as it stays up has no
/// reading to decide on: a screen caught between erasing and repainting its
/// composer reads the same. Nothing is decided on a clock and nothing is
/// written until the composer can be read again or the requester leaves.
#[tokio::test]
async fn an_unreadable_composer_is_waited_on_until_the_requester_leaves() {
    let dir = unique_temp_dir("screen-gate-unreadable-bound");
    let h = Harness::start("screen-gate-unreadable-bound", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new(
            "unreadable",
            "unreadable-marker",
            "key:hidden",
            APPROVAL_HEADING,
        ),
    )
    .await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::NotObserved,
        "the dialog must leave the composer unreadable for this case to mean anything"
    );

    let seq = entry(&h, &window_id).write_seq();
    let (leave, requester) = tokio::sync::watch::channel(false);
    let sender = entry(&h, &window_id);
    let call =
        tokio::spawn(async move { sender.submit_message("hello", None, Some(requester)).await });
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "an unreadable composer is waited on, not given up on by a clock"
    );
    leave.send(true).expect("the call holds its receiver");
    let judgment = call
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Unobservable);
            assert!(!not_placed.envelope_remains);
        }
        other => panic!("a composer that cannot be read takes no body, got {other:?}"),
    }
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq,
        "nothing was written"
    );
    assert_child_took_nothing(&h, &window_id).await;
}

/// Another writer ending that wait is reported as such, not as a composer
/// that could not be read.
#[tokio::test]
async fn an_unreadable_composer_wait_ended_by_another_writer_is_reported_as_interrupted() {
    let dir = unique_temp_dir("screen-gate-unreadable-other-writer");
    let h = Harness::start(
        "screen-gate-unreadable-other-writer",
        detection_config(&dir),
    )
    .await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new(
            "unreadable-other",
            "unreadable-other-marker",
            "key:hidden",
            APPROVAL_HEADING,
        ),
    )
    .await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::NotObserved,
        "the dialog must leave the composer unreadable for this case to mean anything"
    );

    let seq = entry(&h, &window_id).write_seq();
    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "an unreadable composer is waited on, not given up on by a clock"
    );
    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Message)
        .await
        .expect("the other writer's byte must not fail");
    let judgment = call
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
            assert!(!not_placed.envelope_remains);
        }
        other => panic!("a composer that cannot be read takes no body, got {other:?}"),
    }
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq + 1,
        "only the other writer's byte was written"
    );
}

/// The same wait ends in a delivery once the dialog is gone.
#[tokio::test]
async fn a_composer_hidden_for_a_moment_takes_the_body_once_it_is_readable_again() {
    let dir = unique_temp_dir("screen-gate-unreadable-recovers");
    let dialog_gone = dir.join("dialog-gone");
    let h = Harness::start("screen-gate-unreadable-recovers", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = dialog_raised_by("clear", "hidden", &dialog_gone);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("recovers", "recovers-marker", &overlay, APPROVAL_HEADING),
    )
    .await;
    press(&h, &window_id, 0x15).await;
    wait_for_screen(&h, &window_id, "the dialog to be drawn", |screen| {
        shows(screen, APPROVAL_HEADING)
    })
    .await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::NotObserved
    );

    let sender = entry(&h, &window_id);
    let submit = tokio::spawn(async move { sender.submit_message("BODY", None, None).await });
    std::fs::write(&dialog_gone, b"").expect("remove the dialog");
    let judgment = submit
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a composer readable again must take the body, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 1).await,
        1
    );
    assert_eq!(count_marker(&h, &window_id, "APPROVED"), 0);
}

/// A dialog that appears as the probe is typed: the probe shows, but on a
/// screen that must not be written to, so the erase that would follow it is
/// not written either, however long the dialog stays. Another writer's byte
/// does not end the call while the dialog stands (what the probe left is
/// checked on a screen that accepts a write); a second writer's byte does.
#[tokio::test]
async fn nothing_is_written_after_a_probe_once_a_dialog_has_appeared_beside_it() {
    let dir = unique_temp_dir("screen-gate-dialog-at-probe");
    let log = dir.join("child-input.log");
    let h = Harness::start("screen-gate-dialog-at-probe", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let probe_byte = format!("byte={:02x}", SUBMIT_CHILD_PROBE.as_bytes()[0]);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new(
                "atprobe",
                "atprobe-marker",
                &format!("{probe_byte}:beside"),
                APPROVAL_HEADING,
            )
        },
    )
    .await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    wait_for_screen(&h, &window_id, "the dialog to be drawn", |screen| {
        shows(screen, APPROVAL_HEADING)
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a probe that shows on a screen that does not accept a write is waited on, not given up on by a clock"
    );
    press(&h, &window_id, b'Q').await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "the dialog still stands, so what the probe left cannot be checked and the call is not over"
    );
    press(&h, &window_id, b'R').await;
    let judgment = call
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
        }
        other => panic!("a second writer's byte must end the check, got {other:?}"),
    }
    assert_no_acting_byte_reached_the_child(&child_input(&h, &window_id, &log).await);
    assert_eq!(count_marker(&h, &window_id, "APPROVED"), 0);
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

/// A dialog that appears as the probe is erased, with the envelope reading
/// as it did before: the envelope is whole, but the key that would resend it
/// is a key for the dialog too, so it is not pressed however long the dialog
/// stays. Another writer's byte gets the envelope's take-back, which goes to
/// the dialog right in front of it, but does not end the call while the
/// dialog stands; a second writer's byte does.
#[tokio::test]
async fn no_submit_key_follows_a_probe_erase_that_raises_a_dialog_beside_the_envelope() {
    let dir = unique_temp_dir("screen-gate-dialog-at-erase");
    let log = dir.join("child-input.log");
    let h = Harness::start("screen-gate-dialog-at-erase", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new(
                "aterase",
                "aterase-marker",
                "byte=7f:beside",
                APPROVAL_HEADING,
            )
        },
    )
    .await;
    // The seed itself moves the write count past the caller's baseline, so the
    // composer reads as the envelope while the ledger no longer vouches for it:
    // the resend has to confirm behind it first.
    let residue_seq = entry(&h, &window_id).write_seq();
    seed_composer(&h, &window_id, "ENV").await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move {
        sender
            .submit_message(
                "ENV",
                Some(ComposerResidue {
                    body: "ENV",
                    write_seq: residue_seq,
                }),
                None,
            )
            .await
    });
    wait_for_screen(&h, &window_id, "the dialog to be drawn", |screen| {
        shows(screen, APPROVAL_HEADING)
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a dialog beside the envelope is waited on, not given up on by a clock"
    );
    press(&h, &window_id, b'Q').await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "the dialog still stands, so the envelope's take-back cannot be checked and the call is not over"
    );
    press(&h, &window_id, b'R').await;
    let judgment = call
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
        }
        other => panic!("a dialog beside the envelope must stop the resend, got {other:?}"),
    }
    let received = child_input(&h, &window_id, &log).await;
    take_back_in_front_of(&received, b'Q');
    assert!(
        !received.contains(&b'\r'),
        "the submit key must never have been written; the child read {received:?}"
    );
    assert_eq!(
        count_marker(&h, &window_id, "APPROVED"),
        0,
        "a submit key reached the approval dialog"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

// --- a frame caught between erasing the composer and drawing a dialog --------

/// The other writer's byte that ends a torn-frame case; it also removes the
/// fixture's dialog, so what it leaves can be read again.
const FOREIGN: u8 = b'Q';

/// A dialog in `style`, raised by `trigger`, that takes input at once but is
/// painted in two writes: `shape` first, the full dialog once the file `rest`
/// exists. The first read of [`FOREIGN`] removes it.
fn torn_dialog(trigger: &str, style: &str, shape: &str, rest: &Path) -> String {
    format!(
        "{trigger}:{style}:torn|{shape}|{}|until-byte|{FOREIGN:02x}",
        rest.to_str().expect("the dialog path must be valid UTF-8")
    )
}

type SubmitCall = tokio::task::JoinHandle<yagura_daemon::pty::PtyResult<SubmitJudgment>>;

/// Draws the rest of the torn dialog, then writes another writer's byte,
/// returning how `call` ended.
async fn finish_the_dialog_then_write_another_byte(
    h: &Harness,
    window_id: &str,
    rest: &Path,
    call: SubmitCall,
) -> SubmitJudgment {
    std::fs::write(rest, b"").expect("draw the rest of the dialog");
    wait_for_screen(
        h,
        window_id,
        "the rest of the dialog to be drawn",
        |screen| shows(screen, APPROVAL_HEADING),
    )
    .await;
    press(h, window_id, FOREIGN).await;
    tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("another writer's byte must end the call")
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level")
}

/// Asserts no submit key reached the child, and nothing was placed.
async fn assert_no_submit_key_reached_the_child(
    h: &Harness,
    window_id: &str,
    log: &Path,
    judgment: &SubmitJudgment,
) {
    let received = child_input(h, window_id, log).await;
    assert!(
        !received.contains(&b'\r'),
        "a torn frame is no evidence for the submit key; the child read {received:?}"
    );
    assert_eq!(
        count_marker(h, window_id, "APPROVED"),
        0,
        "a submit key reached the approval dialog"
    );
    assert_eq!(count_marker(h, window_id, "SUBMITTED"), 0);
    assert!(
        matches!(judgment.placement, SubmitPlacement::NotPlaced(_)),
        "nothing may be placed on a torn frame, got {:?}",
        judgment.placement
    );
}

/// The dialog's heading is not drawn yet.
fn heading_not_drawn(screen: &ScreenRead) -> bool {
    !shows(screen, APPROVAL_HEADING)
}

/// The body raises a dialog whose first write only erases the composer: the
/// reading after the probe is a torn frame, and nothing is decided on it.
#[tokio::test]
async fn no_submit_key_follows_a_body_whose_composer_reads_as_a_torn_frame() {
    let dir = unique_temp_dir("screen-gate-torn-body");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-body", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = torn_dialog("body", "hidden", "erase", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new("tornbody", "tornbody-marker", &overlay, APPROVAL_HEADING)
        },
    )
    .await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    wait_for_screen(&h, &window_id, "the torn frame after the body", |screen| {
        heading_not_drawn(screen)
            && shows(screen, "PASTE 1")
            && composer_of(screen) == ComposerReadout::NotObserved
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a torn frame is waited on, not decided on and not given up on by a clock"
    );
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
}

/// The probe's erase raises a dialog whose first write only erases the
/// composer: that is no reading of the composer back as it was.
#[tokio::test]
async fn no_submit_key_follows_a_probe_erase_whose_composer_reads_as_a_torn_frame() {
    let dir = unique_temp_dir("screen-gate-torn-erase");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-erase", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = torn_dialog("erase", "hidden", "erase", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new("tornerase", "tornerase-marker", &overlay, APPROVAL_HEADING)
        },
    )
    .await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    let erase_after_the_body = [PASTE_END, SUBMIT_CHILD_PROBE.as_bytes(), &[ERASE]].concat();
    wait_until_input_log_contains(&log, &erase_after_the_body, HANG_GUARD).await;
    wait_for_screen(&h, &window_id, "the torn frame after the erase", |screen| {
        heading_not_drawn(screen) && composer_of(screen) == ComposerReadout::NotObserved
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a torn frame after the erase is waited on, not taken for the composer read back"
    );
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
}

/// Seeds `ENV`, raises the torn dialog with the fixture's key, waits for its
/// first write to read as `torn`, and starts a resend of `ENV` the ledger
/// vouches for.
async fn resend_on_a_torn_frame(
    h: &Harness,
    window_id: &str,
    torn: impl FnMut(&ScreenRead) -> bool,
) -> SubmitCall {
    seed_composer(h, window_id, "ENV").await;
    press(h, window_id, OVERLAY_SHOW).await;
    wait_for_screen(h, window_id, "the torn frame", torn).await;
    let seq = entry(h, window_id).write_seq();
    let sender = entry(h, window_id);
    tokio::spawn(async move {
        sender
            .submit_message(
                "ENV",
                Some(ComposerResidue {
                    body: "ENV",
                    write_seq: seq,
                }),
                None,
            )
            .await
    })
}

/// The ledger vouches for the envelope, but the composer reads as a torn
/// frame: that is no reading of the envelope, so the key-only resend waits.
#[tokio::test]
async fn a_key_only_resend_waits_while_the_composer_reads_as_a_torn_frame() {
    let dir = unique_temp_dir("screen-gate-torn-resend");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-resend", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = torn_dialog("key", "hidden", "erase", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new(
                "tornresend",
                "tornresend-marker",
                &overlay,
                APPROVAL_HEADING,
            )
        },
    )
    .await;

    let call = resend_on_a_torn_frame(&h, &window_id, |screen| {
        heading_not_drawn(screen) && composer_of(screen) == ComposerReadout::NotObserved
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a torn frame is waited on, however much the ledger vouches"
    );
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
}

/// A torn frame can be readable: a dialog's selected row, drawn with the
/// composer's glyph away from the line's start, reads as a body. The ledger
/// vouching is no evidence the envelope is what shows.
#[tokio::test]
async fn no_key_only_resend_is_written_when_a_torn_frame_reads_a_dialog_row_as_the_composer() {
    let dir = unique_temp_dir("screen-gate-torn-option");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-option", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = torn_dialog("key", "option", "option", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new(
                "tornoption",
                "tornoption-marker",
                &overlay,
                APPROVAL_HEADING,
            )
        },
    )
    .await;

    let call = resend_on_a_torn_frame(&h, &window_id, |screen| {
        heading_not_drawn(screen)
            && composer_of(screen) == ComposerReadout::Body("1. Yes".to_string())
    })
    .await;
    let_every_clock_bound_run_out().await;
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
}

/// A torn frame whose response rows, indented like the composer, run up to
/// the top and show the same envelope: the tail matches, but a tail is no
/// evidence for a key-only resend.
#[tokio::test]
async fn no_key_only_resend_is_written_when_a_torn_frame_shows_the_envelope_in_indented_rows() {
    let dir = unique_temp_dir("screen-gate-torn-echo");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-echo", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = torn_dialog("key", "hidden", "echo=ENV", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new("tornecho", "tornecho-marker", &overlay, APPROVAL_HEADING)
        },
    )
    .await;

    let call = resend_on_a_torn_frame(&h, &window_id, |screen| {
        heading_not_drawn(screen)
            && matches!(composer_of(screen), ComposerReadout::Tail(tail) if tail.ends_with("ENV"))
    })
    .await;
    let_every_clock_bound_run_out().await;
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
}

/// A body taller than the screen leaves only its end in view, every row up
/// to the top a continuation row: that tail, ending in the probe, is the
/// body landing, and it is submitted once.
#[tokio::test]
async fn a_body_taller_than_the_screen_is_placed_from_its_tail() {
    let dir = unique_temp_dir("screen-gate-tail");
    let h = Harness::start("screen-gate-tail", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let body: String = (0..300).map(|i| format!("{i:04}")).collect();

    // The fixture's own drawing of such a body, on a window of its own: the
    // shape the call below reads.
    let shape = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("tailshape", "tailshape-marker", "", ""),
    )
    .await;
    entry(&h, &shape)
        .write_pty_input(body.as_bytes(), InputOrigin::Message)
        .await
        .expect("typing the body must not fail");
    wait_until_composer_satisfies(
        &h,
        &shape,
        "the body's tail",
        Duration::from_secs(10),
        |readout| matches!(readout, ComposerReadout::Tail(tail) if body.ends_with(tail.as_str())),
    )
    .await;

    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("tail", "tail-marker", "", ""),
    )
    .await;
    let judgment = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message(&body, None, None),
    )
    .await
    .expect("a body read back by its tail must be decided")
    .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a body whose tail reads back must be placed, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED 0000", 1).await,
        1,
        "the body reaches the receiver exactly once"
    );
}

/// A folded placeholder hides the envelope, so the ledger alone does not
/// press the submit key: the probe shows behind the placeholder first, is
/// erased, and only then the key follows. The body is not written again.
#[tokio::test]
async fn a_folded_envelope_is_resent_only_once_the_probe_has_shown_behind_it() {
    const PLACEHOLDER: &str = "[Pasted text #1]";
    let dir = unique_temp_dir("screen-gate-folded-resend");
    let log = dir.join("child-input.log");
    let h = Harness::start("screen-gate-folded-resend", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            fold_placeholder: PLACEHOLDER,
            ..GateWindow::new("foldedresend", "foldedresend-marker", "", "")
        },
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet";
    entry(&h, &window_id)
        .write_pty_input(
            format!("\x1b[200~{body}\x1b[201~").as_bytes(),
            InputOrigin::Message,
        )
        .await
        .expect("seed write must not fail");
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(PLACEHOLDER.to_string()),
        Duration::from_secs(10),
    )
    .await;
    let seq = entry(&h, &window_id).write_seq();

    let judgment = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message(
            body,
            Some(ComposerResidue {
                body,
                write_seq: seq,
            }),
            None,
        ),
    )
    .await
    .expect("a folded envelope must be decided")
    .expect("the call must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a folded, untouched envelope must still resend, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, &format!("SUBMITTED {body}"), 1).await,
        1
    );
    let received = child_input(&h, &window_id, &log).await;
    assert_eq!(
        count_subslice(&received, b"\x1b[200~"),
        1,
        "the body must not be written again; the child read {received:?}"
    );
    assert_eq!(
        after_the_body(&received),
        format!("{SUBMIT_CHILD_PROBE}\x7f\r").as_bytes(),
        "the probe, its erase, then the submit key; the child read {received:?}"
    );
}

// --- what a reading that shows no evidence must never lead to -------------------

const PASTE_START: &[u8] = b"\x1b[200~";

/// Ends `call` within a bound that is not a clock the daemon owns: a call
/// that is blocked for good fails here, not by hanging the suite.
async fn ended(call: SubmitCall, what: &str) -> SubmitJudgment {
    tokio::time::timeout(Duration::from_secs(10), call)
        .await
        .unwrap_or_else(|_| panic!("{what}"))
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level")
}

/// The ledger vouches for a folded envelope, but the receiver has taken it in
/// by the time the probe lands, so the probe shows alone. That is a body that
/// never reached the composer: no submit key follows, the probe does not stay
/// behind, and the call ends instead of waiting for a composer that can never
/// read as the envelope.
#[tokio::test]
async fn a_probe_that_shows_alone_behind_a_vouched_folded_envelope_is_no_evidence_for_the_submit_key()
 {
    const PLACEHOLDER: &str = "[Pasted text #1]";
    let dir = unique_temp_dir("screen-gate-folded-probe-alone");
    let log = dir.join("child-input.log");
    let h = Harness::start("screen-gate-folded-probe-alone", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            fold_placeholder: PLACEHOLDER,
            clear_scope: "takes-paste-on-typing:",
            ..GateWindow::new("foldedgone", "foldedgone-marker", "", "")
        },
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet";
    entry(&h, &window_id)
        .write_pty_input(
            format!("\x1b[200~{body}\x1b[201~").as_bytes(),
            InputOrigin::Message,
        )
        .await
        .expect("seed write must not fail");
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(PLACEHOLDER.to_string()),
        Duration::from_secs(10),
    )
    .await;
    let seq = entry(&h, &window_id).write_seq();

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move {
        sender
            .submit_message(
                body,
                Some(ComposerResidue {
                    body,
                    write_seq: seq,
                }),
                None,
            )
            .await
    });
    let probe_then_erase = [SUBMIT_CHILD_PROBE.as_bytes(), &[ERASE]].concat();
    wait_until_input_log_contains(&log, &probe_then_erase, HANG_GUARD).await;
    let_every_clock_bound_run_out().await;
    let judgment = ended(
        call,
        "a probe that shows alone is waited on for good: the call must end",
    )
    .await;

    assert!(
        matches!(judgment.placement, SubmitPlacement::NotPlaced(_)),
        "a probe alone is no evidence the envelope is there, got {:?}",
        judgment.placement
    );
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Empty,
        Duration::from_secs(10),
    )
    .await;
    let received = child_input(&h, &window_id, &log).await;
    assert!(
        !received.contains(&b'\r'),
        "no submit key may follow a probe that shows alone; the child read {received:?}"
    );
    assert_eq!(
        count_subslice(&received, PASTE_START),
        1,
        "the body must not be written again; the child read {received:?}"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

/// The end of a body taller than the screen is what shows, the ledger vouches
/// for it, and a dialog the probe raises keeps the screen closed until another
/// writer's byte ends the wait. What the call wrote on top of the draft is
/// still its own probe, at the end of the composer, once the dialog is gone:
/// it does not stay there.
#[tokio::test]
async fn a_probe_written_behind_a_tall_draft_is_erased_once_another_writer_interrupts() {
    let dir = unique_temp_dir("screen-gate-tail-probe-left");
    let log = dir.join("child-input.log");
    let gone = dir.join("dialog-gone");
    let h = Harness::start("screen-gate-tail-probe-left", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let probe_byte = format!("byte={:02x}", SUBMIT_CHILD_PROBE.as_bytes()[0]);
    let overlay = dialog_raised_by(&probe_byte, "hidden", &gone);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new("tailleft", "tailleft-marker", &overlay, APPROVAL_HEADING)
        },
    )
    .await;
    let body: String = (0..300).map(|i| format!("{i:04}")).collect();
    entry(&h, &window_id)
        .write_pty_input(body.as_bytes(), InputOrigin::Message)
        .await
        .expect("typing the draft must not fail");
    wait_until_composer_satisfies(
        &h,
        &window_id,
        "the draft's tail",
        Duration::from_secs(10),
        |readout| matches!(readout, ComposerReadout::Tail(tail) if body.ends_with(tail.as_str())),
    )
    .await;
    let seq = entry(&h, &window_id).write_seq();

    let sender = entry(&h, &window_id);
    let sent = body.clone();
    let call = tokio::spawn(async move {
        sender
            .submit_message(
                &sent,
                Some(ComposerResidue {
                    body: &sent,
                    write_seq: seq,
                }),
                None,
            )
            .await
    });
    wait_for_screen(&h, &window_id, "the dialog the probe raises", |screen| {
        shows(screen, APPROVAL_HEADING)
    })
    .await;
    press(&h, &window_id, b'Q').await;
    std::fs::write(&gone, b"").expect("remove the dialog");
    let judgment = ended(call, "another writer's byte must end the call").await;

    assert!(
        matches!(judgment.placement, SubmitPlacement::NotPlaced(_)),
        "an interrupted call places nothing, got {:?}",
        judgment.placement
    );
    wait_for_screen(&h, &window_id, "the dialog to be gone", |screen| {
        heading_not_drawn(screen) && matches!(composer_of(screen), ComposerReadout::Tail(_))
    })
    .await;
    let after = rebuilt_screen(&h, &window_id).await;
    assert!(
        matches!(composer_of(&after), ComposerReadout::Tail(ref tail) if tail.ends_with("0299")),
        "the call's own probe must not stay at the end of the draft, the composer reads {:?}",
        composer_of(&after)
    );
    let received = child_input(&h, &window_id, &log).await;
    assert!(
        received.contains(&ERASE) && !received.contains(&b'\r'),
        "the probe is erased and no submit key is written; the child read {received:?}"
    );
}

/// The ledger vouches for an envelope the composer does not show, and the
/// probe's own write raises a frame that only erases the composer: that is not
/// the probe alone, so the envelope is neither written again nor submitted.
#[tokio::test]
async fn an_unshown_envelope_is_not_decided_on_while_its_probe_reads_as_a_torn_frame() {
    let dir = unique_temp_dir("screen-gate-torn-unshown");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-unshown", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let probe_byte = format!("byte={:02x}", SUBMIT_CHILD_PROBE.as_bytes()[0]);
    let overlay = torn_dialog(&probe_byte, "hidden", "erase", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            clear_scope: "held:",
            ..GateWindow::new(
                "tornunshown",
                "tornunshown-marker",
                &overlay,
                APPROVAL_HEADING,
            )
        },
    )
    .await;
    entry(&h, &window_id)
        .write_pty_input(b"\x1b[200~ENV\x1b[201~", InputOrigin::Message)
        .await
        .expect("seed write must not fail");
    wait_until_input_log_contains(&log, PASTE_END, HANG_GUARD).await;
    wait_for_screen(&h, &window_id, "the held paste to be read", |screen| {
        shows(screen, "PASTE 1")
    })
    .await;
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty,
        "the held envelope must not show for this case to mean anything"
    );
    let seq = entry(&h, &window_id).write_seq();

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move {
        sender
            .submit_message(
                "ENV",
                Some(ComposerResidue {
                    body: "ENV",
                    write_seq: seq,
                }),
                None,
            )
            .await
    });
    wait_for_screen(&h, &window_id, "the torn frame after the probe", |screen| {
        heading_not_drawn(screen) && composer_of(screen) == ComposerReadout::NotObserved
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a torn frame is waited on, not decided on and not given up on by a clock"
    );
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
    let received = child_input(&h, &window_id, &log).await;
    assert_eq!(
        count_subslice(&received, PASTE_START),
        1,
        "the envelope must not be written again; the child read {received:?}"
    );
    assert_only_the_take_back_reached_the_child_after_the_body(&received, FOREIGN);
}

/// The requester leaves after the body and its probe are written, while the
/// composer reads as a torn frame: the body's take-back needs a reading of
/// the composer too, so the daemon writes none of its own on it; only the one
/// the registered body gets in front of another writer's byte reaches the child.
#[tokio::test]
async fn no_take_back_is_written_for_a_departed_requester_while_the_composer_reads_as_a_torn_frame()
{
    let dir = unique_temp_dir("screen-gate-torn-departed");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-departed", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = torn_dialog("body", "hidden", "erase", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new("tornleft", "tornleft-marker", &overlay, APPROVAL_HEADING)
        },
    )
    .await;

    let (leave, requester) = tokio::sync::watch::channel(false);
    let sender = entry(&h, &window_id);
    let call =
        tokio::spawn(async move { sender.submit_message("hello", None, Some(requester)).await });
    let probe_after_the_body = [PASTE_END, SUBMIT_CHILD_PROBE.as_bytes()].concat();
    wait_until_input_log_contains(&log, &probe_after_the_body, HANG_GUARD).await;
    wait_for_screen(&h, &window_id, "the torn frame after the body", |screen| {
        heading_not_drawn(screen)
            && shows(screen, "PASTE 1")
            && composer_of(screen) == ComposerReadout::NotObserved
    })
    .await;
    leave.send(true).expect("the call holds its receiver");
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a take-back waits for a reading of the composer, however long that takes"
    );
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
    let received = child_input(&h, &window_id, &log).await;
    assert_only_the_take_back_reached_the_child_after_the_body(&received, FOREIGN);
}

/// Another writer's text stands in the composer, the probe shows behind it
/// and is erased, and the erase raises a frame that only erases the
/// composer: that is no sign the erase was reached, so the call goes on
/// waiting and writes no second erase.
#[tokio::test]
async fn a_probe_erase_is_not_taken_for_reached_while_the_composer_reads_as_a_torn_frame() {
    let dir = unique_temp_dir("screen-gate-torn-erase-behind");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-erase-behind", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = torn_dialog(&format!("byte={ERASE:02x}"), "hidden", "erase", &rest);
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new(
                "tornbehind",
                "tornbehind-marker",
                &overlay,
                APPROVAL_HEADING,
            )
        },
    )
    .await;
    seed_composer(&h, &window_id, "ENV").await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    wait_for_screen(&h, &window_id, "the torn frame after the erase", |screen| {
        heading_not_drawn(screen) && composer_of(screen) == ComposerReadout::NotObserved
    })
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a torn frame after the erase is waited on, not taken for the erase reached"
    );
    let judgment = finish_the_dialog_then_write_another_byte(&h, &window_id, &rest, call).await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
    let received = child_input(&h, &window_id, &log).await;
    assert_eq!(
        received.iter().filter(|&&byte| byte == ERASE).count(),
        1,
        "only the probe's own erase is written; the child read {received:?}"
    );
    assert!(
        !received.contains(&CLEAR),
        "no clear sequence follows another writer's text; the child read {received:?}"
    );
}

// --- over the wire --------------------------------------------------------

/// Sends `send_message` with `text` and returns the `not_written` message.
async fn send_text_expecting_not_written(
    client: &mut Connection,
    window_id: &str,
    text: &str,
) -> String {
    send(
        client,
        &ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: text.to_string(),
        },
    )
    .await;
    read_not_written(client).await
}

async fn read_not_written(client: &mut Connection) -> String {
    match read_response(client, Duration::from_secs(15)).await {
        DaemonMessage::Error { code, message, .. } => {
            assert_eq!(code, "not_written", "unexpected error: {message}");
            message
        }
        other => panic!("expected not_written, got {other:?}"),
    }
}

async fn send_text_expecting_text_sent(client: &mut Connection, window_id: &str, text: &str) {
    send(
        client,
        &ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: text.to_string(),
        },
    )
    .await;
    match read_response(client, Duration::from_secs(15)).await {
        DaemonMessage::TextSent { .. } => {}
        other => panic!("an input screen must still take {text:?} written through, got {other:?}"),
    }
}

/// A `send_message` body that is not one terminated line -- one with no
/// terminator, or a bare terminator, which is a submit key on its own -- is
/// written through rather than judged against the composer.
#[tokio::test]
async fn a_written_through_send_message_is_not_written_into_an_approval_dialog() {
    let dir = unique_temp_dir("screen-gate-raw");
    let h = Harness::start("screen-gate-raw", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("raw", "raw-marker", "key:beside", APPROVAL_HEADING),
    )
    .await;
    show_overlay(&h, &window_id, APPROVAL_HEADING).await;

    let judged = send_text_expecting_not_written(&mut client, &window_id, "hello\r").await;
    let unterminated = send_text_expecting_not_written(&mut client, &window_id, "alpha").await;
    let bare_terminator = send_text_expecting_not_written(&mut client, &window_id, "\r").await;
    assert_eq!(
        unterminated, judged,
        "a written-through body declined for the same screen must say the same thing"
    );
    assert_eq!(bare_terminator, judged);
    assert_child_took_nothing(&h, &window_id).await;

    hide_overlay(&h, &window_id, APPROVAL_HEADING).await;
    send_text_expecting_text_sent(&mut client, &window_id, "alpha").await;
    wait_for_screen(
        &h,
        &window_id,
        "the unterminated body to be drawn",
        |screen| composer_of(screen) == ComposerReadout::Body("alpha".to_string()),
    )
    .await;
    send_text_expecting_text_sent(&mut client, &window_id, "\r").await;
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED alpha", 1).await,
        1,
        "a bare terminator must still submit what an input screen holds"
    );
}

#[tokio::test]
async fn the_not_accepting_message_differs_from_every_other_reason() {
    let dir = unique_temp_dir("screen-gate-message");
    let h = Harness::start("screen-gate-message", detection_config(&dir)).await;
    let mut client = h.connect().await;

    let dialog = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new(
            "msg-dialog",
            "msg-dialog-marker",
            "key:beside",
            APPROVAL_HEADING,
        ),
    )
    .await;
    show_overlay(&h, &dialog, APPROVAL_HEADING).await;
    let not_accepting = send_text_expecting_not_written(&mut client, &dialog, "hello\r").await;

    let foreign_window = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow::new("msg-foreign", "msg-foreign-marker", "", ""),
    )
    .await;
    seed_composer(&h, &foreign_window, "SOMEONE-ELSES").await;
    let foreign = send_text_expecting_not_written(&mut client, &foreign_window, "MINE\r").await;

    // A delivery whose requester left after the body was written, with no
    // clear sequence to take it back: the window carries an unconfirmed write.
    let unconfirmed_window = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            clear: "",
            clear_scope: "frozen:",
            ..GateWindow::new("msg-unconfirmed", "msg-unconfirmed-marker", "", "")
        },
    )
    .await;
    let (leave, requester) = tokio::sync::watch::channel(false);
    let sender = entry(&h, &unconfirmed_window);
    let first =
        tokio::spawn(async move { sender.submit_message("first", None, Some(requester)).await });
    h.wait_for_scrollback(&unconfirmed_window, b"PASTE 1", Duration::from_secs(10))
        .await;
    leave.send(true).expect("the call holds its receiver");
    first
        .await
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");
    let unobservable =
        send_text_expecting_not_written(&mut client, &unconfirmed_window, "hello\r").await;

    let withdrawn_window = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            clear_scope: "discard-paste:",
            ..GateWindow::new("msg-withdrawn", "msg-withdrawn-marker", "", "")
        },
    )
    .await;
    let not_reflected_withdrawn =
        send_text_expecting_not_written(&mut client, &withdrawn_window, "hello\r").await;

    let interrupted_window = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            clear_scope: "frozen:",
            ..GateWindow::new("msg-interrupted", "msg-interrupted-marker", "", "")
        },
    )
    .await;
    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: interrupted_window.clone(),
            text: "hello\r".to_string(),
        },
    )
    .await;
    // The body has reached the child but never shows, so the daemon is still
    // waiting for it to reflect when the foreign write lands.
    h.wait_for_scrollback(&interrupted_window, b"PASTE 1", Duration::from_secs(10))
        .await;
    entry(&h, &interrupted_window)
        .write_pty_input(b"X", InputOrigin::Message)
        .await
        .expect("foreign write must not fail");
    let interrupted = read_not_written(&mut client).await;

    let others = [foreign, unobservable, not_reflected_withdrawn, interrupted];
    let distinct: std::collections::HashSet<&String> = others.iter().collect();
    assert_eq!(
        distinct.len(),
        others.len(),
        "each other reason must have been reached on its own: {others:?}"
    );
    for other in &others {
        assert_ne!(
            &not_accepting, other,
            "a screen that was not accepting input must not be reported in another reason's words"
        );
    }
}

/// The body raises a dialog whose first write only erases the composer, and
/// another writer's byte arrives while that frame stands: the dialog takes it
/// and the frame stays as it was. A frame with no composer in it says nothing
/// about the take-back, so the call is not over until a screen that shows a
/// composer, another writer, or the requester's leaving ends the check.
#[tokio::test]
async fn a_torn_frame_standing_after_another_writers_byte_is_not_taken_for_a_finished_check() {
    let dir = unique_temp_dir("screen-gate-torn-after-foreign");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let h = Harness::start("screen-gate-torn-after-foreign", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let overlay = format!(
        "body:hidden:torn|erase|{}",
        rest.to_str().expect("the dialog path must be valid UTF-8")
    );
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        GateWindow {
            input_log: Some(&log),
            ..GateWindow::new(
                "tornafterforeign",
                "tornafterforeign-marker",
                &overlay,
                APPROVAL_HEADING,
            )
        },
    )
    .await;

    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("hello", None, None).await });
    wait_for_screen(&h, &window_id, "the torn frame", |screen| {
        heading_not_drawn(screen) && composer_of(screen) == ComposerReadout::NotObserved
    })
    .await;
    press(&h, &window_id, b'R').await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a frame with no composer in it cannot show what became of the take-back, so the call is not over"
    );

    std::fs::write(&rest, b"").expect("draw the rest of the dialog");
    wait_for_screen(
        &h,
        &window_id,
        "the rest of the dialog to be drawn",
        |screen| shows(screen, APPROVAL_HEADING),
    )
    .await;
    press(&h, &window_id, b'S').await;
    let judgment = ended(call, "a second writer's byte must end the check").await;

    assert_no_submit_key_reached_the_child(&h, &window_id, &log, &judgment).await;
}
