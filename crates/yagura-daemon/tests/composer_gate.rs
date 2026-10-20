#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the window layer's observe-then-act judgment before it
//! writes into, erases, or presses the submit key of a window's composer.
//!
//! Every test drives a real `WindowRegistry` (+ `IpcServer` where a case
//! needs the wire protocol) against `submit_child` launched in composer-draw
//! mode, so what is asserted is what actually reached the receiving child's
//! screen -- never a value read back out of the daemon's own bookkeeping.
//!
//! Most cases call `WindowEntry::submit_message` directly rather than going
//! through `send_text` / `send_handoff`: this file's subject is the judgment
//! itself, and a direct call lets each case supply an exact residue and an
//! exact desired body without racing the real delivery loop's own timing.
//! The handful of cases that are *about* that wiring (two connections
//! racing, a profile's window closing, an approval gate) go through the real
//! IPC messages instead, and say so in their own doc comment.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    HANG_GUARD, Harness, SUBMIT_CHILD_PROBE, SubmitDefinitionInput, approver_base_config, attach,
    count_marker, decide, expect_approval_requested_within, expect_error,
    let_every_clock_bound_run_out, open_submit_child_window_with_composer,
    open_submit_child_window_with_folded_composer, policy, read_composer_now, read_response,
    register_approver, send, send_from_two_connections, settled_marker_count, unique_temp_dir,
    wait_until_composer_is, wait_until_composer_satisfies, write_interactive_input,
    write_submit_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{
    ComposerResidue, InputOrigin, NotPlacedReason, SendStage, SubmitJudgment, SubmitPlacement,
    WindowEntry,
};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

/// A [`DaemonConfig`] whose detection directory is empty: every test writes
/// its own definition under it with [`write_submit_definition`]. Built on
/// [`approver_base_config`] so a case needing a connection to register as an
/// approver does not restate the `approver_exe` that registration is matched
/// against.
fn detection_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        ..approver_base_config()
    }
}

/// Writes a definition that resolves and reaches `waiting_input` but carries
/// no `[composer]` table: the fail-closed case where
/// a window can be ready and still have no composer anything could observe.
///
/// `[debounce]` is written explicitly because a definition is rejected
/// outright unless its `submit_delay_ms` outlasts the `confirm_ms` its
/// windows actually debounce under, and the daemon's own default is far
/// larger than these fixtures want to wait for.
fn write_unobservable_definition(dir: &Path, name: &str, marker: &str) {
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!(
            r#"schema_version = 1
name = "{name}"

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_input"
pattern = '{marker}'
priority = 10
region = "bottom"

[input]
submit_delay_ms = 40
"#
        ),
    )
    .expect("write detection definition file");
}

/// Opens a `submit_child` window in composer-draw mode and observes it reach
/// `waiting_input` before returning: every case here needs a window whose
/// judgment can actually observe a composer, not just one that has opened.
#[allow(clippy::too_many_arguments)]
async fn open_gate_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    clear: &'static str,
    reflect_delay_ms: u64,
) -> String {
    open_submit_child_window_with_composer(
        h,
        client,
        dir,
        name,
        marker,
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear,
            delivery_confirm_ms: 2000,
            resend_max: 2,
        },
        reflect_delay_ms,
    )
    .await
}

/// The window entry backing `window_id`, for the cases that call
/// `submit_message` directly rather than through the wire protocol.
fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// Leaves `body` in the composer the way a previous delivery attempt would
/// have: framed as a paste.
async fn seed_previous_attempt(h: &Harness, window_id: &str, body: &str) {
    let framed = format!("\x1b[200~{body}\x1b[201~");
    entry(h, window_id)
        .write_pty_input(framed.as_bytes(), InputOrigin::Message)
        .await
        .expect("seed write must not fail");
}

/// A body long enough that composer-draw mode wraps it across enough
/// painted rows to run the composer region past the bottom of a standard
/// PTY's viewport: the mechanism the "cannot be fully restored" case
/// needs, and the one the "does not fit on screen" case names.
fn oversized_body() -> String {
    "the retry budget for this delivery has not been reached yet and the receiver has not \
     reacted to the previous attempt so this one carries the same envelope forward "
        .repeat(6)
}

/// What a folded [`open_gate_window_folded`] window paints in place of any
/// non-empty body: fixed and known to the test, unlike the body itself, so a
/// read after a blocked call can assert the region still holds it.
const FOLD_PLACEHOLDER: &str = "[Pasted text #1]";

/// [`open_gate_window`], with the fixture's composer folded: every non-empty
/// body it holds paints as [`FOLD_PLACEHOLDER`] regardless of length, so the
/// "cannot be fully
/// restored" cases below exercise the placeholder path rather than
/// [`oversized_body`]'s past-the-viewport path.
#[allow(clippy::too_many_arguments)]
async fn open_gate_window_folded(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    clear: &'static str,
    reflect_delay_ms: u64,
) -> String {
    open_submit_child_window_with_folded_composer(
        h,
        client,
        dir,
        name,
        marker,
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear,
            delivery_confirm_ms: 2000,
            resend_max: 2,
        },
        reflect_delay_ms,
        FOLD_PLACEHOLDER,
    )
    .await
}

// =====================================================================
// What the judgment does, keyed by observation, residue and the body
// it wants to write.
// =====================================================================

#[tokio::test]
async fn row1_writes_the_body_with_no_clear_when_the_composer_observes_empty() {
    let dir = unique_temp_dir("gate-row1");
    let h = Harness::start("gate-row1", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(&h, &mut client, &dir, "row1", "row1-marker", "", 0).await;

    let result = entry(&h, &window_id)
        .submit_message("hello", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level")
        .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => panic!("an empty composer must accept a fresh write, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED hello", 1).await,
        1,
        "the body actually reached the receiver, unmodified"
    );
}

#[tokio::test]
async fn row2_resends_the_submit_key_only_when_its_own_envelope_is_still_on_screen_unmodified() {
    let dir = unique_temp_dir("gate-row2");
    let h = Harness::start("gate-row2", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id =
        open_gate_window(&h, &mut client, &dir, "row2", "row2-marker", "\\u0015", 0).await;

    // The envelope is already sitting there, unaccepted: this simulates a
    // resend whose previous attempt wrote the body, framed as a paste the way
    // that attempt would have, but has not been re-submitted yet.
    seed_previous_attempt(&h, &window_id, "ENV").await;
    // On screen before the resend reads it: an envelope not drawn yet is
    // confirmed through the probe instead, which is a different case.
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("ENV".to_string()),
        Duration::from_secs(10),
    )
    .await;
    // What the previous attempt left in the composer, as this resend carries
    // it: the text, and the window's write count at that instant.
    let mine = ComposerResidue {
        body: "ENV",
        write_seq: entry(&h, &window_id).write_seq(),
    };

    let result = entry(&h, &window_id)
        .submit_message("ENV", Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level")
        .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the same envelope, still on screen, must resend, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED ENV", 1).await,
        1,
        "the envelope must reach the receiver exactly once"
    );
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        mine.write_seq + 1,
        "a resend of the unchanged envelope must not rewrite the body: the submit key is its \
         only write"
    );
}

#[tokio::test]
async fn row4_blocks_when_the_screen_shows_content_that_does_not_match_the_residue() {
    let dir = unique_temp_dir("gate-row4");
    let h = Harness::start("gate-row4", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id =
        open_gate_window(&h, &mut client, &dir, "row4", "row4-marker", "\\u0015", 0).await;

    entry(&h, &window_id)
        .write_pty_input(b"SOMEONE-ELSES-BODY", InputOrigin::Message)
        .await
        .expect("seed write must not fail");

    // Residue `None`: this call brought nothing of its own to compare
    // against, so the foreign content on screen can only read as someone
    // else's.
    let result = entry(&h, &window_id)
        .submit_message("MINE", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("mismatched content must block the write, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
    assert!(
        !not_placed.envelope_remains,
        "nothing of this call's own was written, so nothing of its own remains"
    );
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "a blocked call must never press the submit key"
    );
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert_eq!(
            text, "SOMEONE-ELSES-BODY",
            "the foreign content must be left exactly as it was"
        ),
        other => panic!("expected the foreign body still on screen, got {other:?}"),
    }
}

#[tokio::test]
async fn row5_resends_via_the_write_record_when_the_screen_cannot_be_fully_restored() {
    let dir = unique_temp_dir("gate-row5-pos");
    let h = Harness::start("gate-row5-pos", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "row5pos",
        "row5pos-marker",
        "\\u0015",
        0,
    )
    .await;
    let body = oversized_body();

    seed_previous_attempt(&h, &window_id, &body).await;
    // What the previous attempt left in the composer, as this resend carries
    // it: the text, and the window's write count at that instant.
    let mine = ComposerResidue {
        body: &body,
        write_seq: entry(&h, &window_id).write_seq(),
    };

    // Nothing else has touched the window since the seed write above, so
    // the write record alone can vouch for this resend even though the
    // screen reconstruction cannot.
    let result = entry(&h, &window_id)
        .submit_message(&body, Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level")
        .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => panic!("an oversized but untouched envelope must still resend, got {other:?}"),
    }
}

/// What a touched envelope does depends on what the screen shows of it, so
/// each screen is fixed here. A tall envelope whose end still reads as a
/// continuation of the composer is someone else's draft now: the call refuses
/// it without writing a byte.
#[tokio::test]
async fn row5_refuses_a_touched_envelope_whose_tail_reads_on_screen_without_writing() {
    let dir = unique_temp_dir("gate-row5-neg-tail");
    let h = Harness::start("gate-row5-neg-tail", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "row5negtail",
        "row5negtail-marker",
        "\\u0015",
        0,
    )
    .await;
    let body = oversized_body();

    entry(&h, &window_id)
        .write_pty_input(body.as_bytes(), InputOrigin::Message)
        .await
        .expect("seed write must not fail");
    // What the previous attempt left in the composer, as this resend carries
    // it: the text, and the window's write count at that instant.
    let mine = ComposerResidue {
        body: &body,
        write_seq: entry(&h, &window_id).write_seq(),
    };
    // A second writer touches the window: `Message` on purpose (record-based
    // matching is not limited to interactive/control origins).
    entry(&h, &window_id)
        .write_pty_input(b"!", InputOrigin::Message)
        .await
        .expect("interfering write must not fail");
    // The call starts only once the touch is on the daemon's screen model, so
    // the screen it reads is this one and not a frame on the way to it.
    wait_until_composer_satisfies(
        &h,
        &window_id,
        "the touched envelope's tail",
        Duration::from_secs(10),
        |readout| matches!(readout, ComposerReadout::Tail(tail) if tail.ends_with('!')),
    )
    .await;

    let result = tokio::time::timeout(
        HANG_GUARD,
        entry(&h, &window_id).submit_message(&body, Some(mine), SendStage::BeforeSubmitKey, None),
    )
    .await
    .expect("a tail that reads on screen must end the call")
    .expect("a refused write is not a PTY error")
    .placement;

    match result {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
        }
        other => panic!("a touched, oversized envelope must not resend blind, got {other:?}"),
    }
    assert_eq!(
        count_marker(&h, &window_id, "PASTE "),
        0,
        "the body must not be written again over the touched envelope"
    );
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "the submit key must not be pressed on the touched envelope"
    );
}

