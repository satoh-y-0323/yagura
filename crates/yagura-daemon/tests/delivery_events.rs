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
    let_every_clock_bound_run_out, let_every_clock_bound_run_out_while_a_thread_is_awaited, policy,
    read_composer_now, read_response, report_status, send, settled_marker_count, unique_temp_dir,
    wait_until_composer_is, wait_until_frame_satisfies, wait_until_input_log_contains,
    wait_until_status, write_submit_definition,
};
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{
    ComposerResidue, InputOrigin, NotPlaced, NotPlacedReason, SendStage, SubmitJudgment,
    SubmitPlacement, WindowEntry,
};
use yagura_daemon::{DaemonConfig, PtyError};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};
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
    let call = tokio::spawn(async move {
        sender
            .submit_message(body, None, SendStage::BeforeSubmitKey, Some(requester))
            .await
    });
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
        entry(&h, &window_id).submit_message("BODY", None, SendStage::BeforeSubmitKey, None),
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
    let call = tokio::spawn(async move {
        sender
            .submit_message("BODY", None, SendStage::BeforeSubmitKey, None)
            .await
    });
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
        entry(&h, &window_id).submit_message("BODY", None, SendStage::BeforeSubmitKey, None),
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
        entry(&h, &window_id).submit_message(
            "BODY",
            None,
            SendStage::BeforeSubmitKey,
            Some(requester),
        ),
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
        entry(&h, &window_id).submit_message("BODY", residue, SendStage::BeforeSubmitKey, None),
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
            entry(&h, &window_id).submit_message(body, None, SendStage::BeforeSubmitKey, None),
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
        entry(&h, &window_id).submit_message(&body, None, SendStage::BeforeSubmitKey, None),
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
        entry(&h, &window_id).submit_message("BODY", None, SendStage::BeforeSubmitKey, None),
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
        entry(&h, &window_id).submit_message("BODY", residue, SendStage::BeforeSubmitKey, None),
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
    let second = tokio::spawn(async move {
        sender
            .submit_message("SECOND", None, SendStage::BeforeSubmitKey, None)
            .await
    });
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
    let call = tokio::spawn(async move {
        sender
            .submit_message("BODY", None, SendStage::BeforeSubmitKey, None)
            .await
    });
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
        entry(&h, &window_id).submit_message("SECOND", None, SendStage::BeforeSubmitKey, None),
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

// =====================================================================
// What confirms a handoff delivery: the composer the envelope sat in
// =====================================================================
//
// A delivery is confirmed when the receiver took the envelope as one message.
// After the submit key, the evidence every receiver gives is the composer the
// envelope was in reading empty on a finished screen; a status that moves from
// waiting for input to working is further evidence. Every case here drives a
// real `send_handoff` against `submit_child` and counts what the child read
// from its input log, so a body written twice shows as two pastes.
//
// The cases that need the receiver to hold something use files the test
// creates, never a delay, and the confirmation window is let run out with
// `let_every_clock_bound_run_out`, never a sleep.

/// How long a `handoff_sent` reply may take before the case is called hung.
const REPLY_TIMEOUT: Duration = Duration::from_secs(60);

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";

/// How a window the cases below deliver to is launched.
struct ReceivingWindow<'a> {
    name: &'a str,
    /// The first this many submit keys after a paste are taken as line breaks.
    paste_guard_keys: u64,
    /// `submit_child`'s `receive_mode` argument; `-` for none.
    receive_mode: &'a str,
    delivery_confirm_ms: u64,
    resend_max: u8,
    /// `submit_child`'s timing argument: a delay in ms, or `pause|<path>`.
    timing: &'a str,
    /// `submit_child`'s `fold_placeholder` argument; `-` for no folding.
    fold_placeholder: &'a str,
}

impl<'a> ReceivingWindow<'a> {
    fn new(name: &'a str) -> Self {
        ReceivingWindow {
            name,
            paste_guard_keys: 0,
            receive_mode: "-",
            delivery_confirm_ms: 2000,
            resend_max: 2,
            timing: "0",
            fold_placeholder: "-",
        }
    }
}

fn handoff_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(dir)
    }
}

