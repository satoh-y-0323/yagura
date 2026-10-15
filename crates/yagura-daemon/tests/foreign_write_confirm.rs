#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: once another writer's bytes have ended a delivery, the
//! call goes back to the screen before it answers. It waits for a screen that
//! accepts a write, reads what is left of its own text, takes it back again if
//! the receiver dropped the take-back, and says `envelope_remains: false` only
//! when the screen shows it gone. A second writer, the requester leaving or
//! the child exiting ends that wait with "may remain" (`true`); nothing but
//! such an event ends it while the screen does not accept a write.
//!
//! The receiver is `submit_child` in composer-draw mode with an approval
//! dialog that takes every byte written to it. Every synchronisation point is
//! an event the child prints or the screen shows. The one clock in play is the
//! definition's `submit_delay_ms`, which the cases that need the other
//! writer's bytes to land before the probe set to its maximum; the paused
//! clock runs it out.

mod common;

use std::path::PathBuf;
use std::sync::Arc;

use common::{
    APPROVAL_HEADING, BODY, CLEAR, CTRL_C, ERASE, GateWindow, HANG_GUARD, Harness, MARKER,
    SUBMIT_CHILD_PROBE, after_the_body, count_marker, gate_config, let_every_clock_bound_run_out,
    open_gate_window, take_back_in_front_of, unique_temp_dir, wait_until_composer_is,
    wait_until_input_log_contains, wait_until_model_shows, window_entry,
};
use tokio::sync::watch;
use tokio::task::JoinHandle;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::pty::{PtyError, PtyResult};
use yagura_daemon::window::{
    InputOrigin, NotPlaced, NotPlacedReason, SubmitJudgment, SubmitPlacement, WindowEntry,
};

/// The largest `submit_delay_ms` a definition may carry. A delivery sleeps
/// this long after the body, before the probe, so a write made once the
/// dialog shows lands first.
const LONGEST_SUBMIT_DELAY_MS: u64 = 5000;
const ORDINARY_SUBMIT_DELAY_MS: u64 = 120;

fn probe() -> u8 {
    SUBMIT_CHILD_PROBE.as_bytes()[0]
}

struct Window {
    id: String,
    log: PathBuf,
}

/// Opens a gate window whose definition takes a body back with Ctrl-U.
/// `overlay` is `submit_child`'s overlay argument (a dialog headed
/// [`APPROVAL_HEADING`]), `clear_scope` its scope argument.
async fn open_window(
    h: &Harness,
    dir: &std::path::Path,
    name: &str,
    overlay: &str,
    clear_scope: &str,
    submit_delay_ms: u64,
) -> Window {
    let log = dir.join(format!("{name}-input.log"));
    let marker = format!("{name}-marker");
    let mut client = h.connect().await;
    let id = open_gate_window(
        h,
        &mut client,
        dir,
        GateWindow {
            clear_scope,
            input_log: Some(&log),
            submit_delay_ms,
            ..GateWindow::new(name, &marker, overlay, APPROVAL_HEADING)
        },
    )
    .await;
    Window { id, log }
}

fn entry(h: &Harness, window: &Window) -> Arc<WindowEntry> {
    window_entry(h, &window.id)
}

fn start_delivery(
    h: &Harness,
    window: &Window,
    requester: Option<watch::Receiver<bool>>,
) -> JoinHandle<PtyResult<SubmitJudgment>> {
    let sender = entry(h, window);
    tokio::spawn(async move { sender.submit_message(BODY, None, requester).await })
}

async fn write_as_another_writer(h: &Harness, window: &Window, bytes: &[u8]) {
    entry(h, window)
        .write_pty_input(bytes, InputOrigin::Interactive)
        .await
        .expect("another writer's bytes must not fail");
}

async fn wait_for_the_dialog(h: &Harness, window: &Window) {
    wait_until_model_shows(h, &window.id, APPROVAL_HEADING.as_bytes(), HANG_GUARD).await;
}

