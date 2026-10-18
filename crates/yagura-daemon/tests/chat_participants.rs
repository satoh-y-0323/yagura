#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the daemon tells an approver (the GUI) when the set of chat
//! participants changes -- a participant opens, exits or is renamed, or a new
//! workspace's room comes into being -- through `chat_participants_changed`, and a
//! `read_chat` / `list_chat_rooms` made after that push already shows the change.
//!
//! Every case synchronises on the push itself; the timeouts below only bound how long a
//! case waits before it fails. A notification still on its way from an earlier change can
//! be taken for the one a later change sends, so a case that measures an instant change
//! (a rename) makes it twice: one stale push can answer the first, never both.

mod common;

use std::collections::HashSet;
use std::path::Path;
use std::time::Duration;

use common::{
    CONTROL_DETECTION_NAME, ChatPlaybook, Harness, chat_config, identify, read_message,
    read_response, register_approver, second_workspace_dir, send, unique_temp_dir,
};
use yagura_protocol::{ChatParticipant, ChatRoomInfo, ClientMessage, DaemonMessage};
use yagura_test_support::ipc::Connection;

/// How long a case waits for something that must happen.
const EVENT_TIMEOUT: Duration = Duration::from_secs(20);

/// A chat-enabled daemon whose playbook also lets any connection close a window.
async fn start(tag: &str, dir: &Path) -> Harness {
    Harness::start_with_chat(
        tag,
        chat_config(
            dir,
            &ChatPlaybook {
                auto_approve: &["send_chat", "open_window", "close_window"],
                ..ChatPlaybook::default()
            },
        ),
    )
    .await
}

/// Opens a window that takes part in chat (its profile declares `[delivery]`).
async fn open_participant(h: &Harness, cwd: Option<&Path>) -> String {
    h.open_delivering_in(
        yagura_test_support::process::echo_child_command(),
        Some(CONTROL_DETECTION_NAME),
        cwd,
    )
    .await
}

/// Reads the approver connection up to and including the next
/// `chat_participants_changed`, skipping the other pushes an approver is sent.
async fn next_participants_changed(approver: &mut Connection) {
    let deadline = tokio::time::Instant::now() + EVENT_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for chat_participants_changed"
        );
        match read_message(approver, remaining).await {
            DaemonMessage::ChatParticipantsChanged => return,
            DaemonMessage::ChatChanged { .. } | DaemonMessage::PlanChanged { .. } => continue,
            other => panic!("expected chat_participants_changed, got {other:?}"),
        }
    }
}

/// Reads the reply to a request made on the approver connection, skipping every push.
async fn approver_reply(approver: &mut Connection) -> DaemonMessage {
    loop {
        match read_response(approver, EVENT_TIMEOUT).await {
            DaemonMessage::ChatParticipantsChanged
            | DaemonMessage::ChatChanged { .. }
            | DaemonMessage::PlanChanged { .. } => continue,
            other => return other,
        }
    }
}

/// The `main` room's roster, as the approver reads it.
async fn main_roster(approver: &mut Connection) -> Vec<ChatParticipant> {
    send(
        approver,
        &ClientMessage::ReadChat {
            room_id: Some("main".to_string()),
            after_seq: Some(0),
            limit: None,
        },
    )
    .await;
    match approver_reply(approver).await {
        DaemonMessage::ChatMessages { roster, .. } => roster,
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

async fn list_rooms(approver: &mut Connection) -> Vec<ChatRoomInfo> {
    send(approver, &ClientMessage::ListChatRooms).await;
    match approver_reply(approver).await {
        DaemonMessage::ChatRooms { rooms } => rooms,
        other => panic!("expected chat_rooms, got {other:?}"),
    }
}

/// Waits for pushes until a `main` roster read after one of them satisfies `reflects`.
async fn roster_after_a_push(
    approver: &mut Connection,
    reflects: impl Fn(&[ChatParticipant]) -> bool,
    what: &str,
) {
    let mut last = Vec::new();
    for _ in 0..16 {
        next_participants_changed(approver).await;
        last = main_roster(approver).await;
        if reflects(&last) {
            return;
        }
    }
    panic!("no roster read after a chat_participants_changed showed {what}; last {last:?}");
}

fn has(roster: &[ChatParticipant], window_id: &str) -> bool {
    roster.iter().any(|p| p.window_id == window_id)
}

fn name_of<'a>(roster: &'a [ChatParticipant], window_id: &str) -> Option<&'a str> {
    roster
        .iter()
        .find(|p| p.window_id == window_id)
        .map(|p| p.name.as_str())
}