async fn open_receiving_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    window: &ReceivingWindow<'_>,
    input_log: &Path,
) -> String {
    let marker = format!("{}-marker", window.name);
    write_submit_definition(
        dir,
        window.name,
        &marker,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear: "\\u0015",
            delivery_confirm_ms: window.delivery_confirm_ms,
            resend_max: window.resend_max,
        },
    );
    let mut command =
        process::submit_child_command_with_composer(&marker, 0, window.paste_guard_keys, 0, 0);
    command[9] = window.timing.to_string();
    command.push(window.fold_placeholder.to_string());
    command.push("-".to_string()); // clear_scope: ordinary
    command.push("-".to_string()); // overlay: none
    command.push("-".to_string()); // overlay_heading
    command.push(
        input_log
            .to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    command.push("0".to_string()); // paste_enter_window_ms
    command.push(window.receive_mode.to_string());
    let window_id = h.open_delivering(command, Some(window.name)).await;
    wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// Sends a `send_handoff` to `window_id` without waiting for its reply.
async fn request_handoff(client: &mut Connection, window_id: &str, note: &str) {
    send(
        client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.to_string()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: note.to_string(),
        },
    )
    .await;
}

/// The handoff's answer as its wire string.
async fn handoff_reply(client: &mut Connection) -> String {
    match read_response(client, REPLY_TIMEOUT).await {
        DaemonMessage::HandoffSent { delivery, .. } => serde_json::to_value(delivery)
            .expect("a delivery encodes")
            .as_str()
            .expect("a delivery is a string on the wire")
            .to_string(),
        other => panic!("expected handoff_sent, got {other:?}"),
    }
}

/// Waits until the receiver has read a submit key: the delivery has pressed it.
async fn wait_for_the_submit_key(input_log: &Path) {
    wait_until_input_log_contains(input_log, b"\r", HANG_GUARD).await;
}

async fn wait_until_confirmed_status(entry: &WindowEntry, wanted: WindowStatus) {
    let mut changes = entry.watch_status();
    tokio::time::timeout(HANG_GUARD, async {
        while entry.current_status() != wanted {
            changes
                .changed()
                .await
                .expect("the window's status channel stays open");
        }
    })
    .await
    .unwrap_or_else(|_| panic!("the window never reached {wanted:?}"));
}

/// Waits until the model holds the frame a `take-on` receiver draws once it
/// holds the submit key: the envelope in a composer whose paint never finishes.
/// A reading the delivery takes from here on is of that frame, never of the
/// finished one from before the key.
async fn wait_until_the_held_envelope_is_drawn_unfinished(h: &Harness, window_id: &str) {
    wait_until_frame_satisfies(
        h,
        window_id,
        "the envelope in an unfinished frame",
        HANG_GUARD,
        |readout, unfinished| {
            unfinished
                && matches!(readout, ComposerReadout::Body(text) if text.contains("[yagura handoff]"))
        },
    )
    .await;
}

/// What the receiver read after the first submit key behind the body's paste.
fn read_after_the_submit_key(read: &[u8]) -> &[u8] {
    let paste_end = read
        .windows(PASTE_END.len())
        .position(|window| window == PASTE_END)
        .unwrap_or_else(|| panic!("the receiver never read the end of a paste; it read {read:?}"))
        + PASTE_END.len();
    let key = read[paste_end..]
        .iter()
        .position(|byte| *byte == b'\r')
        .unwrap_or_else(|| panic!("the receiver never read a submit key; it read {read:?}"));
    &read[paste_end + key + 1..]
}