/// The other screen: nothing on it is the composer (the child's frame is
/// caught between erasing the composer and drawing the rest). Nothing is
/// decided and nothing is written, however long that lasts; the requester
/// leaving ends the call.
#[tokio::test]
async fn row5_waits_on_a_touched_envelope_while_the_screen_shows_no_composer() {
    let dir = unique_temp_dir("gate-row5-neg-unobserved");
    let log = dir.join("child-input.log");
    let rest = dir.join("dialog-rest");
    let overlay = format!(
        "body:hidden:torn|erase|{}",
        rest.to_str().expect("the path must be valid UTF-8")
    );
    let h = Harness::start("gate-row5-neg-unobserved", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = common::open_gate_window(
        &h,
        &mut client,
        &dir,
        common::GateWindow {
            input_log: Some(&log),
            ..common::GateWindow::new(
                "row5negunobserved",
                "row5negunobserved-marker",
                &overlay,
                common::APPROVAL_HEADING,
            )
        },
    )
    .await;
    let body = oversized_body();

    seed_previous_attempt(&h, &window_id, &body).await;
    // The seed raises the torn frame, and it stays until `rest` exists.
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::NotObserved,
        Duration::from_secs(10),
    )
    .await;
    let mine = ComposerResidue {
        body: &body,
        write_seq: entry(&h, &window_id).write_seq(),
    };
    entry(&h, &window_id)
        .write_pty_input(b"!", InputOrigin::Message)
        .await
        .expect("interfering write must not fail");

    let (leave, requester) = tokio::sync::watch::channel(false);
    let sender = entry(&h, &window_id);
    let resend_body = body.clone();
    let baseline = mine.write_seq;
    let call = tokio::spawn(async move {
        let mine = ComposerResidue {
            body: &resend_body,
            write_seq: baseline,
        };
        sender
            .submit_message(
                &resend_body,
                Some(mine),
                SendStage::BeforeSubmitKey,
                Some(requester),
            )
            .await
    });
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a screen that shows no composer is waited on, not given up on by a clock"
    );
    leave.send(true).expect("the call holds its receiver");
    let result = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("the requester leaving must end the wait")
        .expect("the submit task must not panic")
        .expect("a blocked write is not a PTY error")
        .placement;

    assert!(
        matches!(result, SubmitPlacement::NotPlaced(_)),
        "a touched, oversized envelope must not resend blind, got {result:?}"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "PASTE ", 1).await,
        1,
        "only the seed's paste reached the child: the body was not written again"
    );
    assert_eq!(count_marker(&h, &window_id, "APPROVED"), 0);
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
    let received = common::input_log_read_so_far(&entry(&h, &window_id), &log, HANG_GUARD).await;
    assert!(
        !received.contains(&b'\r'),
        "the submit key must not be pressed; the child read {received:?}"
    );
}

/// The screen-unrestorable case reached through a folded composer rather
/// than through [`oversized_body`]. The write count pins that the
/// receiver's own screen-reflected body never got rewritten. A folded screen
/// never shows the body back, so the write record vouches for it and the
/// probe confirms the composer behind the placeholder before the submit key.
#[tokio::test]
async fn row5_folded_resends_via_the_write_record_when_the_placeholder_hides_the_body() {
    let dir = unique_temp_dir("gate-row5-folded-pos");
    let h = Harness::start("gate-row5-folded-pos", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_folded(
        &h,
        &mut client,
        &dir,
        "row5foldedpos",
        "row5foldedpos-marker",
        "\\u0015",
        0,
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet";

    seed_previous_attempt(&h, &window_id, body).await;
    // On screen before the resend reads it: an envelope not drawn yet is
    // confirmed through the probe instead, which is a different case.
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(FOLD_PLACEHOLDER.to_string()),
        Duration::from_secs(10),
    )
    .await;
    // What the previous attempt left in the composer, as this resend carries
    // it: the text, and the window's write count at that instant.
    let mine = ComposerResidue {
        body,
        write_seq: entry(&h, &window_id).write_seq(),
    };

    // Nothing else has touched the window since the seed write above, so the
    // write record alone can vouch for this resend even though folding hides
    // the body from the screen entirely.
    let result = entry(&h, &window_id)
        .submit_message(body, Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level")
        .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a folded but untouched envelope must still resend, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, &format!("SUBMITTED {body}"), 1).await,
        1,
        "the seeded body must reach the receiver exactly once"
    );
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        mine.write_seq + 3,
        "a resend via the write record must not rewrite the body: the probe, its erase and the \
         submit key are its only writes"
    );
}

#[tokio::test]
async fn row5_folded_takes_back_its_own_envelope_to_write_a_different_body() {
    let dir = unique_temp_dir("gate-row5-folded-swap");
    let h = Harness::start("gate-row5-folded-swap", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_folded(
        &h,
        &mut client,
        &dir,
        "row5foldedswap",
        "row5foldedswap-marker",
        "\\u0015",
        0,
    )
    .await;
    let previous = "the retry budget for this delivery has not been reached yet";
    let next = "a newer notice replaces the one still waiting";

    seed_previous_attempt(&h, &window_id, previous).await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(FOLD_PLACEHOLDER.to_string()),
        Duration::from_secs(10),
    )
    .await;
    let mine = ComposerResidue {
        body: previous,
        write_seq: entry(&h, &window_id).write_seq(),
    };

    // Folding hides the previous body, but nothing else has written since, so
    // the write record alone vouches that the composer holds only it.
    let result = entry(&h, &window_id)
        .submit_message(next, Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level")
        .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => {
            panic!("an untouched folded envelope must be taken back for a new body, got {other:?}")
        }
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, &format!("SUBMITTED {next}"), 1).await,
        1,
        "the new body must reach the receiver exactly once"
    );
    assert_eq!(
        count_marker(&h, &window_id, &format!("SUBMITTED {previous}")),
        0,
        "the taken-back body must never be submitted"
    );
}

#[tokio::test]
async fn row5_folded_does_not_take_back_for_a_different_body_once_someone_else_has_touched_the_window()
 {
    let dir = unique_temp_dir("gate-row5-folded-swap-neg");
    let h = Harness::start("gate-row5-folded-swap-neg", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_folded(
        &h,
        &mut client,
        &dir,
        "row5foldedswapneg",
        "row5foldedswapneg-marker",
        "\\u0015",
        0,
    )
    .await;
    let previous = "the retry budget for this delivery has not been reached yet";

    seed_previous_attempt(&h, &window_id, previous).await;
    let mine = ComposerResidue {
        body: previous,
        write_seq: entry(&h, &window_id).write_seq(),
    };
    entry(&h, &window_id)
        .write_pty_input(b" ", InputOrigin::Interactive)
        .await
        .expect("interfering write must not fail");
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(FOLD_PLACEHOLDER.to_string()),
        Duration::from_secs(10),
    )
    .await;

    let result = entry(&h, &window_id)
        .submit_message(
            "a newer notice replaces the one still waiting",
            Some(mine),
            SendStage::BeforeSubmitKey,
            None,
        )
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("a touched folded envelope must not be taken back, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "a blocked call must never press the submit key"
    );
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Body(FOLD_PLACEHOLDER.to_string()),
        "a blocked call must never erase what another writer touched"
    );
}