/// What the call answered, once it has.
async fn answer(call: JoinHandle<PtyResult<SubmitJudgment>>) -> PtyResult<SubmitJudgment> {
    tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("the delivery must answer")
        .expect("the submit task must not panic")
}

fn not_placed(result: PtyResult<SubmitJudgment>) -> NotPlaced {
    match result
        .expect("the call must not fail at the PTY level")
        .placement
    {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("another writer's bytes must end the delivery, got {other:?}"),
    }
}

/// Writes [`MARKER`] behind everything (so the call must have answered
/// already: the marker is another writer's write) and returns what the child
/// read before it.
async fn read_log(h: &Harness, window: &Window) -> Vec<u8> {
    write_as_another_writer(h, window, MARKER).await;
    let mut read = wait_until_input_log_contains(&window.log, MARKER, HANG_GUARD).await;
    let at = read
        .windows(MARKER.len())
        .rposition(|w| w == MARKER)
        .expect("the log was returned holding the marker");
    read.truncate(at);
    read
}

fn count_byte(bytes: &[u8], byte: u8) -> usize {
    bytes.iter().filter(|&&b| b == byte).count()
}

/// The dialog is drawn once the body is in, so another writer's bytes that
/// land while it stands go to the dialog. The body is taken back in front of
/// them (so the dialog swallows that), the call waits for a screen that
/// accepts a write, sees its own text alone there, and takes it back again
/// behind a probe of its own: the composer ends empty and the call says so.
#[tokio::test]
async fn a_body_left_beside_a_dialog_is_taken_back_again_once_the_dialog_is_gone() {
    let dir = unique_temp_dir("confirm-dialog-gone");
    let gone = dir.join("dialog-gone");
    let h = Harness::start("confirm-dialog-gone", gate_config(&dir)).await;
    let overlay = format!(
        "body:hidden:until|{}",
        gone.to_str().expect("the dialog path must be valid UTF-8")
    );
    let window = open_window(&h, &dir, "gone", &overlay, "", LONGEST_SUBMIT_DELAY_MS).await;

    let call = start_delivery(&h, &window, None);
    wait_for_the_dialog(&h, &window).await;
    write_as_another_writer(&h, &window, b"\r").await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "the dialog still stands, so what the take-back did cannot be checked and the call is not \
         over"
    );

    std::fs::write(&gone, b"").expect("remove the dialog");
    let not_placed = not_placed(answer(call).await);
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        !not_placed.envelope_remains,
        "the body was seen alone once the dialog was gone and taken back behind a probe, so the \
         composer is empty"
    );
    wait_until_composer_is(&h, &window.id, &ComposerReadout::Empty, HANG_GUARD).await;

    let read = read_log(&h, &window).await;
    let after = after_the_body(&read);
    let carriage_return = after
        .iter()
        .position(|&b| b == b'\r')
        .unwrap_or_else(|| panic!("the other writer's carriage return was not read: {read:?}"));
    assert!(
        carriage_return > 0 && after[..carriage_return].iter().all(|&b| b == CLEAR),
        "the body's take-back must be all that stands in front of the other writer's bytes; the \
         child read {read:?}"
    );
    let rest = &after[carriage_return + 1..];
    assert_eq!(
        count_byte(after, b'\r'),
        1,
        "the delivery must never press a submit key of its own; the child read {read:?}"
    );
    assert!(
        rest.contains(&probe()) && rest.contains(&CLEAR),
        "the check must write a probe and take the body back behind it; the child read {read:?}"
    );
    assert_eq!(count_marker(&h, &window.id, "SUBMITTED"), 0);
}

