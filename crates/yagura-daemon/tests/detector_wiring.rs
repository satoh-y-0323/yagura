#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: production-path wiring of the state detection engine into
//! `IpcServer`.
//!
//! Scope: only that the engine is actually driven from the production IPC path --
//! not the engine's judgement logic (`tests/detector_engine.rs`) nor CLI argument
//! resolution (`tests/cli_args.rs`). Every test drives a real `WindowRegistry` +
//! `IpcServer` pair purely through the IPC wire protocol.

use crate::common;
use crate::common::bins;

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::broadcast;
use tokio::sync::broadcast::error::TryRecvError;
use yagura_test_support::HANG_GUARD;
use yagura_test_support::ipc::{
    Connection, connect_with_retry, read_daemon_frame, send_client_message, unique_endpoint_name,
};
use yagura_test_support::paths::{unique_detection_dir, write_definition};

use yagura_daemon::waiter::StatusAt;
use yagura_daemon::{DaemonConfig, IpcServer, OpenWindowRequest, WindowRegistry};
use yagura_protocol::{ClientMessage, DaemonMessage, WindowStatus};

/// Known value for the mandatory `token` field of `ClientMessage::StatusEvent`.
///
/// The same value reaches the harness through
/// `DaemonConfig::status_token_override` ([`base_config`]). No getter exposes the
/// real token, so injecting a known one on both sides is the only way to exercise
/// the accepting path.
const TEST_STATUS_TOKEN: &str = "0123456789abcdef0123456789abcdef";

/// A status report as a hook sends it, built from JSON so the tests do not
/// depend on which fields the Rust variant carries.
fn status_event(source: &str, status: WindowStatus, window_id: &str) -> ClientMessage {
    let body = serde_json::json!({
        "type": "status_event",
        "source": source,
        "status": status,
        "window_id": window_id,
        "token": TEST_STATUS_TOKEN,
    });
    yagura_protocol::decode_client_message(body.to_string().as_bytes())
        .expect("a status_event must decode")
}

/// Default harness config: `DaemonConfig::default()` plus
/// `status_token_override`, so `StatusEvent` keeps being accepted.
fn base_config() -> DaemonConfig {
    DaemonConfig {
        status_token_override: Some(TEST_STATUS_TOKEN.into()),
        ..DaemonConfig::default()
    }
}

/// `window_id`-only projection of [`common::open_echo_window`]: this file's
/// tests never inspect anything else the open returns.
async fn open_echo_window_id(registry: &WindowRegistry, detection_name: Option<&str>) -> String {
    common::open_echo_window(registry, detection_name)
        .await
        .window_id
}

/// A temporary detection-definition directory, injected straight into
/// `DaemonConfig.detection_dir` so no environment variable is touched.
fn detection_dir(tag: &str) -> PathBuf {
    unique_detection_dir("yagura-test-detection", tag, "create temp detection dir")
}

fn detection_config(dir: &Path, detection_name: &str) -> DaemonConfig {
    DaemonConfig {
        detection_dir: Some(dir.to_path_buf()),
        detection_name: detection_name.to_string(),
        ..base_config()
    }
}

/// Waits for a `StatusChanged` satisfying `predicate`, discarding frames of
/// other kinds (mostly `Output`) meanwhile. Panics if [`HANG_GUARD`] passes
/// without one arriving.
async fn wait_for_status_changed(
    client: &mut Connection,
    predicate: impl Fn(WindowStatus) -> bool,
) -> WindowStatus {
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        if remaining.is_zero() {
            panic!("timed out waiting for a matching StatusChanged");
        }
        match read_daemon_frame(client)
            .await
            .expect_message("frame while waiting for a matching StatusChanged")
        {
            DaemonMessage::StatusChanged {
                window_id: _,
                status,
                ..
            } if predicate(status) => return status,
            _ => continue,
        }
    }
}

/// Waits for the "current status" `StatusChanged` that follows an attach, skipping
/// view frames: ConPTY itself may emit terminal-mode control sequences ahead
/// of any child output.
async fn expect_status_changed_after_attach(client: &mut Connection, expected: WindowStatus) {
    let status = wait_for_status_changed(client, |_| true).await;
    assert_eq!(
        status, expected,
        "unexpected status in the post-attach StatusChanged"
    );
}

