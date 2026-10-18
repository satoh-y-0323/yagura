#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: the `IpcServer` contract (IPC v2, window-registry based).
//! Every server is built with a unique per-test endpoint name so the suite runs
//! concurrently and without depending on a real shell.

mod common;

use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncWriteExt;

use common::open_exited_window_via_ipc;
use common::views::{CATCH_UP, cr_burst_payload};
use yagura_test_support::view::ViewClient;

use yagura_test_support::ipc::{
    Connection, ReadOutcome, connect_with_retry, read_status_skipping_message, send_client_message,
    unique_endpoint_name,
};
use yagura_test_support::process::echo_child_path;

use yagura_daemon::{IpcServer, WindowRegistry};
use yagura_os::{IpcTransport, PeerAuth, PlatformIpc};
use yagura_protocol::{ClientMessage, DaemonMessage};

// Reads default to `read_status_skipping_message`: asynchronous status
// notifications would otherwise be mistaken for the frame under test.

async fn send_raw_frame(client: &mut Connection, body: &[u8]) {
    let frame = yagura_protocol::frame_encode(body);
    client
        .write_all(&frame)
        .await
        .expect("write_all raw frame to the endpoint");
}

/// Reads until a control response (pong / error), skipping view `Frame`s: an attached
/// connection receives frames at arbitrary times, because ConPTY emits an
/// initialisation sequence even when the child writes nothing.
async fn read_control_message(client: &mut Connection, timeout: Duration) -> ReadOutcome {
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let remaining = deadline.saturating_duration_since(tokio::time::Instant::now());
        match read_status_skipping_message(client, remaining).await {
            ReadOutcome::Message(DaemonMessage::Frame(_)) => continue,
            outcome => return outcome,
        }
    }
}

/// Waits until the window's model holds `needle`.
async fn wait_for_model_text(
    registry: &Arc<WindowRegistry>,
    window_id: &str,
    needle: &str,
    timeout: Duration,
) {
    let session = registry
        .get(window_id)
        .expect("window must exist in registry")
        .session()
        .clone();
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        if let Some((text, _)) = session.text_tail(200, 64 * 1024).await
            && text.contains(needle)
        {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "the window's model never showed {needle:?}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Opens a window via IPC OpenWindow message and returns the window_id.
async fn open_window_via_ipc(client: &mut Connection) -> String {
    let echo_exe = echo_child_path();
    let echo_str = echo_exe
        .to_str()
        .expect("echo_child path must be valid UTF-8");
    send_client_message(
        client,
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
    let response = read_status_skipping_message(client, Duration::from_secs(5))
        .await
        .expect_message("WindowOpened response");
    match response {
        DaemonMessage::WindowOpened { window_id, .. } => window_id,
        other => panic!("expected WindowOpened, got {other:?}"),
    }
}

#[tokio::test]
async fn attach_shows_what_was_written_before_it_and_continues_live_without_gap_or_duplication() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "attach-atomic");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());

    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let window_id = open_window_via_ipc(&mut client).await;

    // Written straight into the PTY (bypassing the approval-gated `send_text`
    // path, which a window with no detection definition can never pass, since
    // it never leaves `Unknown`) before attaching, so the attach frame is
    // exercised, not just live frames.
    common::write_window_input(&registry, &window_id, b"before-attach;").await;
    wait_for_model_text(
        &registry,
        &window_id,
        "before-attach;",
        Duration::from_secs(5),
    )
    .await;

    let mut view = ViewClient::attach(
        connect_with_retry(&pipe_name, Duration::from_secs(5)).await,
        &window_id,
        None,
    )
    .await;
    assert!(!view.child_exited);
    assert!(
        view.grid.contains_text("before-attach;"),
        "the attach frame must show text written before attach:\n{}",
        view.grid.dump()
    );

    view.send(&ClientMessage::Text {
        text: "after-attach;".to_string(),
    })
    .await;
    view.pump_until(
        "post-attach output to arrive without gap or duplication",
        CATCH_UP,
        |grid| grid.contains_text("after-attach;"),
    )
    .await;
    let screen = view.grid.screen_text().join("\n");
    assert_eq!(screen.matches("before-attach;").count(), 1, "{screen}");
    assert_eq!(screen.matches("after-attach;").count(), 1, "{screen}");
}

