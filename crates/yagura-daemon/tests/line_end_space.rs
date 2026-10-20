#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: every judged write ends the text the daemon puts in a
//! window's composer with one ASCII space of the daemon's own, so the word in
//! front of the cursor is never the caller's text when the probe and the
//! submit key follow. A receiver's completion popup opens on that word.
//!
//! Each case drives a real `IpcServer` against `submit_child`, which logs
//! every byte it reads; what is asserted is what the child read.

mod common;

use std::path::{Path, PathBuf};

use common::{
    HANG_GUARD, Harness, SUBMIT_CHILD_PROBE, SubmitDefinitionInput, approver_base_config, policy,
    read_response, send, unique_temp_dir, wait_until_input_log_contains,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::handoff::{EnvelopeFrom, compose_envelope};
use yagura_protocol::{ClientMessage, DaemonMessage};
use yagura_test_support::ipc::Connection;

const PASTE_START: &[u8] = b"\x1b[200~";
const PASTE_END: &[u8] = b"\x1b[201~";
const ERASE: u8 = 0x7f;
const SUBMIT_KEY: u8 = b'\r';

fn config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["open_window", "send_message", "send_handoff"])),
        ..approver_base_config()
    }
}

struct Receiver {
    window_id: String,
    log: PathBuf,
}

async fn open_receiver(
    h: &Harness,
    client: &mut Connection,
    dir: &Path,
    name: &str,
    bracketed_paste: bool,
) -> Receiver {
    let log = dir.join(format!("{name}-input.log"));
    let window_id = common::open_submit_child_window_with_composer_logging_input(
        h,
        client,
        dir,
        name,
        &format!("{name}-marker"),
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste,
            ready_hold_ms: 0,
            submit_delay_ms: 60,
            clear: "\\u0015",
            delivery_confirm_ms: 0,
            resend_max: 0,
        },
        0,
        &log,
    )
    .await;
    Receiver { window_id, log }
}

/// The probe, its erase and the submit key: the last bytes of a judged write.
/// The confirming probe in front of the body is followed by the body, not by
/// the submit key, so the first occurrence is the write's end.
fn write_end() -> Vec<u8> {
    [SUBMIT_CHILD_PROBE.as_bytes(), &[ERASE, SUBMIT_KEY]].concat()
}

/// Everything the child read up to the end of the judged write.
async fn read_through_the_write(receiver: &Receiver, end: &[u8]) -> Vec<u8> {
    let mut read = wait_until_input_log_contains(&receiver.log, end, HANG_GUARD).await;
    let at = read
        .windows(end.len())
        .position(|window| window == end)
        .expect("the log was returned holding the write's end");
    read.truncate(at + end.len());
    read
}

/// The contents of every bracketed paste in `read`, each paired with what the
/// child read right after its end.
fn pastes(read: &[u8]) -> Vec<(&[u8], &[u8])> {
    let mut found = Vec::new();
    let mut rest = read;
    while let Some(start) = rest
        .windows(PASTE_START.len())
        .position(|window| window == PASTE_START)
    {
        let inner = &rest[start + PASTE_START.len()..];
        let end = inner
            .windows(PASTE_END.len())
            .position(|window| window == PASTE_END)
            .unwrap_or_else(|| panic!("a paste was never closed: {read:?}"));
        let after = &inner[end + PASTE_END.len()..];
        found.push((&inner[..end], after));
        rest = after;
    }
    found
}

/// Sends `text` as `send_message` and returns once the daemon has answered
/// that it was written.
async fn send_message(client: &mut Connection, window_id: &str, text: &str) {
    send(
        client,
        &ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: text.to_string(),
        },
    )
    .await;
    match read_response(client, HANG_GUARD).await {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected text_sent, got {other:?}"),
    }
}