/// Sends a `Ping` and returns every `StatusChanged.status` that arrived before
/// its `Pong`, skipping other frames.
async fn statuses_before_pong(client: &mut Connection) -> Vec<WindowStatus> {
    send_client_message(client, &ClientMessage::Ping).await;
    let mut statuses = Vec::new();
    loop {
        match read_daemon_frame(client)
            .await
            .expect_message("frame before the pong")
        {
            DaemonMessage::Pong => return statuses,
            DaemonMessage::StatusChanged { status, .. } => statuses.push(status),
            _ => continue,
        }
    }
}

/// Lets every clock bound run out, then returns every status published to
/// `changes` since it subscribed.
async fn statuses_published_once_every_clock_ran_out(
    changes: &mut broadcast::Receiver<StatusAt>,
) -> Vec<WindowStatus> {
    common::let_every_clock_bound_run_out().await;
    let mut published = Vec::new();
    loop {
        match changes.try_recv() {
            Ok(at) => published.push(at.status.status()),
            Err(TryRecvError::Empty | TryRecvError::Closed) => return published,
            Err(TryRecvError::Lagged(missed)) => {
                panic!("{missed} status change(s) were missed, so their absence cannot be shown")
            }
        }
    }
}

/// Polls the window's model until it shows `marker`.
async fn wait_until_model_shows(registry: &WindowRegistry, window_id: &str, marker: &str) {
    let session = registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .clone();
    let deadline = tokio::time::Instant::now() + HANG_GUARD;
    loop {
        if let Some((text, _)) = session.text_tail(200, 64 * 1024).await
            && text.contains(marker)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's model never showed {marker:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

const WIRING_TEST_DEFINITION: &str = r#"
schema_version = 1
name = "wiring-test"

[debounce]
confirm_ms = 30

[[rules]]
status = "waiting_approval"
pattern = 'NEEDS-APPROVAL'
priority = 10
region = "bottom"
"#;

const STATUS_EVENT_TEST_DEFINITION: &str = r#"
schema_version = 1
name = "status-event-test"
rules = []

[debounce]
confirm_ms = 30
"#;

const RESIZE_TEST_DEFINITION: &str = r#"
schema_version = 1
name = "resize-test"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = 'READY-MARK'
priority = 10
region = "bottom"
"#;

const SELECT_A_DEFINITION: &str = r#"
schema_version = 1
name = "select-a"

[debounce]
confirm_ms = 30

[[rules]]
status = "running"
pattern = 'SELECT-MARK'
priority = 10
region = "bottom"
"#;

const SELECT_B_DEFINITION: &str = r#"
schema_version = 1
name = "select-b"

[debounce]
confirm_ms = 30

[[rules]]
status = "idle"
pattern = 'SELECT-MARK'
priority = 10
region = "bottom"
"#;

const MALFORMED_DEFINITION: &str = r#"
schema_version = 999
name = "broken"
rules = []
"#;

#[tokio::test]
async fn pty_output_pattern_match_reaches_attached_client_as_status_changed() {
    let dir = detection_dir("pattern-match");
    write_definition(&dir, "wiring-test", WIRING_TEST_DEFINITION);
    let config = detection_config(&dir, "wiring-test");
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, Some("wiring-test")).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-pattern-match");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");

    common::write_window_input(&registry, &window_id, b"NEEDS-APPROVAL\r\n").await;

    let status = wait_for_status_changed(&mut client, |s| s == WindowStatus::WaitingApproval).await;
    assert_eq!(status, WindowStatus::WaitingApproval);
}

#[tokio::test]
async fn status_event_reaches_attached_client_as_status_changed() {
    let dir = detection_dir("status-event");
    write_definition(&dir, "status-event-test", STATUS_EVENT_TEST_DEFINITION);
    let config = detection_config(&dir, "status-event-test");
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, Some("status-event-test")).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-status-event");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");

    send_client_message(
        &mut client,
        &status_event("hook-test", WindowStatus::Running, &window_id),
    )
    .await;

    let status = wait_for_status_changed(&mut client, |s| s == WindowStatus::Running).await;
    assert_eq!(status, WindowStatus::Running);
}

#[tokio::test]
async fn child_exit_reaches_attached_client_as_status_changed_exited() {
    let config = base_config();
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let delayed_exit_command = if cfg!(windows) {
        vec![
            "cmd.exe".to_string(),
            "/c".to_string(),
            "timeout /t 1 /nobreak >NUL & exit 7".to_string(),
        ]
    } else {
        vec![
            "/bin/sh".to_string(),
            "-c".to_string(),
            "sleep 1; exit 7".to_string(),
        ]
    };
    let window_id = registry
        .open(OpenWindowRequest {
            command: delayed_exit_command,
            cwd: None,
            name: None,
            vendor: None,
            detection: None,
            on_name_taken: Default::default(),
        })
        .await
        .expect("registry.open should spawn a delayed-exit child")
        .window_id;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-exit");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");

    let status = wait_for_status_changed(&mut client, |s| s == WindowStatus::Exited).await;
    assert_eq!(status, WindowStatus::Exited);
}

