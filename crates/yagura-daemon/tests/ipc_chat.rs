#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the chat room's IPC surface (`send_chat` / `broadcast_chat` /
//! `read_chat`) driven through `Harness::start_with_chat`, a real `WindowRegistry` +
//! `IpcServer` pair with the chat runtime wired in.
//!
//! ## Fixture conventions (contract for this file)
//!
//! - Every case starts a fresh chat-enabled daemon through
//!   [`start_chat`] / [`common::chat_config`] / [`common::ChatPlaybook`]. `ChatPlaybook`'s
//!   `intro` defaults to `Some("")` (no intro injection): only the
//!   intro-observing cases (including the abandoned-on-close case) pass a non-empty value.
//! - A **running / waiting_input / idle-capable participant** is a `submit_child` window
//!   opened through [`common::open_submit_child_window_with_composer`] against a definition
//!   [`common::write_submit_definition`] wrote: it starts at `waiting_input` and can be driven
//!   to `running` by writing raw `REACT` bytes directly to its PTY (matching the definition's
//!   own `running` rule), bypassing the chat delivery path entirely.
//! - Only a window that declares delivery takes part in chat: the openers above declare
//!   one with no way to wake its CLI, so a post reaches them typed while they wait. A
//!   window opened from a raw argv (`Harness::open`) takes no part; [`open_delivering`]
//!   opens any other command as a participant.
//! - A **plain idle participant** (never a delivery target, only a roster entry) is opened
//!   through [`Harness::open_idle_window`] / [`Harness::open_idle_window_with_composer`]
//!   against [`common::CONTROL_DETECTION_NAME`], which [`common::chat_config`] wires into
//!   every daemon's detection directory automatically (via [`common::detection_config`]).
//! - A window whose **detection definition cannot be read** is opened with
//!   `detection: Some("no-such-definition")`: no file of that name exists under the
//!   directory, so resolution fails and the window's `detection_definition()` stays `None`; it never appears in a roster and can only be addressed as `unresolved`.
//! - A window opened with **no `detection` argument at all** resolves to the daemon's default
//!   `detection_name`. [`participant_excluded_by_default`] builds this case by copying the
//!   repository's real `generic-shell.toml` next to the synthetic definitions and pointing
//!   `detection_name` back at the built-in default (`"generic-shell"`), which the default
//!   `[chat] exclude_detections` excludes.
//! - A definition whose **file stem differs from its declared `name`** is written by
//!   [`write_stem_mismatch_definition`]: participation is decided by the stem (the name the
//!   `detection` argument / `--detection` flag actually names), never by the file's own
//!   `name` field.
//! - The chat envelope marker is `"[yagura chat] "` (parallel to the handoff envelope's
//!   `"[yagura handoff] "`); [`wait_for_accepted_chat_envelope_json`] is this file's own
//!   marker-aware reader, mirroring `common::wait_for_accepted_handoff_envelope_json`.

mod common;

use std::path::Path;
use std::time::Duration;

use common::{
    CONTROL_DETECTION_NAME, ChatPlaybook, Harness, SubmitDefinitionInput, chat_config,
    composer_section_toml, decide, expect_approval_requested, expect_error, identify,
    open_submit_child_window_with_composer, open_submit_child_window_with_composer_in_cwd,
    read_response, register_approver, second_workspace_dir, send, unique_temp_dir,
    wait_until_status, write_submit_definition,
};
use yagura_daemon::DaemonConfig;
use yagura_protocol::{
    ChatDeliveryInfo, ChatHalt, ChatMessage, ChatParticipant, ChatRoomInfo, ClientMessage,
    DaemonMessage, RoomRef, WindowStatus,
};

/// Starts a chat-enabled daemon from a synthetic playbook (see
/// [`common::ChatPlaybook`] / [`common::chat_config`]).
async fn start_chat(tag: &str, dir: &Path, spec: &ChatPlaybook<'_>) -> Harness {
    Harness::start_with_chat(tag, chat_config(dir, spec)).await
}

/// The `[delivery]` table of a profile with no way to wake its CLI while it waits, read
/// with the daemon's own defaults otherwise.
fn wake_none_delivery() -> yagura_daemon::profile_delivery::ProfileDelivery {
    yagura_daemon::profile_delivery::ProfileDelivery {
        output: yagura_daemon::inbox::DoorOutputs::all(&yagura_daemon::inbox::OutputForm::Plain),
        max_chars: yagura_daemon::inbox::DEFAULT_MAX_CHARS,
        wake: yagura_daemon::profile_delivery::WakeMode::None,
        wake_stream: yagura_protocol::DeliveryStream::Stderr,
        wake_exit: yagura_daemon::inbox::DEFAULT_WAKE_EXIT,
        launch_prompt: None,
        launch_context: None,
    }
}

/// Opens `command` as a window that declares delivery (see [`wake_none_delivery`]), so it
/// takes part in chat whatever its command line.
async fn open_delivering(
    h: &Harness,
    command: Vec<String>,
    detection: Option<&str>,
    cwd: Option<&Path>,
    name: Option<&str>,
) -> String {
    h.registry
        .open_with_extra_env(
            yagura_daemon::OpenWindowRequest {
                command,
                cwd: cwd.map(|cwd| yagura_daemon::RequestedCwd::from_caller(cwd.to_path_buf())),
                name: name.map(str::to_string),
                vendor: None,
                detection: detection.map(str::to_string),
                on_name_taken: Default::default(),
            },
            Vec::new(),
            Vec::new(),
            yagura_daemon::window::WindowProvenance {
                delivery: Some(wake_none_delivery()),
                ..yagura_daemon::window::WindowProvenance::default()
            },
        )
        .await
        .expect("registry.open_with_extra_env should spawn the child")
        .window_id
}

/// A participant that never leaves `Unknown`: the control definition is loaded, but
/// nothing ever matches its rule.
async fn open_unknown_participant(h: &Harness) -> String {
    open_delivering(
        h,
        yagura_test_support::process::echo_child_command(),
        Some(CONTROL_DETECTION_NAME),
        None,
        None,
    )
    .await
}

/// Calls `door` for `window_id` the way its CLI's hook does, and returns what it printed.
async fn call_door(
    client: &mut yagura_test_support::ipc::Connection,
    door: &str,
    window_id: &str,
) -> String {
    let token = common::VALID_TOKEN.to_string();
    let window_id = window_id.to_string();
    let message = match door {
        "context" => ClientMessage::DeliverContext { window_id, token },
        "take" => ClientMessage::DeliverTake { window_id, token },
        other => panic!("no door named {other}"),
    };
    let (output, _, _) = common::expect_delivery(common::call_door(client, &message).await);
    output
}

/// A post to `window_id`, whose status is `Unknown`, waits in its inbox: it is never
/// typed, and its CLI asking for it gets it.
async fn assert_waits_for_its_cli(
    h: &Harness,
    client: &mut yagura_test_support::ipc::Connection,
    seq: u64,
    window_id: &str,
    needle: &str,
) {
    let entry = h.registry.get(window_id).expect("the window is registered");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry.inbox().pending_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the post never reached {window_id}'s inbox"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let read = expect_chat_messages(read_chat_msg(client, None, Some(0), None).await);
    assert_eq!(
        delivery_state_for(&read.messages, seq, window_id),
        "pending"
    );

    let mut door = h.connect().await;
    let taken = call_door(&mut door, "take", window_id).await;
    assert!(taken.contains(needle), "{taken:?}");
    let state = wait_for_settled_delivery(client, seq, window_id, Duration::from_secs(10)).await;
    assert_eq!(state, "taken");
    assert!(
        !String::from_utf8_lossy(&h.scrollback(window_id)).contains(needle),
        "nothing is typed into a window whose status is Unknown"
    );
}

const CHAT_ENVELOPE_MARKER: &str = "[yagura chat] ";

/// This file's own marker-aware scrollback reader, mirroring
/// `common::wait_for_accepted_handoff_envelope_json` for the chat envelope
/// marker instead of the handoff one (composer-draw mode paints an
/// unsubmitted draft with the same marker text, so only an occurrence that is
/// not immediately preceded by the composer's own marker+separator counts).
async fn wait_for_accepted_chat_envelope_json(
    h: &Harness,
    window_id: &str,
    timeout: Duration,
) -> serde_json::Value {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let text = String::from_utf8_lossy(&h.scrollback(window_id)).into_owned();
        if let Some(json) = accepted_chat_envelope_json(&text) {
            return json;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for an accepted chat envelope line in window {window_id} \
                 scrollback: {text:?}"
            );
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Byte offsets of `needle` in `text` that are accepted lines rather than a
/// composer draft: a draft copy follows the composer's marker and separator.
fn accepted_offsets<'a>(text: &'a str, needle: &'a str) -> impl Iterator<Item = usize> + 'a {
    let draft_prefix = format!(
        "{}{}",
        yagura_test_support::process::COMPOSER_MARKER,
        yagura_test_support::process::COMPOSER_SEPARATOR,
    );
    text.match_indices(needle)
        .map(|(index, _)| index)
        .filter(move |index| !text[..*index].ends_with(&draft_prefix))
}

fn accepted_chat_envelope_json(text: &str) -> Option<serde_json::Value> {
    let start = accepted_offsets(text, CHAT_ENVELOPE_MARKER).next()?;
    let after = text.get(start + CHAT_ENVELOPE_MARKER.len()..)?;
    let end = after.find('\r').unwrap_or(after.len());
    let json_part = after.get(..end)?.trim_end_matches(|c: char| c != '}');
    serde_json::from_str(json_part).ok()
}

/// Writes a minimal detection definition (a single `idle` rule plus
/// [`composer_section_toml`], so the participation this fixture exercises is
/// never itself refused for the missing table) at `{dir}/{file_stem}.toml`,
/// whose declared `name` is `declared_name`: a caller passing
/// `file_stem != declared_name` builds the stem/name-mismatch fixture.
fn write_stem_mismatch_definition(dir: &Path, file_stem: &str, declared_name: &str) {
    std::fs::write(
        dir.join(format!("{file_stem}.toml")),
        format!(
            "schema_version = 1\nname = \"{declared_name}\"\n\n[debounce]\nconfirm_ms = 30\n\n\
             [[rules]]\nstatus = \"idle\"\npattern = 'READY'\npriority = 10\nregion = \"bottom\"\n{}",
            participant_composer_toml()
        ),
    )
    .expect("write stem/name-mismatch chat participant definition");
}

/// A chat participant is a window the daemon can deliver a judged line to,
/// which takes a probe as well as a composer.
fn participant_composer_toml() -> String {
    format!(
        "{}\n[input]\nprobe = \"{}\"\n",
        composer_section_toml(),
        common::SUBMIT_CHILD_PROBE
    )
}

/// [`write_stem_mismatch_definition`] with `file_stem == declared_name` and,
/// when `with_composer` is `false`, no `[composer]` table at all: the one
/// axis that varies while every other property (the `idle` rule, the debounce
/// table) stays identical between the two windows it opens.
fn write_participation_fixture(dir: &Path, stem: &str, with_composer: bool) {
    let composer = if with_composer {
        participant_composer_toml()
    } else {
        String::new()
    };
    std::fs::write(
        dir.join(format!("{stem}.toml")),
        format!(
            "schema_version = 1\nname = \"{stem}\"\n\n[debounce]\nconfirm_ms = 30\n\n\
             [[rules]]\nstatus = \"idle\"\npattern = 'READY'\npriority = 10\nregion = \"bottom\"\n{composer}"
        ),
    )
    .expect("write chat participation fixture definition");
}

/// Copies the repository's real `generic-shell.toml` into `dir` and points
/// `config.detection_name` back at the built-in default (`"generic-shell"`,
/// the same value `DaemonConfig::default()` carries), so a window opened
/// with no `detection` argument resolves to the name the default
/// `[chat] exclude_detections` excludes.
fn participant_excluded_by_default(mut config: DaemonConfig, dir: &Path) -> DaemonConfig {
    let source = std::fs::read_to_string(common::repo_detection_dir().join("generic-shell.toml"))
        .expect("the repository's generic-shell.toml must be readable");
    std::fs::write(dir.join("generic-shell.toml"), source)
        .expect("copy generic-shell.toml into the synthetic detection directory");
    config.detection_name = "generic-shell".to_string();
    config
}

/// Reads the reply to a request, skipping any `chat_changed` push that
/// overtakes it: a connection that is also an approver is pushed to while its
/// own requests are in flight.
async fn read_reply(
    client: &mut yagura_test_support::ipc::Connection,
    timeout: Duration,
) -> DaemonMessage {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(!remaining.is_zero(), "timed out waiting for a reply");
        let frame = read_response(client, remaining).await;
        if !matches!(frame, DaemonMessage::ChatChanged { .. }) {
            return frame;
        }
    }
}

/// [`ClientMessage::SendChat`], threading `room_id` through explicitly:
/// a window's own default (an omitted `room_id`) is its own
/// workspace room rather than `"main"`, so a caller that means to reach a
/// specific room -- most often an approver following a window into its own
/// room -- must say so.
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
    read_reply(client, Duration::from_secs(5)).await
}

/// [`send_chat_msg`], for [`ClientMessage::BroadcastChat`].
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
    read_reply(client, Duration::from_secs(5)).await
}

/// [`send_chat_msg`], for [`ClientMessage::ReadChat`].
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
    read_reply(client, Duration::from_secs(5)).await
}

struct ReadChatResult {
    room_id: String,
    messages: Vec<ChatMessage>,
    halted: Option<ChatHalt>,
    needs_user: bool,
    roster: Vec<ChatParticipant>,
}