/// The foreign-content block, reached through a folded composer. The read after the
/// blocked call is the direct check that
/// blocking never erases -- a regression that fires a clear sequence here
/// would repaint the region empty, and this assertion would catch it.
#[tokio::test]
async fn row5_folded_does_not_resend_or_erase_once_someone_else_has_touched_the_window() {
    let dir = unique_temp_dir("gate-row5-folded-neg");
    let h = Harness::start("gate-row5-folded-neg", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_folded(
        &h,
        &mut client,
        &dir,
        "row5foldedneg",
        "row5foldedneg-marker",
        "\\u0015",
        0,
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet";

    seed_previous_attempt(&h, &window_id, body).await;
    // What the previous attempt left in the composer, as this resend carries
    // it: the text, and the window's write count at that instant.
    let mine = ComposerResidue {
        body,
        write_seq: entry(&h, &window_id).write_seq(),
    };
    // A second writer touches the window: `Message` on purpose (record-based
    // matching is not limited to interactive/control origins).
    entry(&h, &window_id)
        .write_pty_input(b" ", InputOrigin::Message)
        .await
        .expect("interfering write must not fail");

    let result = entry(&h, &window_id)
        .submit_message(body, Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("a touched, folded envelope must not resend blind, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
    assert!(
        !not_placed.envelope_remains,
        "nothing of this call's own was written, so nothing of its own remains"
    );
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "a blocked call must never press the submit key"
    );
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Body(FOLD_PLACEHOLDER.to_string()),
        "a blocked call must never erase: the region must still show the same folded \
         placeholder it did before this call, not an empty composer"
    );
}

/// Guards the interruption check: "no writer other than this send plan" must be
/// judged by write count, not by the ledger's cumulative byte length. A
/// Ctrl-U reset followed by an unrelated draft of the same byte length as
/// the pending envelope must still block the resend and never press the
/// submit key on that draft.
#[tokio::test]
async fn row5_folded_does_not_resend_when_a_ctrl_u_reset_is_followed_by_a_same_length_draft() {
    let dir = unique_temp_dir("gate-row5-folded-lenclash");
    let h = Harness::start("gate-row5-folded-lenclash", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_folded(
        &h,
        &mut client,
        &dir,
        "row5foldedlenclash",
        "row5foldedlenclash-marker",
        "\\u0015",
        0,
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet";
    let draft = "x".repeat(body.len());

    seed_previous_attempt(&h, &window_id, body).await;
    // What the previous attempt left in the composer, as this resend carries
    // it: the text, and the window's write count at that instant.
    let mine = ComposerResidue {
        body,
        write_seq: entry(&h, &window_id).write_seq(),
    };
    // A human resets the composer and retypes an unrelated draft of the same
    // byte length as `body`; both writes are `Interactive`, not this
    // delivery's own.
    entry(&h, &window_id)
        .write_pty_input(&[0x15], InputOrigin::Interactive)
        .await
        .expect("human reset must not fail");
    entry(&h, &window_id)
        .write_pty_input(
            format!("\x1b[200~{draft}\x1b[201~").as_bytes(),
            InputOrigin::Interactive,
        )
        .await
        .expect("human draft must not fail");

    let result = entry(&h, &window_id)
        .submit_message(body, Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "the submit key must never be pressed on a draft this delivery never wrote"
    );
    match result {
        SubmitPlacement::NotPlaced(_) => {}
        other => panic!(
            "a same-length unrelated draft left after a human reset must not be mistaken for \
             this call's own untouched envelope, got {other:?}"
        ),
    }
}

/// Guards the interruption check: the record-based vouch must judge write count, not
/// content identity. A different writer's write of the exact same bytes as
/// this call's own residue must still count as contamination and must not
/// let the resend press the submit key.
#[tokio::test]
async fn row5_folded_does_not_resend_when_another_writer_writes_the_identical_bytes() {
    let dir = unique_temp_dir("gate-row5-folded-identical");
    let h = Harness::start("gate-row5-folded-identical", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_folded(
        &h,
        &mut client,
        &dir,
        "row5foldedidentical",
        "row5foldedidentical-marker",
        "\\u0015",
        0,
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet";

    seed_previous_attempt(&h, &window_id, body).await;
    // What the previous attempt left in the composer, as this resend carries
    // it: the text, and the window's write count at that instant.
    let mine = ComposerResidue {
        body,
        write_seq: entry(&h, &window_id).write_seq(),
    };
    // A different writer (not this delivery) writes the exact same bytes as
    // the seed above: `Interactive` makes it unmistakably not this call's own.
    entry(&h, &window_id)
        .write_pty_input(
            format!("\x1b[200~{body}\x1b[201~").as_bytes(),
            InputOrigin::Interactive,
        )
        .await
        .expect("the other writer's write must not fail");

    let result = entry(&h, &window_id)
        .submit_message(body, Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "a write from another writer landed since the seed, so the submit key must not be \
         pressed even though that writer's bytes happened to match the seed"
    );
    match result {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
        }
        other => panic!(
            "another writer's identical bytes must not be mistaken for this call's own \
             untouched envelope, got {other:?}"
        ),
    }
}

#[tokio::test]
async fn row6_blocks_when_the_window_has_no_composer_section_to_observe() {
    let dir = unique_temp_dir("gate-row6");
    write_unobservable_definition(&dir, "row6", "row6-marker");
    let h = Harness::start("gate-row6", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = h
        .open(
            process::submit_child_command_with_composer("row6-marker", 0, 0, 0, 0),
            Some("row6"),
        )
        .await;
    common::wait_until_status(
        &mut client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;

    let result = entry(&h, &window_id)
        .submit_message("hello", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("a window with no composer section must never write, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::NoComposer);
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

// =====================================================================
// The submit key is written only when (1) nothing else wrote into this
// window since this call's own first byte, and (2) this call's own write
// was confirmed on screen within the upper bound.
// =====================================================================

/// A receiver that throws a pasted body away but keeps the keystrokes behind it:
/// the probe written after the body shows alone, so the body never arrived. The
/// probe is erased and the call says nothing remains.
#[tokio::test]
async fn a_body_the_receiver_drops_is_reported_not_reflected_with_nothing_left_behind() {
    let dir = unique_temp_dir("gate-b-takeback");
    let h = Harness::start("gate-b-takeback", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "btb",
        "btb-marker",
        0,
        2,
        "discard-paste:",
    )
    .await;

    let result = entry(&h, &window_id)
        .submit_message("never-seen", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("an unreflected write must not press submit, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::NotReflected);
    assert!(
        !not_placed.envelope_remains,
        "the probe showed alone, so the body never reached the composer and nothing of this \
         call's own write is left in it"
    );
    // Stage 1: the bytes really did reach the child (the mutation this
    // stage exists to catch is an implementation that never writes at
    // all, which would make stage 2 vacuously true).
    assert_eq!(
        settled_marker_count(&h, &window_id, "PASTE ", 1).await,
        1,
        "the body must actually have been written to the child before being given up on"
    );
    // Stage 2: the composer reads back empty -- the probe was erased, not
    // merely nothing painted.
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Empty => {}
        other => panic!("the composer must be left empty, got {other:?}"),
    }
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

#[tokio::test]
async fn cannot_take_back_without_a_clear_sequence_so_the_envelope_remains() {
    let dir = unique_temp_dir("gate-b-no-clear");
    let h = Harness::start("gate-b-no-clear", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_with_clear_scope_and_clear(
        &h,
        &mut client,
        &dir,
        "bnc",
        "bnc-marker",
        0,
        2,
        "",
        "frozen:",
    )
    .await;

    let (leave, call) = start_delivery(&h, &window_id, "never-seen");
    h.wait_for_scrollback(&window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    leave.send(true).expect("the call holds its receiver");
    let result = call
        .await
        .expect("the submit task must not panic")
        .expect("a blocked write is not a PTY error")
        .placement;

    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("an unreflected write must not press submit, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::NotReflected);
    assert!(
        not_placed.envelope_remains,
        "with no clear sequence to take the write back with, the bytes must be reported as \
         still sitting in the composer"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

/// Reached through the first write made into an observed-empty composer
/// (residue `None`): a folded composer never shows this call's own body
/// back, so the confirmation of the write can never be satisfied, yet no
/// other writer intervened (nothing else touched the window). Distinct from
/// [`takes_back_its_own_write_when_it_never_reflects_and_a_clear_sequence_exists`],
/// whose composer stays `Empty` through the whole wait instead of reading
/// back as the fold placeholder.
#[tokio::test]
async fn presses_submit_on_a_first_write_whose_folded_placeholder_hides_its_own_body() {
    let dir = unique_temp_dir("gate-b-folded-first");
    let h = Harness::start("gate-b-folded-first", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_folded(
        &h,
        &mut client,
        &dir,
        "bfirstfold",
        "bfirstfold-marker",
        "\\u0015",
        0,
    )
    .await;
    let body = "the retry budget for this delivery has not been reached yet";

    let result = entry(&h, &window_id)
        .submit_message(body, None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level")
        .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => panic!(
            "a first write that reached the child must still submit even though folding \
             hides it, got {other:?}"
        ),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, &format!("SUBMITTED {body}"), 1).await,
        1,
        "the body must actually reach the receiver: a fold that hides it from the screen \
         must not stop the submit key from being pressed"
    );
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty,
        "once the receiver accepts the submit, the composer settles back to empty"
    );
}

/// A gate window whose composer shows nothing from the first pasted body on:
/// what lands meanwhile decides the outcome, never a deadline.
async fn open_frozen_gate_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
) -> String {
    open_gate_window_with_clear_scope(h, client, dir, name, marker, 0, 2, "frozen:").await
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

/// Waits until the ledger moves past `seq`: the call has written something.
/// The hang guard only bounds a failure, it never decides an outcome.
async fn wait_for_a_write_after(h: &Harness, window_id: &str, seq: u64) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry(h, window_id).write_seq() == seq {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the call wrote nothing"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

/// Waits until the fixture has read the whole pasted body.
async fn wait_until_body_reached_child(h: &Harness, window_id: &str) {
    h.wait_for_scrollback(window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
}

#[tokio::test]
async fn a_does_not_press_submit_when_interactive_input_lands_before_it() {
    let dir = unique_temp_dir("gate-a-interactive");
    let h = Harness::start("gate-a-interactive", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_frozen_gate_window(&h, &mut client, &dir, "aint", "aint-marker").await;

    let e = entry(&h, &window_id);
    let submit_task = tokio::spawn({
        let e = Arc::clone(&e);
        async move {
            e.submit_message("mixed", None, SendStage::BeforeSubmitKey, None)
                .await
        }
    });
    wait_until_body_reached_child(&h, &window_id).await;
    let mut attached = h.connect().await;
    attach(&mut attached, &window_id).await;
    write_interactive_input(&mut attached, "Z").await;

    let result = submit_task
        .await
        .expect("submit_message task must not panic")
        .expect("a blocked write is not a PTY error")
        .placement;
    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("interactive input before the submit key must block it, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "the submit key must never have been pressed"
    );
}

#[tokio::test]
async fn a_does_not_press_submit_when_a_control_byte_lands_before_it() {
    let dir = unique_temp_dir("gate-a-control");
    let h = Harness::start("gate-a-control", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_frozen_gate_window(&h, &mut client, &dir, "actl", "actl-marker").await;

    let e = entry(&h, &window_id);
    let submit_task = tokio::spawn({
        let e = Arc::clone(&e);
        async move {
            e.submit_message("mixed", None, SendStage::BeforeSubmitKey, None)
                .await
        }
    });
    wait_until_body_reached_child(&h, &window_id).await;
    entry(&h, &window_id)
        .write_pty_input(b"\x03", InputOrigin::Control)
        .await
        .expect("interrupt write must not fail");

    let result = submit_task
        .await
        .expect("submit_message task must not panic")
        .expect("a blocked write is not a PTY error")
        .placement;
    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("a control byte before the submit key must block it, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

#[tokio::test]
async fn a_takeback_is_skipped_once_a_foreign_write_lands_between_verification_and_the_takeback() {
    let dir = unique_temp_dir("gate-a-takeback-race");
    let h = Harness::start("gate-a-takeback-race", detection_config(&dir)).await;
    let mut client = h.connect().await;
    // The body never shows by itself, so this call would reach the takeback
    // -- and a clear sequence exists, so the only thing that can stop the
    // takeback is another writer's intervention.
    let window_id = open_frozen_gate_window(&h, &mut client, &dir, "arace", "arace-marker").await;

    let e = entry(&h, &window_id);
    let submit_task = tokio::spawn({
        let e = Arc::clone(&e);
        async move {
            e.submit_message("never-seen", None, SendStage::BeforeSubmitKey, None)
                .await
        }
    });
    wait_until_body_reached_child(&h, &window_id).await;
    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Message)
        .await
        .expect("interfering write must not fail");

    let result = submit_task
        .await
        .expect("submit_message task must not panic")
        .expect("a blocked write is not a PTY error")
        .placement;
    let not_placed = match result {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("a mixed-in write must stop the takeback too, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        not_placed.envelope_remains,
        "another writer's byte landed before the body ever showed, so the call cannot vouch that the \
         take-back put in front of that byte emptied the composer"
    );
}

// =====================================================================
// Observation, the write and the verification all happen inside the
// same exclusive region.
// =====================================================================

/// Field (f) of the shared harness contract: two connections, a short gap.
/// If the judgment observed outside its lock, the second `send_text` could
/// land between the first's observation and its write; both bodies are
/// short single lines, so a merged or corrupted result is easy to tell from
/// two clean, separate acceptances.
#[tokio::test]
async fn observation_and_write_share_the_exclusive_region_so_a_full_send_cannot_land_between_them()
{
    let dir = unique_temp_dir("gate-p5");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("gate-p5", config).await;
    let mut opener = h.connect().await;
    let window_id = open_gate_window(&h, &mut opener, &dir, "p5", "p5-marker", "\\u0015", 0).await;

    let mut client_a = h.connect().await;
    let mut client_b = h.connect().await;
    send_from_two_connections(
        &mut client_a,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "AAA\r".to_string(),
        },
        &mut client_b,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "BBB\r".to_string(),
        },
        tokio::time::sleep(Duration::from_millis(10)),
    )
    .await;

    for mut client in [client_a, client_b] {
        match read_response(&mut client, Duration::from_secs(5)).await {
            DaemonMessage::TextSent { .. } => {}
            other => panic!("expected text_sent, got {other:?}"),
        }
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED AAA", 1).await,
        1,
        "AAA must reach the receiver on its own, not merged with BBB"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BBB", 1).await,
        1,
        "BBB must reach the receiver on its own, not merged with AAA"
    );
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED AAA BBB")
            + count_marker(&h, &window_id, "SUBMITTED AAABBB"),
        0,
        "the two bodies must never be concatenated"
    );
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED BBB AAA")
            + count_marker(&h, &window_id, "SUBMITTED BBBAAA"),
        0,
        "the two bodies must never be concatenated, either order"
    );
}

/// The (h) shape of the shared harness contract: not concurrent, but back
/// to back. A send whose body the receiver drops cleans up after itself;
/// whatever the next send writes must land on an empty composer, never stacked
/// on the bytes the first one left behind.
#[tokio::test]
async fn a_send_that_failed_to_reflect_never_leaves_something_for_the_next_send_to_stack_onto() {
    let dir = unique_temp_dir("gate-h");
    let h = Harness::start("gate-h", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "h",
        "h-marker",
        0,
        2,
        "discard-paste:",
    )
    .await;

    for body in ["first-body", "second-body"] {
        let result = entry(&h, &window_id)
            .submit_message(body, None, SendStage::BeforeSubmitKey, None)
            .await
            .expect("a blocked write is not a PTY error")
            .placement;
        let not_placed = match result {
            SubmitPlacement::NotPlaced(not_placed) => not_placed,
            other => panic!("a write that never reflects must not press submit, got {other:?}"),
        };
        assert_eq!(not_placed.reason, NotPlacedReason::NotReflected);
        assert!(
            !not_placed.envelope_remains,
            "the receiver dropped the body and nothing else was left, so nothing remains"
        );
        assert_eq!(
            read_composer_now(&h, &window_id).await,
            ComposerReadout::Empty,
            "the composer must be empty again before the next send writes into it"
        );
    }

    settled_marker_count(&h, &window_id, "PASTE ", 2).await;
    assert_eq!(
        count_marker(&h, &window_id, "first-body"),
        0,
        "the first send's leftovers must never appear anywhere in the transcript"
    );
    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}

#[tokio::test]
async fn resend_repeats_up_to_the_retry_limit_while_the_composer_stays_empty() {
    let dir = unique_temp_dir("gate-resend-repeat");
    let h = Harness::start("gate-resend-repeat", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "repeat",
        "repeat-marker",
        "\\u0015",
        0,
    )
    .await;

    for attempt in 0..3 {
        let result = entry(&h, &window_id)
            .submit_message(
                &format!("body{attempt}"),
                None,
                SendStage::BeforeSubmitKey,
                None,
            )
            .await
            .expect("write must not fail at the PTY level")
            .placement;
        assert!(
            matches!(result, SubmitPlacement::Placed(_)),
            "an empty composer must accept every attempt, not just the first"
        );
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED body0", 1).await,
        1,
        "body0 must have been accepted (the composer is empty again after every accept)"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED body1", 1).await,
        1
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED body2", 1).await,
        1
    );
}

// =====================================================================
// What a blocked write returns, and what a placed one returns.
// =====================================================================

#[tokio::test]
async fn a_not_placed_result_carries_no_submitted_at_or_status_before_submit() {
    // A pure type-shape check: `NotPlaced` does not carry a `SubmitOutcome`
    // at all, so a caller reading a blocked result cannot reach either of
    // the two fields that are only meaningful once the submit key has been
    // pressed. This is asserted by construction, not by a comparison: the
    // point is that the `match` arm below is the only way to read a
    // `SubmitPlacement`'s payload, and neither arm exposes both shapes at
    // once.
    let placed = SubmitPlacement::Placed(yagura_daemon::input::SubmitOutcome {
        status_before_submit: WindowStatus::WaitingInput,
        submitted_at: tokio::time::Instant::now(),
    });
    let not_placed = SubmitPlacement::NotPlaced(yagura_daemon::window::NotPlaced {
        envelope_remains: false,
        reason: NotPlacedReason::Unobservable,
    });

    match placed {
        SubmitPlacement::Placed(outcome) => {
            assert_eq!(outcome.status_before_submit, WindowStatus::WaitingInput);
        }
        SubmitPlacement::NotPlaced(_) => panic!("wrong arm"),
    }
    match not_placed {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Unobservable);
            assert!(!not_placed.envelope_remains);
        }
        SubmitPlacement::Placed(_) => panic!("wrong arm"),
    }
}

// =====================================================================
// The two required regression scenarios for a delivery that must not erase
// or overwrite another sender's composer content.
// =====================================================================

/// The central reproduction: a delivery's own residue must never be
/// mistaken for -- or allowed to overwrite -- content some other sender
/// left in the same window's composer.
#[tokio::test]
async fn a_pending_deliverys_resend_does_not_erase_unrelated_content_left_by_another_sender() {
    let dir = unique_temp_dir("gate-resend-repro");
    let h = Harness::start("gate-resend-repro", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "resend",
        "resend-marker",
        "\\u0015",
        0,
    )
    .await;

    // This delivery's own envelope, left behind before anyone else wrote.
    let mine = ComposerResidue {
        body: "MY-ENVELOPE",
        write_seq: entry(&h, &window_id).write_seq(),
    };

    // Another sender's body is sitting in the composer, not yet submitted.
    entry(&h, &window_id)
        .write_pty_input(b"OTHER-SENDERS-BODY", InputOrigin::Message)
        .await
        .expect("seed write must not fail");

    // A pending delivery's resend: its own residue is its own envelope,
    // not the other sender's body.
    let result = entry(&h, &window_id)
        .submit_message("MY-ENVELOPE", Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    assert!(
        matches!(result, SubmitPlacement::NotPlaced(_)),
        "a resend that observes someone else's content must not proceed"
    );
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert_eq!(
            text, "OTHER-SENDERS-BODY",
            "the other sender's body must still be there, untouched"
        ),
        other => panic!("expected the other sender's body still on screen, got {other:?}"),
    }
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "no clear sequence, and no submit key: the resend must not have run its sequence at all"
    );
}

/// The mirror: a fresh send must not overwrite an envelope that is still
/// sitting in the composer waiting on its own delivery.
#[tokio::test]
async fn a_fresh_send_does_not_overwrite_an_envelope_still_sitting_in_the_composer() {
    let dir = unique_temp_dir("gate-mirror");
    let h = Harness::start("gate-mirror", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "mirror",
        "mirror-marker",
        "\\u0015",
        0,
    )
    .await;

    entry(&h, &window_id)
        .write_pty_input(b"HANDOFF-ENVELOPE", InputOrigin::Message)
        .await
        .expect("seed write must not fail");

    let result = entry(&h, &window_id)
        .submit_message("NEW-MESSAGE", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    assert!(
        matches!(result, SubmitPlacement::NotPlaced(_)),
        "a fresh send observing a foreign envelope must not proceed"
    );
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert_eq!(text, "HANDOFF-ENVELOPE"),
        other => panic!("expected the pending envelope still on screen, got {other:?}"),
    }
}

// =====================================================================
// The wiring every case above skips: those call `submit_message` directly
// with a residue the test itself constructs. `deliver_handoff`'s real resend
// loop instead carries forward whatever `submit_message` returned, and
// nothing exercises that hand-off unless the call goes through
// `send_handoff` end to end.
// =====================================================================

/// A real `send_handoff` resend, driven only by what `submit_message` itself
/// returned as the previous attempt's baseline -- never a residue this test
/// builds. The fixture's paste guard swallows the first submit key (the
/// composer keeps the folded envelope on screen, unrestorable), so the
/// second attempt can only succeed if the baseline `deliver_handoff` carried
/// forward still matches this window's write count. A baseline taken before
/// that first attempt's own writes would always read short of the window's
/// count from here on, and every further resend would decline as foreign.
#[tokio::test]
async fn send_handoff_resend_succeeds_on_the_write_seq_submit_message_itself_returned() {
    let dir = unique_temp_dir("gate-carry-resend");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("gate-carry-resend", config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_folded_composer(
        &h,
        &mut client,
        &dir,
        "carry-resend",
        "carry-resend-marker",
        0,
        1,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "\\u0015",
            delivery_confirm_ms: 1200,
            resend_max: 1,
        },
        0,
        FOLD_PLACEHOLDER,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look here".to_string(),
        },
    )
    .await;

    match read_response(&mut client, Duration::from_secs(30)).await {
        DaemonMessage::HandoffSent { delivery, .. } => {
            assert_eq!(
                serde_json::to_value(delivery).expect("a delivery encodes"),
                "taken",
                "the resend must land through the carried-forward write-seq baseline once \
                 the paste guard has taken the first submit key"
            );
        }
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED [yagura handoff] ", 1).await,
        1,
        "the envelope must reach the receiver exactly once, on the resent submit key"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "PASTE ", 1).await,
        1,
        "only the first attempt frames a body as a paste; a baseline read as belonging to \
         someone else would fall back to writing the whole envelope fresh again"
    );
}

/// The resend budget every case below spends: the submit key is pressed at
/// most `1 + RESEND_BUDGET` times for one envelope.
const RESEND_BUDGET: u8 = 2;

/// Delivers one `send_handoff` to a receiver that stays ready and quiet
/// while its paste guard swallows the first `swallowed` submit keys, and
/// returns how many submit keys the receiver took as a submit once the
/// handoff was answered.
async fn deliver_against_swallowed_submit_keys(tag: &str, swallowed: u64) -> usize {
    let dir = unique_temp_dir(tag);
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_handoff"])),
        ..detection_config(&dir)
    };
    let h = Harness::start(tag, config).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_folded_composer(
        &h,
        &mut client,
        &dir,
        tag,
        &format!("{tag}-marker"),
        0,
        swallowed,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 300,
            clear: "\\u0015",
            delivery_confirm_ms: 1200,
            resend_max: RESEND_BUDGET,
        },
        0,
        FOLD_PLACEHOLDER,
    )
    .await;

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "look here".to_string(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(30)).await {
        DaemonMessage::HandoffSent { .. } => {}
        other => panic!("expected handoff_sent, got {other:?}"),
    }
    count_marker(&h, &window_id, "SUBMITTED ")
}

#[tokio::test]
async fn a_quiet_receiver_is_resent_the_submit_key_as_many_times_as_resend_max_allows() {
    assert_eq!(
        deliver_against_swallowed_submit_keys("gate-resend-budget-spent", u64::from(RESEND_BUDGET))
            .await,
        1,
        "the last resend the budget allows must still be pressed"
    );
}

#[tokio::test]
async fn a_quiet_receiver_is_never_resent_the_submit_key_beyond_resend_max() {
    assert_eq!(
        deliver_against_swallowed_submit_keys(
            "gate-resend-budget-exceeded",
            u64::from(RESEND_BUDGET) + 1,
        )
        .await,
        0,
        "no submit key may be pressed past the resend budget"
    );
}

/// The residue tracking also closes an older shape of the
/// same problem: a human's in-progress draft must never be merged with a
/// daemon-delivered line.
#[tokio::test]
async fn a_human_typed_draft_is_never_merged_with_a_daemon_delivered_line() {
    let dir = unique_temp_dir("gate-draft-merge");
    let h = Harness::start("gate-draft-merge", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id =
        open_gate_window(&h, &mut client, &dir, "draft", "draft-marker", "\\u0015", 0).await;

    entry(&h, &window_id)
        .write_pty_input(b"human-typed-draft", InputOrigin::Interactive)
        .await
        .expect("seed write must not fail");

    let result = entry(&h, &window_id)
        .submit_message("delivered-line", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("a blocked write is not a PTY error")
        .placement;

    assert!(matches!(result, SubmitPlacement::NotPlaced(_)));
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "no merged line, draft or otherwise, may ever be accepted"
    );
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert_eq!(
            text, "human-typed-draft",
            "the draft must remain on screen for the human who typed it"
        ),
        other => panic!("expected the draft still on screen, got {other:?}"),
    }
}

// =====================================================================
// The judgment's declared scope: interrupt bytes are exempt. A
// caller-composed body with more than one line does not fall in that
// exemption: `validate_send_text_body`
// rejects it before the judgment -- and any write -- ever runs.
// =====================================================================

#[tokio::test]
async fn a_send_text_with_an_embedded_line_break_is_rejected_before_reaching_the_composer_gate() {
    let dir = unique_temp_dir("gate-raw-exempt");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("gate-raw-exempt", config).await;
    let mut client = h.connect().await;
    let window_id =
        open_gate_window(&h, &mut client, &dir, "raw", "raw-marker", "\\u0015", 0).await;

    entry(&h, &window_id)
        .write_pty_input(b"FOREIGN", InputOrigin::Message)
        .await
        .expect("seed write must not fail");
    // The rejection below never touches the PTY, so nothing else waits for
    // the child to repaint the seed before the composer is read.
    h.wait_for_scrollback_occurrences(&window_id, "FOREIGN", 1, Duration::from_secs(10))
        .await;

    // An embedded line break: rejected by validate_send_text_body before
    // dispatch, so the composer-gate judgment never runs on it at all.
    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "first\rsecond\r".to_string(),
        },
    )
    .await;
    let msg = read_response(&mut client, Duration::from_secs(5)).await;
    expect_error(msg, "invalid_argument");

    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert_eq!(
            text, "FOREIGN",
            "a rejected body must never reach the composer; only the seed write survives"
        ),
        other => panic!("expected the composer to still hold only the seed write, got {other:?}"),
    }
}

#[tokio::test]
async fn an_interrupt_control_byte_is_written_through_regardless_of_foreign_content_on_screen() {
    let dir = unique_temp_dir("gate-control-exempt");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["interrupt_window"])),
        interrupt_bytes: b"INTERRUPT-MARK\r\n".to_vec(),
        ..detection_config(&dir)
    };
    let h = Harness::start("gate-control-exempt", config).await;
    let mut client = h.connect().await;
    let input_log = dir.join("child-input.bin");
    let window_id = open_gate_window_logging_input(
        &h,
        &mut client,
        &dir,
        "ctl",
        "ctl-marker",
        "\\u0015",
        &input_log,
    )
    .await;

    entry(&h, &window_id)
        .write_pty_input(b"FOREIGN", InputOrigin::Message)
        .await
        .expect("seed write must not fail");

    send(
        &mut client,
        &ClientMessage::InterruptWindow {
            window_id: window_id.clone(),
        },
    )
    .await;
    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::WindowInterrupted { .. } => {}
        other => panic!("expected window_interrupted, got {other:?}"),
    }
    // The child reads in order, so once it has recorded this sentinel it has
    // recorded everything the daemon wrote before it.
    const SENTINEL: &[u8] = b"END-OF-INPUT";
    entry(&h, &window_id)
        .write_pty_input(SENTINEL, InputOrigin::Message)
        .await
        .expect("sentinel write must not fail");
    let received = wait_for_child_input_containing(&input_log, SENTINEL).await;
    assert_eq!(
        count_subslice(&received, b"INTERRUPT-MARK\r\n"),
        1,
        "an interrupt must reach the child exactly once, regardless of what the judgment would \
         have decided about a message write"
    );
}

/// [`open_gate_window`] whose child appends every byte it reads to `input_log`.
#[allow(clippy::too_many_arguments)]
async fn open_gate_window_logging_input(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    clear: &'static str,
    input_log: &Path,
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
    let mut command =
        yagura_test_support::process::submit_child_command_with_composer(marker, 0, 0, 0, 0);
    command.push(String::new()); // fold_placeholder: none
    command.push(String::new()); // clear_scope: ordinary
    command.push(String::new()); // overlay: none
    command.push(String::new()); // overlay_heading
    command.push(
        input_log
            .to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    let window_id = h.open(command, Some(name)).await;
    common::wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// Polls `input_log` until it contains `needle` and returns what it holds.
/// The deadline only bounds a hang; it never decides the outcome.
async fn wait_for_child_input_containing(input_log: &Path, needle: &[u8]) -> Vec<u8> {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let received = std::fs::read(input_log).unwrap_or_default();
        if count_subslice(&received, needle) > 0 {
            return received;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child never recorded {needle:?}; it recorded {received:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

fn count_subslice(haystack: &[u8], needle: &[u8]) -> usize {
    haystack
        .windows(needle.len())
        .filter(|window| *window == needle)
        .count()
}

// =====================================================================
// Approval: the judgment runs at write time, not at the time a destructive
// request was queued for approval.
// =====================================================================

#[tokio::test]
async fn a_delivery_held_for_approval_observes_the_composer_at_write_time_not_at_enqueue_time() {
    let dir = unique_temp_dir("gate-approval");
    let config = DaemonConfig {
        approval_policy: None,
        playbook: None,
        ..detection_config(&dir)
    };
    let h = Harness::start("gate-approval", config).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut client = h.connect().await;
    let window_id =
        open_gate_window(&h, &mut client, &dir, "appr", "appr-marker", "\\u0015", 0).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "held-for-approval\r".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested_within(&mut approver, Duration::from_secs(5)).await;
    let request_id = request.request_id.clone();

    // Foreign content lands only after the request was already queued: a
    // judgment that decided at enqueue time would never see it.
    entry(&h, &window_id)
        .write_pty_input(b"LANDED-WHILE-PENDING", InputOrigin::Message)
        .await
        .expect("seed write must not fail");

    decide(&mut approver, &request_id, true).await;

    match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Error { code, .. } => {
            assert_eq!(
                code, "not_written",
                "approved but blocked at write time by the content that landed while pending"
            );
        }
        other => {
            panic!("expected the approved send to still be judged at write time, got {other:?}")
        }
    }
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert_eq!(text, "LANDED-WHILE-PENDING"),
        other => panic!("expected the foreign content still on screen, got {other:?}"),
    }
}

// =====================================================================
// `not_written`'s reason reaches the caller distinguishably: a caller must be able to tell "another actor's text was
// there" apart from "the composer could not be read at all", rather than
// reading one wording asserted for every decline.
// =====================================================================

#[tokio::test]
async fn send_text_not_written_message_distinguishes_foreign_from_unobservable() {
    let dir = unique_temp_dir("gate-reason-foreign");
    let config = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("gate-reason-foreign", config).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "reason-foreign",
        "reason-foreign-marker",
        "\\u0015",
        0,
    )
    .await;
    entry(&h, &window_id)
        .write_pty_input(b"SOMEONE-ELSES-BODY", InputOrigin::Message)
        .await
        .expect("seed write must not fail");

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "MINE\r".to_string(),
        },
    )
    .await;
    let foreign_message = match read_response(&mut client, Duration::from_secs(5)).await {
        DaemonMessage::Error { code, message, .. } => {
            assert_eq!(code, "not_written");
            message
        }
        other => panic!("expected not_written, got {other:?}"),
    };

    // The composer cannot be trusted: an earlier delivery's requester left
    // after its body was written, and with no clear sequence the body could not
    // be taken back, so the window carries a write nothing confirmed.
    let dir2 = unique_temp_dir("gate-reason-unobservable");
    let config2 = DaemonConfig {
        approval_policy: Some(policy(&["send_message"])),
        ..detection_config(&dir2)
    };
    let h2 = Harness::start("gate-reason-unobservable", config2).await;
    let mut client2 = h2.connect().await;
    let window_id2 = open_gate_window_with_clear_scope_and_clear(
        &h2,
        &mut client2,
        &dir2,
        "reason-unobservable",
        "reason-unobservable-marker",
        0,
        2,
        "",
        "frozen:",
    )
    .await;
    let (leave, call) = start_delivery(&h2, &window_id2, "first");
    h2.wait_for_scrollback(&window_id2, b"PASTE 1", Duration::from_secs(10))
        .await;
    leave.send(true).expect("the call holds its receiver");
    call.await
        .expect("the submit task must not panic")
        .expect("a blocked write is not a PTY error");

    send(
        &mut client2,
        &ClientMessage::SendText {
            window_id: window_id2.clone(),
            text: "hello\r".to_string(),
        },
    )
    .await;
    let unobservable_message = match read_response(&mut client2, Duration::from_secs(5)).await {
        DaemonMessage::Error { code, message, .. } => {
            assert_eq!(code, "not_written");
            message
        }
        other => panic!("expected not_written, got {other:?}"),
    };

    assert_ne!(
        foreign_message, unobservable_message,
        "two different reasons must not collapse into the same wording"
    );
    assert!(
        foreign_message.contains("already"),
        "the observed-foreign-content reason must say something was there: {foreign_message:?}"
    );
    assert!(
        !unobservable_message.contains("already"),
        "the daemon never saw the composer at all, so it must not claim content was already \
         there: {unobservable_message:?}"
    );
    assert!(
        unobservable_message.contains("could not be observed"),
        "the unobservable reason must say the composer could not be read: \
         {unobservable_message:?}"
    );
}

// =====================================================================
// Real-CLI stalls: a startup-adjacent stall can
// leave the reflection poll waiting the full upper bound even though the
// composer caught up almost immediately, and a wrapped body's own
// trailing space at the last wrap boundary is what `read_composer` (in
// `screen.rs`) loses when it trims each painted line's trailing
// whitespace -- both cases below trigger through that loss, not through
// any change to the fixture's ordinary (unwrapped) behavior.
// =====================================================================

/// A body 65 characters long: composer-draw mode's own wrap (32 characters
/// per painted line, never ending a line on a space) folds it across two
/// full rows and carries the trailing space alone onto a third row, which
/// is exactly the row `VtScreen::read_composer` trims to nothing (nothing
/// follows that row to carry the space forward, unlike an interior wrap
/// point). Shared by every case in this section so the same reading loss
/// is exercised the same way throughout.
fn wrap_boundary_space_body() -> String {
    "A".repeat(64) + " "
}

/// A body that never reads back verbatim -- [`wrap_boundary_space_body`] loses
/// its last wrap-boundary space -- still counts as shown: the probe behind it,
/// not an exact match, says the body arrived, and the call ends at once.
#[tokio::test]
async fn a_wrap_boundary_space_the_reading_lost_does_not_keep_a_reflected_body_from_being_placed() {
    let dir = unique_temp_dir("gate-wrap-space-poll");
    let h = Harness::start("gate-wrap-space-poll", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "wrapspeed",
        "wrapspeed-marker",
        "\\u0015",
        0,
    )
    .await;

    let body = wrap_boundary_space_body();
    let result = tokio::time::timeout(
        Duration::from_secs(30),
        entry(&h, &window_id).submit_message(&body, None, SendStage::BeforeSubmitKey, None),
    )
    .await
    .expect(
        "the call must end once the probe shows behind the body, not wait for a verbatim \
         match a lost wrap-boundary space can never produce",
    )
    .expect("write must not fail at the PTY level")
    .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a write the fixture reflected must still place, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, &format!("SUBMITTED {body}"), 1).await,
        1,
        "the body actually reached the receiver, unmodified"
    );
}

/// A residue comparison against the screen must ignore the whitespace
/// `VtScreen::read_composer` loses at a wrap boundary (see the case above
/// for why [`wrap_boundary_space_body`] loses it). `stale_seq` is captured
/// before the seed write, so the write ledger alone cannot vouch for this
/// residue (`record_vouches_for_residue` requires a matching `write_seq`)
/// -- only a screen match (`restored_own_envelope`) can place it, and that
/// match must not be broken by a space the reading itself lost.
#[tokio::test]
async fn residue_restored_from_screen_ignores_a_wrap_boundary_space_the_reading_lost() {
    let dir = unique_temp_dir("gate-wrap-space-residue");
    let h = Harness::start("gate-wrap-space-residue", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window(
        &h,
        &mut client,
        &dir,
        "wrapres",
        "wrapres-marker",
        "\\u0015",
        0,
    )
    .await;

    let body = wrap_boundary_space_body();
    let stale_seq = entry(&h, &window_id).write_seq();
    seed_previous_attempt(&h, &window_id, &body).await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(body.trim_end().to_string()),
        Duration::from_secs(10),
    )
    .await;

    let mine = ComposerResidue {
        body: &body,
        write_seq: stale_seq,
    };
    let result = entry(&h, &window_id)
        .submit_message(&body, Some(mine), SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level")
        .placement;

    match result {
        SubmitPlacement::Placed(_) => {}
        other => panic!(
            "the same envelope, restored on screen modulo a lost wrap-boundary space, must \
             resend, got {other:?}"
        ),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, &format!("SUBMITTED {body}"), 1).await,
        1,
        "the envelope, with its actually-written space intact, must reach the receiver \
         exactly once -- resent via the submit key alone, never rewritten"
    );
}

/// [`open_gate_window`], with `submit_child`'s
/// `clear_scope` argument appended to the fixture's argv, right after the
/// (empty, "no folding") `fold_placeholder` slot.
#[allow(clippy::too_many_arguments)]
async fn open_gate_window_with_clear_scope(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    reflect_delay_ms: u64,
    resend_max: u8,
    clear_scope: &str,
) -> String {
    open_gate_window_with_clear_scope_and_clear(
        h,
        client,
        dir,
        name,
        marker,
        reflect_delay_ms,
        resend_max,
        "\\u0015",
        clear_scope,
    )
    .await
}

/// [`open_gate_window_with_clear_scope`] for a definition whose `clear` is the
/// given TOML basic-string body: `""` for a window that can never take a
/// write back.
#[allow(clippy::too_many_arguments)]
async fn open_gate_window_with_clear_scope_and_clear(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    reflect_delay_ms: u64,
    resend_max: u8,
    clear: &'static str,
    clear_scope: &str,
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
            resend_max,
        },
    );
    let mut command = yagura_test_support::process::submit_child_command(marker, 0, 0, 0);
    command.push(yagura_test_support::process::COMPOSER_MARKER.to_string());
    command.push(yagura_test_support::process::COMPOSER_SEPARATOR.to_string());
    command.push(yagura_test_support::process::COMPOSER_CONTINUATION_PREFIX.to_string());
    command.push(reflect_delay_ms.to_string());
    command.push(String::new()); // fold_placeholder: none
    command.push(clear_scope.to_string());
    let window_id = h.open(command, Some(name)).await;
    common::wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// A receiver that reads nothing while a write and its probe queue up, and
/// then works through them in order: the delivery waits for the body to show
/// -- no clock ends that wait -- presses no submit key meanwhile, and once the
/// receiver resumes the body is delivered once, whole.
#[tokio::test]
async fn a_stalled_reflection_is_waited_on_and_the_body_is_delivered_once_and_whole_when_it_resumes()
 {
    let dir = unique_temp_dir("gate-clear-scope-f4");
    let resume = dir.join("resume");
    let h = Harness::start("gate-clear-scope-f4", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "clearscope",
        "clearscope-marker",
        0,
        1,
        &format!(
            "stall-until|{}|line-clear",
            resume
                .to_str()
                .expect("the resume path must be valid UTF-8")
        ),
    )
    .await;

    let body = "hello-f4";
    let sender = entry(&h, &window_id);
    let call = tokio::spawn(async move {
        sender
            .submit_message(body, None, SendStage::BeforeSubmitKey, None)
            .await
    });
    h.wait_for_scrollback(&window_id, b"STALL-BEGAN", Duration::from_secs(10))
        .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a body the receiver has not shown yet is waited on, not given up on by a clock"
    );
    assert_eq!(
        count_marker(&h, &window_id, "STALL-ENDED"),
        0,
        "the receiver must still be stalled while the delivery waits"
    );
    assert_eq!(
        count_marker(&h, &window_id, "SUBMITTED"),
        0,
        "nothing may be submitted while the body has not shown"
    );

    std::fs::write(&resume, b"").expect("let the receiver resume");
    let judgment = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the body must be placed once the receiver shows it, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "STALL-ENDED", 1).await,
        1,
        "the receiver must resume"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, &format!("SUBMITTED {body}"), 1).await,
        1,
        "the body must reach the receiver exactly once and intact"
    );
}