#[tokio::test]
async fn status_event_rejects_invalid_source() {
    let config = base_config();
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    // The target window is resolved before the source is judged, so this event
    // needs a real window to be attributed to.
    let window_id = open_echo_window_id(&registry, None).await;
    let pipe_name = unique_endpoint_name("yagura-test", "status-event-invalid-source");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;

    send_client_message(
        &mut client,
        &status_event("Invalid Source!", WindowStatus::Running, &window_id),
    )
    .await;

    let msg = read_daemon_frame(&mut client)
        .await
        .expect_message("error(invalid_argument) response for invalid source");
    match msg {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "invalid_argument"),
        other => panic!("expected error(invalid_argument), got {other:?}"),
    }

    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_daemon_frame(&mut client)
        .await
        .expect_message("pong after invalid status_event source");
    assert!(matches!(pong, DaemonMessage::Pong));

    // Control case: unless a valid source is also shown to pass, an
    // implementation that rejects every StatusEvent would satisfy the above.
    send_client_message(
        &mut client,
        &status_event("cli", WindowStatus::Running, &window_id),
    )
    .await;
    send_client_message(&mut client, &ClientMessage::Ping).await;
    let next = read_daemon_frame(&mut client)
        .await
        .expect_message("response immediately following a valid status_event");
    match next {
        DaemonMessage::Error { code, .. } => {
            assert_ne!(
                code, "invalid_argument",
                "a valid status_event source must not be rejected as invalid_argument"
            );
        }
        DaemonMessage::Pong => {}
        DaemonMessage::StatusChanged { .. } => {
            let pong2 = read_daemon_frame(&mut client)
                .await
                .expect_message("pong after a valid status_event's StatusChanged");
            assert!(matches!(pong2, DaemonMessage::Pong));
        }
        other => panic!("unexpected frame after a valid status_event: {other:?}"),
    }
}

#[tokio::test]
async fn status_changed_is_not_sent_to_unattached_connection() {
    let dir = detection_dir("unattached");
    write_definition(&dir, "wiring-test", WIRING_TEST_DEFINITION);
    let config = detection_config(&dir, "wiring-test");
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, Some("wiring-test")).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-unattached");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());

    let mut attached_client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut attached_client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut attached_client)
        .await
        .expect_message("attach response");

    let mut unattached_client = connect_with_retry(&pipe_name).await;
    // Answered, so the connection's task is running before the change below.
    let mut leaked = statuses_before_pong(&mut unattached_client).await;

    common::write_window_input(&registry, &window_id, b"NEEDS-APPROVAL\r\n").await;

    let status =
        wait_for_status_changed(&mut attached_client, |s| s == WindowStatus::WaitingApproval).await;
    assert_eq!(status, WindowStatus::WaitingApproval);

    // A connection's own task both pushes its status changes and answers its
    // pings. The change was published to every subscriber before the attached
    // connection got it, so a push owed to this connection comes before the
    // first pong or, once that ping is answered, before the second.
    leaked.extend(statuses_before_pong(&mut unattached_client).await);
    leaked.extend(statuses_before_pong(&mut unattached_client).await);
    assert!(
        leaked.is_empty(),
        "unattached connection must never receive StatusChanged, got {leaked:?}"
    );
}

#[tokio::test]
async fn attach_immediately_sends_current_status_as_status_changed() {
    let config = base_config();
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, None).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-attach-initial");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;

    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let attached = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");
    assert!(matches!(attached, DaemonMessage::Attached { .. }));

    expect_status_changed_after_attach(&mut client, WindowStatus::Unknown).await;
}

#[tokio::test]
async fn resize_then_output_still_reaches_status_changed() {
    let dir = detection_dir("resize");
    write_definition(&dir, "resize-test", RESIZE_TEST_DEFINITION);
    let config = detection_config(&dir, "resize-test");
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, Some("resize-test")).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-resize");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");
    let _ = wait_for_status_changed(&mut client, |_| true).await;

    send_client_message(&mut client, &ClientMessage::Resize { cols: 40, rows: 12 }).await;

    common::write_window_input(&registry, &window_id, b"READY-MARK\r\n").await;

    let status = wait_for_status_changed(&mut client, |s| s == WindowStatus::Running).await;
    assert_eq!(status, WindowStatus::Running);
}

