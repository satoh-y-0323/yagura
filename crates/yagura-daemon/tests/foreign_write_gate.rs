#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: another writer's bytes that reach a window while a
//! delivery has its body in the composer go to the child behind the take-back
//! of that body, and the delivery writes nothing after them. The window's
//! interrupt keys are the one exception: they go out as they are, with no
//! take-back in front of them, and which keys those are is the configuration's
//! `interrupt_bytes`, nothing else.
//!
//! The receiver is `submit_child` in `stall-until` mode: it stops reading as
//! soon as the body's paste begins, so the other writer's bytes queue behind
//! the delivery's own, and it reads on only when the test says so. What is
//! asserted is the child's input log, cut at a marker written after
//! everything else (`M` and an erase, which leave the composer as it was).

mod common;

use std::path::{Path, PathBuf};

use common::{
    BODY, CLEAR, CTRL_C, HANG_GUARD, Harness, MARKER, SUBMIT_CHILD_PROBE, SubmitDefinitionInput,
    after_the_body, approver_base_config, gate_config, read_composer_now, settled_marker_count,
    take_back_in_front_of, unique_temp_dir, wait_until_composer_is, wait_until_input_log_contains,
    wait_until_status, window_entry, write_submit_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::control::interrupt_window;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::pty::PtyResult;
use yagura_daemon::window::{
    InputOrigin, NotPlaced, NotPlacedReason, SendStage, SubmitJudgment, SubmitPlacement,
    WindowEntry,
};
use yagura_protocol::WindowStatus;
use yagura_test_support::ipc::Connection;
use yagura_test_support::process;

/// A stalled receiver: its definition takes the body back with Ctrl-U, and
/// the child stops reading at the body's paste until `resume` exists.
struct Stalled {
    window_id: String,
    log: PathBuf,
    resume: PathBuf,
}

async fn open_stalled_window(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
) -> Stalled {
    let marker = format!("{name}-marker");
    let log = dir.join(format!("{name}-input.log"));
    let resume = dir.join(format!("{name}-resume"));
    write_submit_definition(
        dir,
        name,
        &marker,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 120,
            clear: "\\u0015",
            delivery_confirm_ms: 2000,
            resend_max: 0,
        },
    );
    let mut command = process::submit_child_command_with_composer(&marker, 0, 0, 0, 0);
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
        log.to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    let window_id = h.open(command, Some(name)).await;
    wait_until_status(client, &window_id, WindowStatus::WaitingInput, HANG_GUARD).await;
    Stalled {
        window_id,
        log,
        resume,
    }
}

/// Starts a delivery of [`BODY`] and returns once the child has stopped
/// reading at its paste: the body is out, and nothing written from here on
/// has been read yet.
async fn deliver_until_stalled(
    h: &Harness,
    window: &Stalled,
) -> tokio::task::JoinHandle<PtyResult<SubmitJudgment>> {
    let sender = window_entry(h, &window.window_id);
    let call = tokio::spawn(async move {
        sender
            .submit_message(BODY, None, SendStage::BeforeSubmitKey, None)
            .await
    });
    h.wait_for_scrollback(&window.window_id, b"STALL-BEGAN", HANG_GUARD)
        .await;
    call
}

/// Lets the child read on, writes [`MARKER`] behind everything, and returns
/// what the delivery answered and everything the child read before the marker.
async fn resume_and_collect(
    h: &Harness,
    window: &Stalled,
    call: tokio::task::JoinHandle<PtyResult<SubmitJudgment>>,
) -> (NotPlaced, Vec<u8>) {
    std::fs::write(&window.resume, b"").expect("let the receiver read on");
    window_entry(h, &window.window_id)
        .write_pty_input(MARKER, InputOrigin::Interactive)
        .await
        .expect("the marker must not fail");
    let mut read = wait_until_input_log_contains(&window.log, MARKER, HANG_GUARD).await;
    let at = read
        .windows(MARKER.len())
        .rposition(|w| w == MARKER)
        .expect("the log was returned holding the marker");
    read.truncate(at);
    let judgment = tokio::time::timeout(HANG_GUARD, call)
        .await
        .expect("the delivery must answer once another writer has written")
        .expect("the submit task must not panic")
        .expect("the call must not fail at the PTY level");
    let not_placed = match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed,
        other => panic!("another writer's bytes must end the delivery, got {other:?}"),
    };
    (not_placed, read)
}