// =====================================================================
// A composer reading is evidence only once everything written before it
// has been handled: a draft the user just pasted, not yet drawn, must never
// be sent mixed with a delivery.
// =====================================================================

/// A window whose `submit_child` paints nothing while the file `paused`
/// exists, though it keeps reading input in order and printing its
/// paste-completion lines: the test decides when what was read shows.
async fn open_gate_window_painting_unless(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    paused: &Path,
) -> String {
    open_gate_window_painting_unless_lagging(h, client, dir, name, marker, paused, 0).await
}

/// [`open_gate_window_painting_unless`] with every paint held back `paint_lag_ms` more (the first one after the
/// pause ends included): a reading taken within that margin of the daemon's
/// own write still shows the screen as it stood before the write.
#[allow(clippy::too_many_arguments)]
async fn open_gate_window_painting_unless_lagging(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    paused: &Path,
    paint_lag_ms: u64,
) -> String {
    write_submit_definition(
        dir,
        name,
        marker,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear: "\\u0015",
            delivery_confirm_ms: 2000,
            resend_max: 2,
        },
    );
    let mut command =
        yagura_test_support::process::submit_child_command_with_composer(marker, 0, 0, 0, 0);
    command[9] = if paint_lag_ms == 0 {
        format!("pause|{}", paused.display())
    } else {
        format!("pause|{}|{paint_lag_ms}", paused.display())
    };
    let window_id = h.open(command, Some(name)).await;
    common::wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// How long a lagging screen holds back each paint: the margin within which a
