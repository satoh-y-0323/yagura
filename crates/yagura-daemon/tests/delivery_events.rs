#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: what ends a judged delivery's wait for its write to show.
//!
//! The window layer decides that a body has shown from the order the receiver
//! handles its input in -- a probe typed behind the body shows once everything
//! before it has been handled -- and never from a clock. What can end the wait
//! without the body showing is a short list: the child exits, the requester of
//! the delivery leaves, another writer writes into the window. Every case here
//! drives a real `WindowRegistry` against `submit_child` in composer-draw mode
//! and calls `WindowEntry::submit_message` directly.
//!
//! "Nothing ends the wait by itself" is shown with the test's clock paused and
//! an hour of it let pass (`let_every_clock_bound_run_out`), never with a
//! sleep. The hang guards (`timeout(..)`) only bound a failure.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, SubmitDefinitionInput, approver_base_config, count_marker, input_log_read_so_far,
    let_every_clock_bound_run_out, let_every_clock_bound_run_out_while_a_thread_is_awaited,
    read_composer_now, report_status, settled_marker_count, unique_temp_dir,
    wait_until_composer_is, wait_until_status, write_submit_definition,
};
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{
    ComposerResidue, InputOrigin, NotPlaced, NotPlacedReason, SubmitJudgment, SubmitPlacement,
    WindowEntry,
};
use yagura_daemon::{DaemonConfig, PtyError};
use yagura_protocol::WindowStatus;
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

/// The longest any single case may take before it is called hung.
const HANG_GUARD: Duration = Duration::from_secs(180);

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

/// A `submit_child` window in composer-draw mode under a definition that states
/// no reflection bound of any kind. `clear` is the definition's clear sequence
/// (`""` for none), `timing` the fixture's repaint timing argument, `clear_scope`
/// the fixture's mode, and `input_log` a file the fixture appends everything it
/// reads to.
#[allow(clippy::too_many_arguments)]
async fn open_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    clear: &'static str,
    timing: &str,
    clear_scope: &str,
    input_log: Option<&Path>,
) -> String {
    write_submit_definition(
        dir,
        name,
        marker,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear,
            delivery_confirm_ms: 2000,
            resend_max: 2,
        },
    );
    let mut command = process::submit_child_command_with_composer(marker, 0, 0, 0, 0);
    command[9] = timing.to_string();
    command.push(String::new()); // fold_placeholder: none
    command.push(clear_scope.to_string());
    command.push(String::new()); // overlay: none
    command.push(String::new()); // overlay_heading
    if let Some(log) = input_log {
        command.push(
            log.to_str()
                .expect("the input log path must be valid UTF-8")
                .to_string(),
        );
    }
    let window_id = h.open(command, Some(name)).await;
    wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// Starts `submit_message` for `body` with a requester that can leave.
fn start_delivery(
    h: &Harness,
    window_id: &str,
    body: &'static str,
) -> (
    tokio::sync::watch::Sender<bool>,
    tokio::task::JoinHandle<yagura_daemon::pty::PtyResult<SubmitJudgment>>,
) {
    let (leave, requester) = tokio::sync::watch::channel(false);
    let sender = entry(h, window_id);
    let call =
        tokio::spawn(async move { sender.submit_message(body, None, Some(requester)).await });
    (leave, call)
}

fn not_placed_of(judgment: &SubmitJudgment, what: &str) -> NotPlaced {
    match &judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => *not_placed,
        other => panic!("{what}, got {other:?}"),
    }
}

// =====================================================================
// R1: a body that shows late is still placed
// =====================================================================

/// A definition that states no reflection bound, and a screen that repaints
/// three seconds after each write: far past the 200 ms a bound used to default
/// to. The delivery goes on to place the body once it shows.
#[tokio::test]
async fn a_body_the_screen_shows_three_seconds_late_is_placed_not_given_up_on() {
    let dir = unique_temp_dir("events-late-screen");
    let h = Harness::start("events-late-screen", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "late",
        "late-marker",
        "\\u0015",
        "3000",
        "",
        None,
    )
    .await;

    let judgment = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message("BODY", None, None),
    )
    .await
    .expect("a body that shows late must still be placed")
    .expect("write must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a body that shows late must be placed, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 1).await,
        1,
        "the body reaches the receiver exactly once"
    );
}

// =====================================================================
// R2: a detector that holds the model is waited for
// =====================================================================

