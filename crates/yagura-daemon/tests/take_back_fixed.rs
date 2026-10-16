#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the window layer's judgment once a fixed `clear`
//! sequence is the only way to take a write back (`InputProfile::take_back`
//! returning `TakeBack::Fixed`), against `submit_child` launched in
//! composer-draw mode.
//!
//! Every case here drives a real `WindowRegistry` against a body write that
//! shows nothing, until the requester of the delivery leaves: the window
//! layer then has to write the take-back sequence and decide what the
//! composer says once it does. The contrast -- a stalled receiver whose
//! take-back lands, so the next delivery writes the body once and whole -- is
//! `take_back_line_clear.rs`'s
//! `a_second_attempt_after_a_stalled_take_back_writes_the_body_only_once`.
//! A retry that
//! genuinely still holds its own confirmed envelope (never having gone
//! through a take-back at all) is `composer_gate.rs`'s
//! `row2_resends_the_submit_key_only_when_its_own_envelope_is_still_on_screen_unmodified`.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    Harness, SUBMIT_CHILD_PROBE, approver_base_config, composer_section_toml,
    let_every_clock_bound_run_out, open_submit_child_window_with_clear_scope, read_composer_now,
    settled_marker_count, unique_temp_dir, wait_until_composer_is, wait_until_status,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{
    ComposerResidue, InputOrigin, NotPlaced, NotPlacedReason, SendStage, SubmitJudgment,
    SubmitPlacement, WindowEntry,
};
use yagura_protocol::WindowStatus;
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

/// A [`DaemonConfig`] whose detection directory is empty: every test writes
/// its own definition under it.
fn detection_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        ..approver_base_config()
    }
}

/// The window entry backing `window_id`.
fn entry(h: &Harness, window_id: &str) -> std::sync::Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// Must exceed the definition's fixed `confirm_ms` (30), or the definition
/// fails to load.
const SUBMIT_DELAY_MS: u64 = 120;

/// The fixture paints as soon as it has something to show: which path a case
/// takes is decided by the order of events (`held:<scope>` keeps everything
/// unpainted until the take-back's own clear arrives), not by a delay.
const PAINT_AT_ONCE_MS: u64 = 0;

/// The `[input]` table every case in this file shares: a fixed `clear`
/// sequence (Ctrl-U).
fn fixed_clear_input_toml() -> String {
    format!(
        "bracketed_paste = true\n\
         ready_hold_ms = 0\n\
         submit_delay_ms = {SUBMIT_DELAY_MS}\n\
         clear = \"\\u0015\"\n\
         delivery_confirm_ms = 2000\n\
         resend_max = 2\n\
         probe = \"{SUBMIT_CHILD_PROBE}\"\n"
    )
}

/// A delivery of `body` whose requester can leave, started and held until the
/// body has reached the child. The window layer is then waiting on a body that
/// shows nothing: only the requester leaving, or another writer, ends that.
async fn start_delivery(
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
    h.wait_for_scrollback(window_id, b"PASTE 1", Duration::from_secs(10))
        .await;
    (leave, call)
}

/// Lets the requester of a [`start_delivery`] leave and returns the judgment
/// the window layer reaches once its take-back is done.
async fn leave_and_take_the_judgment(
    leave: tokio::sync::watch::Sender<bool>,
    call: tokio::task::JoinHandle<yagura_daemon::pty::PtyResult<SubmitJudgment>>,
) -> SubmitJudgment {
    leave.send(true).expect("the call holds its receiver");
    call.await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level")
}

/// Polls `input_log` until it holds `needle`: the hang guard only bounds a
/// failure, it never decides an outcome.
async fn wait_for_child_input_containing(input_log: &Path, needle: u8) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        if std::fs::read(input_log)
            .unwrap_or_default()
            .contains(&needle)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the child never read byte 0x{needle:02x}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

#[tokio::test]
async fn a_clear_byte_typed_as_caret_u_literals_leaves_a_residue_that_never_matches_the_body_so_the_submit_key_is_not_pressed()
 {
    let dir = unique_temp_dir("takeback-literal");
    let h = Harness::start("takeback-literal", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "literal",
        "literal-marker",
        &fixed_clear_input_toml(),
        PAINT_AT_ONCE_MS,
        "held:literal-clear",
    )
    .await;
    let body = "hi";

    let (leave, call) = start_delivery(&h, &window_id, body).await;
    let result: SubmitJudgment = leave_and_take_the_judgment(leave, call).await;

    let not_placed: NotPlaced = match result.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!(
            "a residue reading back as the body plus the clear key's own literal caret-u \
             must not be mistaken for the body, got {other:?}"
        ),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::NotReflected);
    assert!(
        not_placed.envelope_remains,
        "the clear byte typed two visible characters instead of erasing anything, so \
         something of this call's own write is still sitting in the composer"
    );
    assert_eq!(
        result.residue_write_seq, None,
        "a take-back whose residue could not be confirmed must not hand a follow-up a \
         baseline to match against"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0,
        "a mismatched residue must never reach the receiver as a submission"
    );
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert!(
            text.starts_with(body) && text.contains("^U"),
            "the clear byte's own literal characters must actually have reached the \
             child, after the body, got {text:?}"
        ),
        other => panic!("expected the literal caret-u residue on screen, got {other:?}"),
    }
}