/// reading still shows the screen as it stood before the daemon's last write.
const PAINT_LAG_MS: u64 = 300;

/// Pastes `draft` the way the GUI passes a user's paste through.
async fn paste_draft(h: &Harness, window_id: &str, draft: &str) {
    let framed = format!("\x1b[200~{draft}\x1b[201~");
    entry(h, window_id)
        .write_pty_input(framed.as_bytes(), InputOrigin::Interactive)
        .await
        .expect("draft write must not fail");
}

/// Runs `submit_message` while painting is paused, and resumes painting once
/// the call has written its next byte after `seq`, so the screen catches up
/// only after the call has committed to what it read.
async fn submit_then_resume_painting(
    h: &Harness,
    window_id: &str,
    paused: &Path,
    seq: u64,
    body: &'static str,
    residue: Option<(&'static str, u64)>,
) -> SubmitJudgment {
    let window = entry(h, window_id);
    let call = tokio::spawn(async move {
        window
            .submit_message(
                body,
                residue.map(|(body, write_seq)| ComposerResidue { body, write_seq }),
                SendStage::BeforeSubmitKey,
                None,
            )
            .await
            .expect("write must not fail at the PTY level")
    });
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry(h, window_id).write_seq() == seq && !call.is_finished() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the call wrote nothing"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    std::fs::remove_file(paused).expect("resume painting");
    call.await.expect("submit_message task must not panic")
}

#[tokio::test]
async fn a_delivery_into_an_empty_reading_never_sends_a_pasted_draft_not_yet_drawn() {
    let dir = unique_temp_dir("gate-undrawn-draft");
    let h = Harness::start("gate-undrawn-draft", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let paused = dir.join("paused");
    let window_id = open_gate_window_painting_unless(
        &h,
        &mut client,
        &dir,
        "undrawn",
        "undrawn-marker",
        &paused,
    )
    .await;

    std::fs::write(&paused, b"").expect("pause painting");
    paste_draft(&h, &window_id, "draft").await;
    let seq = entry(&h, &window_id).write_seq();
    let judgment = submit_then_resume_painting(&h, &window_id, &paused, seq, "BODY", None).await;

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
            assert!(!not_placed.envelope_remains, "the body was never written");
        }
        other => {
            panic!("a composer holding the user's draft must not take the body, got {other:?}")
        }
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0,
        "nothing may be submitted on the user's behalf"
    );
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("draft".to_string()),
        Duration::from_secs(10),
    )
    .await;
}