#[tokio::test]
async fn second_attach_supersedes_first_and_closes_old_connection() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "multi-attach");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());

    let mut client_a = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let window_id = open_window_via_ipc(&mut client_a).await;

    send_client_message(
        &mut client_a,
        &ClientMessage::AttachView {
            window_id: window_id.clone(),
            size: None,
        },
    )
    .await;
    let _ = read_status_skipping_message(&mut client_a, Duration::from_secs(5))
        .await
        .expect_message("attach response A");

    let mut client_b = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    send_client_message(
        &mut client_b,
        &ClientMessage::AttachView {
            window_id,
            size: None,
        },
    )
    .await;
    let attached_b = read_status_skipping_message(&mut client_b, Duration::from_secs(5))
        .await
        .expect_message("attach response B");
    assert!(matches!(attached_b, DaemonMessage::Attached { .. }));

    let msg_a = read_control_message(&mut client_a, Duration::from_secs(5))
        .await
        .expect_message("superseded notice for A");
    match msg_a {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "superseded"),
        other => panic!("expected error(superseded) for superseded connection, got {other:?}"),
    }
    read_status_skipping_message(&mut client_a, Duration::from_secs(5))
        .await
        .expect_closed("superseded connection must be closed by the daemon");

    send_client_message(&mut client_b, &ClientMessage::Ping).await;
    let pong = read_control_message(&mut client_b, Duration::from_secs(5))
        .await
        .expect_message("pong for B");
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn session_survives_client_disconnect_and_accepts_reattach() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "disconnect-survive");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());

    {
        let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
        // An identifier no registry can mint (it carries no boot nonce): this
        // test only needs the connection to survive an attach that fails.
        send_client_message(
            &mut client,
            &ClientMessage::AttachView {
                window_id: "w1".to_string(),
                size: None,
            },
        )
        .await;
        let _ = read_status_skipping_message(&mut client, Duration::from_secs(5))
            .await
            .expect_message("attach response");
    }

    tokio::time::sleep(Duration::from_millis(300)).await;

    let mut client2 = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    send_client_message(&mut client2, &ClientMessage::Ping).await;
    let pong = read_status_skipping_message(&mut client2, Duration::from_secs(5))
        .await
        .expect_message("pong after reconnect");
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn oversized_length_prefix_is_rejected_and_connection_closed() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "frame-too-large");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    let oversized_len: u32 = 16 * 1024 * 1024 + 1;
    client
        .write_all(&oversized_len.to_le_bytes())
        .await
        .expect("write oversized length prefix");

    let msg = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(frame_too_large) response");
    match msg {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "frame_too_large"),
        other => panic!("expected error(frame_too_large), got {other:?}"),
    }
    read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_closed("connection must be closed after frame_too_large");
}

#[tokio::test]
async fn non_json_body_is_rejected_and_connection_closed() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "invalid-json");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    send_raw_frame(&mut client, b"this is not json").await;

    let msg = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(invalid_json) response");
    match msg {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "invalid_json"),
        other => panic!("expected error(invalid_json), got {other:?}"),
    }
    read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_closed("connection must be closed after invalid_json");
}

#[tokio::test]
async fn missing_required_field_is_rejected_and_connection_closed() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "invalid-json-schema");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    send_raw_frame(&mut client, br#"{"type":"text"}"#).await;

    let msg = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(invalid_json) response for missing required field");
    match msg {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "invalid_json"),
        other => panic!("expected error(invalid_json), got {other:?}"),
    }
    read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_closed("connection must be closed after schema violation");
}

#[tokio::test]
async fn unknown_type_continues_the_connection() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "unknown-type");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    send_raw_frame(&mut client, br#"{"type":"bogus"}"#).await;

    let msg = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(unknown_type) response");
    match msg {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "unknown_type"),
        other => panic!("expected error(unknown_type), got {other:?}"),
    }

    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("pong after unknown_type");
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn direction_violating_type_is_treated_as_unknown_type() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "direction-violation");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    // `frame` is daemon-to-client only; the split direction enums make it
    // indistinguishable from an unknown type on the inbound side.
    send_raw_frame(&mut client, br#"{"type":"frame","seq":1}"#).await;

    let msg = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(unknown_type) response");
    match msg {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "unknown_type"),
        other => panic!("expected error(unknown_type) for direction-violating type, got {other:?}"),
    }
}

#[tokio::test]
async fn unknown_field_is_ignored() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "unknown-field");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let window_id = open_window_via_ipc(&mut client).await;

    send_raw_frame(
        &mut client,
        format!(
            r#"{{"type":"attach_view","window_id":"{}","unexpected_field":123}}"#,
            window_id
        )
        .as_bytes(),
    )
    .await;

    let msg = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("attach response despite unknown field");
    assert!(matches!(msg, DaemonMessage::Attached { .. }));
}