/// The call read its body back off the screen, then another writer's bytes
/// arrive. The take-back in front of them empties the composer of the
/// delivery's text, so the screen shows only the other writer's keystroke:
/// nothing more is written and the call can say its text is gone.
///
/// The dialog is drawn when the delivery erases its probe, which it does only
/// after it has read the body and the probe back, and is dropped by the first
/// take-back byte the child reads. The delivery waits on the dialog meanwhile,
/// so the other writer's bytes land at a point the screen has fixed.
#[tokio::test]
async fn a_body_the_call_read_back_is_gone_when_another_writers_keystroke_is_all_that_shows() {
    let dir = unique_temp_dir("confirm-read-back");
    let h = Harness::start("confirm-read-back", gate_config(&dir)).await;
    let window = open_window(
        &h,
        &dir,
        "readback",
        "erase:hidden:until-byte|15",
        "",
        ORDINARY_SUBMIT_DELAY_MS,
    )
    .await;

    let call = start_delivery(&h, &window, None);
    wait_for_the_dialog(&h, &window).await;
    write_as_another_writer(&h, &window, b"h").await;

    let not_placed = not_placed(answer(call).await);
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        !not_placed.envelope_remains,
        "the call had read its body back and the screen no longer shows it, so it is gone"
    );
    wait_until_composer_is(
        &h,
        &window.id,
        &ComposerReadout::Body("h".to_string()),
        HANG_GUARD,
    )
    .await;

    let read = read_log(&h, &window).await;
    let after = after_the_body(&read);
    let (_, run) = take_back_in_front_of(after, b"h");
    assert!(
        run > 0,
        "the body's take-back must reach the child right in front of the other writer's byte; it \
         read {read:?}"
    );
    assert_eq!(
        count_byte(after, probe()),
        1,
        "only the delivery's own probe may have been written; the check writes none when nothing \
         of the body shows; the child read {read:?}"
    );
    assert!(!after.contains(&b'\r'));
    assert_eq!(count_marker(&h, &window.id, "SUBMITTED"), 0);
}

/// The dialog goes away with the other writer's own keystroke, so the screen
/// the call reads shows its body and that keystroke together: it cannot tell
/// what is whose, writes nothing and says its text may remain.
#[tokio::test]
async fn a_body_with_another_writers_keystroke_behind_it_is_left_alone_and_reported_as_remaining() {
    let dir = unique_temp_dir("confirm-keystroke-behind");
    let h = Harness::start("confirm-keystroke-behind", gate_config(&dir)).await;
    let window = open_window(
        &h,
        &dir,
        "behind",
        "body:hidden:until-byte|68",
        "",
        LONGEST_SUBMIT_DELAY_MS,
    )
    .await;

    let call = start_delivery(&h, &window, None);
    wait_for_the_dialog(&h, &window).await;
    write_as_another_writer(&h, &window, b"h").await;
    let_every_clock_bound_run_out().await;

    let not_placed = not_placed(answer(call).await);
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        not_placed.envelope_remains,
        "the body and another writer's keystroke show together, so the call cannot vouch for it"
    );
    wait_until_composer_is(
        &h,
        &window.id,
        &ComposerReadout::Body(format!("{BODY}h")),
        HANG_GUARD,
    )
    .await;

    let read = read_log(&h, &window).await;
    let (rest, run) = take_back_in_front_of(after_the_body(&read), b"h");
    assert!(
        run > 0 && rest.is_empty(),
        "nothing but the body's take-back may stand in front of the other writer's byte, and the \
         check writes nothing of its own; the child read {read:?}"
    );
}