/// While the window's terminal model is held elsewhere nothing can be read
/// from it. The delivery waits for the model; once it is let go the body is
/// placed.
#[tokio::test]
async fn a_delivery_waits_for_a_held_model_and_places_the_body_once_it_is_let_go() {
    let dir = unique_temp_dir("events-held-model");
    let h = Harness::start("events-held-model", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "held-model",
        "held-model-marker",
        "\\u0015",
        "0",
        "",
        None,
    )
    .await;

    let hold = entry(&h, &window_id).session().hold_model();
    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("BODY", None, None).await });
    let_every_clock_bound_run_out_while_a_thread_is_awaited().await;
    assert!(
        !call.is_finished(),
        "a delivery to a window whose model is held waits for the model, not for a clock"
    );
    drop(hold);

    let judgment = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("the delivery must go on once the model is let go")
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the body must be placed once the model is let go, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 1).await,
        1
    );
}

// =====================================================================
// R3: a body the receiver drops
// =====================================================================

/// A receiver that throws a pasted body away but keeps the keystrokes behind
/// it: the probe typed after the body shows alone, so the body did not arrive.
/// Nothing of the call's is left, and the submit key is never pressed.
#[tokio::test]
async fn a_body_the_receiver_drops_is_not_reflected_leaves_nothing_and_never_gets_the_submit_key() {
    let dir = unique_temp_dir("events-dropped-body");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-dropped-body", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "dropped-body",
        "dropped-body-marker",
        "\\u0015",
        "0",
        "discard-paste:",
        Some(&log),
    )
    .await;

    let judgment = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message("BODY", None, None),
    )
    .await
    .expect("a dropped body is reported, not waited on")
    .expect("write must not fail at the PTY level");

    let declined = not_placed_of(&judgment, "a body the receiver dropped must not be placed");
    assert_eq!(declined.reason, NotPlacedReason::NotReflected);
    assert!(
        !declined.envelope_remains,
        "the receiver dropped the body and the probe was erased: nothing remains"
    );
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty,
        "the probe must have been erased before the call answered"
    );
    let received = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert!(
        !received.contains(&b'\r'),
        "the submit key must never have been written; the receiver read {received:?}"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

// =====================================================================
// R4: a receiver that shows nothing is waited on until something ends it
// =====================================================================

/// A receiver that shows nothing once the body arrives. A status change and a
/// resize are cues to look again, not the end of the wait; another writer's
/// byte is, and the body's take-back reaches the receiver right in front of
/// it. The order is the point: had the status change or the resize ended the
/// wait, the call would have answered before the byte was written.
#[tokio::test]
async fn a_receiver_that_shows_nothing_is_waited_on_through_status_changes_and_resizes_until_another_writer_writes()
 {
    let dir = unique_temp_dir("events-stalled");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-stalled", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "stalled",
        "stalled-marker",
        "\\u0015",
        "0",
        "frozen:",
        Some(&log),
    )
    .await;

    let (_leave, call) = start_delivery(&h, &window_id, "BODY");
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    let mut observer = h.connect().await;
    report_status(&mut observer, &window_id, WindowStatus::Running).await;
    report_status(&mut observer, &window_id, WindowStatus::WaitingInput).await;
    entry(&h, &window_id)
        .session()
        .resize(100, 30)
        .await
        .expect("resize");
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "neither a status change, a resize nor a clock ends the wait for a body that shows nothing"
    );

    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Message)
        .await
        .expect("the other writer's byte must not fail");
    let judgment = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("another writer's byte must end the wait")
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");

    let declined = not_placed_of(&judgment, "a body that never showed must not be placed");
    assert_eq!(declined.reason, NotPlacedReason::Interrupted);
    let received = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert!(
        received.contains(&b'Q'),
        "the other writer's byte was written; the receiver read {received:?}"
    );
    let other = received
        .iter()
        .rposition(|&byte| byte == b'Q')
        .expect("the other writer's byte was read");
    let take_back = received[..other]
        .iter()
        .rev()
        .take_while(|&&byte| byte == 0x15)
        .count();
    assert!(
        take_back > 0,
        "the body's take-back must reach the receiver right in front of the other writer's byte; \
         the receiver read {received:?}"
    );
    assert!(
        !received.contains(&b'\r') && !received[..other - take_back].contains(&0x15),
        "neither the submit key nor any other clear may have been written; the receiver read \
         {received:?}"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

// =====================================================================
// R5: the child exits while the call waits
// =====================================================================

#[tokio::test]
async fn a_child_that_exits_while_the_delivery_waits_ends_it_with_child_exited() {
    let dir = unique_temp_dir("events-child-exits");
    let h = Harness::start("events-child-exits", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "child-exits",
        "child-exits-marker",
        "\\u0015",
        "0",
        "frozen:",
        None,
    )
    .await;

    let (_leave, call) = start_delivery(&h, &window_id, "BODY");
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    entry(&h, &window_id).session().terminate();

    let result = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("the child exiting must end the wait")
        .expect("the submit task must not panic");
    assert!(
        matches!(result, Err(PtyError::ChildExited)),
        "a child that exits while the delivery waits ends it with ChildExited, got {result:?}"
    );
}

// =====================================================================
// R6: the requester leaves
// =====================================================================

/// A requester that has already left: nothing is written at all.
#[tokio::test]
async fn a_requester_that_has_already_left_gets_nothing_written() {
    let dir = unique_temp_dir("events-requester-gone-first");
    let h = Harness::start("events-requester-gone-first", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "gone-first",
        "gone-first-marker",
        "\\u0015",
        "0",
        "",
        None,
    )
    .await;

    let seq = entry(&h, &window_id).write_seq();
    let (_leave, requester) = tokio::sync::watch::channel(true);
    let judgment = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message("BODY", None, Some(requester)),
    )
    .await
    .expect("a requester that has left must not be waited for")
    .expect("write must not fail at the PTY level");

    let declined = not_placed_of(&judgment, "nothing may be placed for a requester that left");
    assert!(!declined.envelope_remains, "the body was never written");
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq,
        "nothing was written"
    );
    assert_eq!(count_marker(&h, &window_id, "PASTE "), 0);
}