#[tokio::test]
async fn a_resend_never_presses_over_a_pasted_draft_not_yet_drawn_behind_its_own_envelope() {
    let dir = unique_temp_dir("gate-undrawn-behind");
    let h = Harness::start("gate-undrawn-behind", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let paused = dir.join("paused");
    let window_id = open_gate_window_painting_unless_lagging(
        &h,
        &mut client,
        &dir,
        "behind",
        "behind-marker",
        &paused,
        PAINT_LAG_MS,
    )
    .await;
    seed_previous_attempt(&h, &window_id, "ENV").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("ENV".to_string()),
        Duration::from_secs(10),
    )
    .await;
    let residue_seq = entry(&h, &window_id).write_seq();

    std::fs::write(&paused, b"").expect("pause painting");
    paste_draft(&h, &window_id, "draft").await;
    let seq = entry(&h, &window_id).write_seq();
    let judgment = submit_then_resume_painting(
        &h,
        &window_id,
        &paused,
        seq,
        "ENV",
        Some(("ENV", residue_seq)),
    )
    .await;

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
        }
        other => {
            panic!("an envelope with the user's draft behind it must not resend, got {other:?}")
        }
    }
    // The call returns only once the composer holds what stood before its
    // probe again: the next reader's lock must not be released earlier. The
    // screen lags every paint, so a call that returned right after writing
    // the erase would still see the probe.
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Body("ENVdraft".to_string()),
        "the probe must be gone from the composer by the time the call returns"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0,
        "nothing may be submitted on the user's behalf"
    );
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("ENVdraft".to_string()),
        Duration::from_secs(10),
    )
    .await;
}