/// The receiver ignores the interrupt keys, so the body is still in the
/// composer after them, and nothing was put in front of those keys. The call
/// takes the body back behind a probe of its own and says the composer is
/// empty; what it writes after the interrupt keys is that probe and the
/// take-back, nothing else.
#[tokio::test]
async fn a_body_that_survives_the_interrupt_keys_is_taken_back_after_them() {
    let dir = unique_temp_dir("confirm-interrupt-kept");
    let config = gate_config(&dir);
    assert_eq!(
        config.interrupt_bytes,
        vec![CTRL_C],
        "the default interrupt keys are Ctrl-C"
    );
    let h = Harness::start("confirm-interrupt-kept", config).await;
    let window = open_window(
        &h,
        &dir,
        "kept",
        "",
        "keep-on-interrupt:",
        LONGEST_SUBMIT_DELAY_MS,
    )
    .await;

    let call = start_delivery(&h, &window, None);
    wait_until_composer_is(
        &h,
        &window.id,
        &ComposerReadout::Body(BODY.to_string()),
        HANG_GUARD,
    )
    .await;
    write_as_another_writer(&h, &window, &[CTRL_C]).await;
    let_every_clock_bound_run_out().await;

    let not_placed = not_placed(answer(call).await);
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        !not_placed.envelope_remains,
        "the receiver kept the body through the interrupt keys; the call took it back and saw the \
         composer empty"
    );
    wait_until_composer_is(&h, &window.id, &ComposerReadout::Empty, HANG_GUARD).await;

    let read = read_log(&h, &window).await;
    let after = after_the_body(&read);
    assert_eq!(
        after.first(),
        Some(&CTRL_C),
        "nothing may be put in front of the interrupt keys; the child read {read:?}"
    );
    let rest = &after[1..];
    assert!(
        rest.contains(&probe()) && rest.contains(&CLEAR),
        "the check must write a probe and take the body back behind it; the child read {read:?}"
    );
    assert!(
        rest.iter()
            .all(|&b| b == probe() || b == CLEAR || b == ERASE),
        "after the interrupt keys the call may write only its probe, the take-back and the \
         probe's erase; the child read {read:?}"
    );
}

/// A delivery that is checking its take-back behind a dialog that stays: it
/// has been interrupted by one writer's keystroke (which the dialog took),
/// and nothing has ended the wait yet.
struct Checking {
    h: Harness,
    window: Window,
    call: JoinHandle<PtyResult<SubmitJudgment>>,
    leave: watch::Sender<bool>,
}

async fn start_checking_behind_a_standing_dialog(tag: &str) -> Checking {
    let dir = unique_temp_dir(tag);
    let h = Harness::start(tag, gate_config(&dir)).await;
    let window = open_window(
        &h,
        &dir,
        "standing",
        "body:hidden",
        "",
        LONGEST_SUBMIT_DELAY_MS,
    )
    .await;
    let (leave, requester) = watch::channel(false);
    let call = start_delivery(&h, &window, Some(requester));
    wait_for_the_dialog(&h, &window).await;
    write_as_another_writer(&h, &window, b"h").await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "the dialog stands, so the take-back cannot be checked and the call is not over, however \
         long it takes"
    );
    Checking {
        h,
        window,
        call,
        leave,
    }
}

#[tokio::test]
async fn the_check_behind_a_standing_dialog_ends_when_a_second_writer_writes() {
    let checking = start_checking_behind_a_standing_dialog("confirm-second-writer").await;
    write_as_another_writer(&checking.h, &checking.window, b"h").await;

    let not_placed = not_placed(answer(checking.call).await);
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        not_placed.envelope_remains,
        "a second writer ended the check before it saw the screen, so the body may remain"
    );
    drop(checking.leave);
}

#[tokio::test]
async fn the_check_behind_a_standing_dialog_ends_when_the_requester_leaves() {
    let checking = start_checking_behind_a_standing_dialog("confirm-requester-leaves").await;
    checking
        .leave
        .send(true)
        .expect("the call holds its receiver");

    let not_placed = not_placed(answer(checking.call).await);
    assert!(
        not_placed.envelope_remains,
        "the requester left before the check saw the screen, so the body may remain"
    );
}

#[tokio::test]
async fn the_check_behind_a_standing_dialog_ends_when_the_child_exits() {
    let checking = start_checking_behind_a_standing_dialog("confirm-child-exits").await;
    entry(&checking.h, &checking.window).session().terminate();

    match answer(checking.call).await {
        Err(PtyError::ChildExited) => {}
        Ok(judgment) => match judgment.placement {
            SubmitPlacement::NotPlaced(not_placed) => assert!(
                not_placed.envelope_remains,
                "the child exited before the check saw the screen, so the body may remain"
            ),
            other => panic!("a child that exits ends the check as remaining, got {other:?}"),
        },
        Err(other) => panic!("a child that exits ends the check with ChildExited, got {other:?}"),
    }
    drop(checking.leave);
}
