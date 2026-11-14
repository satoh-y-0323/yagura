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

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use common::{
    CONTROL_DETECTION_NAME, Harness, LEADING_KEYS_DETECTION_NAME, READY_MARKER, VALID_TOKEN,
    approver_base_config, detection_config, expect_delivery, identify, policy, read_response, send,
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

/// An `echo_child` window under `detection` whose profile wakes its CLI by answering the
/// wait the CLI registers, opened as a `launch` window whose hooks are injected, with its
/// credentials folder `root/creds/<name>`. With `wake` its `[hooks]` answer names
/// `{wake_paths}`, so its open gives it a wake file; without, its one answer names nothing.
/// Left `Unknown`: the caller drives it on.
pub(crate) async fn open_wake_window(
    h: &Harness,
    root: &Path,
    name: &str,
    detection: &str,
    wake: bool,
) -> String {
    use crate::hooks_injection::{PLAIN_ANSWER, WAKE_ANSWER};
    let (event, answer) = if wake {
        ("SessionStart", WAKE_ANSWER)
    } else {
        ("Plain", PLAIN_ANSWER)
    };
    let texts: BTreeMap<String, String> = [(event.to_string(), answer.to_string())]
        .into_iter()
        .collect();
    h.registry
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
            vec![yagura_daemon::window::CliInjection {
                kind: yagura_daemon::window::InjectionKind::Hooks,
                args: vec!["--hooks".to_string(), "injected".to_string()],
                config_file: None,
                config_dir: None,
            }],
            yagura_daemon::window::WindowProvenance {
                delivery: Some(yagura_daemon::profile_delivery::ProfileDelivery {
                    wake: yagura_daemon::profile_delivery::WakeMode::Wait,
                    ..common::wake_none_delivery()
                }),
                hooks: yagura_daemon::window::HooksRoute::Injection,
                credentials: yagura_daemon::profile::CredentialsMode::Launch,
                credentials_dir: Some(root.join("creds").join(name)),
                hook_answers: yagura_daemon::wake_file::HookAnswers {
                    texts,
                    wake: wake.then_some(yagura_daemon::wake_file::WakePlace::Folder),
                },
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("registry.open_with_extra_env should spawn the child")
        .window_id
}

/// The wake file the open of `window_id` (a window of [`open_wake_window`] under `root`)
/// handed its CLI to watch.
pub(crate) fn wake_file_of(root: &Path, window_id: &str) -> PathBuf {
    let paths = crate::hooks_injection::wake_paths_of(root, window_id)
        .unwrap_or_else(|| panic!("the open of {window_id} names no wake file"));
    assert_eq!(paths.len(), 1, "one wake file: {paths:?}");
    PathBuf::from(&paths[0])
}

/// Sends a door's read of `door` for `window_id` on `conn`.
pub(crate) async fn send_door_read(conn: &mut Connection, window_id: &str, door: Door) {
    send(
        conn,
        &ClientMessage::DoorRead {
            window_id: window_id.to_string(),
            token: VALID_TOKEN.to_string(),
            door,
            guest: None,
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
    // A connection's frames are handled in order, so the answer to the next one
    // means the daemon has taken the print in.
    send(&mut door, &ClientMessage::GetProtocolRevision).await;
    match read_response(&mut door).await {
        DaemonMessage::ProtocolRevision { .. } => {}
        other => panic!("expected the protocol revision after the print, got {other:?}"),
    }
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

pub(crate) async fn send_text(
    client: &mut Connection,
    window_id: &str,
    text: &str,
) -> DaemonMessage {
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

/// Asks, on `client`, to withdraw the envelope named `id`; returns the answer.
pub(crate) async fn withdraw(client: &mut Connection, id: &str) -> DaemonMessage {
    send(
        client,
        &ClientMessage::WithdrawMessage { id: id.to_string() },
    )
    .await;
    read_response(client).await
}

/// What a withdrawal's answer says: the refusal's code for an error, the answer's
/// `outcome` otherwise (`None` when it carries none).
pub(crate) fn outcome_of(answer: &DaemonMessage) -> Option<String> {
    if let DaemonMessage::Error { code, .. } = answer {
        return Some(code.clone());
    }
    answer_json(answer)["outcome"].as_str().map(str::to_string)
}

/// How many envelopes `list_windows` says wait in `window_id`'s inbox.
pub(crate) async fn listed_queued_count(client: &mut Connection, window_id: &str) -> Option<u64> {
    send(client, &ClientMessage::ListWindows).await;
    match read_response(client).await {
        DaemonMessage::WindowList { windows } => {
            let info = windows
                .iter()
                .find(|info| info.window_id == window_id)
                .unwrap_or_else(|| panic!("{window_id} must be listed: {windows:?}"));
            serde_json::to_value(info).expect("a window info always encodes")["queued_count"]
                .as_u64()
        }
        other => panic!("expected window_list, got {other:?}"),
    }
}

/// What `get_window_status` of `window_id` shows the caller on `client` of the
/// envelopes waiting there: their count, and the ids it may see, sorted (none when
/// the answer names none).
pub(crate) async fn queue_seen_by(
    client: &mut Connection,
    window_id: &str,
) -> (Option<u64>, Vec<String>) {
    send(
        client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.to_string(),
        },
    )
    .await;
    let answer = read_response(client).await;
    assert!(
        matches!(answer, DaemonMessage::WindowStatus { .. }),
        "expected window_status, got {answer:?}"
    );
    let json = answer_json(&answer);
    let mut ids: Vec<String> = match &json["queued_ids"] {
        Value::Null => Vec::new(),
        Value::Array(ids) => ids
            .iter()
            .map(|id| {
                id.as_str()
                    .unwrap_or_else(|| panic!("an id is a string: {json}"))
                    .to_string()
            })
            .collect(),
        other => panic!("queued_ids must be an array of ids, got {other}"),
    };
    ids.sort();
    (json["queued_count"].as_u64(), ids)
}

/// A window of its own (its status stays `Unknown`) and a connection identified as
/// it, which sends as that window.
pub(crate) async fn window_sender(h: &Harness) -> (String, Connection) {
    let window_id = h.open_unknown_window().await;
    let mut client = h.connect().await;
    identify(&mut client, &window_id).await;
    (window_id, client)
}

/// Closes `window_id` from a connection identified as that window, which needs no
/// approval.
pub(crate) async fn close_own_window(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_string(),
            force: Some(true),
        },
    )
    .await;
    match read_response(client).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

/// Sends `text` to `window_id` on `client` and returns the id of the envelope, which
/// must be left waiting.
pub(crate) async fn send_queued(client: &mut Connection, window_id: &str, text: &str) -> String {
    let answer = send_text(client, window_id, text).await;
    assert_eq!(
        state_of(&answer).as_deref(),
        Some("queued"),
        "{text} must be left waiting: {answer:?}"
    );
    id_of(&answer)
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
    assert_eq!(
        listed_queued_count(&mut client, &window_id).await,
        Some(2),
        "list_windows says how many envelopes wait in the window"
    );
    assert_eq!(
        queue_seen_by(&mut client, &window_id).await,
        (Some(2), Vec::new()),
        "a caller that is no window is shown how many wait, and no id"
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
    assert_eq!(
        outcome_of(&withdraw(&mut client, &first_id).await).as_deref(),
        Some("not_waiting"),
        "an envelope a door printed neither waits nor is carried: the inbox holds nothing of it"
    );
    assert_eq!(
        listed_queued_count(&mut client, &window_id)
            .await
            .unwrap_or(0),
        0
    );

    // With no wait registered, a window with a wake file is asked to wait again, which its
    // note says; one without says what the note above says.
    let root = unique_temp_dir("send-message-inbox-wake");
    for (name, wake) in [("asked", true), ("unasked", false)] {
        let wake_window =
            open_wake_window(&h, &root, name, LEADING_KEYS_DETECTION_NAME, wake).await;
        h.write_input(&wake_window, READY_MARKER).await;
        wait_until_status(&mut client, &wake_window, WindowStatus::Idle).await;
        let answer = send_text(&mut client, &wake_window, "WAKE-NOTE-7K").await;
        assert_eq!(state_of(&answer).as_deref(), Some("queued"), "{answer:?}");
        let wake_note = note_of(&answer)
            .unwrap_or_else(|| panic!("{name}: a waiting envelope's answer has a note"));
        assert!(!wake_note.is_empty(), "{name}");
        if wake {
            assert_ne!(
                wake_note, note,
                "a window with a wake file is asked to wait again, so its note says that"
            );
        } else {
            assert_eq!(
                wake_note, note,
                "a window without a wake file waits for its CLI's next turn, as one with no hooks does"
            );
        }
    }
}

/// A CLI that left a wait registered is handed the envelope at once; the answer
/// waits for the door to say it printed it and then needs no note, also in a window
/// with a wake file.
#[tokio::test]
async fn send_message_to_a_window_whose_cli_waits_is_delivered_once_the_door_printed_it() {
    let dir = unique_temp_dir("send-message-waited");
    let h = Harness::start("send-message-waited", sending_config(&dir)).await;
    let mut client = h.connect().await;
    let plain = open_wait_window(&h, &mut client, CONTROL_DETECTION_NAME).await;
    let root = unique_temp_dir("send-message-waited-wake");
    let with_wake_file = open_wake_window(&h, &root, "waited", CONTROL_DETECTION_NAME, true).await;
    h.write_input(&with_wake_file, READY_MARKER).await;
    wait_until_status(&mut client, &with_wake_file, WindowStatus::Idle).await;

    for window_id in [plain, with_wake_file] {
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

// =====================================================================
// withdraw_message: a sender takes back what still waits
// =====================================================================

/// Only the caller that sent an envelope takes it back: another caller that is no
/// window, and a window, leave it waiting. No playbook approves `withdraw_message`
/// and no approver is there, so its answer also shows it asks no approval.
#[tokio::test]
async fn withdraw_message_takes_a_waiting_envelope_out_for_its_sender_alone_and_asks_no_approval() {
    let dir = unique_temp_dir("withdraw-sender");
    let h = Harness::start("withdraw-sender", sending_config(&dir)).await;
    let mut sender = h.connect().await;
    let window_id = open_wait_window(&h, &mut sender, CONTROL_DETECTION_NAME).await;
    let mut other = h.connect().await;
    let (_, mut window) = window_sender(&h).await;

    let id = send_queued(&mut sender, &window_id, "WITHDRAW-ME-6Q").await;
    for (who, client) in [("another caller", &mut other), ("a window", &mut window)] {
        let answer = withdraw(client, &id).await;
        assert_ne!(
            outcome_of(&answer).as_deref(),
            Some("withdrawn"),
            "{who} did not send it: {answer:?}"
        );
        assert_eq!(entry(&h, &window_id).inbox().pending_count(), 1, "{who}");
    }

    let answer = withdraw(&mut sender, &id).await;
    assert_eq!(
        outcome_of(&answer).as_deref(),
        Some("withdrawn"),
        "{answer:?}"
    );
    assert_eq!(entry(&h, &window_id).inbox().pending_count(), 0);
    assert_eq!(
        listed_queued_count(&mut sender, &window_id)
            .await
            .unwrap_or(0),
        0
    );
    assert_eq!(
        outcome_of(&withdraw(&mut sender, "an-id-never-handed-out").await).as_deref(),
        Some("not_waiting")
    );
}

/// A window caller is shown the ids of what it left waiting there and no other's,
/// and withdraws only those; a caller that is no window is shown the count alone.
#[tokio::test]
async fn a_window_sees_and_withdraws_only_what_it_sent_and_a_caller_that_is_no_window_sees_the_count()
 {
    let dir = unique_temp_dir("withdraw-windows");
    let h = Harness::start("withdraw-windows", sending_config(&dir)).await;
    let mut external = h.connect().await;
    let window_id = open_wait_window(&h, &mut external, CONTROL_DETECTION_NAME).await;
    let (_, mut a) = window_sender(&h).await;
    let (_, mut b) = window_sender(&h).await;

    let a_id = send_queued(&mut a, &window_id, "FROM-A-2K").await;
    let b_id = send_queued(&mut b, &window_id, "FROM-B-2K").await;
    let external_id = send_queued(&mut external, &window_id, "FROM-EXTERNAL-2K").await;

    assert_eq!(
        queue_seen_by(&mut a, &window_id).await.1,
        vec![a_id.clone()]
    );
    assert_eq!(
        queue_seen_by(&mut b, &window_id).await.1,
        vec![b_id.clone()]
    );
    assert_eq!(
        queue_seen_by(&mut external, &window_id).await,
        (Some(3), Vec::new())
    );

    for (who, client, id) in [
        ("window B", &mut b, &a_id),
        ("window A", &mut a, &external_id),
    ] {
        let answer = withdraw(client, id).await;
        assert_ne!(
            outcome_of(&answer).as_deref(),
            Some("withdrawn"),
            "{who} did not send {id}: {answer:?}"
        );
    }
    assert_eq!(entry(&h, &window_id).inbox().pending_count(), 3);

    assert_eq!(
        outcome_of(&withdraw(&mut a, &a_id).await).as_deref(),
        Some("withdrawn")
    );
    assert_eq!(
        queue_seen_by(&mut a, &window_id).await.1,
        Vec::<String>::new()
    );
    assert_eq!(queue_seen_by(&mut b, &window_id).await.1, vec![b_id]);
    assert_eq!(
        listed_queued_count(&mut external, &window_id).await,
        Some(2)
    );
    let left = take_and_print(&h, &window_id).await;
    assert!(
        !left.contains("FROM-A-2K") && left.contains("FROM-B-2K") && left.contains(&external_id),
        "{left:?}"
    );
}

#[tokio::test]
async fn withdrawing_an_envelope_whose_window_has_ended_is_not_waiting() {
    let dir = unique_temp_dir("withdraw-ended");
    let h = Harness::start("withdraw-ended", sending_config(&dir)).await;
    let mut sender = h.connect().await;
    let window_id = open_wait_window(&h, &mut sender, CONTROL_DETECTION_NAME).await;
    let id = send_queued(&mut sender, &window_id, "ENDS-WITH-ITS-WINDOW").await;

    let mut own = h.connect().await;
    identify(&mut own, &window_id).await;
    close_own_window(&mut own, &window_id).await;
    until("the window's end to empty its inbox", || {
        entry(&h, &window_id).inbox().pending_count() == 0
    })
    .await;

    assert_eq!(
        outcome_of(&withdraw(&mut sender, &id).await).as_deref(),
        Some("not_waiting")
    );
}

/// A window that ends takes with it what it left waiting in every window's inbox, and
/// nothing another sender left there. One a door carries is not withdrawn: the door
/// ends without printing it, and it waits again.
#[tokio::test]
async fn a_senders_end_withdraws_what_it_left_waiting_everywhere_but_not_what_a_door_carries() {
    let dir = unique_temp_dir("withdraw-sender-end");
    let h = Harness::start("withdraw-sender-end", sending_config(&dir)).await;
    let mut external = h.connect().await;
    let first = open_wait_window(&h, &mut external, CONTROL_DETECTION_NAME).await;
    let second = open_wait_window(&h, &mut external, CONTROL_DETECTION_NAME).await;
    let (sender_id, mut sender) = window_sender(&h).await;

    send_queued(&mut sender, &first, "SENDER-TO-FIRST").await;
    send_queued(&mut external, &first, "EXTERNAL-TO-FIRST").await;
    send_queued(&mut sender, &second, "SENDER-CARRIED").await;
    let mut door = h.connect().await;
    send_door_read(&mut door, &second, Door::Take).await;
    let (carried, _, _) = expect_delivery(read_response(&mut door).await);
    assert!(carried.contains("SENDER-CARRIED"), "{carried:?}");
    send_queued(&mut sender, &second, "SENDER-WAITING").await;

    close_own_window(&mut sender, &sender_id).await;
    until("the sender's waiting envelopes to be withdrawn", || {
        entry(&h, &first).inbox().pending_count() == 1
            && entry(&h, &second).inbox().pending_count() == 0
    })
    .await;

    drop(door);
    until("the carried envelope to wait again", || {
        entry(&h, &second).inbox().pending_count() == 1
    })
    .await;
    let again = take_and_print(&h, &second).await;
    assert!(
        again.contains("SENDER-CARRIED") && !again.contains("SENDER-WAITING"),
        "{again:?}"
    );
    let left = take_and_print(&h, &first).await;
    assert!(
        left.contains("EXTERNAL-TO-FIRST") && !left.contains("SENDER-TO-FIRST"),
        "{left:?}"
    );
}