#[tokio::test]
async fn a_delivery_writes_into_a_composer_whose_text_was_cleared_but_not_yet_redrawn() {
    let dir = unique_temp_dir("gate-stale-body");
    let h = Harness::start("gate-stale-body", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let paused = dir.join("paused");
    let window_id =
        open_gate_window_painting_unless(&h, &mut client, &dir, "stale", "stale-marker", &paused)
            .await;
    paste_draft(&h, &window_id, "OLD").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("OLD".to_string()),
        Duration::from_secs(10),
    )
    .await;

    // The user clears the draft; the screen still shows it when the delivery
    // reads the composer.
    std::fs::write(&paused, b"").expect("pause painting");
    entry(&h, &window_id)
        .write_pty_input(&[0x15], InputOrigin::Interactive)
        .await
        .expect("clear write must not fail");
    let seq = entry(&h, &window_id).write_seq();
    let judgment = submit_then_resume_painting(&h, &window_id, &paused, seq, "NEW", None).await;

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a composer already emptied must take the body, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED NEW", 1).await,
        1,
        "the body reaches the receiver once, with nothing of the cleared draft"
    );
}

/// A text already on screen that ends in the probe's character must not be
/// taken for the probe: the probe has shown only once the composer reads
/// differently from before it was written.
#[tokio::test]
async fn a_stale_text_ending_in_the_probe_character_is_not_taken_for_the_probe() {
    let dir = unique_temp_dir("gate-stale-probe-char");
    let h = Harness::start("gate-stale-probe-char", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let paused = dir.join("paused");
    // Every paint lags, so the stale text is still on screen when the call
    // takes its first reading after writing the probe.
    let window_id = open_gate_window_painting_unless_lagging(
        &h,
        &mut client,
        &dir,
        "staleprobe",
        "staleprobe-marker",
        &paused,
        PAINT_LAG_MS,
    )
    .await;
    assert_eq!(
        SUBMIT_CHILD_PROBE, "x",
        "the stale text below must end in it"
    );
    paste_draft(&h, &window_id, "box").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("box".to_string()),
        Duration::from_secs(10),
    )
    .await;

    std::fs::write(&paused, b"").expect("pause painting");
    entry(&h, &window_id)
        .write_pty_input(&[0x15], InputOrigin::Interactive)
        .await
        .expect("clear write must not fail");
    let seq = entry(&h, &window_id).write_seq();
    let judgment = submit_then_resume_painting(&h, &window_id, &paused, seq, "NEW", None).await;

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("a composer already emptied must take the body, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED NEW", 1).await,
        1,
        "the body reaches the receiver once, with nothing of the cleared draft"
    );
}

