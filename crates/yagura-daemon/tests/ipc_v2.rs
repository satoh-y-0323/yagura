#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: yagura-daemon IPC protocol v2 (multi-window architecture) --
//! per-window attach, window lifecycle management and the `window_id` addition.

mod common;

use std::path::Path;
use std::time::Duration;
use tokio::io::AsyncReadExt;

use yagura_test_support::ipc::{
    Connection, ReadOutcome, read_status_skipping_message, send_client_message,
};
use yagura_test_support::paths::TempDir;
use yagura_test_support::process::{echo_child_command, echo_child_path};

use yagura_daemon::DaemonConfig;
use yagura_daemon::detector::definition::bundled_definition_names;
use yagura_protocol::{ClientMessage, DaemonMessage, Frame, WindowStatus};

use common::{
    Harness, READY_MARKER, open_exited_window_via_ipc, unique_temp_dir, write_window_input,
};

/// A config whose detection directory holds [`common::control_definition`]
/// under [`common::CONTROL_DETECTION_NAME`]: a window opened with `detection
/// = Some(common::CONTROL_DETECTION_NAME)` can leave `Unknown` once
/// [`READY_MARKER`] reaches its PTY.
fn detection_config(dir: &Path) -> DaemonConfig {
    common::detection_config(dir, common::opening_config())
}

/// Drives `window_id` out of `Unknown` by writing [`READY_MARKER`] straight
/// into its PTY input through `harness.registry`, bypassing the
/// approval-gated `send_text` path: a `send_text` aimed at an `Unknown`
/// window is refused before any policy is even consulted (the status gate
/// outranks it), so it cannot be used to reach `idle` in the first place.
async fn drive_window_idle(harness: &Harness, client: &mut Connection, window_id: &str) {
    write_window_input(&harness.registry, window_id, READY_MARKER).await;
    wait_for_window_status(
        client,
        window_id,
        WindowStatus::Idle,
        Duration::from_secs(10),
    )
    .await;
}

/// Reads a raw frame, returning `None` if `timeout` elapses before one starts
/// arriving. A body that never follows its length prefix is a daemon-side
/// protocol violation and still panics.
///
/// This is the one read shape `yagura_test_support::ipc` does not offer: it
/// reports "nothing started arriving" as a value, so silence is not a failure.
async fn try_read_daemon_frame(client: &mut Connection, timeout: Duration) -> Option<ReadOutcome> {
    let mut len_buf = [0u8; 4];
    match tokio::time::timeout(timeout, client.read_exact(&mut len_buf)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return Some(ReadOutcome::Closed),
        Err(_) => return None,
    }
    let len = u32::from_le_bytes(len_buf) as usize;
    let mut body = vec![0u8; len];
    match tokio::time::timeout(timeout, client.read_exact(&mut body)).await {
        Ok(Ok(_)) => {}
        Ok(Err(_)) => return Some(ReadOutcome::Closed),
        Err(_) => panic!("timed out waiting for a frame body"),
    }
    Some(ReadOutcome::Message(
        yagura_protocol::decode_daemon_message(&body)
            .expect("daemon must only ever emit well-formed frames"),
    ))
}