/// Asserts the child read exactly one paste, holding `expected`, with the
/// probe right after its end.
fn assert_one_paste_of(read: &[u8], expected: &[u8]) {
    let found = pastes(read);
    assert_eq!(
        found.len(),
        1,
        "the body must be pasted exactly once; the child read {read:?}"
    );
    let (contents, after) = found[0];
    assert_eq!(
        contents,
        expected,
        "the paste must hold the body followed by the daemon's one space, inside the paste's \
         end; it held {:?}",
        String::from_utf8_lossy(contents)
    );
    assert!(
        after.starts_with(SUBMIT_CHILD_PROBE.as_bytes()),
        "the probe must come right after the paste's end; the child read {read:?}"
    );
}

#[tokio::test]
async fn a_bracketed_send_message_body_ends_with_the_daemons_space_inside_the_paste() {
    let dir = unique_temp_dir("line-end-space-paste");
    let h = Harness::start("line-end-space-paste", config(&dir)).await;
    let mut client = h.connect().await;
    let receiver = open_receiver(&h, &mut client, &dir, "paste", true).await;

    send_message(&mut client, &receiver.window_id, "please review @src\r").await;

    let end = [PASTE_END, &write_end()].concat();
    let read = read_through_the_write(&receiver, &end).await;
    assert_one_paste_of(&read, b"please review @src ");
}

#[tokio::test]
async fn an_unbracketed_send_message_body_ends_with_the_daemons_space_before_the_probe() {
    let dir = unique_temp_dir("line-end-space-plain");
    let h = Harness::start("line-end-space-plain", config(&dir)).await;
    let mut client = h.connect().await;
    let receiver = open_receiver(&h, &mut client, &dir, "plain", false).await;

    send_message(&mut client, &receiver.window_id, "please review @src\r").await;

    let end = write_end();
    let read = read_through_the_write(&receiver, &end).await;
    assert!(
        !read.windows(PASTE_START.len()).any(|w| w == PASTE_START),
        "a definition without bracketed paste must get no paste; the child read {read:?}"
    );
    let before_the_probe = &read[..read.len() - end.len()];
    assert!(
        before_the_probe.ends_with(b"please review @src "),
        "the body must end with the daemon's one space right before the probe; the child read \
         {read:?}"
    );
    assert_eq!(
        before_the_probe
            .windows(b"please review @src".len())
            .filter(|w| *w == b"please review @src")
            .count(),
        1,
        "the body must be written exactly once; the child read {read:?}"
    );
}

#[tokio::test]
async fn a_body_already_ending_in_a_space_still_gets_the_daemons_space() {
    let dir = unique_temp_dir("line-end-space-trailing");
    let h = Harness::start("line-end-space-trailing", config(&dir)).await;
    let mut client = h.connect().await;
    let receiver = open_receiver(&h, &mut client, &dir, "trailing", true).await;

    send_message(&mut client, &receiver.window_id, "abc \r").await;

    let end = [PASTE_END, &write_end()].concat();
    let read = read_through_the_write(&receiver, &end).await;
    assert_one_paste_of(&read, b"abc  ");
}

#[tokio::test]
async fn a_handoff_envelope_ends_with_the_daemons_space_inside_the_paste() {
    let dir = unique_temp_dir("line-end-space-handoff");
    let h = Harness::start("line-end-space-handoff", config(&dir)).await;
    let mut client = h.connect().await;
    let receiver = open_receiver(&h, &mut client, &dir, "handoff", true).await;
    let reference = "a.rs";
    let note = "see @src";

    send(
        &mut client,
        &ClientMessage::SendHandoff {
            window_id: Some(receiver.window_id.clone()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: reference.to_string(),
            note: note.to_string(),
        },
    )
    .await;
    match read_response(&mut client, HANG_GUARD).await {
        DaemonMessage::HandoffSent { window_id, .. } => assert_eq!(window_id, receiver.window_id),
        other => panic!("expected handoff_sent, got {other:?}"),
    }

    let end = [PASTE_END, &write_end()].concat();
    let read = read_through_the_write(&receiver, &end).await;
    // The connection never identified itself as a window, so the daemon
    // composes the envelope of an external requester.
    let envelope = compose_envelope(&EnvelopeFrom::External, reference, note);
    assert_one_paste_of(&read, format!("{} ", envelope.text).as_bytes());
}