#[tokio::test]
async fn a_clear_byte_that_only_erases_the_last_character_leaves_a_residue_that_never_matches_the_body_so_the_submit_key_is_not_pressed()
 {
    let dir = unique_temp_dir("takeback-partial");
    let h = Harness::start("takeback-partial", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "partial",
        "partial-marker",
        &fixed_clear_input_toml(),
        PAINT_AT_ONCE_MS,
        "held:partial-clear",
    )
    .await;
    let body = "abcdefghijklmnopqrstuvwxyz";

    let (leave, call) = start_delivery(&h, &window_id, body).await;
    let result: SubmitJudgment = leave_and_take_the_judgment(leave, call).await;

    let not_placed: NotPlaced = match result.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!(
            "a clear that empties nothing must not be mistaken for a take-back that \
             worked, got {other:?}"
        ),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::NotReflected);
    assert!(
        not_placed.envelope_remains,
        "the clear byte only erased the last character, so this call's own write is \
         still sitting in the composer"
    );
    assert_eq!(
        result.residue_write_seq, None,
        "a take-back whose residue could not be confirmed must not hand a follow-up a \
         baseline to match against"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0,
        "a mismatched residue must never reach the receiver as a submission"
    );
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => {
            let remnant = text.strip_suffix(SUBMIT_CHILD_PROBE).unwrap_or(&text);
            assert!(
                !remnant.is_empty() && body.starts_with(remnant),
                "the clear bytes erased one character each, so part of the body is still on \
                 screen, got {text:?}"
            );
        }
        other => panic!("expected the partially-cleared residue on screen, got {other:?}"),
    }
}

#[tokio::test]
async fn a_retry_that_carries_forward_what_a_declined_takeback_actually_returned_does_not_press_on_the_leftover_residue()
 {
    let dir = unique_temp_dir("takeback-retry");
    let h = Harness::start("takeback-retry", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_submit_child_window_with_clear_scope(
        &h,
        &mut client,
        &dir,
        "retry",
        "retry-marker",
        &fixed_clear_input_toml(),
        PAINT_AT_ONCE_MS,
        "held:literal-clear",
    )
    .await;
    let body = "hi";

    let (leave, call) = start_delivery(&h, &window_id, body).await;
    let first: SubmitJudgment = leave_and_take_the_judgment(leave, call).await;
    assert_eq!(
        first.residue_write_seq, None,
        "a take-back whose residue could not be confirmed must hand a follow-up no baseline \
         to match against"
    );

    let write_seq_before_retry = entry(&h, &window_id).write_seq();
    let residue = first
        .residue_write_seq
        .map(|write_seq| ComposerResidue { body, write_seq });
    let second: SubmitJudgment = tokio::time::timeout(
        Duration::from_secs(20),
        entry(&h, &window_id).submit_message(body, residue, SendStage::BeforeSubmitKey, None),
    )
    .await
    .expect("a retry over a residue it cannot vouch for must answer, not wait")
    .expect("write must not fail at the PTY level");

    match second.placement {
        SubmitPlacement::NotPlaced(_) => {}
        other => panic!(
            "a retry with no confirmed baseline for its own envelope must not press the \
             submit key, got {other:?}"
        ),
    }
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        write_seq_before_retry,
        "the retry must not write anything on top of the residue it could not vouch for"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0,
        "nothing must ever reach the receiver as a submission"
    );
}

/// A receiver that never shows anything once the body arrives: the take-back's
/// probe never shows either, so nothing says whether the take-back was handled.
/// The window layer keeps its turn and waits; another writer's byte ends the
/// wait, and the submit key is never pressed.
#[tokio::test]
async fn a_takeback_whose_probe_never_shows_is_waited_on_until_another_writer_ends_it() {
    let dir = unique_temp_dir("takeback-neverreflects");
    let log = dir.join("child-input.log");
    let h = Harness::start("takeback-neverreflects", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_window_with_clear_raised_approval(
        &h,
        &mut client,
        &dir,
        "never",
        "never-marker",
        &fixed_clear_input_toml(),
        PAINT_AT_ONCE_MS,
        "frozen:",
        None,
        Some(&log),
    )
    .await;
    let body = "hi";

    let (leave, call) = start_delivery(&h, &window_id, body).await;
    leave.send(true).expect("the call holds its receiver");
    wait_for_child_input_containing(&log, 0x15).await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a take-back whose probe never shows is waited on, not given up on by a clock"
    );
    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Message)
        .await
        .expect("the other writer's byte must not fail");
    let result: SubmitJudgment = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");

    let not_placed: NotPlaced = match result.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!(
            "a write and its own clear that never reflect at all must not press, got \
             {other:?}"
        ),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        not_placed.envelope_remains,
        "a receiver that never shows the probe gives no sign it handled the body or the \
         take-back, so something of this call's own write may remain"
    );
    assert!(
        !std::fs::read(&log).unwrap_or_default().contains(&b'\r'),
        "the submit key must never have been written"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0
    );
}