/// Opens a window running `command` via IPC `OpenWindow` and returns its
/// `window_id`.
async fn open_window_via_ipc(client: &mut Connection, command: Vec<String>) -> String {
    send_client_message(
        client,
        &ClientMessage::OpenWindow {
            command,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;
    let response = read_status_skipping_message(client, Duration::from_secs(5))
        .await
        .expect_message("WindowOpened response");
    match response {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected WindowOpened, got {other:?}"),
    }
}

/// One `GetWindowStatus` round trip, returning the status and how long the window
/// has been in it. The pair is read together because reading the two from
/// separate replies would let the state change in between.
async fn query_window_status(client: &mut Connection, window_id: &str) -> (WindowStatus, u64) {
    send_client_message(
        client,
        &ClientMessage::GetWindowStatus {
            window_id: window_id.to_string(),
        },
    )
    .await;
    let response = read_status_skipping_message(client, Duration::from_secs(5))
        .await
        .expect_message("GetWindowStatus response");
    match response {
        DaemonMessage::WindowStatus {
            status, since_ms, ..
        } => (status, since_ms),
        other => panic!("expected WindowStatus, got {other:?}"),
    }
}

/// Polls `GetWindowStatus` until `window_id` reports `expected`; panics on timeout.
///
/// Observing the state rather than sleeping keeps tests from depending on how fast
/// the detection engine confirms a transition.
async fn wait_for_window_status(
    client: &mut Connection,
    window_id: &str,
    expected: WindowStatus,
    timeout: Duration,
) {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let (status, _since_ms) = query_window_status(client, window_id).await;
        if status == expected {
            return;
        }
        if tokio::time::Instant::now() >= deadline {
            panic!(
                "timed out waiting for window {window_id} to reach {expected:?} (last observed {status:?})"
            );
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Every frame that arrived on one connection during an observation window.
struct ObservedFrames {
    frames: Vec<DaemonMessage>,
    /// Whether the daemon closed the connection before the window elapsed.
    closed: bool,
}

/// The text of the rows one frame part carries, a row per line.
fn rows_text(frame: &Frame) -> String {
    frame
        .lines
        .iter()
        .map(|line| {
            line.runs
                .iter()
                .flat_map(|run| run.cells.iter().map(String::as_str))
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

impl ObservedFrames {
    /// The rows of every frame part collected, in arrival order. A diff frame
    /// carries whole rows, so a marker the window printed is looked for in the
    /// joined text.
    fn shown_text(&self) -> String {
        self.frames
            .iter()
            .filter_map(|frame| match frame {
                DaemonMessage::Frame(part) => Some(rows_text(part)),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn contains_error(&self, code: &str) -> bool {
        self.frames
            .iter()
            .any(|frame| matches!(frame, DaemonMessage::Error { code: got, .. } if got == code))
    }
}

/// Collects every frame arriving within `window`, stopping early once `stop`
/// matches a frame (that frame is included in the result).
///
/// Exhausting the window is a normal outcome and never panics: a connection
/// attached to a live window keeps receiving view frames whether or not the
/// property under test holds, so interpreting the frames is left to the caller.
/// Every completed frame is acknowledged, as a renderer does, so the next one
/// is sent.
async fn collect_frames(
    client: &mut Connection,
    window: Duration,
    stop: impl Fn(&DaemonMessage) -> bool,
) -> ObservedFrames {
    collect_frames_acknowledging(client, window, stop, true).await
}

async fn collect_frames_acknowledging(
    client: &mut Connection,
    window: Duration,
    stop: impl Fn(&DaemonMessage) -> bool,
    acknowledge: bool,
) -> ObservedFrames {
    let deadline = tokio::time::Instant::now() + window;
    let mut observed = ObservedFrames {
        frames: Vec::new(),
        closed: false,
    };
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            return observed;
        }
        match try_read_daemon_frame(client, remaining).await {
            Some(ReadOutcome::Message(msg)) => {
                let done = stop(&msg);
                if acknowledge
                    && let DaemonMessage::Frame(part) = &msg
                    && part.last
                {
                    send_client_message(client, &ClientMessage::FrameAck { seq: part.seq }).await;
                }
                observed.frames.push(msg);
                if done {
                    return observed;
                }
            }
            Some(ReadOutcome::Closed) => {
                observed.closed = true;
                return observed;
            }
            None => return observed,
        }
    }
}

/// The variant name of a `DaemonMessage`, never its payload.
///
/// Several variants carry PTY-derived data, so a diagnostic that reports "the
/// wrong message arrived" must not format the message with `{:?}`: that would let
/// scrollback content ride into a `cargo test` failure line and on into CI logs.
fn daemon_message_kind(msg: &DaemonMessage) -> String {
    match msg {
        DaemonMessage::Attached { .. } => "Attached".to_string(),
        DaemonMessage::Frame(_) => "Frame".to_string(),
        DaemonMessage::SelectionText { .. } => "SelectionText".to_string(),
        DaemonMessage::Exit { .. } => "Exit".to_string(),
        DaemonMessage::Pong => "Pong".to_string(),
        // `code` is a fixed machine-readable label, not user/PTY data, so it is
        // safe to include.
        DaemonMessage::Error { code, .. } => format!("Error(code={code})"),
        DaemonMessage::StatusChanged { .. } => "StatusChanged".to_string(),
        DaemonMessage::WindowOpened { .. } => "WindowOpened".to_string(),
        DaemonMessage::WindowList { .. } => "WindowList".to_string(),
        DaemonMessage::WindowStatus { .. } => "WindowStatus".to_string(),
        DaemonMessage::TextSent { .. } => "TextSent".to_string(),
        DaemonMessage::KeysSent { .. } => "KeysSent".to_string(),
        DaemonMessage::WindowInterrupted { .. } => "WindowInterrupted".to_string(),
        DaemonMessage::WindowClosed { .. } => "WindowClosed".to_string(),
        DaemonMessage::WaitResult { .. } => "WaitResult".to_string(),
        DaemonMessage::WindowRenamed { .. } => "WindowRenamed".to_string(),
        DaemonMessage::WindowOutput { .. } => "WindowOutput".to_string(),
        DaemonMessage::DetectionList { .. } => "DetectionList".to_string(),
        DaemonMessage::RestoreCandidateList { .. } => "RestoreCandidateList".to_string(),
        DaemonMessage::WindowLayoutUpdated { .. } => "WindowLayoutUpdated".to_string(),
        DaemonMessage::ProfileList { .. } => "ProfileList".to_string(),
        DaemonMessage::Identified { .. } => "Identified".to_string(),
        DaemonMessage::ApproverRegistered { .. } => "ApproverRegistered".to_string(),
        DaemonMessage::ApprovalRequested { .. } => "ApprovalRequested".to_string(),
        DaemonMessage::ApprovalWithdrawn { .. } => "ApprovalWithdrawn".to_string(),
        DaemonMessage::ApprovalResolved { .. } => "ApprovalResolved".to_string(),
        DaemonMessage::HandoffSent { .. } => "HandoffSent".to_string(),
        DaemonMessage::ChatPosted { .. } => "ChatPosted".to_string(),
        DaemonMessage::ChatMessages { .. } => "ChatMessages".to_string(),
        DaemonMessage::ChatChanged { .. } => "ChatChanged".to_string(),
        DaemonMessage::ChatParticipantsChanged => "ChatParticipantsChanged".to_string(),
        DaemonMessage::ChatRooms { .. } => "ChatRooms".to_string(),
        DaemonMessage::ProtocolRevision { .. } => "ProtocolRevision".to_string(),
        DaemonMessage::DaemonHealth { .. } => "DaemonHealth".to_string(),
        DaemonMessage::Delivery { .. } => "Delivery".to_string(),
        DaemonMessage::SessionRecorded { .. } => "SessionRecorded".to_string(),
        DaemonMessage::PlanDetail { .. } => "PlanDetail".to_string(),
        DaemonMessage::PlanList { .. } => "PlanList".to_string(),
        DaemonMessage::PlanDeleted { .. } => "PlanDeleted".to_string(),
        DaemonMessage::RoleList { .. } => "RoleList".to_string(),
        DaemonMessage::TaskWindowOpened { .. } => "TaskWindowOpened".to_string(),
        DaemonMessage::PlanChanged { .. } => "PlanChanged".to_string(),
        DaemonMessage::ReportSent { .. } => "ReportSent".to_string(),
        DaemonMessage::Reports { .. } => "Reports".to_string(),
        DaemonMessage::ModelChoicesRequested { .. } => "ModelChoicesRequested".to_string(),
        DaemonMessage::ModelChoices { .. } => "ModelChoices".to_string(),
        DaemonMessage::ModelAliasesSaved { .. } => "ModelAliasesSaved".to_string(),
        DaemonMessage::ProfileEnabledSet { .. } => "ProfileEnabledSet".to_string(),
        DaemonMessage::ProfileDetail { .. } => "ProfileDetail".to_string(),
        DaemonMessage::ProfileCommandSet { .. } => "ProfileCommandSet".to_string(),
        DaemonMessage::ApprovalSettings { .. } => "ApprovalSettings".to_string(),
        DaemonMessage::AutoApproveSet { .. } => "AutoApproveSet".to_string(),
        DaemonMessage::PlanApprovalRequiredSet { .. } => "PlanApprovalRequiredSet".to_string(),
        DaemonMessage::McpRegistrationsListed { .. } => "McpRegistrationsListed".to_string(),
        DaemonMessage::McpRegistrationStatus { .. } => "McpRegistrationStatus".to_string(),
        DaemonMessage::McpRegistrationRequested { .. } => "McpRegistrationRequested".to_string(),
        DaemonMessage::McpRegistrationResult { .. } => "McpRegistrationResult".to_string(),
    }
}

/// Sends `Attach` for `window_id` and returns the daemon's `Attached` reply.
///
/// The reply is located by frame kind rather than by position, because the view
/// frames of an attached window can be queued ahead of it. Returning marks the
/// point at which the attach has completed. Frames read on the way are stale
/// (they belong to a previous attach) and are not acknowledged.
async fn attach_via_ipc(client: &mut Connection, window_id: &str) -> DaemonMessage {
    send_client_message(
        client,
        &ClientMessage::AttachView {
            window_id: window_id.to_string(),
            size: None,
        },
    )
    .await;
    let mut observed = collect_frames_acknowledging(
        client,
        Duration::from_secs(5),
        |msg| {
            matches!(
                msg,
                DaemonMessage::Attached { .. } | DaemonMessage::Error { .. }
            )
        },
        false,
    )
    .await;
    match observed.frames.pop() {
        Some(msg @ DaemonMessage::Attached { .. }) => msg,
        // Never format the message with `{:?}` here: see `daemon_message_kind`.
        Some(other) => panic!(
            "expected Attached for window {window_id}, got {}",
            daemon_message_kind(&other)
        ),
        None => panic!(
            "expected Attached for window {window_id}, but no frame arrived within the observation window"
        ),
    }
}

#[tokio::test]
async fn open_window_with_valid_command_spawns_child_and_returns_window_id() {
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "open-valid").await;
    let echo_exe = echo_child_path();
    let echo_str = echo_exe
        .to_str()
        .expect("echo_child path must be valid UTF-8");

    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![echo_str.to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window response");
    match response {
        DaemonMessage::WindowOpened { window_id, .. } => {
            assert!(!window_id.is_empty(), "window_id must not be empty");
        }
        other => panic!("expected WindowOpened, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_with_empty_command_array_returns_invalid_argument() {
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "open-empty-cmd").await;
    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window error response");
    match response {
        DaemonMessage::Error { code, .. } => {
            assert_eq!(
                code, "invalid_argument",
                "empty command must return invalid_argument"
            )
        }
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_with_nonexistent_executable_returns_spawn_failed() {
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "open-spawn-fail").await;
    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec!["C:\\nonexistent\\command.exe".to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window error response for nonexistent executable");
    match response {
        DaemonMessage::Error { code, .. } => {
            assert_eq!(
                code, "spawn_failed",
                "nonexistent executable must return spawn_failed"
            )
        }
        other => panic!("expected error(spawn_failed), got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_with_invalid_detection_name_returns_invalid_argument() {
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "open-invalid-detection").await;
    let echo_exe = echo_child_path();
    let echo_str = echo_exe
        .to_str()
        .expect("echo_child path must be valid UTF-8");

    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![echo_str.to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: Some("..".to_string()),
            on_name_taken: Default::default(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window error response for invalid detection");
    match response {
        DaemonMessage::Error { code, .. } => {
            assert_eq!(
                code, "invalid_argument",
                "invalid detection name must return invalid_argument"
            )
        }
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_with_nonexistent_cwd_returns_invalid_argument() {
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "open-invalid-cwd").await;
    let echo_exe = echo_child_path();
    let echo_str = echo_exe
        .to_str()
        .expect("echo_child path must be valid UTF-8");

    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![echo_str.to_string()],
            cwd: Some("C:\\nonexistent\\directory".to_string()),
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window error response for nonexistent cwd");
    match response {
        DaemonMessage::Error { code, .. } => {
            assert_eq!(
                code, "invalid_argument",
                "nonexistent cwd must return invalid_argument"
            )
        }
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn list_windows_returns_entries_in_insertion_order() {
    // Observed at the IPC boundary: tests/window_registry.rs covers
    // WindowRegistry::list() itself, not the WindowList response's ordering.
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "list-windows-order").await;

    let mut opened = Vec::new();
    for _ in 0..3 {
        opened.push(open_window_via_ipc(&mut client, echo_child_command()).await);
    }

    send_client_message(&mut client, &ClientMessage::ListWindows).await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("WindowList response");
    let listed: Vec<String> = match response {
        DaemonMessage::WindowList { windows } => {
            windows.iter().map(|w| w.window_id.clone()).collect()
        }
        other => panic!("expected WindowList, got {other:?}"),
    };

    assert_eq!(
        listed, opened,
        "WindowList.windows must be ordered by the order the open_window calls were issued"
    );
}

// ListChatRooms / ChatRooms, exercised at the general
// IPC v2 dispatch level (chat-specific behavior -- an approver's actual
// listing -- belongs to ipc_chat.rs; this is only the wiring: an
// unidentified connection sending the new request gets a real answer, not a
// silently dropped frame).
#[tokio::test]
async fn list_chat_rooms_from_an_unidentified_connection_over_ipc_v2_is_auth_failed() {
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "list-chat-rooms-unidentified").await;

    send_client_message(&mut client, &ClientMessage::ListChatRooms).await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("Error(auth_failed) response");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "auth_failed"),
        DaemonMessage::ChatRooms { rooms } => panic!(
            "an unidentified connection must never receive a room listing, got {} room(s)",
            rooms.len()
        ),
        other => panic!("expected an Error(auth_failed) response, got {other:?}"),
    }
}

/// How long both attach-replacement tests watch a connection, at least, once
/// their observation window has opened: the frames they require to be
/// *absent* get this long to arrive.
const ATTACH_ABSENCE_WINDOW: Duration = Duration::from_secs(3);

/// Hang guard on the positive marker both attach-replacement tests wait for.
const ATTACH_MARKER_CEILING: Duration = Duration::from_secs(20);

/// Collects frames until one shows `marker`, then on until
/// [`ATTACH_ABSENCE_WINDOW`] has passed since the start. The marker is waited
/// for as an event, so a slow machine only lengthens the wait; the absence
/// window is never cut short by it. A diff frame carries whole rows, so a
/// printed marker is never split across parts.
async fn collect_frames_past_marker(client: &mut Connection, marker: &str) -> ObservedFrames {
    let started = tokio::time::Instant::now();
    let mut observed = collect_frames(
        client,
        ATTACH_MARKER_CEILING,
        |msg| matches!(msg, DaemonMessage::Frame(part) if rows_text(part).contains(marker)),
    )
    .await;
    if !observed.closed {
        let rest = collect_frames(
            client,
            ATTACH_ABSENCE_WINDOW.saturating_sub(started.elapsed()),
            |_| false,
        )
        .await;
        observed.frames.extend(rest.frames);
        observed.closed = rest.closed;
    }
    observed
}

#[tokio::test]
async fn attach_replacement_case_i_switching_windows_stops_old_output() {
    // On one connection, attaching A then B must leave only B's output reaching
    // it: the switch releases A's slot for this connection and re-points the
    // output subscription.
    const MARKER_A: &str = "case-i-old-window;";
    const MARKER_B: &str = "case-i-new-window;";

    let dir = unique_temp_dir("attach-case-i");
    let config = DaemonConfig {
        approval_policy: Some(common::policy_opening(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("attach-case-i", config).await;
    let mut client = h.connect().await;
    let window_a = open_window_via_ipc(&mut client, echo_child_command()).await;
    let window_b = open_window_via_ipc(&mut client, echo_child_command()).await;
    // `send_text` targeting an Unknown window is refused before any policy is
    // consulted, so both windows must be driven to `idle` before the markers
    // below can be sent at all.
    drive_window_idle(&h, &mut client, &window_a).await;
    drive_window_idle(&h, &mut client, &window_b).await;

    attach_via_ipc(&mut client, &window_a).await;
    attach_via_ipc(&mut client, &window_b).await;

    // Both markers are written only after the switch completed; output A produced
    // before it may still be queued and is out of scope. A is written first, so a
    // subscription left pointing at A would deliver its marker no later than B's.
    for (window_id, marker) in [(&window_a, MARKER_A), (&window_b, MARKER_B)] {
        send_client_message(
            &mut client,
            &ClientMessage::SendText {
                window_id: window_id.clone(),
                text: marker.to_string(),
            },
        )
        .await;
    }

    let observed = collect_frames_past_marker(&mut client, MARKER_B).await;
    assert!(
        !observed.closed,
        "the connection must stay open across an attach replacement"
    );
    let shown = observed.shown_text();
    assert!(
        shown.contains(MARKER_B),
        "output of the newly attached window must keep arriving after the switch"
    );
    assert!(
        !shown.contains(MARKER_A),
        "output produced by the previously attached window after the switch must not arrive"
    );
}

#[tokio::test]
async fn attach_replacement_case_ii_no_superseded_on_separate_attach() {
    // After client1 replaced its own attach A->B, a separate connection attaching
    // to A must not supersede client1: the slot is per window.
    const MARKER_B: &str = "case-ii-still-attached;";

    let dir = unique_temp_dir("attach-case-ii");
    let config = DaemonConfig {
        approval_policy: Some(common::policy_opening(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("attach-case-ii", config).await;
    let mut client1 = h.connect().await;
    let window_a = open_window_via_ipc(&mut client1, echo_child_command()).await;
    let window_b = open_window_via_ipc(&mut client1, echo_child_command()).await;
    drive_window_idle(&h, &mut client1, &window_b).await;

    attach_via_ipc(&mut client1, &window_a).await;
    attach_via_ipc(&mut client1, &window_b).await;

    let mut client2 = h.connect().await;
    // The observation window opens with this acknowledgement, and nothing reads
    // client1 until the collection below, so no reaction frame can slip past it.
    attach_via_ipc(&mut client2, &window_a).await;

    // Positive control: "no superseded arrived" is equally true of a connection
    // attached to nothing, so the same window must also show client1 still
    // receiving B's output. This SendText's ack is left for the collection.
    send_client_message(
        &mut client1,
        &ClientMessage::SendText {
            window_id: window_b.clone(),
            text: MARKER_B.to_string(),
        },
    )
    .await;

    let observed = collect_frames_past_marker(&mut client1, MARKER_B).await;
    assert!(
        !observed.contains_error("superseded"),
        "attaching a separate connection to a window client1 has already left must not supersede it"
    );
    assert!(
        !observed.closed,
        "client1 must stay connected while another connection attaches the window it left"
    );
    assert!(
        observed.shown_text().contains(MARKER_B),
        "client1 must still be attached to B for the absence of superseded to mean anything"
    );
}

#[tokio::test]
async fn send_text_does_not_require_attach() {
    // Unlike input, send_text is not behind the attach gate.
    let dir = unique_temp_dir("send-text-no-attach");
    let config = DaemonConfig {
        approval_policy: Some(common::policy_opening(&["send_message"])),
        ..detection_config(&dir)
    };
    let h = Harness::start("send-text-no-attach", config).await;
    let mut client = h.connect().await;
    let window_id = open_window_via_ipc(&mut client, echo_child_command()).await;
    drive_window_idle(&h, &mut client, &window_id).await;

    send_client_message(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "send-text-before-attach;".to_string(),
        },
    )
    .await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("send_text response");
    match response {
        DaemonMessage::TextSent { window_id: acked } => assert_eq!(acked, window_id),
        other => panic!("expected TextSent, got {other:?}"),
    }

    // The echo must be in the window's model before the attach snapshots it.
    common::wait_until_model_shows(
        &h,
        &window_id,
        b"send-text-before-attach;",
        Duration::from_secs(10),
    )
    .await;

    let DaemonMessage::Attached { .. } = attach_via_ipc(&mut client, &window_id).await else {
        panic!("attach_via_ipc must return an Attached response");
    };
    let observed = collect_frames(
        &mut client,
        Duration::from_secs(5),
        |msg| matches!(msg, DaemonMessage::Frame(part) if part.last),
    )
    .await;
    assert!(
        observed.shown_text().contains("send-text-before-attach;"),
        "the attach frame must show text sent via send_text before any attach occurred"
    );
}

#[tokio::test]
async fn send_text_to_exited_window_returns_child_exited() {
    // The child_exited decision uses current_status() as its SSOT.
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "send-text-exited").await;
    let window_id = open_exited_window_via_ipc(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::SendText {
            window_id: window_id.clone(),
            text: "hello".to_string(),
        },
    )
    .await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("send_text error response for an exited window");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "child_exited"),
        other => panic!("expected error(child_exited), got {other:?}"),
    }
}

#[tokio::test]
async fn send_text_exceeding_1mb_returns_invalid_argument() {
    // The text limit is 1 MiB, separate from the 16 MiB frame limit.
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "send-text-oversized").await;
    let window_id = open_window_via_ipc(&mut client, echo_child_command()).await;

    let oversized_text = "a".repeat(1024 * 1024 + 1);
    send_client_message(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: oversized_text,
        },
    )
    .await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("send_text error response for oversized text");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "invalid_argument"),
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }
}

#[tokio::test]
async fn send_text_to_unknown_window_id_returns_window_not_found() {
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "send-text-unknown-id").await;

    send_client_message(
        &mut client,
        &ClientMessage::SendText {
            window_id: "does-not-exist".to_string(),
            text: "hello".to_string(),
        },
    )
    .await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("send_text error response for an unknown window_id");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "window_not_found"),
        other => panic!("expected error(window_not_found), got {other:?}"),
    }
}

// Both tests below read `since_ms` over IPC from a window on a real PTY, so they
// run on the wall clock. How the age restarts at a transition is measured on a
// paused clock by the unit tests in `window.rs`.

/// How long [`since_ms_remains_nondecreasing_within_same_state`] stays inside
/// one state between its two observations.
const SAME_STATE_OBSERVATION_GAP: Duration = Duration::from_millis(500);

/// Slack allowed between the wait measured by the test and the growth reported by
/// the daemon: both readings are floored to whole milliseconds, so the growth can
/// fall a millisecond short of the wait. No upper bound accompanies it -- load can
/// stretch the true elapsed time arbitrarily, and a ceiling would fail on that.
const SINCE_MS_GROWTH_TOLERANCE_MS: u64 = 50;

#[tokio::test]
async fn since_ms_remains_nondecreasing_within_same_state() {
    // echo_child reads its stdin until an EOF ConPTY never delivers, so the child
    // outlives the measured interval and the "same state" premise cannot silently
    // break. That premise is asserted below, not assumed.
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "since-nondecreasing").await;
    let window_id = open_window_via_ipc(&mut client, echo_child_command()).await;

    let (first_status, first_since_ms) = query_window_status(&mut client, &window_id).await;
    let wait_started_at = std::time::Instant::now();
    tokio::time::sleep(SAME_STATE_OBSERVATION_GAP).await;
    let waited_ms = u64::try_from(wait_started_at.elapsed().as_millis())
        .expect("a sub-second wait fits in u64 milliseconds");
    let (second_status, second_since_ms) = query_window_status(&mut client, &window_id).await;

    assert_eq!(
        first_status, second_status,
        "the window must not have changed state between the two observations"
    );
    assert_ne!(
        first_status,
        WindowStatus::Exited,
        "the child must still be alive, otherwise both observations sit after the exit transition"
    );
    assert!(
        second_since_ms >= first_since_ms,
        "since_ms must not decrease within one state (first {first_since_ms} ms, second {second_since_ms} ms)"
    );
    // The measured wait lies entirely between the two replies, so it is a lower
    // bound on the daemon's own advance; a constant since_ms would pass above.
    assert!(
        second_since_ms - first_since_ms >= waited_ms.saturating_sub(SINCE_MS_GROWTH_TOLERANCE_MS),
        "since_ms must grow by the time actually waited ({waited_ms} ms) but grew from {first_since_ms} ms to {second_since_ms} ms"
    );
}

/// A child that stays in its pre-exit state until a line arrives on its input, then
/// exits: the test, not the child's own clock, decides when the transition happens.
#[cfg(windows)]
fn exit_on_line_command() -> Vec<String> {
    ["cmd.exe", "/c", "set", "/p", "line="]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

/// Unix counterpart of the Windows `exit_on_line_command`.
#[cfg(not(windows))]
fn exit_on_line_command() -> Vec<String> {
    ["/bin/sh", "-c", "read line"]
        .iter()
        .map(|arg| (*arg).to_string())
        .collect()
}

/// How long [`since_ms_resets_on_state_transition`] lets the window sit in its
/// pre-exit state before reading `since_ms_before`.
const RESET_WAIT_BEFORE_TRANSITION: Duration = Duration::from_millis(1200);

/// Floor asserted on `since_ms_before`. The window's state clock starts at window
/// creation, strictly before the wait above begins, so the reading cannot come in
/// under that wait; the difference is slack for truncation and timer granularity.
const SINCE_MS_BEFORE_FLOOR_MS: u64 = 1100;

#[tokio::test]
async fn since_ms_resets_on_state_transition() {
    // since_ms must restart at the transition rather than keep counting from
    // window creation. The child exits only when the test writes a line, so
    // `since_ms_before` is always read before the transition.
    let harness = Harness::start("since-reset", common::opening_config()).await;
    let mut client = harness.connect().await;
    let window_id = open_window_via_ipc(&mut client, exit_on_line_command()).await;

    tokio::time::sleep(RESET_WAIT_BEFORE_TRANSITION).await;
    let (status_before, since_ms_before) = query_window_status(&mut client, &window_id).await;

    assert_ne!(
        status_before,
        WindowStatus::Exited,
        "since_ms_before must be read while the window is still in its pre-exit state"
    );
    assert!(
        since_ms_before >= SINCE_MS_BEFORE_FLOOR_MS,
        "since_ms_before must have accumulated the pre-transition wait, got {since_ms_before} ms"
    );

    harness.write_input(&window_id, b"exit\r").await;
    wait_for_window_status(
        &mut client,
        &window_id,
        WindowStatus::Exited,
        Duration::from_secs(10),
    )
    .await;
    let (status_after, since_ms_after) = query_window_status(&mut client, &window_id).await;

    assert_eq!(
        status_after,
        WindowStatus::Exited,
        "the observed transition must still hold when since_ms_after is read"
    );
    assert!(
        since_ms_after < since_ms_before,
        "since_ms must restart at the transition (before {since_ms_before} ms, after {since_ms_after} ms)"
    );
}

#[tokio::test]
async fn exited_determination_uses_current_status_not_pty_write_error() {
    // The child_exited gate runs before the send_text size limit, hence before any
    // PTY write. Oversized text on an Exited window separates the two: a
    // size-first order would answer invalid_argument.
    let config = common::opening_config();
    let mut client = start_server_and_connect(&config, "exited-before-size-limit").await;
    let window_id = open_exited_window_via_ipc(&mut client).await;

    // One byte over SEND_TEXT_MAX_BYTES, the same boundary used on a live window.
    let oversized_text = "a".repeat(1024 * 1024 + 1);
    send_client_message(
        &mut client,
        &ClientMessage::SendText {
            window_id,
            text: oversized_text,
        },
    )
    .await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("send_text response for oversized text on an exited window");
    match response {
        DaemonMessage::Error { code, .. } => assert_eq!(
            code, "child_exited",
            "the status gate must decide before the size limit (invalid_argument means the order flipped)"
        ),
        other => panic!("expected error(child_exited), got {other:?}"),
    }
}

/// Starts a fresh `IpcServer` over its own `WindowRegistry`, both built from
/// `config`, and returns a connected client. The `Harness` is dropped here;
/// its background server task keeps running regardless, since dropping a
/// `JoinHandle` does not abort the task it tracks.
async fn start_server_and_connect(config: &DaemonConfig, tag: &str) -> Connection {
    Harness::start(tag, config.clone()).await.connect().await
}

#[tokio::test]
async fn list_detections_returns_valid_names_sorted_in_byte_order_without_warning() {
    // A mix of valid definitions, a non-.toml file and an invalid-stem .toml must
    // all be filtered down to the valid names, sorted in byte order.
    let tree = TempDir::new("yagura-cli-test", "list-detections-valid");
    let dir = tree.make_dir("detection");
    tree.write_file("detection/generic-shell.toml", "");
    tree.write_file("detection/claude-code.toml", "");
    tree.write_file("detection/README.md", "");
    // Uppercase fails is_valid_detection_name's [a-z0-9_-]{1,64} charset, so
    // this entry must be skipped rather than surfaced or rejected outright.
    tree.write_file("detection/Invalid-Stem.toml", "");

    let config = DaemonConfig {
        detection_dir: Some(dir),
        ..common::opening_config()
    };
    let mut client = start_server_and_connect(&config, "list-detections-valid").await;

    send_client_message(&mut client, &ClientMessage::ListDetections).await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("list_detections response");
    match response {
        DaemonMessage::DetectionList {
            names,
            directory_names,
            bundled_names,
            warning,
        } => {
            assert_eq!(
                names,
                vec!["claude-code".to_string(), "generic-shell".to_string()],
                "names must be filtered to valid definitions only, byte-sorted"
            );
            assert_eq!(
                directory_names,
                vec!["claude-code".to_string(), "generic-shell".to_string()],
                "an explicit directory layer answers alone, so directory_names equals names"
            );
            assert_eq!(
                bundled_names,
                Vec::<String>::new(),
                "an explicit directory layer switches the bundled layer off"
            );
            assert_eq!(warning, None);
        }
        other => panic!("expected DetectionList, got {other:?}"),
    }
}

#[tokio::test]
async fn list_detections_with_no_detection_dir_resolved_returns_empty_names_without_warning() {
    // detection_dir == None is the normal "detection disabled" state, not a
    // failure: names is empty and warning stays None.
    let config = DaemonConfig {
        detection_dir: None,
        ..common::opening_config()
    };
    let mut client = start_server_and_connect(&config, "list-detections-no-dir").await;

    send_client_message(&mut client, &ClientMessage::ListDetections).await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("list_detections response");
    match response {
        DaemonMessage::DetectionList {
            names,
            directory_names,
            bundled_names,
            warning,
        } => {
            assert_eq!(names, Vec::<String>::new());
            assert_eq!(directory_names, Vec::<String>::new());
            assert_eq!(bundled_names, Vec::<String>::new());
            assert_eq!(warning, None);
        }
        other => panic!("expected DetectionList, got {other:?}"),
    }
}

#[tokio::test]
async fn list_detections_with_an_unresolvable_detection_dir_returns_empty_names_with_warning() {
    // A detection_dir that cannot be read must produce empty names plus a
    // warning, distinct from the "no directory resolved" case above.
    let missing_dir =
        common::unique_temp_dir("list-detections-missing-parent").join("does-not-exist");
    let config = DaemonConfig {
        detection_dir: Some(missing_dir),
        ..common::opening_config()
    };
    let mut client = start_server_and_connect(&config, "list-detections-missing").await;

    send_client_message(&mut client, &ClientMessage::ListDetections).await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("list_detections response");
    match response {
        DaemonMessage::DetectionList {
            names,
            directory_names,
            bundled_names,
            warning,
        } => {
            assert_eq!(names, Vec::<String>::new());
            assert_eq!(
                directory_names,
                Vec::<String>::new(),
                "a directory that could not be scanned contributes no names"
            );
            assert_eq!(
                bundled_names,
                Vec::<String>::new(),
                "the bundled layer is off in this config"
            );
            let warning = warning.expect("an unreadable detection_dir must produce a warning");
            assert!(
                warning.contains("failed to enumerate detection definitions"),
                "warning must report the enumeration failure, got: {warning}"
            );
        }
        other => panic!("expected DetectionList, got {other:?}"),
    }
}

#[tokio::test]
async fn list_detections_with_bundled_definitions_enabled_returns_the_union_of_both_layers() {
    // The config-root shape (bundled_definitions: true) overlays the
    // directory layer's names on top of the bundled layer's: names is their
    // byte-sorted union, and bundled_names carries every bundled name.
    let tree = TempDir::new("yagura-cli-test", "list-detections-union");
    let dir = tree.make_dir("detection");
    tree.write_file("detection/overlay-extra.toml", "");

    let config = DaemonConfig {
        detection_dir: Some(dir),
        bundled_definitions: true,
        ..common::opening_config()
    };
    let mut client = start_server_and_connect(&config, "list-detections-union").await;

    send_client_message(&mut client, &ClientMessage::ListDetections).await;
    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("list_detections response");
    match response {
        DaemonMessage::DetectionList {
            names,
            directory_names,
            bundled_names,
            warning,
        } => {
            assert_eq!(warning, None);
            assert_eq!(
                directory_names,
                vec!["overlay-extra".to_string()],
                "the directory layer's own name must be reported"
            );
            let expected_bundled: Vec<String> = bundled_definition_names()
                .iter()
                .map(|name| (*name).to_string())
                .collect();
            assert_eq!(
                bundled_names, expected_bundled,
                "bundled_names must carry every bundled definition name"
            );
            let mut expected_names = directory_names.clone();
            expected_names.extend(bundled_names.clone());
            expected_names.sort();
            expected_names.dedup();
            assert_eq!(
                names, expected_names,
                "names must be the byte-sorted union of both layers"
            );
        }
        other => panic!("expected DetectionList, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_returns_detection_warning_when_the_default_detection_definition_is_missing() {
    // The default-name path (detection: None), directory layer only (the
    // bundled layer is off, matching an explicit --detection-dir): the
    // window still opens and the response carries the warning over IPC. The
    // fixture path is kept short so the directory it names survives the
    // 200-code-point summary cap.
    let dir = TempDir::new("t-ipc", "miss");
    // No file written: the default detection_name's .toml is absent here.
    let dir_path = dir.path().to_path_buf();

    let config = DaemonConfig {
        detection_dir: Some(dir_path.clone()),
        ..common::opening_config()
    };
    let mut client = start_server_and_connect(&config, "open-window-warn-missing").await;
    let echo_exe = echo_child_path();
    let echo_str = echo_exe
        .to_str()
        .expect("echo_child path must be valid UTF-8");

    send_client_message(
        &mut client,
        &ClientMessage::OpenWindow {
            command: vec![echo_str.to_string()],
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        },
    )
    .await;

    let response = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("open_window response with a missing default detection");
    match response {
        DaemonMessage::WindowOpened {
            window_id,
            detection_warning,
            ..
        } => {
            assert!(!window_id.is_empty(), "window_id must not be empty");
            let warning = detection_warning
                .expect("detection_warning must be Some when the default definition is missing");
            assert!(
                warning.starts_with("failed to load the detection definition ("),
                "warning must report the load failure, got: {warning}"
            );
            // A missing file falls through regardless of `bundled`, so an exclusive
            // directory layer that is itself scannable does not report its own raw read
            // failure; it reports the name as not found instead (the same property
            // tests/detection_layers.rs pins at the resolver level). `DefinitionError::Io`'s own hardcoded
            // prefix, not its locale-dependent OS-supplied suffix, is what tells the two
            // forms apart reliably.
            assert!(
                !warning.contains("failed to read detection definition at"),
                "with the directory layer scannable but missing the name, the raw read-failure \
                 text must no longer pass through, got: {warning}"
            );
            let dir_display = dir_path.display().to_string();
            assert!(
                warning.contains(&dir_display),
                "the warning must still name the directory layer that was searched, got: \
                 {warning}"
            );
            assert!(
                !warning.to_ascii_lowercase().contains("bundled"),
                "with the bundled layer off, the warning must not claim the bundled layer was \
                 searched too, got: {warning}"
            );
        }
        other => panic!("expected WindowOpened with a detection_warning, got {other:?}"),
    }
}