async fn assert_selected_definition_status(
    dir: &Path,
    detection_name: &str,
    expected: WindowStatus,
) {
    let config = detection_config(dir, detection_name);
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, Some(detection_name)).await;
    let pipe_name =
        unique_endpoint_name("yagura-test", &format!("detector-select-{detection_name}"));
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");
    let _ = wait_for_status_changed(&mut client, |_| true).await;

    common::write_window_input(&registry, &window_id, b"SELECT-MARK\r\n").await;

    let status = wait_for_status_changed(&mut client, |s| s == expected).await;
    assert_eq!(status, expected);
}

#[tokio::test]
async fn detection_name_selects_matching_definition_among_multiple_files() {
    let dir = detection_dir("select");
    write_definition(&dir, "select-a", SELECT_A_DEFINITION);
    write_definition(&dir, "select-b", SELECT_B_DEFINITION);

    // The same matching text must be judged only by the definition
    // detection_name points at, so a hard-coded choice cannot satisfy both.
    assert_selected_definition_status(&dir, "select-a", WindowStatus::Running).await;
    assert_selected_definition_status(&dir, "select-b", WindowStatus::Idle).await;
}

#[tokio::test]
async fn invalid_detection_name_keeps_engine_unknown_and_daemon_alive() {
    let dir = detection_dir("invalid-name");
    write_definition(&dir, "wiring-test", WIRING_TEST_DEFINITION);
    let config = DaemonConfig {
        detection_dir: Some(dir),
        // Rejected at the syntax stage: is_valid_detection_name accepts
        // [a-z0-9_-] only. This exercises the DaemonConfig-side default (the
        // window below omits `detection`), not open_window's external input.
        detection_name: "../escape".to_string(),
        ..base_config()
    };
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, None).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-invalid-name");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");
    expect_status_changed_after_attach(&mut client, WindowStatus::Unknown).await;

    // Text the wiring-test definition would match, but no definition was
    // selected, so no transition away from Unknown can occur.
    let mut changes = registry
        .get(&window_id)
        .expect("window must exist in registry")
        .subscribe_status();
    common::write_window_input(&registry, &window_id, b"NEEDS-APPROVAL\r\n").await;
    wait_until_model_shows(&registry, &window_id, "NEEDS-APPROVAL").await;
    let published = statuses_published_once_every_clock_ran_out(&mut changes).await;
    assert!(
        published.iter().all(|s| *s == WindowStatus::Unknown),
        "invalid detection_name must never produce a non-unknown status, got {published:?}"
    );

    // The daemon still answers, and pushed nothing but Unknown on the way.
    let pushed = statuses_before_pong(&mut client).await;
    assert!(
        pushed.iter().all(|s| *s == WindowStatus::Unknown),
        "invalid detection_name must never push a non-unknown status, got {pushed:?}"
    );
}

#[tokio::test]
async fn missing_selected_definition_file_falls_back_to_unknown_and_keeps_daemon_alive() {
    // A detection_name from the valid charset whose .toml file is never created.
    let dir = detection_dir("missing-def");
    let config = detection_config(&dir, "missing-def");
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, Some("missing-def")).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-missing-def");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");
    expect_status_changed_after_attach(&mut client, WindowStatus::Unknown).await;

    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_daemon_frame(&mut client)
        .await
        .expect_message("pong despite missing detection definition file");
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn malformed_selected_definition_file_falls_back_to_unknown_and_keeps_daemon_alive() {
    let dir = detection_dir("malformed-def");
    write_definition(&dir, "broken", MALFORMED_DEFINITION);
    let config = detection_config(&dir, "broken");
    let registry = Arc::new(WindowRegistry::new(bins::config_with_holder(
        config.clone(),
    )));
    let window_id = open_echo_window_id(&registry, Some("broken")).await;
    let pipe_name = unique_endpoint_name("yagura-test", "detector-malformed-def");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_daemon_frame(&mut client)
        .await
        .expect_message("attach response");
    expect_status_changed_after_attach(&mut client, WindowStatus::Unknown).await;

    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_daemon_frame(&mut client)
        .await
        .expect_message("pong despite malformed detection definition file");
    assert!(matches!(pong, DaemonMessage::Pong));
}
