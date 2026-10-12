#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: a window that shows nothing of what it is sent keeps
//! its deliveries waiting for an event, and those waits must not hold what
//! every other window's deliveries share.
//!
//! The shared budgets (chat's delivery budget, the daemon-wide handoff
//! budget) are taken only once a delivery holds the window's turn, so a stuck
//! window holds at most one unit of each. Deliveries waiting for its turn are
//! counted per window instead, and the ones past that count end at once for
//! that window alone. A `frozen:` `submit_child` window is the stuck one: it
//! paints nothing from the first paste on. The hang guards only bound a
//! failure.

mod common;

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use common::{
    CONTROL_DETECTION_NAME, ChatPlaybook, Harness, SubmitDefinitionInput, approver_base_config,
    chat_config, identify, open_submit_child_window_with_clear_scope, policy, read_response, send,
    unique_temp_dir,
};
use yagura_daemon::DaemonConfig;
use yagura_daemon::window::{ChatIntroOutcome, WindowEntry};
use yagura_protocol::{ChatMessage, ClientMessage, DaemonMessage};
use yagura_test_support::ipc::Connection;

/// The longest any single step may take before the case is called hung.
const HANG_GUARD: Duration = Duration::from_secs(60);

const EXHAUSTED: &str = "undelivered_exhausted";

/// States a chat delivery ends in once its envelope was written and submitted.
const DELIVERED: [&str; 3] = ["confirmed", "unconfirmed", "not_observed"];

fn window_input() -> String {
    SubmitDefinitionInput {
        bracketed_paste: true,
        ready_hold_ms: 0,
        submit_delay_ms: 120,
        clear: "\\u0015",
        delivery_confirm_ms: 2000,
        resend_max: 0,
    }
    .input_toml()
}

/// A window that paints nothing it is sent from the first paste on.
async fn open_stuck_window(h: &Harness, client: &mut Connection, dir: &Path) -> String {
    open_submit_child_window_with_clear_scope(
        h,
        client,
        dir,
        "stuck",
        "stuck-marker",
        &window_input(),
        0,
        "frozen:",
    )
    .await
}

/// A window that shows what it is sent and takes the submit key.
async fn open_healthy_window(h: &Harness, client: &mut Connection, dir: &Path) -> String {
    open_submit_child_window_with_clear_scope(
        h,
        client,
        dir,
        "healthy",
        "healthy-marker",
        &window_input(),
        0,
        "",
    )
    .await
}

fn entry(h: &Harness, window_id: &str) -> Arc<WindowEntry> {
    h.registry
        .get(window_id)
        .expect("window must exist in registry")
}

/// The poster's definition is left out of chat: as a participant it would be
/// sent an intro it never shows, holding a unit of the budget under test.
const NON_PARTICIPANTS: &[&str] = &[CONTROL_DETECTION_NAME];

/// Chat limits wide enough that no number of posts in a case halts the room.
fn chat_spec<'a>(intro: Option<&'a str>, chat_delivery_max: u64) -> ChatPlaybook<'a> {
    ChatPlaybook {
        auto_approve: &["send_chat", "close_window"],
        repetition_count: Some(1_000),
        monologue_max: Some(1_000),
        turn_message_max: Some(1_000),
        silence_timeout_secs: Some(86_400),
        intro,
        exclude_detections: Some(NON_PARTICIPANTS),
        chat_delivery_max: Some(chat_delivery_max),
        ..ChatPlaybook::default()
    }
}

/// Reads the reply to a request, skipping the `chat_changed` pushes that may
/// overtake it.
async fn read_reply(client: &mut Connection) -> DaemonMessage {
    loop {
        let frame = read_response(client, HANG_GUARD).await;
        if !matches!(frame, DaemonMessage::ChatChanged { .. }) {
            return frame;
        }
    }
}

/// Posts `body` and returns the post's `seq`, asserting it was addressed to
/// `target`.
async fn post(client: &mut Connection, body: &str, target: &str) -> u64 {
    send(
        client,
        &ClientMessage::SendChat {
            room_id: None,
            body: body.to_string(),
        },
    )
    .await;
    match read_reply(client).await {
        DaemonMessage::ChatPosted { seq, targets, .. } => {
            assert_eq!(targets, vec![target.to_string()], "post {body:?}");
            seq
        }
        other => panic!("expected chat_posted, got {other:?}"),
    }
}