fn expect_chat_messages(msg: DaemonMessage) -> ReadChatResult {
    match msg {
        DaemonMessage::ChatMessages {
            room_id,
            messages,
            halted,
            needs_user,
            roster,
            ..
        } => ReadChatResult {
            room_id,
            messages,
            halted,
            needs_user,
            roster,
        },
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

/// A post's resolved `room_id`, alongside the rest of `chat_posted`'s
/// payload: a window's own default room is derived from the
/// daemon's cwd rather than fixed at `"main"`, so a case that must reach the
/// same room from a second connection (typically an approver) reads it back
/// from here instead of assuming a literal.
struct ChatPostedResult {
    room_id: String,
    seq: u64,
    targets: Vec<String>,
    user: bool,
    unresolved: Vec<String>,
    halted: bool,
}

fn expect_chat_posted(msg: DaemonMessage) -> ChatPostedResult {
    match msg {
        DaemonMessage::ChatPosted {
            room_id,
            seq,
            targets,
            user,
            unresolved,
            halted,
        } => ChatPostedResult {
            room_id,
            seq,
            targets,
            user,
            unresolved,
            halted,
        },
        other => panic!("expected chat_posted, got {other:?}"),
    }
}

fn delivery_state_for<'a>(messages: &'a [ChatMessage], seq: u64, window_id: &str) -> &'a str {
    let message = messages
        .iter()
        .find(|m| m.seq == seq)
        .unwrap_or_else(|| panic!("no message with seq {seq} in {messages:?}"));
    message
        .deliveries
        .iter()
        .find(|d: &&ChatDeliveryInfo| d.window_id == window_id)
        .unwrap_or_else(|| {
            panic!("no delivery entry for {window_id} in message {seq}: {message:?}")
        })
        .state
        .as_str()
}

/// Re-reads the room until `window_id`'s delivery for `seq` has settled, and
/// returns the state it settled on. A destination is recorded as pending
/// before its delivery runs, so the first read after a post can legitimately
/// find it unsettled -- or, for a message no read has caught up with yet,
/// find no entry at all.
async fn wait_for_settled_delivery(
    client: &mut yagura_test_support::ipc::Connection,
    seq: u64,
    window_id: &str,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let read = expect_chat_messages(read_chat_msg(client, None, Some(0), None).await);
        let settled = read
            .messages
            .iter()
            .find(|message| message.seq == seq)
            .and_then(|message| {
                message
                    .deliveries
                    .iter()
                    .find(|entry: &&ChatDeliveryInfo| entry.window_id == window_id)
            })
            .map(|entry| entry.state.clone())
            .filter(|state| state != "pending");
        if let Some(state) = settled {
            return state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for {window_id}'s delivery of message {seq} to settle"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// =============================================================================
// Window-originated send_chat is delivered to a resolved, non-Unknown target
// =============================================================================

#[tokio::test]
async fn send_chat_delivers_the_envelope_to_the_named_target_and_reports_it_taken() {
    let dir = unique_temp_dir("chat-g1-basic");
    let h = start_chat("chat-g1-basic", &dir, &ChatPlaybook::default()).await;

    let mut poster_conn = h.connect().await;
    write_submit_definition(
        &dir,
        "chat-g1-target",
        "chat-g1-marker",
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 3000,
            resend_max: 2,
        },
    );
    let target_id = open_submit_child_window_with_composer(
        &h,
        &mut poster_conn,
        &dir,
        "chat-g1-target",
        "chat-g1-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 3000,
            resend_max: 2,
        },
        0,
    )
    .await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("hello @{target_id}")).await,
    );
    assert_eq!(posted.targets, vec![target_id.clone()]);
    assert!(!posted.user);
    assert!(posted.unresolved.is_empty());
    assert!(!posted.halted);

    let json = wait_for_accepted_chat_envelope_json(&h, &target_id, Duration::from_secs(10)).await;
    assert_eq!(
        json.get("room_id").and_then(serde_json::Value::as_str),
        // An identified window's own default room is its own
        // workspace room, derived from the daemon's cwd -- not a literal
        // "main" -- so the envelope is checked against what the daemon
        // itself resolved and echoed back on chat_posted.
        Some(posted.room_id.as_str())
    );
    assert_eq!(
        json.get("broadcast").and_then(serde_json::Value::as_bool),
        Some(false)
    );
    assert_eq!(
        json.pointer("/reply/tool")
            .and_then(serde_json::Value::as_str),
        Some("send_chat"),
        "the chat envelope's reply hint must always name send_chat, got: {json}"
    );
    assert_eq!(
        json.pointer("/reply/room_id")
            .and_then(serde_json::Value::as_str),
        Some(posted.room_id.as_str()),
        "the chat envelope's reply hint must carry the room a reply belongs in, got: {json}"
    );

    let state = wait_for_settled_delivery(
        &mut poster_conn,
        posted.seq,
        &target_id,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        state, "taken",
        "an envelope typed up to its submit key is taken, whatever the composer showed after"
    );
}

#[tokio::test]
async fn send_chat_still_delivers_to_a_target_that_is_currently_running() {
    let dir = unique_temp_dir("chat-g1-running");
    let h = start_chat("chat-g1-running", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;

    let target_id = open_submit_child_window_with_composer(
        &h,
        &mut poster_conn,
        &dir,
        "chat-g1-running-target",
        "chat-g1-running-marker",
        0,
        1,
        1,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 2000,
            resend_max: 1,
        },
        0,
    )
    .await;
    // A reacting fixture prints `REACT` when a bracketed paste ends, matched byte by byte, so
    // unlike a typed submit this never depends on how the fixture's reads split the input.
    h.write_input(&target_id, b"\x1b[200~\x1b[201~").await;
    wait_until_status(
        &mut poster_conn,
        &target_id,
        WindowStatus::Running,
        Duration::from_secs(10),
    )
    .await;

    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;
    let posted = expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("still there @{target_id}")).await,
    );
    assert_eq!(
        posted.targets,
        vec![target_id],
        "a Running (not Unknown) target must still be addressed"
    );
}

// =============================================================================
// An Unknown destination is never pushed to: the post waits in its inbox for its CLI
// =============================================================================

#[tokio::test]
async fn send_chat_to_an_unknown_window_waits_in_its_inbox_and_is_never_typed() {
    let dir = unique_temp_dir("chat-g2-send");
    let h = start_chat("chat-g2-send", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;

    let unknown_id = open_unknown_participant(&h).await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut client, None, &format!("hi @{unknown_id} G2SEND")).await,
    );
    assert_eq!(posted.targets, vec![unknown_id.clone()]);

    assert_waits_for_its_cli(&h, &mut client, posted.seq, &unknown_id, "G2SEND").await;
}