/// A requester that leaves once the body is in: the submit key is not written,
/// and the call goes on until the composer reads empty -- the take-back is
/// confirmed before the call answers, not left to the next delivery.
#[tokio::test]
async fn a_requester_that_leaves_after_the_body_gets_it_taken_back_and_the_composer_empty_before_the_call_answers()
 {
    let dir = unique_temp_dir("events-requester-gone-takeback");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-requester-gone-takeback", detection_config(&dir)).await;
    let mut client = h.connect().await;
    // `held:` keeps everything unpainted from the first pasted body until its
    // take-back's own clear arrives.
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "gone-takeback",
        "gone-takeback-marker",
        "\\u0015",
        "0",
        "held:",
        Some(&log),
    )
    .await;

    let (leave, call) = start_delivery(&h, &window_id, "BODY");
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    leave.send(true).expect("the call holds its receiver");
    let judgment = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("the take-back must be confirmed, not waited on forever")
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");

    let declined = not_placed_of(&judgment, "a requester that left must not get a placement");
    assert_eq!(declined.reason, NotPlacedReason::NotReflected);
    assert!(
        !declined.envelope_remains,
        "the take-back was confirmed by the probe showing alone"
    );
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty,
        "the composer must read empty by the time the call answers"
    );
    let received = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert!(
        received.contains(&0x15),
        "the take-back's clear sequence must have been written; the receiver read {received:?}"
    );
    assert!(
        !received.contains(&b'\r'),
        "the submit key must never have been written; the receiver read {received:?}"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

/// A requester that leaves once the body is in, and no clear sequence to take
/// it back with: the call says something remains, and the next delivery does
/// not write the body a second time.
#[tokio::test]
async fn a_requester_that_leaves_when_nothing_can_take_the_body_back_leaves_a_mark_so_the_next_delivery_does_not_write_it_twice()
 {
    let dir = unique_temp_dir("events-requester-gone-marked");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-requester-gone-marked", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "gone-marked",
        "gone-marked-marker",
        "",
        "0",
        "frozen:",
        Some(&log),
    )
    .await;

    let (leave, call) = start_delivery(&h, &window_id, "BODY");
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    leave.send(true).expect("the call holds its receiver");
    let first = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("a requester that left must not be waited for")
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    let declined = not_placed_of(&first, "a requester that left must not get a placement");
    assert_eq!(declined.reason, NotPlacedReason::NotReflected);
    assert!(
        declined.envelope_remains,
        "nothing can take the body back, so it may remain"
    );

    let seq = entry(&h, &window_id).write_seq();
    let residue = first.residue_write_seq.map(|write_seq| ComposerResidue {
        body: "BODY",
        write_seq,
    });
    let second = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message("BODY", residue, None),
    )
    .await
    .expect("the next delivery must answer at once over a write nothing confirmed")
    .expect("write must not fail at the PTY level");
    let declined_again = not_placed_of(
        &second,
        "the body must not be written over a write nothing confirmed",
    );
    assert!(
        !declined_again.envelope_remains,
        "the next delivery wrote nothing of its own"
    );
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq,
        "the next delivery must write nothing"
    );
    let received = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    let pastes = received
        .windows(b"\x1b[200~BODY".len())
        .filter(|window| *window == b"\x1b[200~BODY")
        .count();
    assert_eq!(
        pastes, 1,
        "the body must have been written once; the receiver read {received:?}"
    );
}