async fn read_room(client: &mut Connection) -> Vec<ChatMessage> {
    send(
        client,
        &ClientMessage::ReadChat {
            room_id: None,
            after_seq: Some(0),
            limit: Some(200),
        },
    )
    .await;
    match read_reply(client).await {
        DaemonMessage::ChatMessages { messages, .. } => messages,
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

fn state_of(messages: &[ChatMessage], seq: u64, window_id: &str) -> String {
    messages
        .iter()
        .find(|message| message.seq == seq)
        .and_then(|message| {
            message
                .deliveries
                .iter()
                .find(|delivery| delivery.window_id == window_id)
        })
        .unwrap_or_else(|| panic!("no delivery of post {seq} to {window_id} in {messages:?}"))
        .state
        .clone()
}

/// Reads the room until every one of `seqs` has left `pending` for
/// `window_id`, and returns their states in order.
async fn wait_until_settled(client: &mut Connection, seqs: &[u64], window_id: &str) -> Vec<String> {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let messages = read_room(client).await;
        let states: Vec<String> = seqs
            .iter()
            .map(|seq| state_of(&messages, *seq, window_id))
            .collect();
        if states.iter().all(|state| state != "pending") {
            return states;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the deliveries to {window_id} never settled: {states:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Reads the room until `count` of `seqs` are exhausted for `window_id`.
async fn wait_until_exhausted(
    client: &mut Connection,
    seqs: &[u64],
    window_id: &str,
    count: usize,
) {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let messages = read_room(client).await;
        let states: Vec<String> = seqs
            .iter()
            .map(|seq| state_of(&messages, *seq, window_id))
            .collect();
        if states.iter().filter(|state| *state == EXHAUSTED).count() >= count {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the deliveries past the window's turn waiters were never turned away: {states:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

async fn close_window(client: &mut Connection, window_id: &str) {
    send(
        client,
        &ClientMessage::CloseWindow {
            window_id: window_id.to_string(),
            force: Some(true),
        },
    )
    .await;
    match read_reply(client).await {
        DaemonMessage::WindowClosed { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected window_closed, got {other:?}"),
    }
}

/// The identified window chat posts are made from; it takes no deliveries.
async fn open_poster(h: &Harness, opener: &mut Connection) -> Connection {
    let poster_id = h.open_idle_window(opener).await;
    let mut poster = h.connect().await;
    identify(&mut poster, &poster_id).await;
    poster
}

// =====================================================================
// Chat
// =====================================================================

#[tokio::test]
async fn posts_beyond_the_chat_budget_to_a_stuck_window_do_not_keep_another_windows_post_from_being_delivered()
 {
    let dir = unique_temp_dir("stuck-chat-budget");
    let h =
        Harness::start_with_chat("stuck-chat-budget", chat_config(&dir, &chat_spec(None, 2))).await;
    let mut client = h.connect().await;
    let stuck = open_stuck_window(&h, &mut client, &dir).await;
    let healthy = open_healthy_window(&h, &mut client, &dir).await;
    let mut poster = open_poster(&h, &mut client).await;

    post(&mut poster, &format!("@{stuck} first"), &stuck).await;
    h.wait_for_scrollback(&stuck, b"PASTE 1", HANG_GUARD).await;
    for index in 0..3 {
        post(
            &mut poster,
            &format!("@{stuck} queued number {index}"),
            &stuck,
        )
        .await;
    }

    let seq = post(&mut poster, &format!("@{healthy} hello"), &healthy).await;
    let states = wait_until_settled(&mut poster, &[seq], &healthy).await;
    assert!(
        DELIVERED.contains(&states[0].as_str()),
        "a window that shows its input must get its post while another window is stuck, got {states:?}"
    );

    close_window(&mut client, &stuck).await;
}

#[tokio::test]
async fn posts_past_a_stuck_windows_turn_waiters_are_exhausted_for_that_window_alone() {
    let dir = unique_temp_dir("stuck-chat-waiters");
    let config = DaemonConfig {
        max_turn_waiters_per_window: 2,
        ..chat_config(&dir, &chat_spec(None, 32))
    };
    let h = Harness::start_with_chat("stuck-chat-waiters", config).await;
    let mut client = h.connect().await;
    let stuck = open_stuck_window(&h, &mut client, &dir).await;
    let healthy = open_healthy_window(&h, &mut client, &dir).await;
    let mut poster = open_poster(&h, &mut client).await;

    let mut stuck_seqs = vec![post(&mut poster, &format!("@{stuck} first"), &stuck).await];
    h.wait_for_scrollback(&stuck, b"PASTE 1", HANG_GUARD).await;
    for index in 0..4 {
        stuck_seqs.push(
            post(
                &mut poster,
                &format!("@{stuck} queued number {index}"),
                &stuck,
            )
            .await,
        );
    }
    wait_until_exhausted(&mut poster, &stuck_seqs, &stuck, 2).await;

    let healthy_seq = post(&mut poster, &format!("@{healthy} hello"), &healthy).await;
    let healthy_state = wait_until_settled(&mut poster, &[healthy_seq], &healthy)
        .await
        .remove(0);
    assert!(
        DELIVERED.contains(&healthy_state.as_str()),
        "another window's post must be delivered, got {healthy_state}"
    );

    // Closing the stuck window ends every delivery still waiting on it, so the
    // count read afterwards is final.
    close_window(&mut client, &stuck).await;
    let states = wait_until_settled(&mut poster, &stuck_seqs, &stuck).await;
    assert_eq!(
        states.iter().filter(|state| *state == EXHAUSTED).count(),
        2,
        "one post holds the turn and two wait for it, so exactly the other two are exhausted: \
         {states:?}"
    );
    assert_eq!(
        wait_until_settled(&mut poster, &[healthy_seq], &healthy).await,
        vec![healthy_state],
        "the other window's record must not change"
    );
}

#[tokio::test]
async fn a_stuck_windows_intro_and_posts_do_not_keep_another_windows_intro_from_being_sent() {
    let dir = unique_temp_dir("stuck-chat-intro");
    let config = DaemonConfig {
        max_turn_waiters_per_window: 1,
        ..chat_config(&dir, &chat_spec(Some("welcome to the room"), 2))
    };
    let h = Harness::start_with_chat("stuck-chat-intro", config).await;
    let mut client = h.connect().await;
    let stuck = open_stuck_window(&h, &mut client, &dir).await;
    h.wait_for_scrollback(&stuck, b"PASTE 1", HANG_GUARD).await;
    let mut poster = open_poster(&h, &mut client).await;

    let mut stuck_seqs = Vec::new();
    for index in 0..3 {
        stuck_seqs.push(
            post(
                &mut poster,
                &format!("@{stuck} queued number {index}"),
                &stuck,
            )
            .await,
        );
    }

    let healthy = open_healthy_window(&h, &mut client, &dir).await;
    let healthy_entry = entry(&h, &healthy);
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    while healthy_entry.chat_intro_outcome() != Some(ChatIntroOutcome::Sent) {
        assert!(
            tokio::time::Instant::now() < deadline,
            "another window's intro must be sent while a stuck window holds its own, got {:?}",
            healthy_entry.chat_intro_outcome()
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    close_window(&mut client, &stuck).await;
    let states = wait_until_settled(&mut poster, &stuck_seqs, &stuck).await;
    assert_eq!(
        states.iter().filter(|state| *state == EXHAUSTED).count(),
        2,
        "the intro holds the turn and one post waits for it, so exactly the other two are \
         exhausted: {states:?}"
    );
}

// =====================================================================
// Handoff
// =====================================================================

fn handoff_config(dir: &Path) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        approval_policy: Some(policy(&["send_handoff", "close_window"])),
        max_open_handoffs: 2,
        max_turn_waiters_per_window: 1,
        ..approver_base_config()
    }
}

async fn send_handoff(client: &mut Connection, window_id: &str, note: &str) {
    send(
        client,
        &ClientMessage::SendHandoff {
            window_id: Some(window_id.to_string()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/lib.rs".to_string(),
            note: note.to_string(),
        },
    )
    .await;
}

fn is_exhausted(reply: &DaemonMessage) -> bool {
    matches!(reply, DaemonMessage::Error { code, .. } if code == "resource_exhausted")
}

/// Sends a handoff to `window_id` on a connection of its own and forwards
/// the reply, whenever it comes, to `replies`.
async fn spawn_handoff(
    h: &Harness,
    window_id: &str,
    note: &str,
    replies: &tokio::sync::mpsc::UnboundedSender<DaemonMessage>,
) {
    let mut connection = h.connect().await;
    send_handoff(&mut connection, window_id, note).await;
    let replies = replies.clone();
    tokio::spawn(async move {
        let reply = read_response(&mut connection, HANG_GUARD * 3).await;
        let _ = replies.send(reply);
    });
}

async fn next_reply(
    replies: &mut tokio::sync::mpsc::UnboundedReceiver<DaemonMessage>,
) -> DaemonMessage {
    tokio::time::timeout(HANG_GUARD, replies.recv())
        .await
        .expect("timed out waiting for a handoff reply")
        .expect("a reply sender must be alive")
}

#[tokio::test]
async fn handoffs_beyond_the_daemon_budget_to_a_stuck_window_leave_another_windows_handoff_delivered()
 {
    let dir = unique_temp_dir("stuck-handoff-budget");
    let h = Harness::start("stuck-handoff-budget", handoff_config(&dir)).await;
    let mut client = h.connect().await;
    let stuck = open_stuck_window(&h, &mut client, &dir).await;
    let healthy = open_healthy_window(&h, &mut client, &dir).await;
    let (sender, mut replies) = tokio::sync::mpsc::unbounded_channel();

    spawn_handoff(&h, &stuck, "holds the turn", &sender).await;
    h.wait_for_scrollback(&stuck, b"PASTE 1", HANG_GUARD).await;
    spawn_handoff(&h, &stuck, "waits for the turn", &sender).await;
    spawn_handoff(&h, &stuck, "past the waiters", &sender).await;

    let first = next_reply(&mut replies).await;
    assert!(
        is_exhausted(&first),
        "the handoff past the stuck window's waiters must be refused, got {first:?}"
    );

    let mut other = h.connect().await;
    send_handoff(&mut other, &healthy, "for the other window").await;
    match read_response(&mut other, HANG_GUARD).await {
        DaemonMessage::HandoffSent { window_id, .. } => assert_eq!(window_id, healthy),
        reply => panic!(
            "a handoff to another window must be delivered while a stuck window holds its \
             turn and its waiters, got {reply:?}"
        ),
    }

    // Closing the stuck window ends the two handoffs still on it, so every
    // reply to it is in.
    close_window(&mut client, &stuck).await;
    for _ in 0..2 {
        let reply = next_reply(&mut replies).await;
        assert!(
            !is_exhausted(&reply),
            "only the handoff past the waiters is refused for lack of resources, got {reply:?}"
        );
    }
}