/// Another writer's keystroke and its erase land while the body has not shown:
/// the body is taken back in front of them, so the composer ends up empty, and
/// the delivery neither probes nor submits after them.
#[tokio::test]
async fn a_foreign_write_while_the_body_waits_to_show_gets_the_body_taken_back_in_front_of_it() {
    let dir = unique_temp_dir("foreign-gate-erased");
    let h = Harness::start("foreign-gate-erased", gate_config(&dir)).await;
    let mut client = h.connect().await;
    let window = open_stalled_window(&h, &mut client, &dir, "erased").await;

    let call = deliver_until_stalled(&h, &window).await;
    window_entry(&h, &window.window_id)
        .write_pty_input(b"h\x7f", InputOrigin::Interactive)
        .await
        .expect("another writer's bytes must not fail");
    let (not_placed, read) = resume_and_collect(&h, &window, call).await;

    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(
        not_placed.envelope_remains,
        "the body was cut off before it showed, so the call cannot vouch that it is gone"
    );
    let (rest, run) = take_back_in_front_of(after_the_body(&read), b"h\x7f");
    assert!(
        run > 0,
        "the body's take-back must reach the child right in front of the other writer's bytes; \
         it read {read:?}"
    );
    assert!(
        !rest.contains(&b'\r') && !rest.contains(&CLEAR),
        "nothing but the take-back may stand between the body and the other writer's bytes; \
         it read {read:?}"
    );
    wait_until_composer_is(&h, &window.window_id, &ComposerReadout::Empty, HANG_GUARD).await;

    let judgment = tokio::time::timeout(
        HANG_GUARD,
        window_entry(&h, &window.window_id).submit_message(
            BODY,
            None,
            SendStage::BeforeSubmitKey,
            None,
        ),
    )
    .await
    .expect("the next delivery must not hang")
    .expect("the call must not fail at the PTY level");
    assert!(
        matches!(judgment.placement, SubmitPlacement::Placed(_)),
        "the same body sent again must be placed into the emptied composer, got {judgment:?}"
    );
    assert_eq!(
        settled_marker_count(&h, &window.window_id, &format!("SUBMITTED {BODY}"), 1).await,
        1,
        "the body must be submitted once, on its own"
    );
}

/// Another writer's keystroke that stays: the composer holds that keystroke
/// alone, with neither the body nor the probe left in front of it.
#[tokio::test]
async fn a_foreign_keystroke_is_all_the_composer_holds_once_the_body_is_taken_back_in_front_of_it()
{
    let dir = unique_temp_dir("foreign-gate-kept");
    let h = Harness::start("foreign-gate-kept", gate_config(&dir)).await;
    let mut client = h.connect().await;
    let window = open_stalled_window(&h, &mut client, &dir, "kept").await;

    let call = deliver_until_stalled(&h, &window).await;
    window_entry(&h, &window.window_id)
        .write_pty_input(b"h", InputOrigin::Interactive)
        .await
        .expect("another writer's byte must not fail");
    let (not_placed, read) = resume_and_collect(&h, &window, call).await;

    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    assert!(not_placed.envelope_remains);
    let (_, run) = take_back_in_front_of(after_the_body(&read), b"h");
    assert!(
        run > 0,
        "the body's take-back must reach the child right in front of the other writer's byte; \
         it read {read:?}"
    );
    wait_until_composer_is(
        &h,
        &window.window_id,
        &ComposerReadout::Body("h".to_string()),
        HANG_GUARD,
    )
    .await;
    let readout = read_composer_now(&h, &window.window_id).await;
    assert_eq!(
        readout,
        ComposerReadout::Body("h".to_string()),
        "neither the body nor the probe {SUBMIT_CHILD_PROBE:?} may be left beside the other \
         writer's byte"
    );
}

