#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Boundary-value probes for the chat IPC surface that `ipc_chat.rs` does not
//! already cover: a `room_id` naming a room the caller is not a member of,
//! for all three tools, the `limit` boundary at 0 / 1 / exactly the maximum,
//! `after_seq` at 0 and at `u64::MAX`, and a room with no participants at
//! all.
//!
//! Shares `tests/common/mod.rs` with `ipc_chat.rs` but duplicates that file's
//! small message-shaped helpers locally rather than depending on its
//! private (non-`pub`) functions.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{ChatPlaybook, Harness, chat_config, expect_error, identify, read_response, send};
use yagura_protocol::{CHAT_READ_LIMIT_MAX, ClientMessage, DaemonMessage};

async fn start_chat(tag: &str, dir: &Path, spec: &ChatPlaybook<'_>) -> Harness {
    Harness::start_with_chat(tag, chat_config(dir, spec)).await
}

async fn send_chat_msg(
    client: &mut yagura_test_support::ipc::Connection,
    room_id: Option<&str>,
    body: &str,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::SendChat {
            room_id: room_id.map(str::to_string),
            body: body.to_string(),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

async fn broadcast_chat_msg(
    client: &mut yagura_test_support::ipc::Connection,
    room_id: Option<&str>,
    body: &str,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::BroadcastChat {
            room_id: room_id.map(str::to_string),
            body: body.to_string(),
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

async fn read_chat_msg(
    client: &mut yagura_test_support::ipc::Connection,
    room_id: Option<&str>,
    after_seq: Option<u64>,
    limit: Option<u32>,
) -> DaemonMessage {
    send(
        client,
        &ClientMessage::ReadChat {
            room_id: room_id.map(str::to_string),
            after_seq,
            limit,
        },
    )
    .await;
    read_response(client, Duration::from_secs(5)).await
}

fn expect_chat_messages_roster_len(msg: DaemonMessage) -> (usize, usize) {
    match msg {
        DaemonMessage::ChatMessages {
            messages, roster, ..
        } => (messages.len(), roster.len()),
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

// =============================================================================
// A room a caller does not belong to is refused -- for all
// three tools. "side-room" is a well-formed room id (passes is_valid_token);
// what makes it a denial is that the caller opened into a different room and
// was never added as a member of this one.
// =============================================================================

#[tokio::test]
async fn send_chat_to_a_room_it_does_not_belong_to_is_invalid_argument() {
    let dir = common::unique_temp_dir("chat-boundary-room-id-send");
    let h = start_chat(
        "chat-boundary-room-id-send",
        &dir,
        &ChatPlaybook {
            auto_approve: &["send_chat"],
            ..Default::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = send_chat_msg(&mut client, Some("side-room"), "hi").await;
    expect_error(result, "invalid_argument");
}

#[tokio::test]
async fn broadcast_chat_to_a_room_it_does_not_belong_to_is_invalid_argument() {
    let dir = common::unique_temp_dir("chat-boundary-room-id-broadcast");
    let h = start_chat(
        "chat-boundary-room-id-broadcast",
        &dir,
        &ChatPlaybook {
            auto_approve: &["broadcast_chat"],
            ..Default::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = broadcast_chat_msg(&mut client, Some("side-room"), "hi").await;
    expect_error(result, "invalid_argument");
}

#[tokio::test]
async fn read_chat_from_a_room_it_does_not_belong_to_is_invalid_argument() {
    let dir = common::unique_temp_dir("chat-boundary-room-id-read");
    let h = start_chat("chat-boundary-room-id-read", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = read_chat_msg(&mut client, Some("side-room"), Some(0), None).await;
    expect_error(result, "invalid_argument");
}

// =============================================================================
// limit boundary: 0 (rejected), 1 (accepted), exactly CHAT_READ_LIMIT_MAX
// (accepted). The over-the-maximum case is already covered by
// `ipc_chat.rs::read_chat_rejects_a_limit_over_the_maximum`.
// =============================================================================

#[tokio::test]
async fn read_chat_rejects_a_limit_of_exactly_zero() {
    let dir = common::unique_temp_dir("chat-boundary-limit-zero");
    let h = start_chat("chat-boundary-limit-zero", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = read_chat_msg(&mut client, None, Some(0), Some(0)).await;
    expect_error(result, "invalid_argument");
}

#[tokio::test]
async fn read_chat_accepts_a_limit_of_exactly_one() {
    let dir = common::unique_temp_dir("chat-boundary-limit-one");
    let h = start_chat(
        "chat-boundary-limit-one",
        &dir,
        &ChatPlaybook {
            auto_approve: &["send_chat"],
            ..Default::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;
    for text in ["a", "b", "c"] {
        send_chat_msg(&mut client, None, text).await;
    }

    let result = read_chat_msg(&mut client, None, Some(0), Some(1)).await;
    let (messages, _roster) = expect_chat_messages_roster_len(result);
    assert_eq!(
        messages, 1,
        "a limit of exactly 1 must return exactly 1 row"
    );
}

#[tokio::test]
async fn read_chat_accepts_a_limit_of_exactly_the_documented_maximum() {
    let dir = common::unique_temp_dir("chat-boundary-limit-max");
    let h = start_chat("chat-boundary-limit-max", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    // No messages exist yet; the point of this case is that the daemon
    // accepts the boundary value itself rather than what it returns.
    let result = read_chat_msg(&mut client, None, Some(0), Some(CHAT_READ_LIMIT_MAX)).await;
    let (messages, _roster) = expect_chat_messages_roster_len(result);
    assert_eq!(messages, 0);
}

// =============================================================================
// after_seq boundary: 0 (explicit, accepted) and u64::MAX (accepted, and
// does not panic on the u64 -> i64 conversion the store layer performs).
// =============================================================================

#[tokio::test]
async fn read_chat_accepts_an_explicit_after_seq_of_zero() {
    let dir = common::unique_temp_dir("chat-boundary-after-seq-zero");
    let h = start_chat(
        "chat-boundary-after-seq-zero",
        &dir,
        &ChatPlaybook {
            auto_approve: &["send_chat"],
            ..Default::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;
    send_chat_msg(&mut client, None, "hello").await;

    let result = read_chat_msg(&mut client, None, Some(0), None).await;
    let (messages, _roster) = expect_chat_messages_roster_len(result);
    assert_eq!(
        messages, 1,
        "after_seq=0 must be treated as \"from the start\", returning every posted message"
    );
}

#[tokio::test]
async fn read_chat_accepts_an_after_seq_of_u64_max_without_panicking() {
    let dir = common::unique_temp_dir("chat-boundary-after-seq-max");
    let h = start_chat(
        "chat-boundary-after-seq-max",
        &dir,
        &ChatPlaybook {
            auto_approve: &["send_chat"],
            ..Default::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;
    send_chat_msg(&mut client, None, "hello").await;

    let result = read_chat_msg(&mut client, None, Some(u64::MAX), None).await;
    let (messages, _roster) = expect_chat_messages_roster_len(result);
    assert_eq!(
        messages, 0,
        "after_seq=u64::MAX must return no messages (there is nothing past it), not error"
    );
}

// =============================================================================
// An empty roster: no window has ever been opened on this daemon.
// =============================================================================

#[tokio::test]
async fn read_chat_with_no_windows_opened_reports_an_empty_roster() {
    let dir = common::unique_temp_dir("chat-boundary-empty-roster");
    let h = start_chat("chat-boundary-empty-roster", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    common::register_approver(&mut approver).await;

    let result = read_chat_msg(&mut approver, None, None, None).await;
    let (messages, roster) = expect_chat_messages_roster_len(result);
    assert_eq!(messages, 0);
    assert_eq!(
        roster, 0,
        "a daemon with no windows opened must report an empty roster, not error or omit it"
    );
}