/// A residue the caller's own record vouches for is taken back only while the
/// record still says so: once another writer has written, what the screen
/// shows may be that writer's text on top of the residue, and a take-back
/// would clear it.
#[tokio::test]
async fn a_residue_is_not_taken_back_once_someone_else_has_written_behind_it() {
    let dir = unique_temp_dir("gate-no-takeback-after-foreign");
    let h = Harness::start("gate-no-takeback-after-foreign", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let paused = dir.join("paused");
    let window_id = open_gate_window_painting_unless(
        &h,
        &mut client,
        &dir,
        "notakeback",
        "notakeback-marker",
        &paused,
    )
    .await;
    seed_previous_attempt(&h, &window_id, "ENV").await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("ENV".to_string()),
        Duration::from_secs(10),
    )
    .await;
    let residue_seq = entry(&h, &window_id).write_seq();

    // The user's paste is not drawn yet, so the screen still reads as the
    // residue alone while the write count has moved on.
    std::fs::write(&paused, b"").expect("pause painting");
    paste_draft(&h, &window_id, "draft").await;
    let seq = entry(&h, &window_id).write_seq();
    let judgment = submit_then_resume_painting(
        &h,
        &window_id,
        &paused,
        seq,
        "NEW",
        Some(("ENV", residue_seq)),
    )
    .await;

    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Foreign);
        }
        other => panic!("a draft behind the residue must not be cleared, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0,
        "nothing may be submitted on the user's behalf"
    );
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("ENVdraft".to_string()),
        Duration::from_secs(10),
    )
    .await;
}

/// A receiver that drops its first input (a CLI draining input while it
/// starts): the probe of the first delivery never shows, so the delivery waits
/// -- no clock ends that wait. Another writer's byte ends it, and the next
/// delivery probes again and places the body once.
#[tokio::test]
async fn a_probe_a_starting_receiver_dropped_is_waited_on_and_probed_again_by_the_next_delivery() {
    let dir = unique_temp_dir("gate-dropped-probe");
    let h = Harness::start("gate-dropped-probe", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_composer(
        &h,
        &mut client,
        &dir,
        "dropped",
        "dropped-marker",
        1,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear: "\\u0015",
            delivery_confirm_ms: 2000,
            resend_max: 2,
        },
        0,
    )
    .await;

    let seq = entry(&h, &window_id).write_seq();
    let (_leave, first) = start_delivery(&h, &window_id, "BODY");
    wait_for_a_write_after(&h, &window_id, seq).await;
    let_every_clock_bound_run_out().await;
    assert!(
        !first.is_finished(),
        "a probe the receiver dropped is waited on, not given up on by a clock"
    );
    // A keystroke typed and erased in one write reaches the receiver once it
    // has started: it ends the wait and leaves the composer as empty as before.
    entry(&h, &window_id)
        .write_pty_input(b"Z\x7f", InputOrigin::Interactive)
        .await
        .expect("the keystroke must not fail");
    let first = first
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    match first.placement {
        SubmitPlacement::NotPlaced(not_placed) => {
            assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
            assert!(!not_placed.envelope_remains, "the body was never written");
        }
        other => panic!("another writer's byte must end the wait, got {other:?}"),
    }

    let second = entry(&h, &window_id)
        .submit_message("BODY", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level");

    match second.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the next delivery must probe again and place the body, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 1).await,
        1,
        "the body reaches the receiver exactly once"
    );
}

/// A probe that is only drawn late (the screen lags the child): the delivery
/// that wrote it is left by its requester, and the next one must recognise the
/// probe now standing alone as its predecessor's, erase it and place the body,
/// instead of reading it as someone's text.
#[tokio::test]
async fn a_probe_drawn_after_its_requester_left_is_settled_by_the_next_delivery() {
    let dir = unique_temp_dir("gate-late-probe");
    let h = Harness::start("gate-late-probe", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let paused = dir.join("paused");
    let window_id = open_gate_window_painting_unless_lagging(
        &h,
        &mut client,
        &dir,
        "lateprobe",
        "lateprobe-marker",
        &paused,
        0,
    )
    .await;

    std::fs::write(&paused, b"").expect("pause painting");
    let seq = entry(&h, &window_id).write_seq();
    let (leave, call) = start_delivery(&h, &window_id, "BODY");
    wait_for_a_write_after(&h, &window_id, seq).await;
    leave.send(true).expect("the call holds its receiver");
    let first = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    assert!(
        matches!(first.placement, SubmitPlacement::NotPlaced(_)),
        "a probe that never shows places nothing, got {first:?}"
    );
    std::fs::remove_file(&paused).expect("resume painting");
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(SUBMIT_CHILD_PROBE.to_string()),
        Duration::from_secs(10),
    )
    .await;

    let second = entry(&h, &window_id)
        .submit_message("BODY", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level");

    match second.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("the late probe must be settled and the body placed, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 1).await,
        1,
        "the body reaches the receiver exactly once, after the late probe's erase"
    );
}

/// Opens `submit_child` with its Enter, within `paste_enter_window_ms` of a
/// paste's end, taken as a line break.
async fn open_gate_window_with_paste_enter_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    submit_delay_ms: u64,
    paste_enter_window_ms: u64,
) -> String {
    write_submit_definition(
        dir,
        name,
        marker,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms,
            clear: "\\u0015",
            delivery_confirm_ms: 2000,
            resend_max: 0,
        },
    );
    let mut command =
        yagura_test_support::process::submit_child_command_with_composer(marker, 0, 0, 0, 0);
    // fold_placeholder, clear_scope, overlay, overlay_heading, input_log:
    // none; then the window.
    command.extend(std::iter::repeat_n(String::new(), 5));
    command.push(paste_enter_window_ms.to_string());
    let window_id = h.open(command, Some(name)).await;
    common::wait_until_status(
        client,
        &window_id,
        WindowStatus::WaitingInput,
        Duration::from_secs(10),
    )
    .await;
    window_id
}

/// A receiver that reads an Enter shortly after a paste as a line break (as
/// Codex does): the definition's `submit_delay_ms` is the one thing that
/// separates the body from its submit key, so the delivery must leave it.
/// The window is the receiver's own contract in time, not a way to line
/// events up.
#[tokio::test]
async fn the_submit_key_waits_the_submit_delay_after_the_body_so_a_pasting_receiver_takes_it() {
    let dir = unique_temp_dir("gate-submit-delay");
    let h = Harness::start("gate-submit-delay", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_with_paste_enter_window(
        &h,
        &mut client,
        &dir,
        "submitdelay",
        "submitdelay-marker",
        500,
        300,
    )
    .await;

    let judgment = entry(&h, &window_id)
        .submit_message("BODY", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level");

    match judgment.placement {
        SubmitPlacement::Placed(_) => {}
        other => panic!("an empty composer must take the body, got {other:?}"),
    }
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED BODY", 1).await,
        1,
        "the submit key must come after the receiver's paste window, so it submits"
    );
}

/// The case above means something only if the receiver does take an Enter
/// inside its paste window as a line break: here no submit delay outlasts it.
#[tokio::test]
async fn a_submit_key_inside_the_receivers_paste_window_leaves_the_body_unsubmitted() {
    let dir = unique_temp_dir("gate-inside-paste-window");
    let h = Harness::start("gate-inside-paste-window", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_gate_window_with_paste_enter_window(
        &h,
        &mut client,
        &dir,
        "insidewindow",
        "insidewindow-marker",
        120,
        600_000,
    )
    .await;

    entry(&h, &window_id)
        .submit_message("BODY", None, SendStage::BeforeSubmitKey, None)
        .await
        .expect("write must not fail at the PTY level");
    // Typed after the submit key, so once it shows, the submit key was read.
    entry(&h, &window_id)
        .write_pty_input(b"x", InputOrigin::Interactive)
        .await
        .expect("the trailing keystroke must not fail");
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body("BODY x".to_string()),
        Duration::from_secs(10),
    )
    .await;

    assert_eq!(count_marker(&h, &window_id, "SUBMITTED"), 0);
}