/// The window's interrupt keys from another writer land as they are: nothing
/// is put in front of them, and the delivery writes nothing after them.
async fn assert_interrupt_keys_go_bare(
    tag: &str,
    config: DaemonConfig,
    interrupt: impl AsyncFnOnce(&WindowEntry, &DaemonConfig),
) {
    let dir = unique_temp_dir(tag);
    let config = DaemonConfig {
        detection_dir: Some(dir.clone()),
        ..config
    };
    let h = Harness::start(tag, config.clone()).await;
    let mut client = h.connect().await;
    let window = open_stalled_window(&h, &mut client, &dir, "interrupt").await;

    let call = deliver_until_stalled(&h, &window).await;
    interrupt(&window_entry(&h, &window.window_id), &config).await;
    let (not_placed, read) = resume_and_collect(&h, &window, call).await;

    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    let after_body = after_the_body(&read);
    let (_, run) = take_back_in_front_of(after_body, &config.interrupt_bytes);
    assert_eq!(
        run, 0,
        "no take-back may go in front of the interrupt keys; the child read {read:?}"
    );
    assert!(
        !after_body.contains(&CLEAR) && !after_body.contains(&b'\r'),
        "neither a take-back nor a submit key may reach the child after the body; it read {read:?}"
    );
}

#[tokio::test]
async fn the_interrupt_keys_typed_during_a_delivery_go_bare_and_the_delivery_writes_nothing_after_them()
 {
    let config = approver_base_config();
    assert_eq!(
        config.interrupt_bytes,
        vec![CTRL_C],
        "the default interrupt keys are Ctrl-C"
    );
    assert_interrupt_keys_go_bare(
        "foreign-gate-typed-interrupt",
        config,
        async |entry: &WindowEntry, config: &DaemonConfig| {
            entry
                .write_pty_input(&config.interrupt_bytes, InputOrigin::Interactive)
                .await
                .expect("the typed interrupt keys must not fail");
        },
    )
    .await;
}

#[tokio::test]
async fn interrupt_window_during_a_delivery_goes_bare_and_the_delivery_writes_nothing_after_it() {
    assert_interrupt_keys_go_bare(
        "foreign-gate-interrupt-window",
        approver_base_config(),
        async |entry: &WindowEntry, config: &DaemonConfig| {
            interrupt_window(entry, config)
                .await
                .expect("interrupt_window must write its keys");
        },
    )
    .await;
}

/// Which keys go without a take-back is the configured `interrupt_bytes`,
/// matched whole: with two Ctrl-Cs configured, one Ctrl-C from another writer
/// is an ordinary write and gets the take-back, while `interrupt_window`'s two
/// do not.
#[tokio::test]
async fn which_keys_go_without_a_take_back_is_decided_by_the_configured_interrupt_keys() {
    let config = DaemonConfig {
        interrupt_bytes: vec![CTRL_C, CTRL_C],
        ..approver_base_config()
    };

    let dir = unique_temp_dir("foreign-gate-configured-ctrl-c");
    let h = Harness::start(
        "foreign-gate-configured-ctrl-c",
        DaemonConfig {
            detection_dir: Some(dir.clone()),
            ..config.clone()
        },
    )
    .await;
    let mut client = h.connect().await;
    let window = open_stalled_window(&h, &mut client, &dir, "single").await;
    let call = deliver_until_stalled(&h, &window).await;
    window_entry(&h, &window.window_id)
        .write_pty_input(&[CTRL_C], InputOrigin::Interactive)
        .await
        .expect("another writer's byte must not fail");
    let (not_placed, read) = resume_and_collect(&h, &window, call).await;
    assert_eq!(not_placed.reason, NotPlacedReason::Interrupted);
    let (_, run) = take_back_in_front_of(after_the_body(&read), &[CTRL_C]);
    assert!(
        run > 0,
        "a Ctrl-C that is not the configured interrupt keys gets the take-back in front of it; \
         the child read {read:?}"
    );

    assert_interrupt_keys_go_bare(
        "foreign-gate-configured-interrupt-window",
        config,
        async |entry: &WindowEntry, config: &DaemonConfig| {
            interrupt_window(entry, config)
                .await
                .expect("interrupt_window must write its keys");
        },
    )
    .await;
}