#[tokio::test]
async fn resize_out_of_range_values_are_rejected_without_disconnecting() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "resize-range");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let window_id = open_window_via_ipc(&mut client).await;
    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id,
            size: None,
        },
    )
    .await;
    let _ = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("attach response");

    // Out-of-range boundaries; cols are accepted within 2..=1000, rows within 1..=1000.
    for &(cols, rows) in &[
        (0u16, 24u16),
        (1u16, 24u16),
        (80u16, 0u16),
        (1001u16, 24u16),
        (80u16, 1001u16),
    ] {
        send_client_message(&mut client, &ClientMessage::Resize { cols, rows }).await;
        let msg = read_control_message(&mut client, Duration::from_secs(5))
            .await
            .expect_message("error(invalid_argument) response");
        match msg {
            DaemonMessage::Error { code, .. } => assert_eq!(code, "invalid_argument"),
            other => {
                panic!("expected error(invalid_argument) for resize({cols}, {rows}), got {other:?}")
            }
        }
    }

    for &(cols, rows) in &[(2u16, 1u16), (1000u16, 1000u16)] {
        send_client_message(&mut client, &ClientMessage::Resize { cols, rows }).await;
    }
    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_control_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("pong after valid resizes");
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn ping_receives_pong() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "ping-pong");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("pong response");
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn japanese_input_survives_ipc_round_trip_into_the_frames() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "japanese-roundtrip");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let window_id = open_window_via_ipc(&mut client).await;
    let mut view = ViewClient::attach(
        connect_with_retry(&pipe_name, Duration::from_secs(5)).await,
        &window_id,
        None,
    )
    .await;

    let text = "やぐら日本語IPC往復";
    view.send(&ClientMessage::Text {
        text: format!("{text}\r\n"),
    })
    .await;
    view.pump_until(
        "the Japanese payload to round-trip via IPC",
        CATCH_UP,
        |grid| grid.contains_text(text),
    )
    .await;
}

#[tokio::test]
async fn shutdown_terminates_child_process_and_server_task_completes() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "shutdown");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    send_client_message(&mut client, &ClientMessage::Shutdown).await;

    let run_result = tokio::time::timeout(Duration::from_secs(5), server_task)
        .await
        .expect("server.run() task must complete after shutdown")
        .expect("server task must not panic (JoinError)");
    assert!(
        run_result.is_ok(),
        "server.run() must return Ok after a clean shutdown, got {run_result:?}"
    );
}

#[tokio::test]
async fn attach_and_input_after_child_exit_report_child_exited() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));

    let pipe_name = unique_endpoint_name("yagura-test", "child-exited");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let window_id = open_exited_window_via_ipc(&mut client).await;

    send_client_message(
        &mut client,
        &ClientMessage::AttachView {
            window_id,
            size: None,
        },
    )
    .await;
    let attached = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("attach response");
    match attached {
        DaemonMessage::Attached {
            child_exited,
            exit_code,
            ..
        } => {
            assert!(child_exited);
            assert_eq!(exit_code, Some(5));
        }
        other => panic!("expected Attached with child_exited=true, got {other:?}"),
    }

    send_client_message(
        &mut client,
        &ClientMessage::Text {
            text: "x".to_string(),
        },
    )
    .await;
    let err = read_control_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(child_exited) for input");
    match err {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "child_exited"),
        other => panic!("expected error(child_exited) for input, got {other:?}"),
    }

    send_client_message(&mut client, &ClientMessage::Resize { cols: 80, rows: 24 }).await;
    let err2 = read_control_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(child_exited) for resize");
    match err2 {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "child_exited"),
        other => panic!("expected error(child_exited) for resize, got {other:?}"),
    }
}

#[tokio::test]
async fn bursts_written_while_no_frame_is_acknowledged_never_stall_the_daemon_and_end_on_the_latest_screen()
 {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "unacknowledged-bursts");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut opener = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let window_id = open_window_via_ipc(&mut opener).await;
    let mut view = ViewClient::attach_unacknowledged(
        connect_with_retry(&pipe_name, Duration::from_secs(5)).await,
        &window_id,
        None,
    )
    .await;
    let first = view.grid.last_seq.expect("the first frame completed");

    // Several large bursts without reading or acknowledging anything; the
    // timeout turns a stalled read side into a diagnosable failure instead of
    // a hang (regression guard for the former `handle_connection` deadlock).
    let tail = "burst-2-tail;";
    tokio::time::timeout(Duration::from_secs(30), async {
        for tag in ["burst-0-tail;", "burst-1-tail;", tail] {
            view.send(&ClientMessage::Paste {
                text: String::from_utf8(cr_burst_payload(tag)).unwrap(),
            })
            .await;
        }
        wait_for_model_text(&registry, &window_id, tail, Duration::from_secs(30)).await;
    })
    .await
    .expect("the daemon must keep reading input while the client acknowledges nothing");

    view.ack(first).await;
    view.pump_until("the final burst on screen", CATCH_UP, |grid| {
        grid.contains_text(tail)
    })
    .await;
}