/// The heading of the dialog `submit_child` draws beside its composer once a
/// `clear` byte is read (its `clear:beside` overlay), matched by
/// [`write_definition_with_clear_raised_approval`]'s approval rule.
const CLEAR_RAISED_APPROVAL_HEADING: &str = "TAKEBACK-APPROVAL";

/// A rules-only definition matching
/// [`open_submit_child_window_with_clear_scope`]'s own shape, with one extra
/// rule: the dialog the fixture draws above its composer in answer to a
/// `clear` byte reads as an approval prompt, so a take-back's own clear write
/// can put the screen into a state that stops accepting input. Status rules
/// never read the composer's own text, so the rule matches the dialog's
/// heading drawn outside it.
fn write_definition_with_clear_raised_approval(
    dir: &Path,
    name: &str,
    marker: &str,
    input_toml: &str,
) {
    let toml = format!(
        r#"schema_version = 1
name = "{name}"
bottom_line_count = 5

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_approval"
pattern = '{CLEAR_RAISED_APPROVAL_HEADING}'
priority = 30
region = "bottom"

[[rules]]
status = "running"
pattern = '(SUBMITTED)|(REACT)'
priority = 20
region = "bottom"

[[rules]]
status = "waiting_input"
pattern = '{marker}'
priority = 10
region = "bottom"

[input]
{input_toml}
"#
    );
    std::fs::write(
        dir.join(format!("{name}.toml")),
        format!("{toml}{}", composer_section_toml()),
    )
    .expect("write detection definition file");
}

/// Opens `submit_child` under [`write_definition_with_clear_raised_approval`]
/// with `clear_scope`; `clear_overlay` (`clear:beside`, say) has the fixture
/// draw an overlay headed by the approval heading once the first `clear` byte
/// is read.
#[allow(clippy::too_many_arguments)]
async fn open_window_with_clear_raised_approval(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    marker: &str,
    input_toml: &str,
    reflect_delay_ms: u64,
    clear_scope: &str,
    clear_overlay: Option<&str>,
    input_log: Option<&Path>,
) -> String {
    write_definition_with_clear_raised_approval(dir, name, marker, input_toml);
    let mut command = process::submit_child_command_with_clear_scope(
        marker,
        0,
        0,
        0,
        reflect_delay_ms,
        clear_scope,
    );
    if clear_overlay.is_some() || input_log.is_some() {
        command.push(clear_overlay.unwrap_or_default().to_string());
        command.push(if clear_overlay.is_some() {
            CLEAR_RAISED_APPROVAL_HEADING.to_string()
        } else {
            String::new()
        });
    }
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

#[tokio::test]
async fn a_takebacks_own_clear_write_that_lands_beside_an_approval_dialog_leaves_no_residue_baseline_for_a_later_attempt()
 {
    let dir = unique_temp_dir("takeback-dialog");
    let h = Harness::start("takeback-dialog", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let body = "hi";

    // Contrast: the very same rules, but with a real clear sequence that
    // raises no dialog -- the take-back succeeds and nothing of this call's
    // own write remains.
    let empty_window_id = open_window_with_clear_raised_approval(
        &h,
        &mut client,
        &dir,
        "dialog-empties",
        "dialog-empties-marker",
        &fixed_clear_input_toml(),
        PAINT_AT_ONCE_MS,
        "held:",
        None,
        None,
    )
    .await;
    let (leave, call) = start_delivery(&h, &empty_window_id, body).await;
    let empty_result: SubmitJudgment = leave_and_take_the_judgment(leave, call).await;
    let empty_not_placed: NotPlaced = match empty_result.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!(
            "a real clear that actually empties the composer must not press either, got \
             {other:?}"
        ),
    };
    assert_eq!(empty_not_placed.reason, NotPlacedReason::NotReflected);
    assert!(
        !empty_not_placed.envelope_remains,
        "a real clear that empties the composer leaves nothing behind"
    );

    // The take-back's own clear write lands beside an approval dialog:
    // `literal-clear` types the same two characters an unbound Ctrl-U binding
    // would, and the fixture draws its dialog above the composer that still
    // holds them. The screen does not accept a write for as long as the dialog
    // stays, so the window layer waits; another writer's byte ends the wait.
    let window_id = open_window_with_clear_raised_approval(
        &h,
        &mut client,
        &dir,
        "dialog",
        "dialog-marker",
        &fixed_clear_input_toml(),
        PAINT_AT_ONCE_MS,
        "held:literal-clear",
        Some("clear:beside"),
        None,
    )
    .await;
    let (leave, call) = start_delivery(&h, &window_id, body).await;
    leave.send(true).expect("the call holds its receiver");
    h.wait_for_scrollback(
        &window_id,
        CLEAR_RAISED_APPROVAL_HEADING.as_bytes(),
        Duration::from_secs(10),
    )
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a screen that stopped accepting input right after the take-back's own clear write \
         is waited on, not given up on by a clock"
    );
    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Message)
        .await
        .expect("the other writer's byte must not fail");
    let result: SubmitJudgment = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    let not_placed: NotPlaced = match result.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!(
            "a screen that stopped accepting input right after the take-back's own clear \
             write must not press, got {other:?}"
        ),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        not_placed.envelope_remains,
        "the clear key's own characters are still sitting in the composer, unconfirmed"
    );
    assert_eq!(
        result.residue_write_seq, None,
        "a take-back that could not be confirmed must not hand a follow-up a baseline to \
         match against the dialog's own residue"
    );

    let write_seq_before_retry = entry(&h, &window_id).write_seq();
    let residue = result
        .residue_write_seq
        .map(|write_seq| ComposerResidue { body, write_seq });
    let retry: SubmitJudgment = tokio::time::timeout(
        Duration::from_secs(20),
        entry(&h, &window_id).submit_message(body, residue, SendStage::BeforeSubmitKey, None),
    )
    .await
    .expect("a retry over a screen that does not accept a write must answer, not wait")
    .expect("write must not fail at the PTY level");
    match retry.placement {
        SubmitPlacement::NotPlaced(_) => {}
        other => panic!(
            "a retry over a screen that never confirmed its own envelope must not press, got \
             {other:?}"
        ),
    }
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        write_seq_before_retry,
        "the retry must not write anything on top of the dialog's own residue"
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0
    );
}