// =====================================================================
// R7: a body made of, or starting with, the probe's character
// =====================================================================

#[tokio::test]
async fn bodies_made_of_or_starting_with_the_probe_character_are_placed() {
    let dir = unique_temp_dir("events-probe-character");
    let h = Harness::start("events-probe-character", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "probe-character",
        "probe-character-marker",
        "\\u0015",
        "0",
        "",
        None,
    )
    .await;

    for body in ["x", "xx", "xylophone"] {
        let judgment = tokio::time::timeout(
            HANG_GUARD,
            entry(&h, &window_id).submit_message(body, None, None),
        )
        .await
        .expect("a body that starts with the probe's character must not be waited on forever")
        .expect("write must not fail at the PTY level");
        match judgment.placement {
            SubmitPlacement::Placed(_) => {}
            other => panic!("{body:?} must be placed, got {other:?}"),
        }
        assert_eq!(
            settled_marker_count(&h, &window_id, &format!("SUBMITTED {body}\r"), 1).await,
            1,
            "{body:?} reaches the receiver exactly once"
        );
    }
}

// =====================================================================
// R8: a long body whose reading is unobservable
// =====================================================================

/// A body long enough to run the composer past the bottom of the viewport: the
/// reading after the probe cannot be restored, but the ledger shows nothing
/// but the call's own writes, so the body counts as shown.
#[tokio::test]
async fn a_body_too_long_to_read_back_is_placed_when_only_the_calls_own_writes_were_made() {
    let dir = unique_temp_dir("events-long-body");
    let h = Harness::start("events-long-body", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "long-body",
        "long-body-marker",
        "\\u0015",
        "0",
        "",
        None,
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet and the receiver \
                has not reacted to the previous attempt so this one carries the same envelope \
                forward "
        .repeat(6);

    let judgment = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message(&body, None, None),
    )
    .await
    .expect("a body that cannot be read back must still be decided")
    .expect("write must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the body must be placed, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED the retry budget", 1).await,
        1,
        "the body reaches the receiver exactly once"
    );
}

// =====================================================================
// A resend whose own envelope the receiver already took
// =====================================================================

/// The ledger vouches that only the caller's own envelope was written since
/// its baseline, yet the receiver has taken the line and left its composer
/// empty without reacting. Nothing will draw the envelope again, so the call
/// must not wait for it to show: the probe written behind it shows alone, and
/// the same call goes on to write the body fresh, once. No requester and no
/// other writer ends this call; it ends on its own.
#[tokio::test]
async fn a_resend_whose_envelope_the_receiver_already_took_writes_the_body_fresh_once() {
    let dir = unique_temp_dir("events-envelope-taken");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-envelope-taken", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "envelope-taken",
        "envelope-taken-marker",
        "\\u0015",
        "0",
        "",
        Some(&log),
    )
    .await;

    let first = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message("BODY", None, None),
    )
    .await
    .expect("the first attempt must be decided")
    .expect("write must not fail at the PTY level");
    assert!(matches!(first.placement, SubmitPlacement::Placed(_)));
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 1).await,
        1
    );
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Empty,
        Duration::from_secs(10),
    )
    .await;

    let residue = first.residue_write_seq.map(|write_seq| ComposerResidue {
        body: "BODY",
        write_seq,
    });
    let second = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message("BODY", residue, None),
    )
    .await
    .expect("an envelope the receiver already took must not be waited on to show")
    .expect("write must not fail at the PTY level");

    match second.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the body must be written fresh and placed, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 2).await,
        2,
        "the resend reaches the receiver as one more submission"
    );
    let received = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    let pastes = received
        .windows(b"[200~BODY".len())
        .filter(|window| *window == b"[200~BODY")
        .count();
    assert_eq!(
        pastes, 2,
        "the resend writes the body exactly once more; the receiver read {received:?}"
    );
}