async fn close(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_string(),
            force: Some(true),
        },
    )
    .await;
    match read_response(client, EVENT_TIMEOUT).await {
        DaemonMessage::WindowClosed { window_id: closed } => assert_eq!(closed, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

async fn rename(client: &mut Connection, window_id: &str, name: &str) {
    send(
        client,
        &ClientMessage::RenameWindow {
            window_id: window_id.to_string(),
            name: name.to_string(),
        },
    )
    .await;
    match read_response(client, EVENT_TIMEOUT).await {
        DaemonMessage::WindowRenamed { name: stored, .. } => assert_eq!(stored, name),
        other => panic!("expected window_renamed, got {other:?}"),
    }
}

#[tokio::test]
async fn opening_a_participant_pushes_participants_changed_and_the_roster_read_after_it_has_the_window()
 {
    let dir = unique_temp_dir("chat-participants-open");
    let h = start("chat-participants-open", &dir).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let opened = open_participant(&h, None).await;

    next_participants_changed(&mut approver).await;
    let roster = main_roster(&mut approver).await;
    assert!(
        has(&roster, &opened),
        "the roster read after the first chat_participants_changed must have the window \
         just opened, got {roster:?}"
    );
}

#[tokio::test]
async fn closing_a_participant_pushes_participants_changed_and_the_roster_read_after_it_lacks_the_window()
 {
    let dir = unique_temp_dir("chat-participants-close");
    let h = start("chat-participants-close", &dir).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let mut client = h.connect().await;

    let first = open_participant(&h, None).await;
    roster_after_a_push(&mut approver, |r| has(r, &first), "the first window").await;
    let second = open_participant(&h, None).await;
    roster_after_a_push(&mut approver, |r| has(r, &second), "the second window").await;

    close(&mut client, &first).await;
    roster_after_a_push(
        &mut approver,
        |r| !has(r, &first) && has(r, &second),
        "the first window gone",
    )
    .await;

    close(&mut client, &second).await;
    roster_after_a_push(
        &mut approver,
        |r| !has(r, &second),
        "the second window gone",
    )
    .await;
}

#[tokio::test]
async fn renaming_a_participant_pushes_participants_changed_and_the_roster_read_after_it_has_the_new_name()
 {
    let dir = unique_temp_dir("chat-participants-rename");
    let h = start("chat-participants-rename", &dir).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let mut client = h.connect().await;

    let window = open_participant(&h, None).await;
    roster_after_a_push(&mut approver, |r| has(r, &window), "the window").await;

    rename(&mut client, &window, "renamed-once").await;
    roster_after_a_push(
        &mut approver,
        |r| name_of(r, &window) == Some("renamed-once"),
        "the first new name",
    )
    .await;

    rename(&mut client, &window, "renamed-twice").await;
    roster_after_a_push(
        &mut approver,
        |r| name_of(r, &window) == Some("renamed-twice"),
        "the second new name",
    )
    .await;
}

#[tokio::test]
async fn a_participant_in_a_workspace_with_no_room_yet_is_listed_in_its_new_room_after_a_push() {
    let dir = unique_temp_dir("chat-participants-room");
    let h = start("chat-participants-room", &dir).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let before: HashSet<String> = list_rooms(&mut approver)
        .await
        .into_iter()
        .map(|room| room.room_id)
        .collect();

    let workspace = second_workspace_dir("chat-participants-room-ws");
    let window = open_participant(&h, Some(&workspace)).await;

    let mut last = Vec::new();
    let mut found = None;
    for _ in 0..16 {
        next_participants_changed(&mut approver).await;
        last = list_rooms(&mut approver).await;
        found = last
            .iter()
            .find(|room| !before.contains(&room.room_id) && room.participant_count == 1)
            .map(|room| room.room_id.clone());
        if found.is_some() {
            break;
        }
    }
    let Some(room_id) = found else {
        panic!(
            "no room listing read after a chat_participants_changed showed the new \
             workspace's room with its one participant; last {last:?}"
        );
    };

    let mut own = h.connect().await;
    identify(&mut own, &window).await;
    send(
        &mut own,
        &ClientMessage::ReadChat {
            room_id: None,
            after_seq: None,
            limit: None,
        },
    )
    .await;
    match read_response(&mut own, EVENT_TIMEOUT).await {
        DaemonMessage::ChatMessages {
            room_id: own_room, ..
        } => assert_eq!(
            own_room, room_id,
            "the room that appeared must be the window's own workspace room"
        ),
        other => panic!("expected chat_messages, got {other:?}"),
    }
}