#[tokio::test]
async fn a_probe_that_shows_alone_while_the_screen_reads_as_approval_is_not_erased_until_it_accepts_input()
 {
    let dir = unique_temp_dir("takeback-probe-gate");
    let h = Harness::start("takeback-probe-gate", detection_config(&dir)).await;
    let mut client = h.connect().await;
    let body = "hi";
    let approval_gone = dir.join("approval-gone");
    // Drawn on the take-back's own clear, headed like an approval prompt, but
    // swallowing no input: the probe still lands in the composer and shows
    // alone while the screen reads as not accepting a write.
    let overlay = format!("clear:working:until|{}", approval_gone.display());
    let window_id = open_window_with_clear_raised_approval(
        &h,
        &mut client,
        &dir,
        "probe-gate",
        "probe-gate-marker",
        &fixed_clear_input_toml(),
        PAINT_AT_ONCE_MS,
        "held:",
        Some(&overlay),
        None,
    )
    .await;

    let (leave, call) = start_delivery(&h, &window_id, body).await;
    leave.send(true).expect("the call holds its receiver");
    h.wait_for_scrollback(
        &window_id,
        CLEAR_RAISED_APPROVAL_HEADING.as_bytes(),
        Duration::from_secs(10),
    )
    .await;
    wait_until_composer_is(
        &h,
        &window_id,
        &ComposerReadout::Body(SUBMIT_CHILD_PROBE.to_string()),
        Duration::from_secs(10),
    )
    .await;
    let_every_clock_bound_run_out().await;
    assert!(
        !call.is_finished(),
        "a probe that shows alone on a screen that does not accept a write is waited on, not \
         given up on by a clock"
    );
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Body(SUBMIT_CHILD_PROBE.to_string()),
        "the probe left unanswered is not erased while the screen still reads as approval"
    );

    std::fs::write(&approval_gone, b"").expect("create the approval gate file");
    let after: SubmitJudgment = call
        .await
        .expect("the submit task must not panic")
        .expect("write must not fail at the PTY level");
    let not_placed: NotPlaced = match after.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("the requester left, so nothing may be placed, got {other:?}"),
    };
    assert_eq!(not_placed.reason, NotPlacedReason::NotReflected);
    assert!(
        !not_placed.envelope_remains,
        "once the screen accepts input the leftover probe is erased and this attempt's own \
         take-back is confirmed"
    );
    assert_eq!(
        read_composer_now(&h, &window_id).await,
        ComposerReadout::Empty
    );
    assert_eq!(
        settled_marker_count(&h, &window_id, "SUBMITTED", 0).await,
        0
    );
}