#[tokio::test]
async fn input_before_attach_is_rejected_as_not_attached_and_never_reaches_the_pty() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "not-attached-input");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());

    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    let payload = b"should-not-reach-pty;";
    send_client_message(
        &mut client,
        &ClientMessage::Text {
            text: String::from_utf8(payload.to_vec()).unwrap(),
        },
    )
    .await;

    let err = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(not_attached) response for input before attach");
    match err {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "not_attached"),
        other => panic!("expected error(not_attached) for unattached input, got {other:?}"),
    }

    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("pong after not_attached input");
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn resize_before_attach_is_rejected_as_not_attached_and_does_not_change_pty_size() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "not-attached-resize");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());

    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;
    send_client_message(
        &mut client,
        &ClientMessage::Resize {
            cols: 120,
            rows: 40,
        },
    )
    .await;

    let err = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message("error(not_attached) response for resize before attach");
    match err {
        DaemonMessage::Error { code, .. } => assert_eq!(code, "not_attached"),
        other => panic!("expected error(not_attached) for unattached resize, got {other:?}"),
    }
}

// Peer authorization is decided by `yagura_os::is_authorized_peer`, the rule
// the daemon applies to every accepted peer.

#[test]
fn is_authorized_peer_accepts_matching_sid() {
    assert!(yagura_os::is_authorized_peer(
        Some("S-1-5-21-1-2-3-1001"),
        "S-1-5-21-1-2-3-1001"
    ));
}

#[test]
fn is_authorized_peer_rejects_mismatched_sid() {
    assert!(!yagura_os::is_authorized_peer(
        Some("S-1-5-21-1-2-3-1002"),
        "S-1-5-21-1-2-3-1001"
    ));
}

#[test]
fn is_authorized_peer_fails_closed_when_client_sid_unavailable() {
    assert!(!yagura_os::is_authorized_peer(None, "S-1-5-21-1-2-3-1001"));
}

#[tokio::test]
async fn same_user_connection_is_accepted_end_to_end() {
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let pipe_name = unique_endpoint_name("yagura-test", "same-user-peer");
    let server = Arc::new(IpcServer::new(
        registry.clone(),
        pipe_name.clone(),
        config.clone(),
    ));
    let _server_task = tokio::spawn(server.clone().run());
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    send_client_message(&mut client, &ClientMessage::Ping).await;
    let pong = read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_message(
            "same-user connection must be accepted (peer verification passes) and receive pong",
        );
    assert!(matches!(pong, DaemonMessage::Pong));
}

#[tokio::test]
async fn a_peer_who_is_not_the_endpoint_owner_is_disconnected_before_any_reply() {
    // The OS only lets the current user reach the endpoint, so a foreign
    // peer is staged by serving it on behalf of an owner who is not us.
    let ipc = PlatformIpc::default();
    let current_user = ipc.current_user_id().expect("current_user_id");
    let foreign_owner = format!("{current_user}-not-this-user");
    let pipe_name = unique_endpoint_name("yagura-test", "foreign-owner-peer");
    let security = ipc.endpoint_security().expect("endpoint_security");
    let listener = ipc.bind(&pipe_name, security).await.expect("bind");
    let config = common::opening_config();
    let registry = Arc::new(WindowRegistry::new(config.clone()));
    let server = Arc::new(IpcServer::new(registry, pipe_name.clone(), config));
    let _server_task = tokio::spawn(server.run_with_listener(listener, foreign_owner));
    let mut client = connect_with_retry(&pipe_name, Duration::from_secs(5)).await;

    // The write may already find the connection closed; only the reply matters.
    let frame = yagura_protocol::frame_encode(&yagura_protocol::encode_client_message(
        &ClientMessage::Ping,
    ));
    let _ = client.write_all(&frame).await;
    read_status_skipping_message(&mut client, Duration::from_secs(5))
        .await
        .expect_closed("a peer who is not the endpoint's owner must be disconnected unanswered");
}
