#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: whether a `profile`-addressed `send_handoff` closes the
//! window it opened is decided by the whole delivery, not by its last
//! attempt. A delivery that never pressed the submit key leaves an envelope
//! behind if any one of its attempts did; a delivery that pressed it at
//! least once never tears its window down, whatever a later attempt reports.
//!
//! The opposite side, a delivery whose every attempt left nothing behind and
//! whose window is closed, is `profile_close_ii_...` in `composer_gate.rs`.

mod common;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use common::{
    Harness, SUBMIT_CHILD_PROBE, call_send_handoff_from_profile_within, empty_lookup,
    expect_handoff_sent, handoff_profile_config, policy, read_composer_now,
    send_handoff_from_profile, submit_child_profile_source_logging_input,
    submit_child_profile_source_with_detection_and_clear_scope, unique_temp_dir,
    wait_for_only_window, wait_until_input_log_contains,
    wait_until_the_handoff_budget_is_given_back, with_detection_name, write_profile,
    write_submit_definition_with_input_toml,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::screen::ComposerReadout;
use yagura_daemon::window::{InputOrigin, WindowEntry};
use yagura_protocol::{DaemonMessage, HandoffDelivery};
use yagura_test_support::process;

/// Every handoff envelope starts with this heading.
const ENVELOPE_HEADING: &str = "[yagura handoff]";

/// Upper bound on a `handoff_sent` reply: a profile launch plus every attempt
/// the definitions below allow.
const REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// A config resolving profiles from `profiles_dir` against `detection_dir`,
/// with `send_handoff` auto-approved so no approver connection is needed.
fn profile_config(profiles_dir: PathBuf, detection_dir: &Path) -> DaemonConfig {
    handoff_profile_config(
        Some(profiles_dir),
        empty_lookup(),
        detection_dir,
        None,
        Some(policy(&["send_handoff"])),
        30,
    )
}

/// The `profiles` / `detection` directory pair a case writes into, created
/// up front so a definition or profile write never fails on a missing parent.
fn profile_dirs(dir: &Path) -> (PathBuf, PathBuf) {
    let profiles_dir = dir.join("profiles");
    let detection_dir = dir.join("detection");
    std::fs::create_dir_all(&profiles_dir).expect("make profiles dir");
    std::fs::create_dir_all(&detection_dir).expect("make detection dir");
    (profiles_dir, detection_dir)
}

fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// Types one character into `window_id` and waits for its child to paint it.
/// A teardown is decided before the reply is sent, so a window being closed
/// could no longer paint it.
async fn assert_window_still_takes_input(h: &Harness, window_id: &str, why: &str) {
    entry(h, window_id)
        .write_pty_input(b"Y", InputOrigin::Interactive)
        .await
        .expect("type into the window");
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    loop {
        if let ComposerReadout::Body(text) = read_composer_now(h, window_id).await
            && text.ends_with('Y')
        {
            break;
        }
        assert!(tokio::time::Instant::now() < deadline, "{why}");
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let exit = entry(h, window_id).session().exit_status();
    assert!(exit.is_none(), "{why} (child exited: {exit:?})");
}

/// The end of a bracketed paste, as the fixture logs it.
const PASTE_END: &[u8] = b"\x1b[201~";

/// Typed into a window last; read back, it proves the child has read
/// everything written before it.
const SENTINEL: &[u8] = b"SENTINEL-AFTER";

/// Waits until the receiver has read a submit key after the envelope's paste
/// ended: the first attempt has pressed it. The hang guard only bounds a
/// failure.
async fn wait_for_the_submit_key_after_the_envelope(input_log: &Path) {
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    loop {
        let read = std::fs::read(input_log).unwrap_or_default();
        if let Some(end) = read
            .windows(PASTE_END.len())
            .position(|window| window == PASTE_END)
            && read[end..].contains(&b'\r')
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the submit key never followed the envelope"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

// ---------------------------------------------------------------------
// Never pressed: an envelope an earlier attempt left behind still counts
// ---------------------------------------------------------------------

const LATE_SUBMIT_DELAY_MS: u64 = 120;

/// The window two attempts are made against: it paints nothing from the
/// envelope's paste on, and has no way to take a write back. Another writer
/// ends the first attempt with its envelope left in the composer; the
/// requester leaving ends the second before it writes anything of its own.
#[tokio::test]
async fn an_envelope_left_by_an_earlier_attempt_keeps_the_window_open_after_a_later_attempt_leaves_nothing()
 {
    let dir = unique_temp_dir("envelope-remains-late");
    let (profiles_dir, detection_dir) = profile_dirs(&dir);
    let input_log = dir.join("child-input.log");
    write_submit_definition_with_input_toml(
        &detection_dir,
        "remains-late",
        "remains-late-marker",
        &format!(
            "bracketed_paste = true\n\
             ready_hold_ms = 0\n\
             submit_delay_ms = {LATE_SUBMIT_DELAY_MS}\n\
             delivery_confirm_ms = 2000\n\
             resend_max = 1\n\
             probe = \"{SUBMIT_CHILD_PROBE}\"\n"
        ),
    );
    write_profile(
        &profiles_dir,
        "remains-late",
        &submit_child_profile_source_logging_input(
            "Remains Late",
            "remains-late-marker",
            "remains-late",
            "frozen:",
            &input_log,
        ),
    );
    write_profile(
        &profiles_dir,
        "budget-probe",
        &submit_child_profile_source_with_detection_and_clear_scope(
            "Budget Probe",
            "remains-late-marker",
            "remains-late",
            "-",
        ),
    );

    // One daemon-wide handoff at a time, so the budget coming back tells the
    // test the first handoff has decided its window's fate.
    let config = DaemonConfig {
        max_open_handoffs: 1,
        ..profile_config(profiles_dir, &detection_dir)
    };
    let h = Harness::start("envelope-remains-late", config).await;
    let mut client = h.connect().await;
    send_handoff_from_profile(
        &mut client,
        "remains-late",
        None,
        None,
        "src/foo.rs",
        "look",
    )
    .await;
    let window_id = wait_for_only_window(&h, REPLY_TIMEOUT).await;
    h.wait_for_scrollback(&window_id, b"PASTE 1", REPLY_TIMEOUT)
        .await;

    entry(&h, &window_id)
        .write_pty_input(b"Q", InputOrigin::Interactive)
        .await
        .expect("another writer's byte must not fail");
    let after_other_writer = entry(&h, &window_id).write_seq();
    let deadline = tokio::time::Instant::now() + REPLY_TIMEOUT;
    while entry(&h, &window_id).write_seq() == after_other_writer {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the second attempt never wrote its probe"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    drop(client);

    wait_until_the_handoff_budget_is_given_back(&h, "budget-probe", REPLY_TIMEOUT).await;
    assert!(
        entry(&h, &window_id).session().exit_status().is_none(),
        "the first attempt's envelope is still in the receiver's composer, so a later attempt \
         that left nothing of its own must not get the window closed under it"
    );
    entry(&h, &window_id)
        .write_pty_input(SENTINEL, InputOrigin::Interactive)
        .await
        .expect("the sentinel must reach a window that was kept open");
    let read = wait_until_input_log_contains(&input_log, SENTINEL, REPLY_TIMEOUT).await;
    assert_eq!(
        read.windows(ENVELOPE_HEADING.len())
            .filter(|window| *window == ENVELOPE_HEADING.as_bytes())
            .count(),
        1,
        "the envelope must have been written once, by the first attempt: {read:?}"
    );
    let envelope_end = read
        .windows(PASTE_END.len())
        .position(|window| window == PASTE_END)
        .expect("the envelope must have reached the receiver");
    assert!(
        !read[envelope_end..].contains(&b'\r'),
        "the submit key must never have been pressed: {read:?}"
    );
}

// ---------------------------------------------------------------------
// Pressed once: a later declined attempt does not reopen teardown
// ---------------------------------------------------------------------

/// Long enough for the test to see the envelope land before the submit key
/// follows it.
const PRESSED_SUBMIT_DELAY_MS: u64 = 1000;

/// The window the test has to slip a foreign byte in after the submit key and
/// before the resend re-reads the composer; the write count below tells if
/// the byte came too late.
const PRESSED_DELIVERY_CONFIRM_MS: u64 = 10_000;

/// Swallows every submit key after a paste, so the receiver never reacts and
/// the delivery goes on to resend.
const PRESSED_PASTE_GUARD_KEYS: u64 = u64::MAX;

#[tokio::test]
async fn a_delivery_that_pressed_the_submit_key_keeps_the_window_open_when_a_later_attempt_is_declined()
 {
    let dir = unique_temp_dir("envelope-remains-pressed");
    let (profiles_dir, detection_dir) = profile_dirs(&dir);
    let input_log = dir.join("child-input.log");
    write_submit_definition_with_input_toml(
        &detection_dir,
        "remains-pressed",
        "remains-pressed-marker",
        &format!(
            "bracketed_paste = true\n\
             ready_hold_ms = 0\n\
             submit_delay_ms = {PRESSED_SUBMIT_DELAY_MS}\n\
             delivery_confirm_ms = {PRESSED_DELIVERY_CONFIRM_MS}\n\
             resend_max = 1\n\
             probe = \"{SUBMIT_CHILD_PROBE}\"\n"
        ),
    );
    // The fixture logs what it reads; a profile's argv admits no empty
    // element, so `-` stands for "none" in the slots before the log.
    let mut command = process::submit_child_command_with_composer(
        "remains-pressed-marker",
        0,
        PRESSED_PASTE_GUARD_KEYS,
        0,
        0,
    );
    command.extend(std::iter::repeat_n("-".to_string(), 4));
    command.push(
        input_log
            .to_str()
            .expect("the input log path must be valid UTF-8")
            .to_string(),
    );
    write_profile(
        &profiles_dir,
        "remains-pressed",
        &with_detection_name(
            &process::profile_source("Remains Pressed", &command),
            "remains-pressed",
        ),
    );

    let h = Harness::start(
        "envelope-remains-pressed",
        profile_config(profiles_dir, &detection_dir),
    )
    .await;
    let mut client = h.connect().await;

    let slip_in_foreign_byte = async {
        let window_id = wait_for_only_window(&h, REPLY_TIMEOUT).await;
        wait_for_the_submit_key_after_the_envelope(&input_log).await;
        // Another writer's byte after the submit key: the resend can no
        // longer vouch for the composer as its own and declines.
        entry(&h, &window_id)
            .write_pty_input(b"Z", InputOrigin::Message)
            .await
            .expect("foreign write must not fail");
    };
    let (reply, ()) = tokio::join!(
        call_send_handoff_from_profile_within(
            &mut client,
            "remains-pressed",
            None,
            None,
            "src/foo.rs",
            "look",
            REPLY_TIMEOUT,
        ),
        slip_in_foreign_byte,
    );
    let reply: DaemonMessage = reply;
    let (window_id, opened, delivery) = expect_handoff_sent(reply);
    let delivery: HandoffDelivery = delivery;
    assert!(opened);
    assert_eq!(
        delivery,
        HandoffDelivery::Unconfirmed,
        "the submit key was pressed once and the receiver never reacted"
    );
    let read = std::fs::read(&input_log).unwrap_or_default();
    let envelope_end = read
        .windows(PASTE_END.len())
        .position(|window| window == PASTE_END)
        .expect("the envelope must have reached the receiver");
    assert_eq!(
        read[envelope_end..]
            .iter()
            .filter(|byte| **byte == b'\r')
            .count(),
        1,
        "the submit key was pressed once: a resend over a composer another writer's byte \
         followed would have pressed it again, the receiver read {read:?}"
    );
    assert_window_still_takes_input(
        &h,
        &window_id,
        "an envelope that was submitted may already be in front of the receiver, so a later \
         declined attempt must not get the window closed",
    )
    .await;
    match read_composer_now(&h, &window_id).await {
        ComposerReadout::Body(text) => assert!(
            text.contains(ENVELOPE_HEADING),
            "the swallowed envelope must still be in the receiver's composer: {text:?}"
        ),
        other => panic!("expected the envelope still in the composer, got {other:?}"),
    }
}
