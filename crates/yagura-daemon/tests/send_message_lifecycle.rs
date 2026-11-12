#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: `send_message` puts an envelope in the window's inbox and
//! never writes to its PTY. The window's CLI takes it through its own doors.
//!
//! The answer says what became of the envelope (`state`), names it (`id`) and,
//! while it waits, says when it is handed over (`note`). A window whose CLI has a
//! wait registered is handed the envelope at once and the answer waits for the
//! door to say it printed it. A window with no doors is refused. Every case
//! drives a real `IpcServer`; the hang guards only bound a failure.

use crate::common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use common::{
    CONTROL_DETECTION_NAME, Harness, LEADING_KEYS_DETECTION_NAME, READY_MARKER, VALID_TOKEN,
    approver_base_config, detection_config, expect_delivery, policy, read_response, send,
    unique_temp_dir, wait_until_status, write_leading_keys_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::window::WindowEntry;
use yagura_protocol::{ClientMessage, DaemonMessage, Door, WindowStatus};
use yagura_test_support::ipc::Connection;

/// `send_message` and `send_handoff` are approved by the playbook; the control and
/// leading-keys definitions are under `dir`.
pub(crate) fn sending_config(dir: &Path) -> DaemonConfig {
    write_leading_keys_definition(dir);
    DaemonConfig {
        approval_policy: Some(policy(&["send_message", "send_handoff"])),
        ..detection_config(dir, approver_base_config())
    }
}

pub(crate) fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// An `echo_child` window under `detection` whose profile declares delivery and
/// wakes its CLI by answering the wait the CLI registers; driven to `idle`.
pub(crate) async fn open_wait_window(
    h: &Harness,
    client: &mut Connection,
    detection: &str,
) -> String {
    let window_id = h
        .registry
        .open_with_extra_env(
            yagura_daemon::OpenWindowRequest {
                command: yagura_test_support::process::echo_child_command(),
                cwd: None,
                name: None,
                vendor: None,
                detection: Some(detection.to_string()),
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                delivery: Some(yagura_daemon::profile_delivery::ProfileDelivery {
                    wake: yagura_daemon::profile_delivery::WakeMode::Wait,
                    ..common::wake_none_delivery()
                }),
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("registry.open_with_extra_env should spawn the child")
        .window_id;
    h.write_input(&window_id, READY_MARKER).await;
    wait_until_status(client, &window_id, WindowStatus::Idle).await;
    window_id
}

/// Sends a door's read of `door` for `window_id` on `conn`.
pub(crate) async fn send_door_read(conn: &mut Connection, window_id: &str, door: Door) {
    send(
        conn,
        &ClientMessage::DoorRead {
            window_id: window_id.to_string(),
            token: VALID_TOKEN.to_string(),
            door,
        },
    )
    .await;
}

/// Registers a wait for `window_id` on a connection of its own, as a CLI's wait
/// hook does, and returns once the daemon holds it.
pub(crate) async fn register_wait(h: &Harness, window_id: &str) -> Connection {
    let mut door = h.connect().await;
    send_door_read(&mut door, window_id, Door::Wait).await;
    until("the wait to be registered", || {
        entry(h, window_id).inbox().has_waiter()
    })
    .await;
    door
}

/// Takes through the window's take door on a connection of its own, prints it and
/// says so; returns what was printed.
pub(crate) async fn take_and_print(h: &Harness, window_id: &str) -> String {
    let mut door = h.connect().await;
    send_door_read(&mut door, window_id, Door::Take).await;
    let (output, _, _) = expect_delivery(read_response(&mut door).await);
    send(&mut door, &ClientMessage::DeliverPrinted).await;
    output
}

/// Polls an observable daemon-side state; the deadline only detects a hang.
pub(crate) async fn until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = tokio::time::Instant::now() + yagura_test_support::HANG_GUARD;
    while !condition() {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {what}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// What a send's answer says became of its envelope: the refusal's code for an
/// error, the answer's `state` otherwise (`None` when it carries none).
pub(crate) fn state_of(answer: &DaemonMessage) -> Option<String> {
    if let DaemonMessage::Error { code, .. } = answer {
        return Some(code.clone());
    }
    answer_json(answer)["state"].as_str().map(str::to_string)
}

/// The answer's `note`; `None` when it has none.
pub(crate) fn note_of(answer: &DaemonMessage) -> Option<String> {
    answer_json(answer)["note"].as_str().map(str::to_string)
}

/// The answer's envelope `id`, which must be a non-empty string.
pub(crate) fn id_of(answer: &DaemonMessage) -> String {
    let id = answer_json(answer)["id"]
        .as_str()
        .unwrap_or_else(|| panic!("the answer must name its envelope with an id: {answer:?}"))
        .to_string();
    assert!(!id.is_empty(), "the envelope's id must not be empty");
    id
}

fn answer_json(answer: &DaemonMessage) -> Value {
    serde_json::to_value(answer).expect("a daemon message always encodes")
}

/// Which of two connections spoke first.
pub(crate) enum First {
    Door(DaemonMessage),
    Requester(DaemonMessage),
}

/// The first frame either `door` or `requester` receives.
pub(crate) async fn first_of(door: &mut Connection, requester: &mut Connection) -> First {
    tokio::select! {
        biased;
        answer = read_response(requester) => First::Requester(answer),
        delivery = read_response(door) => First::Door(delivery),
    }
}

async fn send_text(client: &mut Connection, window_id: &str, text: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendText {
            window_id: window_id.to_string(),
            text: text.to_string(),
        },
    )
    .await;
    read_response(client).await
}

// =====================================================================
// The envelope waits in the inbox
// =====================================================================

/// The body starts with a key the window's CLI would read as a mode key and holds
/// a line break mid-text: nothing is typed, so neither is refused.
#[tokio::test]
async fn send_message_puts_its_envelope_in_the_inbox_and_writes_nothing_to_the_window() {
    let dir = unique_temp_dir("send-message-inbox");
    let h = Harness::start("send-message-inbox", sending_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_wait_window(&h, &mut client, LEADING_KEYS_DETECTION_NAME).await;
    let writes_before = entry(&h, &window_id).write_seq();

    let first = send_text(&mut client, &window_id, "!FIRST-LINE-7K\nSECOND-LINE-7K").await;
    assert_eq!(
        state_of(&first).as_deref(),
        Some("queued"),
        "with no wait registered nothing is handed over, so the envelope waits: {first:?}"
    );
    let note = note_of(&first)
        .unwrap_or_else(|| panic!("a waiting envelope's answer says when it is handed over"));
    assert!(!note.is_empty());
    let first_id = id_of(&first);

    let second = send_text(&mut client, &window_id, "NEXT-MESSAGE-7K").await;
    assert_eq!(state_of(&second).as_deref(), Some("queued"), "{second:?}");
    let second_id = id_of(&second);
    assert_ne!(first_id, second_id, "every send names its envelope anew");

    assert_eq!(entry(&h, &window_id).inbox().pending_count(), 2);
    assert_eq!(
        entry(&h, &window_id).write_seq(),
        writes_before,
        "send_message must write nothing to the window's PTY"
    );

    let taken = take_and_print(&h, &window_id).await;
    for needle in [
        "FIRST-LINE-7K",
        "SECOND-LINE-7K",
        "NEXT-MESSAGE-7K",
        first_id.as_str(),
        second_id.as_str(),
    ] {
        assert!(
            taken.contains(needle),
            "the take door hands over both envelopes, each with its id ({needle}): {taken:?}"
        );
    }
    assert_eq!(entry(&h, &window_id).inbox().pending_count(), 0);
    assert_eq!(entry(&h, &window_id).write_seq(), writes_before);
}

/// A CLI that left a wait registered is handed the envelope at once; the answer
/// waits for the door to say it printed it and then needs no note.
#[tokio::test]
async fn send_message_to_a_window_whose_cli_waits_is_delivered_once_the_door_printed_it() {
    let dir = unique_temp_dir("send-message-waited");
    let h = Harness::start("send-message-waited", sending_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = open_wait_window(&h, &mut client, CONTROL_DETECTION_NAME).await;
    let writes_before = entry(&h, &window_id).write_seq();
    let mut door = register_wait(&h, &window_id).await;

    send(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "WAITED-BODY-3R".to_string(),
        },
    )
    .await;
    let output = match first_of(&mut door, &mut client).await {
        First::Door(delivery) => expect_delivery(delivery).0,
        First::Requester(answer) => {
            panic!("the answer must wait for the door the envelope was handed to: {answer:?}")
        }
    };
    assert!(output.contains("WAITED-BODY-3R"), "{output:?}");
    send(&mut door, &ClientMessage::DeliverPrinted).await;

    let answer = read_response(&mut client).await;
    assert_eq!(
        state_of(&answer).as_deref(),
        Some("delivered"),
        "{answer:?}"
    );
    assert_eq!(
        note_of(&answer),
        None,
        "a wait was registered, so there is nothing to say about when it arrives"
    );
    assert!(output.contains(&id_of(&answer)), "{output:?}");
    assert_eq!(entry(&h, &window_id).write_seq(), writes_before);
}

// =====================================================================
// What is refused
// =====================================================================

#[tokio::test]
async fn send_message_to_a_window_without_doors_is_undelivered_no_delivery_and_writes_nothing() {
    let dir = unique_temp_dir("send-message-no-doors");
    let h = Harness::start("send-message-no-doors", sending_config(&dir)).await;
    let mut client = h.connect().await;
    let window_id = h
        .open(
            yagura_test_support::process::echo_child_command(),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    h.write_input(&window_id, READY_MARKER).await;
    wait_until_status(&mut client, &window_id, WindowStatus::Idle).await;
    let writes_before = entry(&h, &window_id).write_seq();

    let answer = send_text(&mut client, &window_id, "NO-DOORS-BODY").await;
    assert_eq!(
        state_of(&answer).as_deref(),
        Some("undelivered_no_delivery"),
        "{answer:?}"
    );
    assert_eq!(entry(&h, &window_id).inbox().pending_count(), 0);
    assert_eq!(entry(&h, &window_id).write_seq(), writes_before);
}