fn count_of(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

fn create_signal_file(path: &Path) {
    std::fs::write(path, b"").expect("create the file the receiver watches");
}

/// A note long enough to run the envelope past the bottom of the screen.
fn note_taller_than_the_screen() -> String {
    "lorem ipsum dolor sit amet ".repeat(36)
}

/// R1: the draft is taller than the screen, so the window's marker line is out
/// of view and the status before the submit key is `unknown`. The composer
/// emptying after the key is the evidence the delivery has.
#[tokio::test]
async fn a_body_taller_than_the_screen_is_taken_once_its_composer_empties_after_the_submit_key() {
    let dir = unique_temp_dir("events-confirm-tall");
    let log = dir.join("child-input.log");
    let probe_read = dir.join("probe-may-be-read");
    let h = Harness::start("events-confirm-tall", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let mode = format!("hold-probe|{}", probe_read.display());
    let window = ReceivingWindow {
        receive_mode: &mode,
        resend_max: 0,
        ..ReceivingWindow::new("confirm-tall")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, &note_taller_than_the_screen()).await;
    // The child holds the probe unread behind the pasted body: the delivery
    // waits for it to show while the body, taller than the screen, stands alone.
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    wait_until_confirmed_status(&entry(&h, &window_id), WindowStatus::Unknown).await;
    create_signal_file(&probe_read);

    assert_eq!(
        handoff_reply(&mut client).await,
        "taken",
        "the composer emptied after the submit key, whatever the status before it was"
    );
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(count_of(&read, PASTE_START), 1, "the body is written once");
    assert_eq!(count_of(&read, b"\r"), 1, "the submit key is pressed once");
}

/// R3: the receiver takes the submit key as a line break, so the envelope
/// stays in its composer. Nothing is confirmed, and only the submit key goes
/// out again, as many times as `resend_max` allows.
#[tokio::test]
async fn a_receiver_that_keeps_the_envelope_gets_only_the_submit_key_again() {
    let dir = unique_temp_dir("events-confirm-kept");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-confirm-kept", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let window = ReceivingWindow {
        paste_guard_keys: u64::MAX,
        delivery_confirm_ms: 600,
        resend_max: 2,
        ..ReceivingWindow::new("confirm-kept")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;

    assert_eq!(
        handoff_reply(&mut client).await,
        "taken",
        "a submitted envelope is taken however its submission was judged"
    );
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(count_of(&read, PASTE_START), 1, "the body is written once");
    assert_eq!(
        count_of(&read, b"\r"),
        3,
        "the submit key is pressed once and resent resend_max (2) times"
    );
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert!(
            text.contains("[yagura handoff]"),
            "the envelope is still in the composer: {text:?}"
        ),
        other => panic!("expected the envelope still in the composer, got {other:?}"),
    }
}

/// R4: the receiver takes the line and empties its composer without any status
/// reacting. The envelope has been received; writing it again is a second
/// message.
#[tokio::test]
async fn a_receiver_that_empties_its_composer_without_reacting_is_not_written_to_twice() {
    let dir = unique_temp_dir("events-confirm-silent");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-confirm-silent", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let window = ReceivingWindow {
        receive_mode: "silent",
        delivery_confirm_ms: 600,
        resend_max: 2,
        ..ReceivingWindow::new("confirm-silent")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;

    let delivery = handoff_reply(&mut client).await;
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        count_of(&read, PASTE_START),
        1,
        "the body must reach the receiver once; it read {read:?}"
    );
    assert_eq!(
        delivery, "taken",
        "the composer emptied after the submit key"
    );
}

/// R5: another writer wrote after the submit key and before the composer
/// emptied, so the composer reading empty says nothing about the envelope. The
/// delivery stays unconfirmed, and nothing is written over it: `resend_max` is
/// 1, which a rewrite of the body would spend.
#[tokio::test]
async fn another_writers_write_before_the_composer_empties_is_not_a_confirmation() {
    let dir = unique_temp_dir("events-confirm-foreign");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-confirm-foreign", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let window = ReceivingWindow {
        paste_guard_keys: u64::MAX,
        resend_max: 1,
        ..ReceivingWindow::new("confirm-foreign")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;
    wait_for_the_submit_key(&log).await;
    // The key was taken as a line break, so the envelope is still in the
    // composer. Another writer types behind it and clears the composer.
    for bytes in [&b"Q"[..], &b"\x15"[..]] {
        entry(&h, &window_id)
            .write_pty_input(bytes, InputOrigin::Interactive)
            .await
            .expect("the other writer's bytes must not fail");
    }
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Empty,
        Duration::from_secs(10),
    )
    .await;
    let_every_clock_bound_run_out().await;

    assert_eq!(
        handoff_reply(&mut client).await,
        "taken",
        "a submitted envelope is taken however its submission was judged"
    );
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        count_of(&read, PASTE_START),
        1,
        "the body is written once; it read {read:?}"
    );
}