#[tokio::test]
async fn broadcast_chat_to_an_unknown_window_waits_in_its_inbox_and_is_never_typed() {
    let dir = unique_temp_dir("chat-g2-broadcast");
    let h = start_chat(
        "chat-g2-broadcast",
        &dir,
        &ChatPlaybook {
            auto_approve: &["broadcast_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;

    let unknown_id = open_unknown_participant(&h).await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let posted =
        expect_chat_posted(broadcast_chat_msg(&mut client, None, "status update G2CAST").await);
    assert!(posted.targets.contains(&unknown_id));

    assert_waits_for_its_cli(&h, &mut client, posted.seq, &unknown_id, "G2CAST").await;
}

// =============================================================================
// Participation
// =============================================================================

#[tokio::test]
async fn a_window_excluded_by_name_is_unresolved_and_absent_from_the_roster() {
    let dir = unique_temp_dir("chat-g3-excluded");
    let h = start_chat(
        "chat-g3-excluded",
        &dir,
        &ChatPlaybook {
            exclude_detections: Some(&[CONTROL_DETECTION_NAME]),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let excluded_id = h.open_idle_window(&mut client).await;
    let poster_id = h
        .open(
            yagura_test_support::process::echo_child_command(),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    // The poster is itself excluded too, but a poster's own request always
    // reaches the gate as its own window; participation is about roster
    // membership and mention resolution, checked below.
    identify(&mut client, &poster_id).await;

    let posted =
        expect_chat_posted(send_chat_msg(&mut client, None, &format!("hey @{excluded_id}")).await);
    assert!(
        posted.targets.is_empty(),
        "an excluded window must never become a resolved target"
    );
    assert_eq!(posted.unresolved, vec![excluded_id.clone()]);

    let read = expect_chat_messages(read_chat_msg(&mut client, None, Some(0), None).await);
    assert!(
        !read.roster.iter().any(|p| p.window_id == excluded_id),
        "an excluded window must not appear in the roster, got {:?}",
        read.roster
    );
}

#[tokio::test]
async fn a_window_opened_with_no_detection_argument_defaults_to_the_excluded_generic_shell() {
    let dir = unique_temp_dir("chat-g3-default");
    let config = participant_excluded_by_default(chat_config(&dir, &ChatPlaybook::default()), &dir);
    let h = Harness::start_with_chat("chat-g3-default", config).await;
    let mut client = h.connect().await;

    let default_id = open_delivering(
        &h,
        yagura_test_support::process::echo_child_command(),
        None,
        None,
        None,
    )
    .await;
    let poster_id = h
        .open(
            yagura_test_support::process::echo_child_command(),
            Some(CONTROL_DETECTION_NAME),
        )
        .await;
    identify(&mut client, &poster_id).await;

    let posted =
        expect_chat_posted(send_chat_msg(&mut client, None, &format!("hey @{default_id}")).await);
    assert_eq!(
        posted.unresolved,
        vec![default_id],
        "a window opened with no detection argument resolves to the built-in default \
         (generic-shell), which is excluded by default"
    );
}

#[tokio::test]
async fn a_definition_whose_stem_differs_from_its_declared_name_is_judged_by_the_stem() {
    let dir = unique_temp_dir("chat-g3-stem");
    write_stem_mismatch_definition(&dir, "weird-stem", "totally-different-name");
    let h = start_chat("chat-g3-stem", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;

    let stem_id = open_delivering(
        &h,
        yagura_test_support::process::echo_child_command(),
        Some("weird-stem"),
        None,
        None,
    )
    .await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let posted =
        expect_chat_posted(send_chat_msg(&mut client, None, &format!("hey @{stem_id}")).await);
    assert_eq!(
        posted.targets,
        vec![stem_id.clone()],
        "\"weird-stem\" is not on the default exclude list, so the window must resolve as \
         a participant regardless of its file's own name field"
    );

    let read = expect_chat_messages(read_chat_msg(&mut client, None, Some(0), None).await);
    assert!(read.roster.iter().any(|p| p.window_id == stem_id));
}

#[tokio::test]
async fn a_window_that_declares_delivery_takes_part_without_a_composer_and_one_that_does_not_never_does()
 {
    let dir = unique_temp_dir("chat-g3-no-composer");
    write_participation_fixture(&dir, "g3-silent", false);
    write_participation_fixture(&dir, "g3-drawn", true);
    let h = start_chat(
        "chat-g3-no-composer",
        &dir,
        &ChatPlaybook {
            // Empty, not the default's `["generic-shell"]`: only the delivery
            // declaration -- not the exclude list -- may decide who takes part.
            exclude_detections: Some(&[]),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;

    // Declares delivery, and its definition no composer.
    let silent_id = open_delivering(
        &h,
        yagura_test_support::process::echo_child_command(),
        Some("g3-silent"),
        None,
        None,
    )
    .await;
    // Its definition declares a composer and a probe, and it was opened from a raw argv.
    let drawn_id = h
        .open(
            yagura_test_support::process::echo_child_command(),
            Some("g3-drawn"),
        )
        .await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(
            &mut client,
            None,
            &format!("hey @{silent_id} and @{drawn_id}"),
        )
        .await,
    );
    assert_eq!(
        posted.targets,
        vec![silent_id.clone()],
        "taking part is decided by declaring delivery, never by declaring a composer"
    );
    assert_eq!(posted.unresolved, vec![drawn_id.clone()]);

    let read = expect_chat_messages(read_chat_msg(&mut client, None, Some(0), None).await);
    assert!(
        !read.roster.iter().any(|p| p.window_id == drawn_id),
        "a window that declares no delivery must not appear in the roster, got {:?}",
        read.roster
    );
    assert!(read.roster.iter().any(|p| p.window_id == silent_id));
}

// =============================================================================
// Approval
// =============================================================================

#[tokio::test]
async fn window_origin_send_chat_is_allowed_without_approval_when_auto_approved() {
    let dir = unique_temp_dir("chat-g4-auto");
    // The bundled default: send_chat ships auto-approved, broadcast_chat does not.
    let h = start_chat(
        "chat-g4-auto",
        &dir,
        &ChatPlaybook {
            auto_approve: &["send_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let posted = expect_chat_posted(send_chat_msg(&mut client, None, "no approval needed").await);
    assert!(!posted.halted);
}

#[tokio::test]
async fn window_origin_broadcast_chat_needs_approval_and_is_refused_without_an_approver() {
    let dir = unique_temp_dir("chat-g4-no-approver");
    let h = start_chat("chat-g4-no-approver", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = broadcast_chat_msg(&mut client, None, "everyone please look").await;
    expect_error(result, "approval_required");
}

#[tokio::test]
async fn window_origin_broadcast_chat_awaits_a_targetless_approval_and_delivers_once_approved() {
    let dir = unique_temp_dir("chat-g4-approved");
    let h = start_chat("chat-g4-approved", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send(
        &mut poster_conn,
        &ClientMessage::BroadcastChat {
            room_id: None,
            body: "everyone please look".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "broadcast_chat");
    assert!(
        request.target.is_none(),
        "a chat request is not addressed to any one window: target must be None"
    );

    // The requester's connection must keep answering other requests while
    // its own broadcast is awaiting a decision.
    send(&mut poster_conn, &ClientMessage::ListWindows).await;
    let first = read_response(&mut poster_conn, Duration::from_secs(5)).await;
    assert!(
        matches!(first, DaemonMessage::WindowList { .. }),
        "list_windows must be answered while the broadcast is still pending, got {first:?}"
    );

    decide(&mut approver, &request.request_id, true).await;
    let posted = expect_chat_posted(read_response(&mut poster_conn, Duration::from_secs(5)).await);
    assert!(
        posted.targets.contains(&recipient_id),
        "an approved broadcast must reach every participant but the poster"
    );
    assert!(!posted.targets.contains(&poster_id));
}

#[tokio::test]
async fn window_origin_broadcast_chat_denied_returns_approval_denied() {
    let dir = unique_temp_dir("chat-g4-denied");
    let h = start_chat("chat-g4-denied", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send(
        &mut poster_conn,
        &ClientMessage::BroadcastChat {
            room_id: None,
            body: "please decide".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    decide(&mut approver, &request.request_id, false).await;

    let result = read_response(&mut poster_conn, Duration::from_secs(5)).await;
    expect_error(result, "approval_denied");
}

#[tokio::test]
async fn window_origin_broadcast_chat_approval_times_out_and_withdraws() {
    let dir = unique_temp_dir("chat-g4-timeout");
    let h = start_chat(
        "chat-g4-timeout",
        &dir,
        &ChatPlaybook {
            approval_timeout_secs: Some(1),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send(
        &mut poster_conn,
        &ClientMessage::BroadcastChat {
            room_id: None,
            body: "will time out".to_string(),
        },
    )
    .await;
    let _request = expect_approval_requested(&mut approver).await;

    let result = read_response(&mut poster_conn, Duration::from_secs(10)).await;
    expect_error(result, "approval_timeout");

    let withdrawn = read_response(&mut approver, Duration::from_secs(5)).await;
    assert!(
        matches!(
            withdrawn,
            DaemonMessage::ApprovalWithdrawn {
                reason: yagura_protocol::WithdrawReason::Timeout,
                ..
            }
        ),
        "the approver must be told the dialog ended by timeout, got {withdrawn:?}"
    );
}

#[tokio::test]
async fn an_external_connections_send_chat_posts_without_approval_when_the_playbook_allows_it() {
    let dir = unique_temp_dir("chat-g4-external-required");
    let h = start_chat(
        "chat-g4-external-required",
        &dir,
        &ChatPlaybook {
            auto_approve: &["send_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;
    // No identify(): the connection stays External.
    let mut client = h.connect().await;

    let result = send_chat_msg(&mut client, None, "from outside").await;
    let posted = expect_chat_posted(result);
    assert!(!posted.halted);
}

#[tokio::test]
async fn an_external_connections_send_chat_awaits_approval_when_not_auto_approved_and_delivers_once_approved()
 {
    let dir = unique_temp_dir("chat-g4-external-approved");
    let h = start_chat(
        "chat-g4-external-approved",
        &dir,
        &ChatPlaybook {
            auto_approve: &[],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut external = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    send(
        &mut external,
        &ClientMessage::SendChat {
            room_id: None,
            body: "from outside".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "send_chat");
    assert!(request.target.is_none());

    decide(&mut approver, &request.request_id, true).await;
    let posted = expect_chat_posted(read_response(&mut external, Duration::from_secs(5)).await);
    assert!(!posted.halted);
}

#[tokio::test]
async fn window_origin_send_chat_needs_approval_when_not_auto_approved_and_is_refused_without_an_approver()
 {
    let dir = unique_temp_dir("chat-g4-send-no-approver");
    let h = start_chat(
        "chat-g4-send-no-approver",
        &dir,
        &ChatPlaybook {
            auto_approve: &[],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = send_chat_msg(&mut client, None, "no auto-approval configured").await;
    expect_error(result, "approval_required");
}

#[tokio::test]
async fn window_origin_send_chat_awaits_a_targetless_approval_and_delivers_once_approved() {
    let dir = unique_temp_dir("chat-g4-send-approved");
    let h = start_chat(
        "chat-g4-send-approved",
        &dir,
        &ChatPlaybook {
            auto_approve: &[],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send(
        &mut poster_conn,
        &ClientMessage::SendChat {
            room_id: None,
            body: format!("hi @{recipient_id}"),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "send_chat");
    assert!(
        request.target.is_none(),
        "a chat request is not addressed to any one window: target must be None"
    );
    let envelope = request
        .text
        .as_deref()
        .expect("a send_chat approval request must carry the envelope as its body");
    assert!(
        envelope.starts_with(CHAT_ENVELOPE_MARKER),
        "the approval body must be the chat envelope: {envelope:?}"
    );

    decide(&mut approver, &request.request_id, true).await;
    let posted = expect_chat_posted(read_response(&mut poster_conn, Duration::from_secs(5)).await);
    assert!(posted.targets.contains(&recipient_id));

    let approved_json: serde_json::Value = serde_json::from_str(
        envelope
            .strip_prefix(CHAT_ENVELOPE_MARKER)
            .expect("already asserted the marker prefix is present"),
    )
    .expect("the approval body must be the JSON the daemon composed");
    let delivered =
        wait_for_accepted_chat_envelope_json(&h, &recipient_id, Duration::from_secs(10)).await;
    assert_eq!(
        approved_json, delivered,
        "the text a human approved and the envelope actually written must carry identical fields"
    );
}

#[tokio::test]
async fn window_origin_send_chat_denied_returns_approval_denied() {
    let dir = unique_temp_dir("chat-g4-send-denied");
    let h = start_chat(
        "chat-g4-send-denied",
        &dir,
        &ChatPlaybook {
            auto_approve: &["broadcast_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send(
        &mut poster_conn,
        &ClientMessage::SendChat {
            room_id: None,
            body: "please decide".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    decide(&mut approver, &request.request_id, false).await;

    let result = read_response(&mut poster_conn, Duration::from_secs(5)).await;
    expect_error(result, "approval_denied");
}

// =============================================================================
// Repetition halts the room and is a hand-back to the user
// =============================================================================

#[tokio::test]
async fn repetition_halts_the_room_and_notifies_the_approver() {
    let dir = unique_temp_dir("chat-g5-repetition");
    let h = start_chat(
        "chat-g5-repetition",
        &dir,
        &ChatPlaybook {
            auto_approve: &["broadcast_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let _recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let mut last_seq = 0u64;
    for _ in 0..4 {
        let posted =
            expect_chat_posted(broadcast_chat_msg(&mut poster_conn, None, "same message").await);
        last_seq = posted.seq;
    }

    // The repeat that trips the rule is the one that pushes; the posts after
    // it arrive into an already-halted room, which is announced once.
    let changed_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(5)).await;
    assert!(
        changed_seq > 0 && changed_seq <= last_seq,
        "the push must name one of the posts made here, got {changed_seq}"
    );

    let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, Some(0), None).await);
    assert!(read.needs_user);
    let halt = read.halted.expect("the room must be halted");
    assert_eq!(halt.reason, "repetition");
    let system_message = read
        .messages
        .iter()
        .find(|m| m.from_kind == "system")
        .expect("a system message must record the halt");
    assert!(
        system_message.mentions.iter().any(|m| m == "user"),
        "the system message must mention user, got {:?}",
        system_message.mentions
    );
    assert!(last_seq > 0);
}

#[tokio::test]
async fn after_a_halt_ai_posts_are_undelivered_and_a_user_post_clears_it() {
    let dir = unique_temp_dir("chat-g5-recovery");
    let h = start_chat(
        "chat-g5-recovery",
        &dir,
        &ChatPlaybook {
            auto_approve: &["broadcast_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let _recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    for _ in 0..4 {
        broadcast_chat_msg(&mut poster_conn, None, "same message").await;
    }
    let _halted_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(5)).await;

    let after_halt =
        expect_chat_posted(broadcast_chat_msg(&mut poster_conn, None, "still talking").await);
    assert!(after_halt.halted);

    // The approver's own post (from_kind = user) clears the halt -- must
    // land in the same room the poster's window belongs to, not the
    // approver's own "main" default.
    let user_post = expect_chat_posted(
        broadcast_chat_msg(
            &mut approver,
            Some(after_halt.room_id.as_str()),
            "let's continue",
        )
        .await,
    );
    assert!(!user_post.halted);

    let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, Some(0), None).await);
    assert!(
        read.halted.is_none(),
        "a user post must clear the halt, got {:?}",
        read.halted
    );
    assert!(!read.needs_user);
}

// =============================================================================
// read_chat's after_seq table
// =============================================================================

#[tokio::test]
async fn a_window_readers_default_after_seq_is_its_own_last_post() {
    let dir = unique_temp_dir("chat-g6-window-default");
    let h = start_chat("chat-g6-window-default", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let first = expect_chat_posted(send_chat_msg(&mut client, None, "one").await);
    let _second = expect_chat_posted(send_chat_msg(&mut client, None, "two").await);

    // A read with no after_seq, from the same window, must start after its
    // own last post -- not from the beginning.
    send(
        &mut client,
        &ClientMessage::ReadChat {
            room_id: None,
            after_seq: None,
            limit: None,
        },
    )
    .await;
    let read = expect_chat_messages(read_response(&mut client, Duration::from_secs(5)).await);
    assert!(
        read.messages.iter().all(|m| m.seq > first.seq + 1),
        "a window's default read must start after its own most recent post, got {:?}",
        read.messages
    );
}

#[tokio::test]
async fn an_approvers_default_after_seq_is_the_recent_limit_ascending() {
    let dir = unique_temp_dir("chat-g6-approver-default");
    let h = start_chat("chat-g6-approver-default", &dir, &ChatPlaybook::default()).await;
    let mut poster = h.connect().await;
    let poster_id = h.open_idle_window(&mut poster).await;
    identify(&mut poster, &poster_id).await;
    let mut room_id = String::new();
    for text in ["a", "b", "c"] {
        let posted = expect_chat_posted(send_chat_msg(&mut poster, None, text).await);
        room_id = posted.room_id;
    }

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    // The approver's own default room is "main"; this case is about the
    // after_seq default, so the room is named explicitly to follow the
    // poster's window into its own room.
    let read = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), None, Some(2)).await,
    );
    assert_eq!(read.messages.len(), 2);
    assert!(
        read.messages[0].seq < read.messages[1].seq,
        "messages must be ascending, got {:?}",
        read.messages
    );
}

#[tokio::test]
async fn an_external_readers_omitted_after_seq_is_invalid_argument() {
    let dir = unique_temp_dir("chat-g6-external");
    let h = start_chat("chat-g6-external", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;

    let result = read_chat_msg(&mut client, None, None, None).await;
    expect_error(result, "invalid_argument");
}

#[tokio::test]
async fn read_chat_rejects_a_limit_over_the_maximum() {
    let dir = unique_temp_dir("chat-g6-limit");
    let h = start_chat("chat-g6-limit", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = read_chat_msg(
        &mut client,
        None,
        Some(0),
        Some(yagura_protocol::CHAT_READ_LIMIT_MAX + 1),
    )
    .await;
    expect_error(result, "invalid_argument");
}

// =============================================================================
// @user sets needs_user; an approver's reply clears it
// =============================================================================

#[tokio::test]
async fn mentioning_user_sets_needs_user_and_an_approver_reply_clears_it() {
    let dir = unique_temp_dir("chat-g7-user-mention");
    let h = start_chat("chat-g7-user-mention", &dir, &ChatPlaybook::default()).await;
    let mut poster = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let poster_id = h.open_idle_window(&mut poster).await;
    identify(&mut poster, &poster_id).await;

    let posted =
        expect_chat_posted(send_chat_msg(&mut poster, None, "@user please look at this").await);
    assert!(posted.user);

    let changed_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(5)).await;
    assert_eq!(changed_seq, posted.seq);

    // The reply must reach the poster's own room, not the approver's "main"
    // default.
    let cleared = expect_chat_posted(
        broadcast_chat_msg(&mut approver, Some(posted.room_id.as_str()), "on it").await,
    );
    assert!(!cleared.halted);
    let read = expect_chat_messages(read_chat_msg(&mut poster, None, Some(0), None).await);
    assert!(!read.needs_user);
}

// =============================================================================
// intro, including the abandoned-on-close case
// =============================================================================

#[tokio::test]
async fn the_start_up_door_carries_the_intro_and_nothing_is_typed() {
    let dir = unique_temp_dir("chat-g8-sent");
    let h = start_chat(
        "chat-g8-sent",
        &dir,
        &ChatPlaybook {
            intro: Some("welcome to the room"),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let recipient_id = h.open_idle_window_with_composer(&mut client).await;

    let mut door = h.connect().await;
    let context = call_door(&mut door, "context", &recipient_id).await;
    assert!(context.contains("welcome to the room"), "{context:?}");

    // Whatever the daemon queued for the window on its own is written before a later
    // post, which is typed into this window while it waits.
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;
    expect_chat_posted(send_chat_msg(&mut client, None, &format!("hello @{recipient_id}")).await);
    wait_for_accepted_chat_envelope_json(&h, &recipient_id, Duration::from_secs(10)).await;
    assert!(
        !String::from_utf8_lossy(&h.scrollback(&recipient_id)).contains("welcome to the room"),
        "the intro is never typed into a window"
    );
}

#[tokio::test]
async fn an_empty_intro_leaves_the_start_up_door_with_nothing_to_say() {
    let dir = unique_temp_dir("chat-g8-empty");
    let h = start_chat("chat-g8-empty", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let recipient_id = h.open_idle_window_with_composer(&mut client).await;

    let mut door = h.connect().await;
    assert_eq!(call_door(&mut door, "context", &recipient_id).await, "");
}

// =============================================================================
// Chat history survives a daemon restart against the same DB path
// =============================================================================

#[tokio::test]
async fn chat_history_survives_a_daemon_restart_with_the_same_db_path() {
    let dir = unique_temp_dir("chat-g9-restart");
    let spec = ChatPlaybook::default();

    // The room id is deterministic (derived from the daemon process's own
    // cwd), so it is the same across both boots below even though the two
    // Harness instances are otherwise independent.
    let room_id;
    {
        let h = start_chat("chat-g9-restart-a", &dir, &spec).await;
        let mut client = h.connect().await;
        let poster_id = h.open_idle_window(&mut client).await;
        identify(&mut client, &poster_id).await;
        let posted =
            expect_chat_posted(send_chat_msg(&mut client, None, "before the restart").await);
        room_id = posted.room_id;
    }

    let h = start_chat("chat-g9-restart-b", &dir, &spec).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let read = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    assert!(
        read.messages.iter().any(|m| m.body == "before the restart"),
        "history must survive a restart against the same DB path, got {:?}",
        read.messages
    );
}

#[tokio::test]
async fn a_post_still_waiting_when_the_daemon_stopped_reads_back_undelivered_after_a_restart() {
    let dir = unique_temp_dir("chat-g9-pending-restart");
    let spec = ChatPlaybook::default();

    let (room_id, seq);
    {
        let h = start_chat("chat-g9-pending-restart-a", &dir, &spec).await;
        let mut client = h.connect().await;
        // Unknown, so the post is never pushed and waits in the inbox.
        let target_id = open_unknown_participant(&h).await;
        let poster_id = h.open_idle_window(&mut client).await;
        identify(&mut client, &poster_id).await;
        let posted =
            expect_chat_posted(send_chat_msg(&mut client, None, &format!("hi @{target_id}")).await);
        let entry = h
            .registry
            .get(&target_id)
            .expect("the window is registered");
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while entry.inbox().pending_count() == 0 {
            assert!(
                tokio::time::Instant::now() < deadline,
                "the post never reached the inbox"
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        room_id = posted.room_id;
        seq = posted.seq;
        h.registry.terminate_all().await;
    }

    // An inbox ends with its daemon: the next one reads the post as never delivered.
    let h = start_chat("chat-g9-pending-restart-b", &dir, &spec).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let read = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    let message = read
        .messages
        .iter()
        .find(|m| m.seq == seq)
        .expect("the post survives the restart");
    let states: Vec<&str> = message
        .deliveries
        .iter()
        .map(|d| d.state.as_str())
        .collect();
    assert_eq!(states, vec!["undelivered_restart"]);
}

#[tokio::test]
async fn a_halt_and_needs_user_survive_a_daemon_restart_with_the_same_db_path() {
    let dir = unique_temp_dir("chat-g9-halt-restart");
    let spec = ChatPlaybook {
        monologue_max: Some(1),
        auto_approve: &["broadcast_chat"],
        ..ChatPlaybook::default()
    };

    // Same determinism as chat_history_survives_a_daemon_restart_with_the_same_db_path.
    let room_id;
    {
        let h = start_chat("chat-g9-halt-restart-a", &dir, &spec).await;
        let mut poster = h.connect().await;
        let _recipient_id = h.open_idle_window(&mut poster).await;
        let poster_id = h.open_idle_window(&mut poster).await;
        identify(&mut poster, &poster_id).await;
        let posted =
            expect_chat_posted(broadcast_chat_msg(&mut poster, None, "trip the halt").await);
        assert!(posted.halted, "the first post must trip monologue_max = 1");
        room_id = posted.room_id;
    }

    let h = start_chat("chat-g9-halt-restart-b", &dir, &spec).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let read_before_new_traffic = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    let halt = read_before_new_traffic
        .halted
        .expect("a halt recorded before the restart must still be reported after it");
    assert_eq!(halt.reason, "monologue");
    assert!(
        read_before_new_traffic.needs_user,
        "needs_user must still be true after a restart against the same halted room"
    );

    let mut poster = h.connect().await;
    let recipient_id = h.open_idle_window(&mut poster).await;
    let poster_id = h.open_idle_window(&mut poster).await;
    identify(&mut poster, &poster_id).await;
    let after_restart_post = expect_chat_posted(
        broadcast_chat_msg(&mut poster, None, "still halted after restart").await,
    );
    assert!(
        after_restart_post.halted,
        "a room halted before the restart must still refuse an AI post after it"
    );
    // The AI post above raises the room's attention flag again on this boot's
    // fresh mirror; drain that push so it does not stand in for a reply below.
    let _changed_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(5)).await;

    let state = wait_for_settled_delivery(
        &mut poster,
        after_restart_post.seq,
        &recipient_id,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(state, "undelivered_halted");

    let user_post = expect_chat_posted(
        broadcast_chat_msg(&mut approver, Some(room_id.as_str()), "let's continue").await,
    );
    assert!(
        !user_post.halted,
        "the approver's own post must clear a halt carried over a restart"
    );

    let cleared = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    assert!(cleared.halted.is_none());
    assert!(!cleared.needs_user);
}

// =============================================================================
// Silence
// =============================================================================

#[tokio::test]
async fn silence_after_a_taken_post_marks_needs_user_and_notifies_the_approver() {
    let dir = unique_temp_dir("chat-g10-silence");
    let h = start_chat(
        "chat-g10-silence",
        &dir,
        &ChatPlaybook {
            silence_timeout_secs: Some(1),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send_chat_msg(&mut poster_conn, None, &format!("hello @{recipient_id}")).await;

    let _changed_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(10)).await;

    let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, None, None).await);
    assert!(read.needs_user);
    assert!(
        read.messages.iter().any(|m| m.from_kind == "system"),
        "a system silence notice must be readable from the poster's own default after_seq"
    );
}

#[tokio::test]
async fn a_follow_up_from_the_poster_itself_does_not_count_as_the_addressed_windows_reply() {
    let dir = unique_temp_dir("chat-silence-follow-up");
    let h = start_chat(
        "chat-silence-follow-up",
        &dir,
        &ChatPlaybook {
            silence_timeout_secs: Some(2),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("hello @{recipient_id}")).await,
    );
    expect_chat_posted(send_chat_msg(&mut poster_conn, None, "anyone there?").await);

    let _changed_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(10)).await;
    let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, None, None).await);
    assert!(
        read.messages.iter().any(|m| m.from_kind == "system"),
        "only the addressed window speaking again may cancel the silence notice"
    );
}

// =============================================================================
// Chat delivery budget
// =============================================================================

#[tokio::test]
async fn a_full_delivery_budget_leaves_exactly_one_destination_undelivered_exhausted() {
    let dir = unique_temp_dir("chat-g11-budget");
    let h = start_chat(
        "chat-g11-budget",
        &dir,
        &ChatPlaybook {
            auto_approve: &["broadcast_chat"],
            chat_delivery_max: Some(1),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let first_id = h.open_idle_window_with_composer(&mut client).await;
    let second_id = h.open_idle_window_with_composer(&mut client).await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let posted = expect_chat_posted(broadcast_chat_msg(&mut client, None, "budget check").await);
    assert_eq!(posted.targets.len(), 2);

    // Both destinations start out pending; the read is repeated until the
    // budget has turned one away or neither is pending any more.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(20);
    let states: Vec<String> = loop {
        let read = expect_chat_messages(read_chat_msg(&mut client, None, Some(0), None).await);
        let states: Vec<String> = [&first_id, &second_id]
            .iter()
            .map(|id| delivery_state_for(&read.messages, posted.seq, id).to_string())
            .collect();
        let settled = states.iter().any(|s| s == "undelivered_exhausted")
            || states.iter().all(|s| s != "pending");
        if settled {
            break states;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the deliveries never settled, last read {states:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert_eq!(
        states
            .iter()
            .filter(|s| *s == "undelivered_exhausted")
            .count(),
        1,
        "exactly one destination must be exhausted, got {states:?}"
    );
    assert!(
        states
            .iter()
            .any(|s| matches!(s.as_str(), "taken" | "pending")),
        "the other destination must have been attempted, got {states:?}"
    );
}

// =============================================================================
// Monologue
// =============================================================================

#[tokio::test]
async fn monologue_halts_after_the_maximum_consecutive_posts() {
    let dir = unique_temp_dir("chat-g12-monologue");
    let h = start_chat(
        "chat-g12-monologue",
        &dir,
        &ChatPlaybook {
            monologue_max: Some(2),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    let first = expect_chat_posted(send_chat_msg(&mut client, None, "one").await);
    assert!(!first.halted);
    let second = expect_chat_posted(send_chat_msg(&mut client, None, "two, different text").await);
    assert!(
        second.halted,
        "the 2nd consecutive post from the same window must trip monologue_max = 2"
    );

    let read = expect_chat_messages(read_chat_msg(&mut client, None, Some(0), None).await);
    assert_eq!(read.halted.expect("must be halted").reason, "monologue");
}

// =============================================================================
// Turn budget
// =============================================================================

#[tokio::test]
async fn turn_budget_halts_after_the_maximum_ai_posts_since_the_last_user_post() {
    let dir = unique_temp_dir("chat-g13-turn-budget");
    let h = start_chat(
        "chat-g13-turn-budget",
        &dir,
        &ChatPlaybook {
            turn_message_max: Some(2),
            monologue_max: Some(100),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let mut window_a = h.connect().await;
    let mut window_b = h.connect().await;
    let a_id = h.open_idle_window(&mut window_a).await;
    let b_id = h.open_idle_window(&mut window_b).await;
    identify(&mut window_a, &a_id).await;
    identify(&mut window_b, &b_id).await;

    // window_a and window_b were opened by the same daemon process, so they
    // share one workspace room; a side-effect-free read (not a post, so it
    // never counts toward turn_message_max) is enough to learn its id before
    // the approver's post below.
    let room_id =
        expect_chat_messages(read_chat_msg(&mut window_a, None, None, None).await).room_id;

    // A user post resets the turn counter.
    broadcast_chat_msg(&mut approver, Some(room_id.as_str()), "go ahead").await;

    let first = expect_chat_posted(send_chat_msg(&mut window_a, None, "ai message one").await);
    assert!(!first.halted);
    let second =
        expect_chat_posted(send_chat_msg(&mut window_b, None, "ai message two, different").await);
    assert!(
        second.halted,
        "the 2nd AI post since the last user post must trip turn_message_max = 2"
    );

    let changed_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(5)).await;
    assert_eq!(
        changed_seq, second.seq,
        "the halt must push the room as needing the user, naming the post that tripped it"
    );

    let read = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    assert_eq!(read.halted.expect("must be halted").reason, "turn_budget");
}

// =============================================================================
// Chat disabled (store absent, or a broken playbook)
// =============================================================================

#[tokio::test]
async fn a_daemon_with_no_resolved_store_path_returns_unsupported_for_every_chat_tool() {
    // The existing Harness::start path: IpcServer::new (no chat wiring at
    // all) must still answer the three tools, just with unsupported.
    let dir = unique_temp_dir("chat-g14-no-store");
    let config = common::detection_config(&dir, common::approver_base_config());
    assert_eq!(config.store_db_path_resolved, None);
    let h = Harness::start("chat-g14-no-store", config).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    for msg in [
        ClientMessage::SendChat {
            room_id: None,
            body: "hi".to_string(),
        },
        ClientMessage::BroadcastChat {
            room_id: None,
            body: "hi".to_string(),
        },
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: None,
            limit: None,
        },
    ] {
        send(&mut client, &msg).await;
        let result = read_response(&mut client, Duration::from_secs(5)).await;
        expect_error(result, "unsupported");
    }
}

#[tokio::test]
async fn a_broken_playbook_disables_chat_entirely_including_the_intro() {
    let dir = unique_temp_dir("chat-g14-broken-playbook");
    let config = DaemonConfig {
        playbook: Some(dir.join("does-not-exist.toml")),
        approval_policy: None,
        store_db_path_resolved: Some(dir.join("chat.db")),
        ..common::detection_config(&dir, common::approver_base_config())
    };
    let h = Harness::start_with_chat("chat-g14-broken-playbook", config).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window_with_composer(&mut client).await;
    identify(&mut client, &poster_id).await;

    let result = send_chat_msg(&mut client, None, "hi").await;
    expect_error(result, "unsupported");

    let mut door = h.connect().await;
    assert_eq!(
        call_door(&mut door, "context", &poster_id).await,
        "",
        "a Failed playbook must disable the intro too, not just the three tools"
    );
}

// =============================================================================
// Delivery state: every destination starts out pending, and every settled
// destination is pushed to the approvers
// =============================================================================

/// The `[input]` shape of a destination whose delivery cannot start while a
/// case runs: `ready_hold_ms` sits at the definition loader's maximum, far
/// longer than any case here takes, so a window that has only just reached
/// `waiting_input` keeps the delivery waiting at the startup gate instead of
/// writing.
fn held_destination_input() -> SubmitDefinitionInput {
    SubmitDefinitionInput {
        bracketed_paste: true,
        ready_hold_ms: 10_000,
        submit_delay_ms: 50,
        clear: "",
        delivery_confirm_ms: 1000,
        resend_max: 0,
    }
}

/// Opens one destination whose delivery stays at the startup gate for the
/// whole case (see [`held_destination_input`]).
async fn open_held_destination(
    h: &Harness,
    client: &mut yagura_test_support::ipc::Connection,
    dir: &Path,
    name: &str,
    marker: &str,
) -> String {
    write_submit_definition(dir, name, marker, held_destination_input());
    open_submit_child_window_with_composer(
        h,
        client,
        dir,
        name,
        marker,
        0,
        0,
        0,
        held_destination_input(),
        0,
    )
    .await
}

/// The next `chat_changed` push on `client`, skipping frames that are not
/// one. Panics when none arrives inside `timeout`.
async fn next_chat_changed(
    client: &mut yagura_test_support::ipc::Connection,
    timeout: Duration,
) -> (String, u64, bool) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        // The inner budget is the longer one on purpose: the deadline here is
        // what must decide the outcome, with its own message.
        let frame = tokio::time::timeout(remaining, read_response(client, timeout + timeout))
            .await
            .unwrap_or_else(|_| panic!("timed out waiting for a chat_changed push"));
        if let DaemonMessage::ChatChanged {
            room_id,
            seq,
            needs_user,
        } = frame
        {
            return (room_id, seq, needs_user);
        }
    }
}

/// How long the single-push cases watch for a second `chat_changed` push.
const SECOND_PUSH_OBSERVATION: Duration = Duration::from_millis(300);

/// The `(room_id, seq)` of every `chat_changed` push arriving on `client`
/// within `window`. Other pushes an approver connection receives are not
/// what the single-push cases count, so they are read past.
async fn chat_changed_within(
    client: &mut yagura_test_support::ipc::Connection,
    window: Duration,
) -> Vec<(String, u64)> {
    let deadline = tokio::time::Instant::now() + window;
    let mut seen = Vec::new();
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return seen;
        }
        match tokio::time::timeout(remaining, read_response(client, Duration::from_secs(5))).await {
            Ok(DaemonMessage::ChatChanged { room_id, seq, .. }) => seen.push((room_id, seq)),
            Ok(_) => {}
            Err(_) => return seen,
        }
    }
}

/// The next `chat_changed` push that reports the room as needing the user,
/// skipping the ones that only report a delivery moving on.
async fn next_chat_changed_needing_user(
    client: &mut yagura_test_support::ipc::Connection,
    timeout: Duration,
) -> u64 {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for a chat_changed push reporting needs_user"
        );
        let (_, seq, needs_user) = next_chat_changed(client, remaining).await;
        if needs_user {
            return seq;
        }
    }
}

#[tokio::test]
async fn every_destination_of_a_fresh_post_is_pending_until_its_delivery_settles() {
    let dir = unique_temp_dir("chat-pending-destinations");
    let h = start_chat("chat-pending-destinations", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;

    let first_id = open_held_destination(
        &h,
        &mut poster_conn,
        &dir,
        "chat-held-alpha",
        "chat-held-alpha-marker",
    )
    .await;
    let second_id = open_held_destination(
        &h,
        &mut poster_conn,
        &dir,
        "chat-held-beta",
        "chat-held-beta-marker",
    )
    .await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(
            &mut poster_conn,
            None,
            &format!("hello @{first_id} @{second_id}"),
        )
        .await,
    );
    assert_eq!(
        posted.targets.len(),
        2,
        "both destinations must be resolved"
    );

    let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, Some(0), None).await);
    assert_eq!(
        delivery_state_for(&read.messages, posted.seq, &first_id),
        "pending",
        "a destination whose delivery has not settled must be readable as pending right \
         after the post"
    );
    assert_eq!(
        delivery_state_for(&read.messages, posted.seq, &second_id),
        "pending"
    );
}

#[tokio::test]
async fn a_settled_delivery_pushes_chat_changed_carrying_the_posts_seq() {
    let dir = unique_temp_dir("chat-settled-push");
    let h = start_chat("chat-settled-push", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("hello @{recipient_id}")).await,
    );

    // The post being recorded pushes once on its own, before the delivery
    // has had a chance to settle; the delivery settling pushes again once
    // it does. Every push for this post must name the poster's own room and
    // this post's seq, so read after each until the delivery reads back
    // settled rather than assuming the first push already announces it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let settled_state = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let (room_id, seq, needs_user) = next_chat_changed(&mut approver, remaining).await;
        assert_eq!(
            room_id, posted.room_id,
            "every push for this post must name the poster's own room, not a literal \
             \"main\""
        );
        assert_eq!(
            seq, posted.seq,
            "every push for this post must name the post whose delivery moved on"
        );
        assert!(
            !needs_user,
            "every push for this post must carry the room's stored needs_user flag, which \
             nothing raised here"
        );
        let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, Some(0), None).await);
        let state = delivery_state_for(&read.messages, posted.seq, &recipient_id).to_string();
        if state != "pending" {
            break state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a chat_changed push announcing the settled delivery"
        );
    };
    assert_ne!(
        settled_state, "pending",
        "the state one of the pushes announced must be readable back as settled"
    );
}

#[tokio::test]
async fn a_settled_deliverys_push_reports_the_rooms_stored_attention_flag() {
    let dir = unique_temp_dir("chat-settled-push-attention");
    let h = start_chat(
        "chat-settled-push-attention",
        &dir,
        &ChatPlaybook::default(),
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    // Raise the room's attention flag first, and take the push it makes.
    let asked =
        expect_chat_posted(send_chat_msg(&mut poster_conn, None, "@user look at this").await);
    let raised_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(raised_seq, asked.seq);

    // A post addressed at a window neither raises nor clears the flag, so its
    // own delivery's push must still report it as raised.
    let posted = expect_chat_posted(
        send_chat_msg(
            &mut poster_conn,
            None,
            &format!("meanwhile @{recipient_id}"),
        )
        .await,
    );
    let (_, seq, needs_user) = next_chat_changed(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(seq, posted.seq);
    assert!(
        needs_user,
        "the flag the push carries must be the one the store holds, not one derived from \
         the post that triggered the delivery"
    );
}

#[tokio::test]
async fn a_destination_whose_cli_takes_the_post_pushes_chat_changed_when_it_is_recorded() {
    let dir = unique_temp_dir("chat-unknown-push");
    let h = start_chat("chat-unknown-push", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let unknown_id = open_unknown_participant(&h).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("hi @{unknown_id}")).await,
    );
    let entry = h
        .registry
        .get(&unknown_id)
        .expect("the window is registered");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry.inbox().pending_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the post never reached the inbox"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    let mut door = h.connect().await;
    call_door(&mut door, "take", &unknown_id).await;

    // The post being recorded pushes once on its own; read after each push until the
    // destination reads back taken rather than assuming the first push announces it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let settled_state = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let (_, seq, needs_user) = next_chat_changed(&mut approver, remaining).await;
        assert_eq!(seq, posted.seq);
        assert!(!needs_user);
        let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, Some(0), None).await);
        let state = delivery_state_for(&read.messages, posted.seq, &unknown_id).to_string();
        if state != "pending" {
            break state;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a chat_changed push announcing the taken post"
        );
    };
    assert_eq!(settled_state, "taken");
}

#[tokio::test]
async fn a_destination_refused_by_the_delivery_budget_pushes_chat_changed_when_it_is_recorded() {
    let dir = unique_temp_dir("chat-exhausted-push");
    let h = start_chat(
        "chat-exhausted-push",
        &dir,
        &ChatPlaybook {
            chat_delivery_max: Some(1),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    // Both destinations hold their delivery at the startup gate, so whichever
    // wins the single budget slot keeps it for the whole case and the other
    // is refused: which one is refused is unspecified, that one of them is is
    // not.
    let first_id = open_held_destination(
        &h,
        &mut poster_conn,
        &dir,
        "chat-budget-alpha",
        "chat-budget-alpha-marker",
    )
    .await;
    let second_id = open_held_destination(
        &h,
        &mut poster_conn,
        &dir,
        "chat-budget-beta",
        "chat-budget-beta-marker",
    )
    .await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(
            &mut poster_conn,
            None,
            &format!("hello @{first_id} @{second_id}"),
        )
        .await,
    );

    // The post being recorded pushes once on its own, before either spawned
    // delivery task has had a chance to record the budget's outcome; read
    // after each push until one destination reads back refused rather than
    // assuming the first push already announces it.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    let exhausted_count = loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        let (_, seq, needs_user) = next_chat_changed(&mut approver, remaining).await;
        assert_eq!(seq, posted.seq);
        assert!(!needs_user);
        let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, Some(0), None).await);
        let states: Vec<&str> = [&first_id, &second_id]
            .iter()
            .map(|id| delivery_state_for(&read.messages, posted.seq, id))
            .collect();
        let count = states
            .iter()
            .filter(|state| **state == "undelivered_exhausted")
            .count();
        if count > 0 {
            break count;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for a chat_changed push announcing the budget-refused \
             destination, got {states:?}"
        );
    };
    assert_eq!(
        exhausted_count, 1,
        "exactly one destination must be refused by the budget"
    );
}

#[tokio::test]
async fn an_undelivered_post_into_a_halted_room_pushes_chat_changed_of_its_own() {
    let dir = unique_temp_dir("chat-halted-push");
    let h = start_chat(
        "chat-halted-push",
        &dir,
        &ChatPlaybook {
            monologue_max: Some(1),
            auto_approve: &["broadcast_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let recipient_id = h.open_idle_window_with_composer(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let halting =
        expect_chat_posted(broadcast_chat_msg(&mut poster_conn, None, "trip the halt").await);
    assert!(halting.halted, "the first post must trip monologue_max = 1");
    // Take the halting post's own push before the second post, so what is
    // read below can only be the second post's.
    let halted_seq = next_chat_changed_needing_user(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(halted_seq, halting.seq);

    let refused =
        expect_chat_posted(broadcast_chat_msg(&mut poster_conn, None, "anyone there").await);
    assert!(refused.halted);

    let (_, seq, needs_user) = next_chat_changed(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(
        seq, refused.seq,
        "a destination recorded as undelivered because the room is halted must be pushed \
         like any other settled destination"
    );
    assert!(
        needs_user,
        "the room still needs the user, so the push must say so"
    );

    let read = expect_chat_messages(read_chat_msg(&mut poster_conn, None, Some(0), None).await);
    assert_eq!(
        delivery_state_for(&read.messages, refused.seq, &recipient_id),
        "undelivered_halted"
    );
}

// =============================================================================
// A post that addresses nobody and mentions no user still pushes chat_changed
// once, so an approver whose panel is closed learns the room has new content
// to re-read.
// =============================================================================

#[tokio::test]
async fn a_window_post_with_no_destination_and_no_mention_pushes_chat_changed_once() {
    let dir = unique_temp_dir("chat-unaddressed-push");
    let h = start_chat("chat-unaddressed-push", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(send_chat_msg(&mut poster_conn, None, "PONG").await);
    assert!(
        posted.targets.is_empty(),
        "a body with no @mention must resolve no destinations"
    );
    assert!(
        !posted.user,
        "a body with no @user must not raise needs_user"
    );

    let (room_id, seq, needs_user) = next_chat_changed(&mut approver, Duration::from_secs(5)).await;
    assert_eq!(room_id, posted.room_id);
    assert_eq!(seq, posted.seq);
    assert!(
        !needs_user,
        "nothing in this post raised the room's needs_user flag"
    );

    // Neither a delivery nor a halt announcement follows an unaddressed,
    // non-mentioning post, so this push must be the only one for it.
    let extra = chat_changed_within(&mut approver, SECOND_PUSH_OBSERVATION).await;
    assert!(
        extra.is_empty(),
        "expected exactly one chat_changed push for this post, got {extra:?}"
    );
}

#[tokio::test]
async fn a_user_post_with_no_destination_and_no_mention_pushes_chat_changed_once() {
    let dir = unique_temp_dir("chat-unaddressed-user-push");
    let h = start_chat("chat-unaddressed-user-push", &dir, &ChatPlaybook::default()).await;
    let mut poster = h.connect().await;
    register_approver(&mut poster).await;
    let mut watcher = h.connect().await;
    register_approver(&mut watcher).await;

    let posted = expect_chat_posted(send_chat_msg(&mut poster, None, "PONG").await);
    assert!(
        posted.targets.is_empty(),
        "a body with no @mention must resolve no destinations"
    );
    assert!(
        !posted.user,
        "a body with no @user must not raise needs_user"
    );

    let (room_id, seq, needs_user) = next_chat_changed(&mut watcher, Duration::from_secs(5)).await;
    assert_eq!(room_id, posted.room_id);
    assert_eq!(seq, posted.seq);
    assert!(
        !needs_user,
        "nothing in this post raised the room's needs_user flag"
    );

    let extra = chat_changed_within(&mut watcher, SECOND_PUSH_OBSERVATION).await;
    assert!(
        extra.is_empty(),
        "expected exactly one chat_changed push for this post, got {extra:?}"
    );
}

/// Companion to the two cases above: an `@user` mention already pushes
/// `chat_changed` through its own branch, so the unconditional push for
/// every accepted post must not land as a second, duplicate push for the
/// same one.
#[tokio::test]
async fn an_at_user_mention_still_pushes_chat_changed_only_once() {
    let dir = unique_temp_dir("chat-at-user-single-push");
    let h = start_chat("chat-at-user-single-push", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted =
        expect_chat_posted(send_chat_msg(&mut poster_conn, None, "@user look at this").await);
    assert!(posted.user);

    let (room_id, seq, needs_user) = next_chat_changed(&mut approver, Duration::from_secs(5)).await;
    assert_eq!(room_id, posted.room_id);
    assert_eq!(seq, posted.seq);
    assert!(needs_user);

    let extra = chat_changed_within(&mut approver, SECOND_PUSH_OBSERVATION).await;
    assert!(
        extra.is_empty(),
        "expected exactly one chat_changed push for an @user mention, got {extra:?}"
    );
}

// =============================================================================
// Rooms per workspace -- room resolution by origin, per-room
// isolation of the roster/broadcast/halt/needs_user state, room creation on
// window open, the ListChatRooms listing, the room a chat approval request
// names, and history sharing across two daemon instances against one store.
// =============================================================================

/// [`next_chat_changed`], returning the room_id of the next push that
/// reports the room as needing the user (parallel to
/// `next_chat_changed_needing_user`, which returns its seq instead).
async fn next_chat_changed_needing_user_room(
    client: &mut yagura_test_support::ipc::Connection,
    timeout: Duration,
) -> String {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out waiting for a chat_changed push reporting needs_user"
        );
        let (room_id, _seq, needs_user) = next_chat_changed(client, remaining).await;
        if needs_user {
            return room_id;
        }
    }
}

/// Opens a plain (non-composer) participant window in `cwd`, identifies it,
/// and returns its window id. A second workspace's window only ever needs
/// to exist and be addressable, never to accept a delivered write.
async fn open_identified_participant_in_cwd(
    h: &Harness,
    client: &mut yagura_test_support::ipc::Connection,
    cwd: &Path,
) -> String {
    let window_id = open_delivering(
        h,
        yagura_test_support::process::echo_child_command(),
        Some(CONTROL_DETECTION_NAME),
        Some(cwd),
        None,
    )
    .await;
    identify(client, &window_id).await;
    window_id
}

/// Sends `list_chat_rooms` on `client` (already a registered approver) and
/// returns the listing.
async fn list_chat_rooms(client: &mut yagura_test_support::ipc::Connection) -> Vec<ChatRoomInfo> {
    send(client, &ClientMessage::ListChatRooms).await;
    match read_reply(client, Duration::from_secs(5)).await {
        DaemonMessage::ChatRooms { rooms } => rooms,
        other => panic!("expected chat_rooms, got {other:?}"),
    }
}

// =============================================================================
// Room resolution by origin x requested room_id
// =============================================================================

#[tokio::test]
async fn window_origin_room_resolution_covers_every_requested_room_case() {
    let dir = unique_temp_dir("chat-room-g1-window");
    let h = start_chat("chat-room-g1-window", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    // Omitted: the window's own workspace room, never "main".
    let own_room = expect_chat_posted(send_chat_msg(&mut client, None, "omitted").await).room_id;
    assert!(
        own_room.starts_with("r-") && own_room != "main",
        "an identified window's own default room must be its own workspace room, got \
         {own_room:?}"
    );

    // Explicitly its own room: accepted, same id.
    let explicit_own = expect_chat_posted(
        send_chat_msg(&mut client, Some(own_room.as_str()), "explicit own").await,
    );
    assert_eq!(explicit_own.room_id, own_room);

    // Explicitly "main": accepted, a different room than its own.
    let to_main =
        expect_chat_posted(send_chat_msg(&mut client, Some("main"), "explicit main").await);
    assert_eq!(to_main.room_id, "main");

    // A second, existing room this window does not belong to: refused,
    // without revealing that it exists.
    let second_dir = second_workspace_dir("chat-room-g1-window-b");
    let mut other_client = h.connect().await;
    let other_room = {
        let _other_id =
            open_identified_participant_in_cwd(&h, &mut other_client, &second_dir).await;
        expect_chat_messages(read_chat_msg(&mut other_client, None, None, None).await).room_id
    };
    assert_ne!(other_room, own_room);

    let denied_message = expect_error(
        send_chat_msg(&mut client, Some(other_room.as_str()), "not my room").await,
        "invalid_argument",
    );

    // A well-formed-but-unknown room id, and a malformed one, must both be
    // indistinguishable from the denial above (existence is never revealed
    // to a window).
    let unknown_wellformed_message = expect_error(
        send_chat_msg(
            &mut client,
            Some("r-0000000000000000"),
            "unknown wellformed",
        )
        .await,
        "invalid_argument",
    );
    assert_eq!(unknown_wellformed_message, denied_message);
    let unknown_malformed_message = expect_error(
        send_chat_msg(&mut client, Some("not-a-room-id"), "unknown malformed").await,
        "invalid_argument",
    );
    assert_eq!(unknown_malformed_message, denied_message);
}

#[tokio::test]
async fn approver_origin_room_resolution_covers_every_requested_room_case() {
    let dir = unique_temp_dir("chat-room-g1-approver");
    let h = start_chat("chat-room-g1-approver", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    // Omitted: "main".
    let to_main = expect_chat_posted(broadcast_chat_msg(&mut approver, None, "omitted").await);
    assert_eq!(to_main.room_id, "main");

    // An existing, non-main room (created by a window opened in a second
    // workspace): the approver may address it explicitly by id, through the
    // find_room path rather than ensure_room (it is not the approver's own
    // room).
    let second_dir = second_workspace_dir("chat-room-g1-approver-b");
    let mut window_client = h.connect().await;
    let other_room = {
        let _other_id =
            open_identified_participant_in_cwd(&h, &mut window_client, &second_dir).await;
        expect_chat_messages(read_chat_msg(&mut window_client, None, None, None).await).room_id
    };

    let to_other = expect_chat_posted(
        broadcast_chat_msg(&mut approver, Some(other_room.as_str()), "explicit other").await,
    );
    assert_eq!(to_other.room_id, other_room);

    // An unknown room id (well-formed and malformed alike): refused, and --
    // unlike a window's denial -- the approver may be told the room simply
    // does not exist.
    let wellformed_message = expect_error(
        read_chat_msg(&mut approver, Some("r-0000000000000000"), Some(0), None).await,
        "invalid_argument",
    );
    assert_eq!(wellformed_message, "unknown room");
    let malformed_message = expect_error(
        read_chat_msg(&mut approver, Some("not-a-room-id"), Some(0), None).await,
        "invalid_argument",
    );
    assert_eq!(malformed_message, "unknown room");
}

#[tokio::test]
async fn external_origin_room_resolution_covers_every_requested_room_case() {
    let dir = unique_temp_dir("chat-room-g1-external");
    let h = start_chat("chat-room-g1-external", &dir, &ChatPlaybook::default()).await;
    let mut external = h.connect().await;

    // Omitted: "main" (an unidentified reader must always supply after_seq).
    let to_main = expect_chat_messages(read_chat_msg(&mut external, None, Some(0), None).await);
    assert_eq!(to_main.room_id, "main");

    // A second, existing room: refused without revealing it exists, the
    // same posture as a window.
    let second_dir = second_workspace_dir("chat-room-g1-external-b");
    let mut window_client = h.connect().await;
    let other_room = {
        let _other_id =
            open_identified_participant_in_cwd(&h, &mut window_client, &second_dir).await;
        expect_chat_messages(read_chat_msg(&mut window_client, None, None, None).await).room_id
    };
    expect_error(
        read_chat_msg(&mut external, Some(other_room.as_str()), Some(0), None).await,
        "invalid_argument",
    );
}

// =============================================================================
// The roster, broadcast targets and read_chat's roster are scoped to
// the room a message belongs to
// =============================================================================

#[tokio::test]
async fn broadcast_and_roster_are_scoped_to_the_posters_own_room() {
    let dir = unique_temp_dir("chat-room-g2");
    let h = start_chat(
        "chat-room-g2",
        &dir,
        &ChatPlaybook {
            auto_approve: &["broadcast_chat"],
            ..ChatPlaybook::default()
        },
    )
    .await;

    let mut a_client = h.connect().await;
    let a_recipient_id = h.open_idle_window_with_composer(&mut a_client).await;
    let a_poster_id = h.open_idle_window(&mut a_client).await;
    identify(&mut a_client, &a_poster_id).await;

    let second_dir = second_workspace_dir("chat-room-g2-b");
    let mut b_client = h.connect().await;
    let b_recipient_id = open_identified_participant_in_cwd(&h, &mut b_client, &second_dir).await;
    let b_poster_id = h
        .open_in_cwd(
            yagura_test_support::process::echo_child_command(),
            Some(CONTROL_DETECTION_NAME),
            &second_dir,
        )
        .await;
    let mut b_poster_client = h.connect().await;
    identify(&mut b_poster_client, &b_poster_id).await;

    // A's broadcast must reach only A's own room's participants.
    let a_posted = expect_chat_posted(broadcast_chat_msg(&mut a_client, None, "hi room A").await);
    assert!(a_posted.targets.contains(&a_recipient_id));
    assert!(!a_posted.targets.contains(&b_recipient_id));
    assert!(!a_posted.targets.contains(&b_poster_id));

    // B's own roster (read via read_chat) must not include A's participants.
    let b_read = expect_chat_messages(read_chat_msg(&mut b_client, None, Some(0), None).await);
    assert!(b_read.roster.iter().any(|p| p.window_id == b_recipient_id));
    assert!(!b_read.roster.iter().any(|p| p.window_id == a_recipient_id));
    assert!(!b_read.roster.iter().any(|p| p.window_id == a_poster_id));
    assert_ne!(b_read.room_id, a_posted.room_id);

    // A's own read_chat roster must not include B's participants either.
    let a_read = expect_chat_messages(read_chat_msg(&mut a_client, None, Some(0), None).await);
    assert!(!a_read.roster.iter().any(|p| p.window_id == b_recipient_id));
}

#[tokio::test]
async fn an_approver_reading_a_non_main_room_gets_that_rooms_roster_only() {
    let dir = unique_temp_dir("chat-room-g2-approver");
    let h = start_chat("chat-room-g2-approver", &dir, &ChatPlaybook::default()).await;

    let mut default_client = h.connect().await;
    let default_participant_id = h.open_idle_window(&mut default_client).await;

    let second_dir = second_workspace_dir("chat-room-g2-approver-b");
    let mut b_client = h.connect().await;
    let b_participant_id = open_identified_participant_in_cwd(&h, &mut b_client, &second_dir).await;
    let b_room = expect_chat_messages(read_chat_msg(&mut b_client, None, None, None).await).room_id;

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    // find_room path: the approver names a room it never
    // created itself.
    let read = expect_chat_messages(
        read_chat_msg(&mut approver, Some(b_room.as_str()), Some(0), None).await,
    );
    assert!(read.roster.iter().any(|p| p.window_id == b_participant_id));
    assert!(
        !read
            .roster
            .iter()
            .any(|p| p.window_id == default_participant_id)
    );
}

// =============================================================================
// A room row's creation is decoupled from the intro / a window becoming
// ready, and posting/reading works immediately after opening
// =============================================================================

#[tokio::test]
async fn a_room_row_appears_in_the_listing_before_any_post_or_read_and_before_the_window_is_ready()
{
    let dir = unique_temp_dir("chat-room-g3a");
    let h = start_chat(
        "chat-room-g3a",
        &dir,
        &ChatPlaybook {
            intro: Some(""),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let mut opener = h.connect().await;

    // held_destination_input's ready_hold_ms (10s) outlives this test's own
    // 5s poll budget, so the window is never considered "held ready" by the
    // intro watcher's own wait_for_any_held within that budget.
    let _held_id = open_held_destination(
        &h,
        &mut opener,
        &dir,
        "chat-g3a-held",
        "chat-g3a-held-marker",
    )
    .await;

    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let rooms = loop {
        let rooms = list_chat_rooms(&mut approver).await;
        if rooms.iter().any(|r| r.room_id != "main") {
            break rooms;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for the opened window's room row to appear in the listing, \
             without ever posting or reading in it"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    assert!(rooms.iter().any(|r| r.room_id.starts_with("r-")));
}

#[tokio::test]
async fn opening_a_participant_creates_its_own_room_row_and_its_intro_stays_plain_text() {
    let dir = unique_temp_dir("chat-room-g3b");
    let h = start_chat(
        "chat-room-g3b",
        &dir,
        &ChatPlaybook {
            intro: Some("welcome to the room"),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let recipient_id = h.open_idle_window_with_composer(&mut client).await;

    let mut door = h.connect().await;
    let context = call_door(&mut door, "context", &recipient_id).await;
    assert!(context.contains("welcome to the room"), "{context:?}");
    // Plain text, not the JSON chat envelope: an intro is never wrapped, unlike a post.
    assert!(
        !context.contains(CHAT_ENVELOPE_MARKER),
        "an intro must never be wrapped in the chat envelope: {context:?}"
    );

    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let rooms = list_chat_rooms(&mut approver).await;
        if rooms.iter().any(|r| r.room_id.starts_with("r-")) {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the recipient's own room row must exist once it is open, got {rooms:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn send_chat_and_read_chat_succeed_immediately_after_opening_without_waiting_for_intro() {
    let dir = unique_temp_dir("chat-room-g3c");
    let h = start_chat(
        "chat-room-g3c",
        &dir,
        &ChatPlaybook {
            intro: Some("welcome"),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    // No wait for the intro watcher here at all.
    let posted = expect_chat_posted(send_chat_msg(&mut client, None, "hello immediately").await);
    assert!(!posted.halted);
    let read = expect_chat_messages(read_chat_msg(&mut client, None, Some(0), None).await);
    assert!(read.messages.iter().any(|m| m.body == "hello immediately"));
}

// =============================================================================
// Halt / needs_user / its notification are independent per room
//
// =============================================================================

#[tokio::test]
async fn halt_and_its_clearing_and_notification_are_independent_per_room() {
    let dir = unique_temp_dir("chat-room-g4");
    let h = start_chat(
        "chat-room-g4",
        &dir,
        &ChatPlaybook {
            monologue_max: Some(2),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut a_client = h.connect().await;
    let a_id = h.open_idle_window(&mut a_client).await;
    identify(&mut a_client, &a_id).await;

    let second_dir = second_workspace_dir("chat-room-g4-b");
    let mut b_client = h.connect().await;
    let _b_id = open_identified_participant_in_cwd(&h, &mut b_client, &second_dir).await;

    let a_first = expect_chat_posted(send_chat_msg(&mut a_client, None, "A one").await);
    assert!(!a_first.halted);
    let a_second = expect_chat_posted(send_chat_msg(&mut a_client, None, "A two, different").await);
    assert!(a_second.halted);
    let a_changed_room =
        next_chat_changed_needing_user_room(&mut approver, Duration::from_secs(5)).await;
    assert_eq!(a_changed_room, a_first.room_id);

    // Room B's own monologue counter must be independent of room A's halt.
    let b_first = expect_chat_posted(send_chat_msg(&mut b_client, None, "B one").await);
    assert!(!b_first.halted);
    assert_ne!(b_first.room_id, a_first.room_id);

    let b_second = expect_chat_posted(send_chat_msg(&mut b_client, None, "B two, different").await);
    assert!(b_second.halted);
    let b_changed_room =
        next_chat_changed_needing_user_room(&mut approver, Duration::from_secs(5)).await;
    assert_eq!(b_changed_room, b_first.room_id);

    // A post to "main" must not clear either A's or B's active halt.
    broadcast_chat_msg(&mut approver, Some("main"), "not relevant").await;
    let a_read_before =
        expect_chat_messages(read_chat_msg(&mut a_client, None, Some(0), None).await);
    assert!(
        a_read_before.halted.is_some(),
        "a post to main must not clear room A's halt"
    );
    let b_read_before =
        expect_chat_messages(read_chat_msg(&mut b_client, None, Some(0), None).await);
    assert!(
        b_read_before.halted.is_some(),
        "a post to main must not clear room B's halt"
    );

    // The approver's reply addressed specifically to A clears only A.
    let a_cleared = expect_chat_posted(
        broadcast_chat_msg(&mut approver, Some(a_first.room_id.as_str()), "cleared").await,
    );
    assert!(!a_cleared.halted);
    let a_read_after =
        expect_chat_messages(read_chat_msg(&mut a_client, None, Some(0), None).await);
    assert!(a_read_after.halted.is_none());
    let b_read_after =
        expect_chat_messages(read_chat_msg(&mut b_client, None, Some(0), None).await);
    assert!(
        b_read_after.halted.is_some(),
        "clearing room A's halt must not clear room B's own halt"
    );
}

#[tokio::test]
async fn silence_after_a_taken_post_is_independent_per_room() {
    let dir = unique_temp_dir("chat-room-g4-silence");
    let h = start_chat(
        "chat-room-g4-silence",
        &dir,
        &ChatPlaybook {
            silence_timeout_secs: Some(1),
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let mut a_client = h.connect().await;
    let a_recipient_id = h.open_idle_window_with_composer(&mut a_client).await;
    let a_poster_id = h.open_idle_window(&mut a_client).await;
    identify(&mut a_client, &a_poster_id).await;

    let a_posted = expect_chat_posted(
        send_chat_msg(&mut a_client, None, &format!("hello @{a_recipient_id}")).await,
    );
    let changed_room =
        next_chat_changed_needing_user_room(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(changed_room, a_posted.room_id);

    // A second, freshly opened room, with nobody addressed, must not also
    // report needing the user just because A's silence watch fired.
    let second_dir = second_workspace_dir("chat-room-g4-silence-b");
    let mut b_client = h.connect().await;
    let _b_id = open_identified_participant_in_cwd(&h, &mut b_client, &second_dir).await;
    let b_read = expect_chat_messages(read_chat_msg(&mut b_client, None, None, None).await);
    assert!(!b_read.needs_user);
}

// =============================================================================
// ListChatRooms
// =============================================================================

#[tokio::test]
async fn list_chat_rooms_from_a_window_connection_is_refused() {
    let dir = unique_temp_dir("chat-room-g5-window");
    let h = start_chat("chat-room-g5-window", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;
    let poster_id = h.open_idle_window(&mut client).await;
    identify(&mut client, &poster_id).await;

    send(&mut client, &ClientMessage::ListChatRooms).await;
    let result = read_reply(&mut client, Duration::from_secs(5)).await;
    expect_error(result, "auth_failed");
}

#[tokio::test]
async fn list_chat_rooms_from_an_external_connection_is_refused() {
    let dir = unique_temp_dir("chat-room-g5-external");
    let h = start_chat("chat-room-g5-external", &dir, &ChatPlaybook::default()).await;
    let mut client = h.connect().await;

    send(&mut client, &ClientMessage::ListChatRooms).await;
    let result = read_reply(&mut client, Duration::from_secs(5)).await;
    expect_error(result, "auth_failed");
}

#[tokio::test]
async fn list_chat_rooms_puts_main_first_regardless_of_recency() {
    let dir = unique_temp_dir("chat-room-g5-order");
    let h = start_chat("chat-room-g5-order", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let second_dir = second_workspace_dir("chat-room-g5-order-b");
    let mut b_client = h.connect().await;
    let _b_id = open_identified_participant_in_cwd(&h, &mut b_client, &second_dir).await;
    let b_posted = expect_chat_posted(send_chat_msg(&mut b_client, None, "in room B").await);

    // main is posted to after B, so it is also the most recently active --
    // this must not change that main is first.
    let posted_to_main =
        expect_chat_posted(broadcast_chat_msg(&mut approver, None, "in main").await);
    assert_eq!(posted_to_main.room_id, "main");

    let rooms = list_chat_rooms(&mut approver).await;
    assert_eq!(
        rooms.first().map(|r| r.room_id.as_str()),
        Some("main"),
        "main must be first, got {rooms:?}"
    );
    assert!(
        rooms.iter().any(|r| r.room_id == b_posted.room_id),
        "room B must also be listed, got {rooms:?}"
    );
}

#[tokio::test]
async fn list_chat_rooms_reports_participant_count_and_survives_the_window_closing() {
    let dir = unique_temp_dir("chat-room-g5-count");
    let h = start_chat("chat-room-g5-count", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let second_dir = second_workspace_dir("chat-room-g5-count-b");
    let mut b_client = h.connect().await;
    let b_id = open_identified_participant_in_cwd(&h, &mut b_client, &second_dir).await;
    let b_room = expect_chat_messages(read_chat_msg(&mut b_client, None, None, None).await).room_id;

    let rooms = list_chat_rooms(&mut approver).await;
    let entry = rooms
        .iter()
        .find(|r| r.room_id == b_room)
        .expect("room B must be listed");
    assert_eq!(entry.participant_count, 1);

    send(
        &mut b_client,
        &ClientMessage::CloseWindow {
            window_id: b_id.clone(),
            force: Some(true),
        },
    )
    .await;
    match read_response(&mut b_client, Duration::from_secs(5)).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }

    let rooms_after = list_chat_rooms(&mut approver).await;
    let entry_after = rooms_after
        .iter()
        .find(|r| r.room_id == b_room)
        .expect("room B must still be listed after the window closes (rows are never deleted)");
    assert_eq!(entry_after.participant_count, 0);
}

// =============================================================================
// broadcast_chat's approval request carries the resolved room; a non-chat tool's request never does
// =============================================================================

#[tokio::test]
async fn a_broadcast_chat_approval_request_carries_the_rooms_id_and_display_name() {
    let dir = unique_temp_dir("chat-room-g7");
    let h = start_chat("chat-room-g7", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send(
        &mut poster_conn,
        &ClientMessage::BroadcastChat {
            room_id: None,
            body: "everyone please look".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    let room: RoomRef = request
        .room
        .expect("a chat post's approval request must carry a room");
    assert!(room.room_id.starts_with("r-"));
    assert!(!room.name.is_empty());

    decide(&mut approver, &request.request_id, true).await;
    let _ = read_response(&mut poster_conn, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn a_send_chat_approval_request_shows_the_rooms_own_name_and_the_mentioned_windows_name() {
    let dir = unique_temp_dir("chat-room-approval-names");
    let h = start_chat(
        "chat-room-approval-names",
        &dir,
        &ChatPlaybook {
            auto_approve: &[],
            ..ChatPlaybook::default()
        },
    )
    .await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    let workspace = second_workspace_dir("chat-room-approval-names-ws");
    let mut poster_conn = h.connect().await;
    let _poster_id = open_identified_participant_in_cwd(&h, &mut poster_conn, &workspace).await;
    let _bob_id = open_delivering(
        &h,
        yagura_test_support::process::echo_child_command(),
        None,
        Some(&workspace),
        Some("Bob"),
    )
    .await;

    send(
        &mut poster_conn,
        &ClientMessage::SendChat {
            room_id: None,
            body: "@Bob please look".to_string(),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;

    let room: RoomRef = request
        .room
        .clone()
        .expect("a chat post's approval request must carry a room");
    let workspace_name = workspace
        .file_name()
        .expect("the workspace directory has a final component")
        .to_string_lossy()
        .into_owned();
    assert_eq!(
        room.name, workspace_name,
        "the approver must be shown the posting room's own name, not another room's"
    );
    let text = request
        .text
        .clone()
        .expect("a chat post's approval request must carry its envelope");
    assert!(
        text.contains("\"mentions\":[\"Bob\"]"),
        "the envelope must name the mentioned window: {text}"
    );

    decide(&mut approver, &request.request_id, false).await;
    let _ = read_response(&mut poster_conn, Duration::from_secs(5)).await;
}

#[tokio::test]
async fn a_close_window_approval_request_carries_no_room() {
    let dir = unique_temp_dir("chat-room-g7-close");
    let h = start_chat("chat-room-g7-close", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let target_id = h.open_idle_window(&mut poster_conn).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    send(
        &mut poster_conn,
        &ClientMessage::CloseWindow {
            window_id: target_id.clone(),
            force: Some(true),
        },
    )
    .await;
    let request = expect_approval_requested(&mut approver).await;
    assert_eq!(request.tool, "close_window");
    assert_eq!(
        request.room, None,
        "a non-chat tool's approval request must never carry a room"
    );
    decide(&mut approver, &request.request_id, true).await;
    let _ = read_response(&mut poster_conn, Duration::from_secs(5)).await;
}

// =============================================================================
// Two daemon instances sharing one store resolve a window opened in
// the same cwd to the same room, and share its history.
// Restarting a single instance mid-test is not measured here -- Harness has
// no stop/restart handle, only the ability to start independent instances
// against the same store dir; a true restart is covered by
// chat_history_survives_a_daemon_restart_with_the_same_db_path above (which
// scopes and drops one Harness before starting the next) and by a manual
// check against a real daemon restart.
// =============================================================================

#[tokio::test]
async fn two_daemon_instances_sharing_a_store_resolve_the_same_cwd_to_the_same_room() {
    let dir = unique_temp_dir("chat-room-g11");
    let spec = ChatPlaybook::default();

    let h1 = start_chat("chat-room-g11-1", &dir, &spec).await;
    let mut client1 = h1.connect().await;
    let poster1_id = h1.open_idle_window(&mut client1).await;
    identify(&mut client1, &poster1_id).await;
    let posted1 =
        expect_chat_posted(send_chat_msg(&mut client1, None, "from the first instance").await);

    let h2 = start_chat("chat-room-g11-2", &dir, &spec).await;
    let mut client2 = h2.connect().await;
    let poster2_id = h2.open_idle_window(&mut client2).await;
    identify(&mut client2, &poster2_id).await;
    let read2 = expect_chat_messages(read_chat_msg(&mut client2, None, Some(0), None).await);
    assert_eq!(
        read2.room_id, posted1.room_id,
        "the same cwd must resolve to the same room across two daemon instances sharing \
         one store"
    );
    assert!(
        read2
            .messages
            .iter()
            .any(|m| m.body == "from the first instance")
    );
}

// A real, delivery-capable participant opened in a second workspace
// (open_submit_child_window_with_composer_in_cwd) still gets a full
// delivery inside its own room.
#[tokio::test]
async fn send_chat_delivers_to_a_composer_target_opened_in_a_second_workspace() {
    let dir = unique_temp_dir("chat-room-f0-composer");
    let h = start_chat("chat-room-f0-composer", &dir, &ChatPlaybook::default()).await;
    let second_dir = second_workspace_dir("chat-room-f0-composer-b");
    let mut b_client = h.connect().await;

    let target_id = open_submit_child_window_with_composer_in_cwd(
        &h,
        &mut b_client,
        &dir,
        &second_dir,
        "chat-room-f0-target",
        "chat-room-f0-marker",
        0,
        0,
        0,
        SubmitDefinitionInput {
            bracketed_paste: true,
            ready_hold_ms: 0,
            submit_delay_ms: 50,
            clear: "",
            delivery_confirm_ms: 3000,
            resend_max: 2,
        },
        0,
    )
    .await;
    let _poster_id = open_identified_participant_in_cwd(&h, &mut b_client, &second_dir).await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut b_client, None, &format!("hello @{target_id}")).await,
    );
    assert_eq!(posted.targets, vec![target_id.clone()]);
    assert!(
        posted.room_id.starts_with("r-"),
        "a window opened against a second workspace must resolve to its own room, got {:?}",
        posted.room_id
    );

    let state = wait_for_settled_delivery(
        &mut b_client,
        posted.seq,
        &target_id,
        Duration::from_secs(10),
    )
    .await;
    assert_eq!(
        state, "taken",
        "an addressed, non-Unknown target in a second workspace is typed into and taken"
    );
}

#[tokio::test]
async fn a_delivery_settling_after_its_post_was_cleared_announces_nothing_and_leaves_no_row() {
    let dir = unique_temp_dir("chat-cleared-delivery-settles");
    let h = start_chat(
        "chat-cleared-delivery-settles",
        &dir,
        &ChatPlaybook::default(),
    )
    .await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let unknown_id = open_unknown_participant(&h).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("hi @{unknown_id}")).await,
    );
    let entry = h
        .registry
        .get(&unknown_id)
        .expect("the window is registered");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry.inbox().pending_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the post never reached the inbox"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    // The history is cleared while the parcel is still on its way to the CLI.
    let mut store = yagura_daemon::chat_store::ChatStore::open(&dir.join("chat.db"))
        .expect("a second connection to the chat store must open");
    store
        .clear_history(&posted.room_id)
        .expect("clear_history must succeed");
    let mut door = h.connect().await;
    call_door(&mut door, "take", &unknown_id).await;

    assert!(
        chat_changed_within(&mut approver, SECOND_PUSH_OBSERVATION)
            .await
            .is_empty(),
        "the post is gone, so its delivery settling has nothing to announce"
    );
    let read = expect_chat_messages(
        read_chat_msg(
            &mut poster_conn,
            Some(posted.room_id.as_str()),
            Some(0),
            None,
        )
        .await,
    );
    assert!(read.messages.is_empty(), "got {:?}", read.messages);

    // The same wiring still announces a post that is there, numbered past the cleared one.
    let second = expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("again @{unknown_id}")).await,
    );
    assert!(second.seq > posted.seq, "{} vs {}", second.seq, posted.seq);
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry.inbox().pending_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the second post never reached the inbox"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    call_door(&mut door, "take", &unknown_id).await;
    let (_, seq, _) = next_chat_changed(&mut approver, Duration::from_secs(10)).await;
    assert_eq!(seq, second.seq);
}

// =============================================================================
// clear_chat_room / delete_chat_room
// =============================================================================

const UNKNOWN_ROOM_ID: &str = "r-0000000000000000";

fn clear_request(room_id: &str) -> ClientMessage {
    ClientMessage::ClearChatRoom {
        room_id: room_id.to_string(),
    }
}

fn delete_request(room_id: &str) -> ClientMessage {
    ClientMessage::DeleteChatRoom {
        room_id: room_id.to_string(),
    }
}

/// What an approver saw after asking to clear or delete a room: the answer,
/// the room a `chat_history_cleared` push named, and whether a
/// `chat_participants_changed` push came. Reads until every push the success
/// path owes has arrived, so a missing one fails here rather than passing.
struct RemovalOutcome {
    reply: DaemonMessage,
    cleared_push: Option<String>,
    participants_changed: bool,
}

async fn removal_outcome(
    client: &mut yagura_test_support::ipc::Connection,
    request: &ClientMessage,
    expect_participants_push: bool,
) -> RemovalOutcome {
    send(client, request).await;
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    let mut outcome = RemovalOutcome {
        reply: DaemonMessage::Pong,
        cleared_push: None,
        participants_changed: false,
    };
    let mut replied = false;
    while !replied
        || outcome.cleared_push.is_none()
        || (expect_participants_push && !outcome.participants_changed)
    {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        assert!(
            !remaining.is_zero(),
            "timed out: replied={replied} cleared_push={:?} participants_changed={}",
            outcome.cleared_push,
            outcome.participants_changed
        );
        match common::read_message(client, remaining).await {
            DaemonMessage::ChatChanged { .. } => {}
            DaemonMessage::ChatParticipantsChanged => outcome.participants_changed = true,
            DaemonMessage::ChatHistoryCleared { room_id } => outcome.cleared_push = Some(room_id),
            other => {
                outcome.reply = other;
                replied = true;
                // A refusal is followed by no push.
                if matches!(outcome.reply, DaemonMessage::Error { .. }) {
                    return outcome;
                }
            }
        }
    }
    outcome
}

/// The answer to a request an approver is refused or rejects: no push follows.
async fn refusal_of(
    client: &mut yagura_test_support::ipc::Connection,
    request: &ClientMessage,
) -> DaemonMessage {
    send(client, request).await;
    loop {
        match common::read_message(client, Duration::from_secs(5)).await {
            DaemonMessage::ChatChanged { .. } | DaemonMessage::ChatParticipantsChanged => {}
            other => return other,
        }
    }
}

/// Opens a participant in a second workspace and posts once, so the
/// workspace's room exists with history. Returns the room, the poster's
/// window, its connection and the post's seq.
async fn room_with_a_post(
    h: &Harness,
    workspace: &Path,
) -> (String, String, yagura_test_support::ipc::Connection, u64) {
    let mut client = h.connect().await;
    let window_id = open_identified_participant_in_cwd(h, &mut client, workspace).await;
    let posted = expect_chat_posted(send_chat_msg(&mut client, None, "first").await);
    (posted.room_id, window_id, client, posted.seq)
}

#[tokio::test]
async fn only_an_approver_may_clear_or_delete_a_chat_room() {
    let dir = unique_temp_dir("chat-remove-auth");
    let h = start_chat("chat-remove-auth", &dir, &ChatPlaybook::default()).await;
    let (room_id, _window, mut window_conn, _) =
        room_with_a_post(&h, &second_workspace_dir("chat-remove-auth-b")).await;
    let mut external = h.connect().await;

    for request in [clear_request(&room_id), delete_request(&room_id)] {
        send(&mut window_conn, &request).await;
        let from_window = read_reply(&mut window_conn, Duration::from_secs(5)).await;
        expect_error(from_window, "auth_failed");
        send(&mut external, &request).await;
        let from_external = read_reply(&mut external, Duration::from_secs(5)).await;
        expect_error(from_external, "auth_failed");
    }
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let read = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    assert_eq!(read.messages.len(), 1, "a refused request removes nothing");
}

#[tokio::test]
async fn clearing_or_deleting_a_room_that_does_not_exist_is_invalid_argument() {
    let dir = unique_temp_dir("chat-remove-unknown");
    let h = start_chat("chat-remove-unknown", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;

    for request in [
        clear_request(UNKNOWN_ROOM_ID),
        delete_request(UNKNOWN_ROOM_ID),
    ] {
        let reply = refusal_of(&mut approver, &request).await;
        let message = expect_error(reply, "invalid_argument");
        assert_eq!(message, "unknown room");
    }
}

#[tokio::test]
async fn the_main_room_can_be_cleared_but_never_deleted() {
    let dir = unique_temp_dir("chat-remove-main");
    let h = start_chat("chat-remove-main", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let first = expect_chat_posted(broadcast_chat_msg(&mut approver, None, "to main").await);

    let reply = refusal_of(&mut approver, &delete_request("main")).await;
    expect_error(reply, "invalid_argument");
    let still = expect_chat_messages(read_chat_msg(&mut approver, None, Some(0), None).await);
    assert_eq!(still.messages.len(), 1, "a refused delete removes nothing");

    let outcome = removal_outcome(&mut approver, &clear_request("main"), false).await;
    assert_eq!(
        outcome.reply,
        DaemonMessage::ChatRoomCleared {
            room_id: "main".to_string()
        }
    );
    assert_eq!(outcome.cleared_push.as_deref(), Some("main"));
    let after = expect_chat_messages(read_chat_msg(&mut approver, None, Some(0), None).await);
    assert!(after.messages.is_empty(), "got {:?}", after.messages);
    let next = expect_chat_posted(broadcast_chat_msg(&mut approver, None, "again").await);
    assert!(next.seq > first.seq, "{} vs {}", next.seq, first.seq);
}

#[tokio::test]
async fn clearing_a_room_empties_its_history_lifts_the_attention_flag_and_keeps_numbering() {
    let dir = unique_temp_dir("chat-remove-clear");
    let h = start_chat("chat-remove-clear", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let (room_id, _window, mut window_conn, _) =
        room_with_a_post(&h, &second_workspace_dir("chat-remove-clear-b")).await;
    let asked =
        expect_chat_posted(send_chat_msg(&mut window_conn, None, "@user look at this").await);
    assert!(asked.user);
    let before = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    assert!(before.needs_user);

    let outcome = removal_outcome(&mut approver, &clear_request(&room_id), false).await;
    assert_eq!(
        outcome.reply,
        DaemonMessage::ChatRoomCleared {
            room_id: room_id.clone()
        }
    );
    assert_eq!(outcome.cleared_push.as_deref(), Some(room_id.as_str()));

    let after = expect_chat_messages(
        read_chat_msg(&mut approver, Some(room_id.as_str()), Some(0), None).await,
    );
    assert!(after.messages.is_empty(), "got {:?}", after.messages);
    assert!(!after.needs_user);
    assert!(after.halted.is_none());
    let listed = list_chat_rooms(&mut approver).await;
    assert!(
        listed.iter().any(|room| room.room_id == room_id),
        "a cleared room stays listed: {listed:?}"
    );
    let next = expect_chat_posted(send_chat_msg(&mut window_conn, None, "after").await);
    assert!(next.seq > asked.seq, "{} vs {}", next.seq, asked.seq);
}

#[tokio::test]
async fn clearing_takes_a_waiting_post_back_so_the_cli_never_receives_it() {
    let dir = unique_temp_dir("chat-remove-withdraw");
    let h = start_chat("chat-remove-withdraw", &dir, &ChatPlaybook::default()).await;
    let mut poster_conn = h.connect().await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let unknown_id = open_unknown_participant(&h).await;
    let poster_id = h.open_idle_window(&mut poster_conn).await;
    identify(&mut poster_conn, &poster_id).await;

    let posted = expect_chat_posted(
        send_chat_msg(&mut poster_conn, None, &format!("hi @{unknown_id}")).await,
    );
    let entry = h
        .registry
        .get(&unknown_id)
        .expect("the window is registered");
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    while entry.inbox().pending_count() == 0 {
        assert!(
            tokio::time::Instant::now() < deadline,
            "the post never reached the inbox"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    let outcome = removal_outcome(&mut approver, &clear_request(&posted.room_id), false).await;
    assert!(matches!(
        outcome.reply,
        DaemonMessage::ChatRoomCleared { .. }
    ));
    assert_eq!(
        entry.inbox().pending_count(),
        0,
        "the answer comes after the waiting post was taken back"
    );
    let mut door = h.connect().await;
    assert_eq!(
        call_door(&mut door, "take", &unknown_id).await,
        "",
        "the cleared post must not reach the CLI"
    );
}

#[tokio::test]
async fn a_room_is_in_use_while_its_window_runs_and_can_be_deleted_once_it_has_exited() {
    let dir = unique_temp_dir("chat-remove-in-use");
    let h = start_chat("chat-remove-in-use", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let (room_id, window_id, mut window_conn, _) =
        room_with_a_post(&h, &second_workspace_dir("chat-remove-in-use-b")).await;

    let listed = list_chat_rooms(&mut approver).await;
    let info = listed.iter().find(|room| room.room_id == room_id).unwrap();
    assert!(info.has_open_windows);
    let main = listed.iter().find(|room| room.room_id == "main").unwrap();
    assert!(!main.has_open_windows, "main belongs to no workspace");

    let reply = refusal_of(&mut approver, &delete_request(&room_id)).await;
    expect_error(reply, "room_in_use");

    send(
        &mut window_conn,
        &ClientMessage::CloseWindow {
            window_id,
            force: Some(true),
        },
    )
    .await;
    match read_response(&mut window_conn, Duration::from_secs(5)).await {
        DaemonMessage::WindowClosed { .. } => {}
        other => panic!("expected window_closed, got {other:?}"),
    }
    // `window_closed` waits for the confirmed `Exited` only up to a bound,
    // so the listing is read until the status has settled.
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let listed = list_chat_rooms(&mut approver).await;
        let info = listed.iter().find(|room| room.room_id == room_id).unwrap();
        if !info.has_open_windows {
            break;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "a closed window has no pane and cannot use chat, so it holds no room"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let outcome = removal_outcome(&mut approver, &delete_request(&room_id), true).await;
    assert_eq!(
        outcome.reply,
        DaemonMessage::ChatRoomDeleted {
            room_id: room_id.clone()
        }
    );
}

#[tokio::test]
async fn a_room_no_window_belongs_to_is_deleted_announced_and_refuses_later_posts() {
    let dir = unique_temp_dir("chat-remove-delete");
    let workspace = second_workspace_dir("chat-remove-delete-ws");
    let room_id;
    let first_seq;
    {
        let h = start_chat("chat-remove-delete-a", &dir, &ChatPlaybook::default()).await;
        let (room, _window, _conn, seq) = room_with_a_post(&h, &workspace).await;
        room_id = room;
        first_seq = seq;
    }

    // A fresh daemon on the same store: the room has history and no window.
    let h = start_chat("chat-remove-delete-c", &dir, &ChatPlaybook::default()).await;
    let mut approver = h.connect().await;
    register_approver(&mut approver).await;
    let listed = list_chat_rooms(&mut approver).await;
    let info = listed.iter().find(|room| room.room_id == room_id).unwrap();
    assert!(!info.has_open_windows);

    let outcome = removal_outcome(&mut approver, &delete_request(&room_id), true).await;
    assert_eq!(
        outcome.reply,
        DaemonMessage::ChatRoomDeleted {
            room_id: room_id.clone()
        }
    );
    assert_eq!(outcome.cleared_push.as_deref(), Some(room_id.as_str()));
    assert!(outcome.participants_changed);

    let listed = list_chat_rooms(&mut approver).await;
    assert!(
        listed.iter().all(|room| room.room_id != room_id),
        "{listed:?}"
    );
    let reply = broadcast_chat_msg(&mut approver, Some(room_id.as_str()), "late").await;
    assert_eq!(expect_error(reply, "invalid_argument"), "unknown room");

    // A window of that workspace opening later re-creates the room, and its
    // numbering continues past the deleted history.
    let (again, _window, _conn, seq) = room_with_a_post(&h, &workspace).await;
    assert_eq!(again, room_id);
    assert!(seq > first_seq, "{seq} vs {first_seq}");
}