// =====================================================================
// R9: a detector that does not answer
// =====================================================================

/// While the model is held the detector cannot answer. The requester leaving
/// still ends the wait, and the window's turn is handed on: the next delivery
/// goes ahead as soon as the model is let go.
#[tokio::test]
async fn a_requester_leaving_ends_the_wait_on_a_detector_that_does_not_answer_and_frees_the_turn() {
    let dir = unique_temp_dir("events-detector-silent");
    let h = Harness::start("events-detector-silent", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "silent",
        "silent-marker",
        "\\u0015",
        "0",
        "",
        None,
    )
    .await;

    let hold = entry(&h, &window_id).session().hold_model();
    let (leave, first) = start_delivery(&h, &window_id, "FIRST");
    let_every_clock_bound_run_out_while_a_thread_is_awaited().await;
    assert!(
        !first.is_finished(),
        "the call waits for a detector that does not answer"
    );
    leave.send(true).expect("the call holds its receiver");
    let judgment = tokio::time::timeout(HANG_GUARD, first)
        .await
        .expect("the requester leaving must end the wait, the model still held")
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    not_placed_of(&judgment, "a requester that left must not get a placement");

    let sender = entry(&h, &window_id);
    let second = tokio::spawn(async move { sender.submit_message("SECOND", None, None).await });
    let_every_clock_bound_run_out_while_a_thread_is_awaited().await;
    assert!(
        !second.is_finished(),
        "the next delivery has the turn and waits for the model"
    );
    drop(hold);
    let judgment = tokio::time::timeout(HANG_GUARD, second)
        .await
        .expect("the next delivery must go ahead once the model is let go")
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the next delivery must place its body, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED SECOND", 1).await,
        1
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED FIRST"), 0);
}

/// The child exiting ends the wait on a detector that does not answer too.
#[tokio::test]
async fn a_child_that_exits_ends_the_wait_on_a_detector_that_does_not_answer() {
    let dir = unique_temp_dir("events-detector-silent-exit");
    let h = Harness::start("events-detector-silent-exit", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "silent-exit",
        "silent-exit-marker",
        "\\u0015",
        "0",
        "",
        None,
    )
    .await;

    let hold = entry(&h, &window_id).session().hold_model();
    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move { sender.submit_message("BODY", None, None).await });
    let_every_clock_bound_run_out_while_a_thread_is_awaited().await;
    assert!(
        !call.is_finished(),
        "the call waits for a detector that does not answer"
    );
    entry(&h, &window_id).session().terminate();
    let result = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("the child exiting must end the wait, the model still held")
        .expect("the submit task must not panic");
    drop(hold);
    assert!(
        matches!(result, Err(PtyError::ChildExited)),
        "a child that exits ends the wait with ChildExited, got {result:?}"
    );
}

// =====================================================================
// R10: an unconfirmed write declines at once
// =====================================================================

/// A window that carries a write nothing confirmed declines the next judged
/// delivery as unobservable at once: it has nothing to wait for, so it answers
/// although no event will ever come.
#[tokio::test]
async fn a_window_carrying_an_unconfirmed_write_declines_the_next_delivery_at_once() {
    let dir = unique_temp_dir("events-unconfirmed");
    let h = Harness::start("events-unconfirmed", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window(
        &h,
        &mut client,
        &dir,
        "unconfirmed",
        "unconfirmed-marker",
        "",
        "0",
        "frozen:",
        None,
    )
    .await;
    let (leave, call) = start_delivery(&h, &window_id, "FIRST");
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    leave.send(true).expect("the call holds its receiver");
    tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("a requester that left must not be waited for")
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");

    let seq = entry(&h, &window_id).write_seq();
    let judgment = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message("SECOND", None, None),
    )
    .await
    .expect("a window carrying an unconfirmed write answers at once, it waits for nothing")
    .expect("write must not fail at the PTY level");

    let declined = not_placed_of(
        &judgment,
        "nothing may be written over an unconfirmed write",
    );
    assert_eq!(declined.reason, NotPlacedReason::Unobservable);
    assert!(!declined.envelope_remains);
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        seq,
        "nothing was written"
    );
}