/// R4b: the composer is drawn unfinished after the submit key and empties only
/// once the confirmation window has run out, so the delivery's own wait for the
/// composer sees nothing and its resend attempt is the one that finds it empty.
/// That the key was pressed and the composer is empty is enough: the body is
/// not written again, and the delivery is confirmed.
#[tokio::test]
async fn a_composer_that_empties_only_after_the_confirmation_window_is_taken_without_a_rewrite() {
    let dir = unique_temp_dir("events-confirm-late-empty");
    let log = dir.join("child-input.log");
    let take_now = dir.join("take-now");
    let h = Harness::start("events-confirm-late-empty", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let mode = format!("take-on|{}", take_now.display());
    let window = ReceivingWindow {
        receive_mode: &mode,
        ..ReceivingWindow::new("confirm-late-empty")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;
    wait_for_the_submit_key(&log).await;
    wait_until_the_held_envelope_is_drawn_unfinished(&h, &window_id).await;
    let_every_clock_bound_run_out().await;
    create_signal_file(&take_now);

    let delivery = handoff_reply(&mut client).await;
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        count_of(&read, PASTE_START),
        1,
        "the body must not be written again over an envelope the receiver took; the delivery was {delivery:?}, the receiver read {read:?}"
    );
    assert_eq!(
        delivery, "taken",
        "the composer the submit key was pressed in read empty on a finished screen"
    );
}

/// R7: another writer wrote between the submit key and the composer emptying.
/// What the composer reads then is not evidence about the envelope either way,
/// and the body is not written a second time.
#[tokio::test]
async fn another_writer_between_the_submit_key_and_the_empty_composer_does_not_get_the_body_rewritten()
 {
    let dir = unique_temp_dir("events-confirm-late-foreign");
    let log = dir.join("child-input.log");
    let take_now = dir.join("take-now");
    let h = Harness::start("events-confirm-late-foreign", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let mode = format!("take-on|{}", take_now.display());
    let window = ReceivingWindow {
        receive_mode: &mode,
        resend_max: 2,
        ..ReceivingWindow::new("confirm-late-foreign")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;
    wait_for_the_submit_key(&log).await;
    wait_until_the_held_envelope_is_drawn_unfinished(&h, &window_id).await;
    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Interactive)
        .await
        .expect("the other writer's byte must not fail");
    let_every_clock_bound_run_out().await;
    create_signal_file(&take_now);

    assert_eq!(
        handoff_reply(&mut client).await,
        "taken",
        "a submitted envelope is taken however its submission was judged"
    );
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        count_of(&read, PASTE_START),
        1,
        "the body must not be written again; it read {read:?}"
    );
}

/// R8: a resend attempt after the submit key is declined with nothing to carry
/// forward (another writer ended its wait behind its probe), and a later
/// attempt reads the composer empty. The submit key having been pressed is a
/// fact of the delivery, not of the last attempt's record: the body is still
/// not written again.
///
/// The envelope shows exactly as written while another writer's bytes cancel
/// each other out, which an existing route resends only behind a probe; the
/// paint is held so that probe stays unread on screen until the test lets it go.
#[tokio::test]
async fn a_declined_resend_attempt_does_not_let_a_later_empty_composer_get_the_body_rewritten() {
    let dir = unique_temp_dir("events-confirm-declined-attempt");
    let log = dir.join("child-input.log");
    let paint_held = dir.join("paint-held");
    let h = Harness::start("events-confirm-declined-attempt", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let timing = format!("pause|{}", paint_held.display());
    let window = ReceivingWindow {
        paste_guard_keys: u64::MAX,
        delivery_confirm_ms: 1000,
        resend_max: 3,
        timing: &timing,
        ..ReceivingWindow::new("confirm-declined-attempt")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;
    wait_for_the_submit_key(&log).await;
    // Nothing is repainted from here: the screen keeps showing the envelope
    // while another writer types a byte and takes it back.
    create_signal_file(&paint_held);
    for bytes in [&b"Q"[..], &b""[..]] {
        entry(&h, &window_id)
            .write_pty_input(bytes, InputOrigin::Interactive)
            .await
            .expect("the other writer's bytes must not fail");
    }
    let_every_clock_bound_run_out().await;
    // Read from here on: a read started before the clock was let run out would
    // spend its own bound with it.
    let mut reply = tokio::spawn(async move { handoff_reply(&mut client).await });
    // The resend attempt writes a probe behind the envelope it reads and waits
    // for it to show, which a held paint never lets it. A delivery that
    // answers instead has no attempt left to interrupt, and is judged on its
    // answer alone.
    let answered_early = tokio::select! {
        _ = wait_until_input_log_contains(&log, b"Qx", HANG_GUARD) => None,
        delivery = &mut reply => Some(delivery.expect("the reply task must not panic")),
    };
    let delivery = match answered_early {
        Some(delivery) => delivery,
        None => {
            // Another writer ends that wait, then the composer is emptied and
            // painted.
            entry(&h, &window_id)
                .write_pty_input(b"", InputOrigin::Interactive)
                .await
                .expect("the other writer's byte must not fail");
            std::fs::remove_file(&paint_held).expect("let the receiver paint again");
            reply.await.expect("the reply task must not panic")
        }
    };

    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        count_of(&read, PASTE_START),
        1,
        "the body must not be written again; the delivery was {delivery:?}, the receiver read {read:?}"
    );
    assert_eq!(
        delivery, "taken",
        "a submitted envelope is taken however its submission was judged"
    );
}

/// R9a: another writer wrote after the submit key, into a composer still
/// holding the envelope. Whatever the composer reads then, nothing of the
/// delivery's own follows the other writer's input: no probe, no submit key
/// pressed over it.
#[tokio::test]
async fn another_writers_input_after_the_submit_key_gets_nothing_written_behind_it() {
    let dir = unique_temp_dir("events-confirm-foreign-after-key");
    let log = dir.join("child-input.log");
    let h = Harness::start("events-confirm-foreign-after-key", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let window = ReceivingWindow {
        paste_guard_keys: u64::MAX,
        resend_max: 2,
        ..ReceivingWindow::new("confirm-foreign-after-key")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;
    // The key is taken as a line break, so the envelope stays in the composer
    // and another writer types behind it.
    wait_for_the_submit_key(&log).await;
    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Interactive)
        .await
        .expect("the other writer's byte must not fail");
    wait_until_frame_satisfies(
        &h,
        &window_id,
        "the envelope and the other writer's byte in a finished frame",
        HANG_GUARD,
        |readout, unfinished| {
            !unfinished
                && matches!(readout, ComposerReadout::Body(text) if text.trim_end().ends_with('Q'))
        },
    )
    .await;
    let_every_clock_bound_run_out().await;

    let delivery = handoff_reply(&mut client).await;
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        read_after_the_submit_key(&read),
        b"Q",
        "after the submit key, only the other writer's byte reaches the receiver; the delivery was {delivery:?}, the receiver read {read:?}"
    );
    assert_eq!(
        delivery, "taken",
        "a submitted envelope is taken however its submission was judged"
    );
    assert_eq!(count_of(&read, PASTE_START), 1, "the body is written once");
}

/// R9b: after the submit key, a resend attempt reads the composer empty on a
/// finished screen, nobody else having written. The envelope was taken, which
/// that reading already says: nothing is written to check it.
#[tokio::test]
async fn a_resend_attempt_that_reads_the_composer_empty_after_the_submit_key_writes_no_probe() {
    let dir = unique_temp_dir("events-confirm-empty-no-probe");
    let log = dir.join("child-input.log");
    let take_now = dir.join("take-now");
    let h = Harness::start("events-confirm-empty-no-probe", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let mode = format!("take-on|{}", take_now.display());
    let window = ReceivingWindow {
        receive_mode: &mode,
        resend_max: 2,
        ..ReceivingWindow::new("confirm-empty-no-probe")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;
    wait_for_the_submit_key(&log).await;
    wait_until_the_held_envelope_is_drawn_unfinished(&h, &window_id).await;
    // The delivery's own wait for the composer runs out on the unfinished
    // frame; its resend attempt is what reads the composer empty.
    let_every_clock_bound_run_out().await;
    create_signal_file(&take_now);

    let delivery = handoff_reply(&mut client).await;
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        read_after_the_submit_key(&read),
        b"",
        "after the submit key nothing more is written, a probe included; the delivery was {delivery:?}, the receiver read {read:?}"
    );
    assert_eq!(
        delivery, "taken",
        "the composer the submit key was pressed in read empty on a finished screen"
    );
    assert_eq!(count_of(&read, PASTE_START), 1, "the body is written once");
}

/// What the folding receiver of R10 paints in place of the pasted envelope.
const FOLD_PLACEHOLDER: &str = "[Pasted text #1]";

/// R10: after the submit key (taken as a line break, the envelope stays in the
/// composer, drawn folded), the receiver takes the envelope just before the
/// next typed byte and paints that byte alone. The probe a resend attempt
/// writes then shows alone, which says the envelope left the composer: it was
/// taken, and the probe is erased, not left behind.
#[tokio::test]
async fn a_folded_envelope_the_receiver_takes_before_the_resend_probe_is_taken_and_leaves_no_probe()
{
    // The confirmation window runs out on its own; the test waits on the
    // receiver's input and screen, never on the clock.
    let delivery = folded_envelope_taken_by_the_resend_probe(
        "events-confirm-folded-taken-by-probe",
        "0",
        ClockBounds::RunOnTheirOwn,
    )
    .await;
    assert_eq!(delivery, "taken");
}

/// R10 again, with the handoff's own delivery bound running out while the
/// resend probe is on its way to the receiver: the probe a delivery has begun
/// is carried to its end and erased, not left in the receiver's composer.
#[tokio::test]
async fn a_handoff_that_times_out_while_its_resend_probe_is_on_its_way_leaves_no_probe() {
    let delivery = folded_envelope_taken_by_the_resend_probe(
        "events-confirm-folded-probe-after-timeout",
        // The receiver reflects what it reads after a delay, so the probe is
        // still on its way when the hour passes.
        "30",
        ClockBounds::RunOutBeforeTheProbe,
    )
    .await;
    assert_eq!(
        delivery, "taken",
        "the push that began the probe settled the handoff when it finished"
    );
}

/// How the clock bounds of R10's delivery end.
#[derive(Clone, Copy)]
enum ClockBounds {
    /// The test's clock is left alone.
    RunOnTheirOwn,
    /// An hour of the test's clock passes once the folded envelope is drawn,
    /// whatever the receiver still has to do.
    RunOutBeforeTheProbe,
}

/// Plays R10 and checks what the receiver was left with; returns the handoff's
/// delivery.
async fn folded_envelope_taken_by_the_resend_probe(
    name: &str,
    timing: &str,
    clock: ClockBounds,
) -> String {
    let dir = unique_temp_dir(name);
    let log = dir.join("child-input.log");
    let h = Harness::start(name, handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let window = ReceivingWindow {
        paste_guard_keys: u64::MAX,
        receive_mode: "take-on-typing-after-key",
        delivery_confirm_ms: 600,
        resend_max: 2,
        timing,
        fold_placeholder: FOLD_PLACEHOLDER,
        ..ReceivingWindow::new("confirm-folded-taken-by-probe")
    };
    let window_id = open_receiving_window(&h, &mut client, &dir, &window, &log).await;

    request_handoff(&mut client, &window_id, "look here").await;
    // The key is taken as a line break: the envelope stays, drawn folded.
    wait_for_the_submit_key(&log).await;
    wait_until_frame_satisfies(
        &h,
        &window_id,
        "the folded envelope in a finished frame",
        HANG_GUARD,
        |readout, unfinished| {
            !unfinished
                && matches!(readout, ComposerReadout::Body(text) if text == FOLD_PLACEHOLDER)
        },
    )
    .await;
    if matches!(clock, ClockBounds::RunOutBeforeTheProbe) {
        let_every_clock_bound_run_out().await;
    }

    let delivery = handoff_reply(&mut client).await;
    // Before the sentinel below, which this receiver paints as typed text.
    wait_until_composer_is(&h, &window_id, &ComposerReadout::Empty, HANG_GUARD).await;
    let read = input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert_eq!(
        delivery, "taken",
        "the probe the receiver read after the submit key showed alone: the envelope was taken; the receiver read {read:?}"
    );
    let after_the_key = read_after_the_submit_key(&read);
    assert_eq!(
        count_of(after_the_key, b"x"),
        1,
        "the resend attempt wrote its probe once; the receiver read {after_the_key:?} after the key"
    );
    assert!(
        after_the_key.ends_with(&[0x7f]),
        "the probe was erased; the receiver read {after_the_key:?} after the key"
    );
    assert_eq!(count_of(&read, PASTE_START), 1, "the body is written once");
    delivery
}
