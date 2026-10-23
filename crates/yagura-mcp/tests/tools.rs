#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests for the yagura-mcp tool layer: argument validation, the
//! `ClientMessage` each tool sends, and the JSON it projects from the daemon's
//! reply.
//!
//! Each tool's required arguments are `Option`s so that "missing" and
//! "empty"/"zero" are both constructible against the same pure Rust API; both
//! must be rejected as `invalid_argument`.
//!
//! Two entry checks belong to this layer alone, because the daemon cannot see
//! them: `wait_for_status` refuses a `target_status` that is empty, `"unknown"`
//! or not a status value, and refuses a missing or zero `timeout_sec` rather
//! than inventing a default the daemon never agreed to. `lines` and `force`
//! are `Option` on the wire and are forwarded verbatim instead.

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use yagura_mcp::plan_tools::{OpenTaskWindowArgs, open_task_window};
use yagura_mcp::tools::{
    CloseWindowArgs, GetWindowStatusArgs, InterruptWindowArgs, OpenWindowArgs, ReadChatArgs,
    ReadOutputArgs, SendChatArgs, SendHandoffArgs, SendKeysArgs, SendMessageArgs, ToolError,
    WaitForStatusArgs, broadcast_chat, close_window, get_window_status, interrupt_window,
    list_detections, list_profiles, list_windows, open_window, read_chat, read_output, send_chat,
    send_handoff, send_keys, send_message, wait_for_status,
};
use yagura_mcp::transport::{DaemonTransport, RequesterIdentity, TransportError};
use yagura_protocol::{
    ChatDeliveryInfo, ChatHalt, ChatMessage, ChatParticipant, ClientMessage, ControlKey,
    DaemonMessage, HandoffDelivery, LaunchOptionAlias, LaunchOptionInfo,
    OpenWindowExclusivityError, ProfileInfo, ProfileUnavailableKind, RoomRef, UnknownReason,
    WindowInfo, WindowStatus,
};

/// A scripted [`DaemonTransport`] double: replays queued responses in order
/// and records every request, so a test can assert both what came back and
/// whether the daemon was contacted at all.
struct MockTransport {
    responses: Mutex<Vec<Result<DaemonMessage, TransportError>>>,
    sent: Mutex<Vec<ClientMessage>>,
    call_count: AtomicUsize,
    identity: RequesterIdentity,
}

impl MockTransport {
    /// A transport that must never be called: `round_trip` panics, so a
    /// missing short-circuit fails loudly instead of passing silently.
    fn never_called() -> Self {
        Self {
            responses: Mutex::new(Vec::new()),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
            identity: RequesterIdentity::External,
        }
    }

    /// A transport that replies with a single scripted response, regardless
    /// of the request.
    fn with_response(response: Result<DaemonMessage, TransportError>) -> Self {
        Self {
            responses: Mutex::new(vec![response]),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
            identity: RequesterIdentity::External,
        }
    }

    /// A transport that replies with a single scripted response and reports
    /// `identity` as what the daemon settled about this connection's claim.
    fn with_identity(
        response: Result<DaemonMessage, TransportError>,
        identity: RequesterIdentity,
    ) -> Self {
        Self {
            responses: Mutex::new(vec![response]),
            sent: Mutex::new(Vec::new()),
            call_count: AtomicUsize::new(0),
            identity,
        }
    }

    fn call_count(&self) -> usize {
        self.call_count.load(Ordering::SeqCst)
    }

    fn last_sent(&self) -> ClientMessage {
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .last()
            .expect("round_trip must have been called at least once")
            .clone()
    }
}

impl DaemonTransport for MockTransport {
    fn requester_identity(&self) -> RequesterIdentity {
        self.identity.clone()
    }

    async fn round_trip(&self, msg: ClientMessage) -> Result<DaemonMessage, TransportError> {
        self.call_count.fetch_add(1, Ordering::SeqCst);
        self.sent
            .lock()
            .expect("sent log mutex must not be poisoned")
            .push(msg);
        let mut responses = self
            .responses
            .lock()
            .expect("responses mutex must not be poisoned");
        if responses.is_empty() {
            panic!("MockTransport::round_trip called with no scripted response queued");
        }
        responses.remove(0)
    }
}

fn sample_window_info(window_id: &str) -> WindowInfo {
    WindowInfo {
        window_id: window_id.to_string(),
        name: "pwsh".to_string(),
        vendor: None,
        cwd: r"C:\Users\example".to_string(),
        status: WindowStatus::Running,
        exit_code: None,
        layout_json: None,
        room: None,
        unknown_reason: None,
        detection_warning: None,
        status_rev: None,
        mcp_connected: None,
        status_reports_missing: None,
    }
}

// ---------------------------------------------------------------------
// list_windows
// ---------------------------------------------------------------------

#[tokio::test]
async fn list_windows_maps_window_list_response_to_a_windows_array() {
    let windows = vec![
        sample_window_info("w1"),
        WindowInfo {
            window_id: "w2".to_string(),
            name: "codex".to_string(),
            vendor: Some("openai".to_string()),
            cwd: r"C:\work\proj".to_string(),
            status: WindowStatus::Exited,
            exit_code: Some(0),
            layout_json: None,
            room: None,
            unknown_reason: None,
            detection_warning: None,
            status_rev: None,
            mcp_connected: None,
            status_reports_missing: None,
        },
    ];
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: windows.clone(),
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(transport.last_sent(), ClientMessage::ListWindows);
    let expected_windows: Vec<serde_json::Value> = windows
        .iter()
        .map(|w| serde_json::to_value(w).expect("WindowInfo must serialize"))
        .collect();
    assert_eq!(
        result["windows"],
        serde_json::Value::Array(expected_windows)
    );
}

#[tokio::test]
async fn list_windows_maps_transport_unavailable_to_daemon_unavailable() {
    let transport = MockTransport::with_response(Err(TransportError::Unavailable(
        "pipe not connected".to_string(),
    )));

    let err = list_windows(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "daemon_unavailable");
}

#[tokio::test]
async fn list_windows_maps_transport_protocol_error_to_internal() {
    let transport = MockTransport::with_response(Err(TransportError::Protocol(
        "undecodable frame".to_string(),
    )));

    let err = list_windows(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn list_windows_never_exposes_layout_json_even_when_the_daemon_reports_one() {
    // layout_json is a GUI-internal presentation detail and must not reach
    // MCP callers even when the daemon's WindowInfo carries a value.
    let mut with_layout = sample_window_info("w1");
    with_layout.layout_json = Some("{\"v\":1,\"pos\":{\"x\":1,\"y\":2}}".to_string());
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: vec![with_layout],
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    let windows = result["windows"]
        .as_array()
        .expect("windows must be a JSON array");
    assert_eq!(windows.len(), 1);
    assert!(
        windows[0].get("layout_json").is_none(),
        "the layout_json key must be absent from the MCP list_windows response even when the \
         daemon's WindowInfo carries a Some value, got: {}",
        windows[0]
    );
}

#[tokio::test]
async fn list_windows_passes_the_room_through_while_still_omitting_layout_json() {
    let mut with_room = sample_window_info("w1");
    with_room.room = Some(RoomRef {
        room_id: "r-0123456789abcdef".to_string(),
        name: "yagura".to_string(),
    });
    with_room.layout_json = Some("{\"v\":1}".to_string());
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: vec![with_room],
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    let windows = result["windows"]
        .as_array()
        .expect("windows must be a JSON array");
    assert_eq!(windows.len(), 1);
    assert_eq!(
        windows[0]["room"]["room_id"],
        serde_json::json!("r-0123456789abcdef")
    );
    assert_eq!(windows[0]["room"]["name"], serde_json::json!("yagura"));
    assert!(
        windows[0].get("layout_json").is_none(),
        "layout_json must still be dropped even when room is present, got: {}",
        windows[0]
    );
}

#[tokio::test]
async fn list_windows_maps_an_unexpected_daemon_message_variant_to_internal() {
    // A well-formed reply of the wrong variant is a daemon contract
    // violation: it must map to `internal`, not panic.
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));

    let err = list_windows(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn list_windows_unexpected_daemon_message_error_names_the_variant_without_its_payload() {
    // A payload value distinctive enough that it could only end up in the
    // error text via a full `{:?}` dump of the message -- which would leak
    // connection-internal data (a window id, here) into a tool's error.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "should-not-leak-into-the-error-message".to_string(),
        status: WindowStatus::Idle,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: None,
    }));

    let err = list_windows(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
    let message = err.to_string();
    assert!(
        !message.contains("should-not-leak-into-the-error-message"),
        "the internal error must name the unexpected variant, not dump its full payload via \
         {{:?}}, got {message:?}"
    );
    assert!(
        message.contains("WindowStatus"),
        "the internal error must still name which variant arrived, got {message:?}"
    );
}

#[tokio::test]
async fn list_windows_passes_the_unknown_reason_through() {
    let mut with_reason = sample_window_info("w1");
    with_reason.status = WindowStatus::Unknown;
    with_reason.unknown_reason = Some(UnknownReason::ScreenLost);
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: vec![with_reason],
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    let windows = result["windows"]
        .as_array()
        .expect("windows must be a JSON array");
    assert_eq!(
        windows[0]["unknown_reason"],
        serde_json::json!("screen_lost")
    );
}

#[tokio::test]
async fn list_windows_omits_the_unknown_reason_key_when_the_daemon_reports_none() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: vec![sample_window_info("w1")],
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    let windows = result["windows"]
        .as_array()
        .expect("windows must be a JSON array");
    assert!(
        windows[0].get("unknown_reason").is_none(),
        "the unknown_reason key must be absent (not null) when the daemon reports none, got: {}",
        windows[0]
    );
}

#[tokio::test]
async fn list_windows_passes_the_detection_warning_through() {
    let mut with_warning = sample_window_info("w1");
    with_warning.detection_warning =
        Some("failed to load the detection definition (os error)".to_string());
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: vec![with_warning],
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    let windows = result["windows"]
        .as_array()
        .expect("windows must be a JSON array");
    assert_eq!(
        windows[0]["detection_warning"],
        "failed to load the detection definition (os error)"
    );
}

#[tokio::test]
async fn list_windows_omits_the_detection_warning_key_when_the_daemon_reports_none() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: vec![sample_window_info("w1")],
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    let windows = result["windows"]
        .as_array()
        .expect("windows must be a JSON array");
    assert!(
        windows[0].get("detection_warning").is_none(),
        "the detection_warning key must be absent (not null) when the daemon reports none, got: \
         {}",
        windows[0]
    );
}

// ---------------------------------------------------------------------
// get_window_status
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_window_status_rejects_a_missing_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = GetWindowStatusArgs { window_id: None };

    let err = get_window_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn get_window_status_rejects_an_empty_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = GetWindowStatusArgs {
        window_id: Some(String::new()),
    };

    let err = get_window_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn get_window_status_maps_a_successful_response_transparently() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::WaitingApproval,
        since_ms: 4200,
        last_output_summary: Some("$ ".to_string()),
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::GetWindowStatus {
            window_id: "w1".to_string(),
        }
    );
    assert_eq!(result["window_id"], "w1");
    assert_eq!(result["status"], "waiting_approval");
    // since_ms is always present and passed through verbatim.
    assert_eq!(result["since_ms"].as_u64(), Some(4200));
    assert_eq!(result["last_output_summary"], "$ ");
    assert!(result["exit_code"].is_null());
}

#[tokio::test]
async fn get_window_status_passes_through_null_optional_fields() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Idle,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert!(result["last_output_summary"].is_null());
    assert!(result["exit_code"].is_null());
    assert_eq!(result["since_ms"].as_u64(), Some(0));
}

#[tokio::test]
async fn get_window_status_maps_daemon_window_not_found_error() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "window_not_found".to_string(),
        message: "no such window".to_string(),
        rejected_argument: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("does-not-exist".to_string()),
    };

    let err = get_window_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "window_not_found");
}

#[tokio::test]
async fn get_window_status_maps_transport_unavailable_to_daemon_unavailable() {
    let transport = MockTransport::with_response(Err(TransportError::Unavailable(
        "pipe not connected".to_string(),
    )));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let err = get_window_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "daemon_unavailable");
}

#[tokio::test]
async fn get_window_status_maps_transport_protocol_error_to_internal() {
    let transport = MockTransport::with_response(Err(TransportError::Protocol(
        "undecodable frame".to_string(),
    )));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let err = get_window_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn get_window_status_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let err = get_window_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn get_window_status_includes_the_unknown_reason_key_when_the_daemon_reports_one() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Unknown,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: Some(UnknownReason::ScreenLost),
        detection_warning: None,
        mcp_connected: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(result["unknown_reason"], serde_json::json!("screen_lost"));
}

#[tokio::test]
async fn get_window_status_omits_the_unknown_reason_key_when_the_daemon_reports_none() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Idle,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert!(
        result.get("unknown_reason").is_none(),
        "the unknown_reason key must be absent (not null) when the daemon reports none, got: \
         {result}"
    );
}

#[tokio::test]
async fn get_window_status_includes_the_detection_warning_key_when_the_daemon_reports_one() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Unknown,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: Some(UnknownReason::NoDefinition),
        detection_warning: Some("failed to load the detection definition (os error)".to_string()),
        mcp_connected: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(
        result["detection_warning"],
        "failed to load the detection definition (os error)"
    );
}

#[tokio::test]
async fn get_window_status_omits_the_detection_warning_key_when_the_daemon_reports_none() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Idle,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert!(
        result.get("detection_warning").is_none(),
        "the detection_warning key must be absent (not null) when the daemon reports none, got: \
         {result}"
    );
}

// ---------------------------------------------------------------------
// get_window_status: mcp_connected
// ---------------------------------------------------------------------

#[tokio::test]
async fn get_window_status_includes_mcp_connected_true_when_the_daemon_reports_it() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Running,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: Some(true),
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(
        result.get("mcp_connected"),
        Some(&serde_json::json!(true)),
        "get_window_status must surface the daemon's mcp_connected fact, got: {result}"
    );
}

#[tokio::test]
async fn get_window_status_includes_mcp_connected_false_when_the_daemon_reports_it() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Running,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: Some(false),
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(
        result.get("mcp_connected"),
        Some(&serde_json::json!(false)),
        "get_window_status must surface a false mcp_connected fact too, got: {result}"
    );
}

#[tokio::test]
async fn get_window_status_omits_mcp_connected_when_the_daemon_reports_none() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Running,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: None,
    }));
    let args = GetWindowStatusArgs {
        window_id: Some("w1".to_string()),
    };

    let result = get_window_status(&transport, args)
        .await
        .expect("must succeed");

    assert!(
        result.get("mcp_connected").is_none(),
        "the mcp_connected key must be absent (not null) when the daemon reports none, \
         got: {result}"
    );
}

#[tokio::test]
async fn list_windows_passes_a_true_mcp_connected_through() {
    // list_windows serializes WindowInfo wholesale (unlike get_window_status,
    // which is destructured field by field), so this pins the same
    // property on the other tool that reports it.
    let mut connected = sample_window_info("w1");
    connected.mcp_connected = Some(true);
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowList {
        windows: vec![connected],
    }));

    let result = list_windows(&transport).await.expect("must succeed");

    let windows = result["windows"]
        .as_array()
        .expect("windows must be a JSON array");
    assert_eq!(
        windows[0]["mcp_connected"],
        serde_json::json!(true),
        "list_windows must surface the daemon's mcp_connected fact, got: {}",
        windows[0]
    );
}

// ---------------------------------------------------------------------
// send_message
// ---------------------------------------------------------------------

#[tokio::test]
async fn send_message_rejects_a_missing_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = SendMessageArgs {
        window_id: None,
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

// ---------------------------------------------------------------------
// send_keys
// ---------------------------------------------------------------------

#[tokio::test]
async fn send_keys_rejects_a_missing_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = SendKeysArgs {
        window_id: None,
        keys: Some(vec![ControlKey::Enter]),
    };

    let err = send_keys(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_keys_rejects_an_empty_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = SendKeysArgs {
        window_id: Some(String::new()),
        keys: Some(vec![ControlKey::Enter]),
    };

    let err = send_keys(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_keys_rejects_missing_empty_overlong_and_unknown_keys_without_calling_the_transport() {
    let cases = [
        (None, "keys is required"),
        (Some(Vec::new()), "keys must contain at least one key"),
        (
            Some(vec![ControlKey::Enter; 17]),
            "keys must contain at most 16 keys",
        ),
    ];
    for (keys, expected_message) in cases {
        let transport = MockTransport::never_called();
        let args = SendKeysArgs {
            window_id: Some("w1".to_string()),
            keys,
        };

        let err = send_keys(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), "invalid_argument");
        assert!(err.to_string().ends_with(expected_message));
        assert_eq!(transport.call_count(), 0);
    }
}

#[tokio::test]
async fn send_keys_rejects_unknown_wire_names_without_calling_the_transport() {
    for name in ["tab", "\u{0003}"] {
        let transport = MockTransport::never_called();
        let args: SendKeysArgs = serde_json::from_value(serde_json::json!({
            "window_id": "w1",
            "keys": [name],
        }))
        .expect("unknown wire names must deserialize to ControlKey::Unknown");

        let err = send_keys(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), "invalid_argument");
        assert!(err.to_string().ends_with("unknown control key"));
        assert_eq!(transport.call_count(), 0);
    }
}

#[tokio::test]
async fn send_keys_rejects_escape_before_the_last_key_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = SendKeysArgs {
        window_id: Some("w1".to_string()),
        keys: Some(vec![ControlKey::Escape, ControlKey::Enter]),
    };

    let err = send_keys(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert!(err.to_string().ends_with("escape must be the last key"));
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_message_rejects_an_empty_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = SendMessageArgs {
        window_id: Some(String::new()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_message_rejects_a_missing_text_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: None,
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_message_defaults_an_omitted_mode_to_immediate_and_calls_the_transport() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::TextSent {
        window_id: "w1".to_string(),
    }));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let result = send_message(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::SendText {
            window_id: "w1".to_string(),
            text: "hello".to_string(),
        }
    );
    assert_eq!(result["ok"].as_bool(), Some(true));
}

#[tokio::test]
async fn send_message_accepts_an_explicit_immediate_mode() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::TextSent {
        window_id: "w1".to_string(),
    }));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: Some("immediate".to_string()),
    };

    let result = send_message(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(result["ok"].as_bool(), Some(true));
}

#[tokio::test]
async fn send_message_rejects_an_empty_mode_as_unsupported_without_calling_the_transport() {
    // Mode evaluation happens before any daemon round trip, whatever the
    // target window's state, so the daemon is never consulted.
    let transport = MockTransport::never_called();
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: Some(String::new()),
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "unsupported");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_message_rejects_a_differently_cased_mode_as_unsupported() {
    let transport = MockTransport::never_called();
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: Some("Immediate".to_string()),
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "unsupported");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_message_rejects_an_unknown_mode_value_as_unsupported() {
    let transport = MockTransport::never_called();
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: Some("queue_until_input".to_string()),
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "unsupported");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_message_maps_daemon_child_exited_error() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "child_exited".to_string(),
        message: "process has exited".to_string(),
        rejected_argument: None,
    }));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "child_exited");
}

#[tokio::test]
async fn send_message_maps_daemon_window_not_found_error() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "window_not_found".to_string(),
        message: "no such window".to_string(),
        rejected_argument: None,
    }));
    let args = SendMessageArgs {
        window_id: Some("does-not-exist".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "window_not_found");
}

/// The daemon refuses a send whose text it never wrote with its own code, so
/// a caller can tell "nothing was written" from a daemon-side failure.
#[tokio::test]
async fn send_message_maps_daemon_not_written_error() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "not_written".to_string(),
        message: "the window's composer already held content this send could not account for"
            .to_string(),
        rejected_argument: None,
    }));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "not_written");
    assert!(
        err.to_string()
            .contains("the window's composer already held content this send could not account for"),
        "the daemon's reason must survive the mapping, got {err}"
    );
}

#[tokio::test]
async fn send_message_maps_transport_unavailable_to_daemon_unavailable() {
    let transport = MockTransport::with_response(Err(TransportError::Unavailable(
        "pipe not connected".to_string(),
    )));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "daemon_unavailable");
}

#[tokio::test]
async fn send_message_maps_transport_protocol_error_to_internal() {
    let transport = MockTransport::with_response(Err(TransportError::Protocol(
        "undecodable frame".to_string(),
    )));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

/// Sends `text` through a transport double that accepts the round trip, and
/// returns the `text` the daemon would have received.
async fn sent_text_for(text: &str) -> String {
    let transport = MockTransport::with_response(Ok(DaemonMessage::TextSent {
        window_id: "w1".to_string(),
    }));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some(text.to_string()),
        mode: None,
    };

    send_message(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    match transport.last_sent() {
        ClientMessage::SendText { window_id, text } => {
            assert_eq!(window_id, "w1");
            text
        }
        other => panic!("expected a SendText request, got {other:?}"),
    }
}

#[tokio::test]
async fn send_message_normalizes_a_trailing_lone_lf_to_a_lone_cr() {
    // An MCP client cannot express a raw CR in a JSON string argument, so a
    // trailing LF is the only submit it can write -- and a bare LF does not
    // submit the line on a Windows console.
    assert_eq!(sent_text_for("cmd\n").await, "cmd\r");
}

#[tokio::test]
async fn send_message_collapses_an_existing_crlf_to_a_lone_cr() {
    // A CRLF reaching PSReadLine is read as Enter followed by a newline
    // insertion, which leaves a `>>` continuation prompt behind.
    assert_eq!(sent_text_for("a\r\nb\n").await, "a\rb\r");
}

#[tokio::test]
async fn send_message_leaves_text_without_a_newline_unchanged() {
    // "type this but do not submit it" must stay expressible: no terminator
    // is ever appended.
    assert_eq!(sent_text_for("no newline").await, "no newline");
}

#[tokio::test]
async fn send_message_normalizes_every_line_terminator_not_just_the_last() {
    assert_eq!(sent_text_for("line1\nline2\n").await, "line1\rline2\r");
}

#[tokio::test]
async fn send_message_maps_a_non_text_sent_non_error_response_to_internal() {
    // Only `TextSent` counts as success: any other well-formed non-error
    // reply is a daemon contract violation and maps to `internal`.
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn send_message_does_not_enforce_a_text_length_limit_itself() {
    // The text-size limit belongs to the daemon's `send_text` handler alone.
    // Short-circuiting on size here would duplicate that constant, so
    // oversized text is forwarded and the daemon's error relayed.
    let oversized_text = "a".repeat(2 * 1024 * 1024);
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "invalid_argument".to_string(),
        message: "text exceeds maximum size".to_string(),
        rejected_argument: None,
    }));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some(oversized_text.clone()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    // The transport must actually have been called: proves the MCP layer did
    // not reject the oversized text itself before reaching the daemon.
    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::SendText {
            window_id: "w1".to_string(),
            text: oversized_text,
        }
    );
    assert_eq!(err.code(), "invalid_argument");
}

// ---------------------------------------------------------------------
// open_window
// ---------------------------------------------------------------------

#[tokio::test]
async fn open_window_rejects_an_empty_command_array_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn open_window_returns_the_window_id_from_a_successful_response() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w2".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string(), "-NoLogo".to_string()],
        cwd: Some(r"C:\work".to_string()),
        name: Some("build-shell".to_string()),
        vendor: Some("microsoft".to_string()),
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let result = open_window(&transport, args).await.expect("must succeed");

    assert_eq!(result["window_id"], "w2");
}

#[tokio::test]
async fn open_window_omits_detection_from_the_daemon_message_when_not_provided() {
    // Omitting `detection` forwards `detection: None`, so a caller that never
    // sets it keeps getting the daemon's own default.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w2".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    open_window(&transport, args).await.expect("must succeed");

    match transport.last_sent() {
        ClientMessage::OpenWindow { detection, .. } => assert_eq!(detection, None),
        other => panic!("expected ClientMessage::OpenWindow, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_forwards_a_provided_detection_argument_verbatim() {
    // The bridge forwards `detection` unchanged; validating the name stays
    // the daemon's job, so this only pins the value reaching the wire.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w2".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: Some("claude-code".to_string()),
        profile: None,
        model: None,
        effort: None,
    };

    open_window(&transport, args).await.expect("must succeed");

    match transport.last_sent() {
        ClientMessage::OpenWindow { detection, .. } => {
            assert_eq!(detection, Some("claude-code".to_string()));
        }
        other => panic!("expected ClientMessage::OpenWindow, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_includes_the_detection_warning_key_when_the_daemon_reports_one() {
    // Passthrough is "Some -> key present"; the paired test below covers the
    // other half, "None -> key absent".
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w9".to_string(),
        detection_warning: Some("failed to load the detection definition (os error)".to_string()),
        name: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let result = open_window(&transport, args).await.expect("must succeed");

    assert_eq!(result["window_id"], "w9");
    assert_eq!(
        result["detection_warning"],
        "failed to load the detection definition (os error)"
    );
}

#[tokio::test]
async fn open_window_omits_the_detection_warning_key_when_the_daemon_reports_none() {
    // None omits the key entirely (never `"detection_warning": null`),
    // matching the wire's own skip_serializing_if contract.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w10".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let result = open_window(&transport, args).await.expect("must succeed");

    assert_eq!(result["window_id"], "w10");
    assert!(
        result.get("detection_warning").is_none(),
        "detection_warning key must be absent (not null) when the daemon reports no warning, \
         got: {result}"
    );
}

#[tokio::test]
async fn open_window_maps_daemon_resource_exhausted_error() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "resource_exhausted".to_string(),
        message: "too many windows".to_string(),
        rejected_argument: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "resource_exhausted");
}

#[tokio::test]
async fn open_window_maps_daemon_spawn_failed_error() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "spawn_failed".to_string(),
        message: "executable not found".to_string(),
        rejected_argument: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["does-not-exist.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "spawn_failed");
}

#[tokio::test]
async fn open_window_maps_daemon_invalid_argument_error() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "invalid_argument".to_string(),
        message: "cwd does not exist".to_string(),
        rejected_argument: None,
    }));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: Some(r"C:\does\not\exist".to_string()),
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
}

#[tokio::test]
async fn open_window_maps_transport_unavailable_to_daemon_unavailable() {
    let transport = MockTransport::with_response(Err(TransportError::Unavailable(
        "pipe not connected".to_string(),
    )));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "daemon_unavailable");
}

#[tokio::test]
async fn open_window_maps_transport_protocol_error_to_internal() {
    let transport = MockTransport::with_response(Err(TransportError::Protocol(
        "undecodable frame".to_string(),
    )));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn open_window_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// ---------------------------------------------------------------------
// ToolError::code: the fixed error-code vocabulary
// ---------------------------------------------------------------------

#[test]
fn tool_error_code_covers_every_adr_mcp_2_error_code() {
    let cases: [(ToolError, &str); 9] = [
        (
            ToolError::DaemonUnavailable("x".to_string()),
            "daemon_unavailable",
        ),
        (
            ToolError::InvalidArgument("x".to_string()),
            "invalid_argument",
        ),
        (
            ToolError::WindowNotFound("x".to_string()),
            "window_not_found",
        ),
        (ToolError::Unsupported("x".to_string()), "unsupported"),
        (ToolError::ChildExited("x".to_string()), "child_exited"),
        (
            ToolError::ResourceExhausted("x".to_string()),
            "resource_exhausted",
        ),
        (ToolError::SpawnFailed("x".to_string()), "spawn_failed"),
        (
            ToolError::ProfileDisabled("x".to_string()),
            "profile_disabled",
        ),
        (ToolError::Internal("x".to_string()), "internal"),
    ];
    for (err, expected_code) in cases {
        assert_eq!(err.code(), expected_code);
    }
}

// =====================================================================
// Control tools
// =====================================================================

/// Every daemon error code the control tools can receive back. Each must
/// reach the caller as the *same* MCP code: the bridge must never collapse a
/// specific refusal into `internal`.
const CONTROL_DAEMON_ERROR_CODES: [&str; 6] = [
    "approval_required",
    "status_unknown",
    "rate_limited",
    "auth_failed",
    "window_not_found",
    "child_exited",
];

/// The message every scripted daemon refusal carries, so an assertion can
/// require it to survive the mapping instead of being swallowed.
const DAEMON_REFUSAL_MESSAGE: &str = "daemon refused the request";

fn daemon_error(code: &str) -> DaemonMessage {
    DaemonMessage::Error {
        code: code.to_string(),
        message: DAEMON_REFUSAL_MESSAGE.to_string(),
        rejected_argument: None,
    }
}

// ---------------------------------------------------------------------
// interrupt_window
// ---------------------------------------------------------------------

#[tokio::test]
async fn interrupt_window_rejects_a_missing_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = InterruptWindowArgs { window_id: None };

    let err = interrupt_window(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn interrupt_window_rejects_an_empty_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = InterruptWindowArgs {
        window_id: Some(String::new()),
    };

    let err = interrupt_window(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn interrupt_window_maps_a_window_interrupted_response_to_the_window_id() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowInterrupted {
        window_id: "w1".to_string(),
    }));
    let args = InterruptWindowArgs {
        window_id: Some("w1".to_string()),
    };

    let result = interrupt_window(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::InterruptWindow {
            window_id: "w1".to_string(),
        }
    );
    assert_eq!(result["window_id"], "w1");
}

#[tokio::test]
async fn interrupt_window_relays_every_daemon_error_code_unchanged() {
    for code in CONTROL_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = InterruptWindowArgs {
            window_id: Some("w1".to_string()),
        };

        let err = interrupt_window(&transport, args)
            .await
            .expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn interrupt_window_relays_the_status_unknown_message_unchanged() {
    let message = "the window's status could not be detected (the screen model was lost), so \
                    this operation is refused";
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "status_unknown".to_string(),
        message: message.to_string(),
        rejected_argument: None,
    }));
    let args = InterruptWindowArgs {
        window_id: Some("w1".to_string()),
    };

    let err = interrupt_window(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "status_unknown");
    assert_eq!(
        err.to_string(),
        format!("the window's status is unknown: {message}")
    );
}

#[tokio::test]
async fn interrupt_window_maps_an_unexpected_daemon_message_variant_to_internal() {
    // A `window_closed` reply to an interrupt request is a daemon contract
    // violation: control responses are not interchangeable.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowClosed {
        window_id: "w1".to_string(),
    }));
    let args = InterruptWindowArgs {
        window_id: Some("w1".to_string()),
    };

    let err = interrupt_window(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// ---------------------------------------------------------------------
// close_window
// ---------------------------------------------------------------------

#[tokio::test]
async fn close_window_rejects_a_missing_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = CloseWindowArgs {
        window_id: None,
        force: None,
    };

    let err = close_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn close_window_rejects_an_empty_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = CloseWindowArgs {
        window_id: Some(String::new()),
        force: None,
    };

    let err = close_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn close_window_maps_a_window_closed_response_to_the_window_id() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowClosed {
        window_id: "w1".to_string(),
    }));
    let args = CloseWindowArgs {
        window_id: Some("w1".to_string()),
        force: None,
    };

    let result = close_window(&transport, args).await.expect("must succeed");

    assert_eq!(result["window_id"], "w1");
}

#[tokio::test]
async fn close_window_forwards_an_omitted_force_as_none() {
    // The `force` default lives in the daemon: forwarding an omitted value as
    // `None` keeps that default with exactly one owner.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowClosed {
        window_id: "w1".to_string(),
    }));
    let args = CloseWindowArgs {
        window_id: Some("w1".to_string()),
        force: None,
    };

    close_window(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::CloseWindow {
            window_id: "w1".to_string(),
            force: None,
        }
    );
}

#[tokio::test]
async fn close_window_forwards_an_explicit_force_true() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowClosed {
        window_id: "w1".to_string(),
    }));
    let args = CloseWindowArgs {
        window_id: Some("w1".to_string()),
        force: Some(true),
    };

    close_window(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::CloseWindow {
            window_id: "w1".to_string(),
            force: Some(true),
        }
    );
}

#[tokio::test]
async fn close_window_forwards_an_explicit_force_false() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowClosed {
        window_id: "w1".to_string(),
    }));
    let args = CloseWindowArgs {
        window_id: Some("w1".to_string()),
        force: Some(false),
    };

    close_window(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::CloseWindow {
            window_id: "w1".to_string(),
            force: Some(false),
        }
    );
}

#[tokio::test]
async fn close_window_relays_every_daemon_error_code_unchanged() {
    for code in CONTROL_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = CloseWindowArgs {
            window_id: Some("w1".to_string()),
            force: None,
        };

        let err = close_window(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn close_window_relays_the_status_unknown_message_unchanged() {
    let message = "the window's status could not be detected (nothing has been judged yet), so \
                    this operation is refused";
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "status_unknown".to_string(),
        message: message.to_string(),
        rejected_argument: None,
    }));
    let args = CloseWindowArgs {
        window_id: Some("w1".to_string()),
        force: None,
    };

    let err = close_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "status_unknown");
    assert_eq!(
        err.to_string(),
        format!("the window's status is unknown: {message}")
    );
}

#[tokio::test]
async fn close_window_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = CloseWindowArgs {
        window_id: Some("w1".to_string()),
        force: None,
    };

    let err = close_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// ---------------------------------------------------------------------
// wait_for_status
// ---------------------------------------------------------------------

#[tokio::test]
async fn wait_for_status_rejects_a_missing_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: None,
        target_status: Some("idle".to_string()),
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_rejects_an_empty_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: Some(String::new()),
        target_status: Some("idle".to_string()),
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_rejects_a_missing_target_status_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: None,
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_rejects_an_empty_target_status_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some(String::new()),
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_rejects_unknown_as_a_target_status_without_calling_the_transport() {
    // Waiting *for* `Unknown` would turn the fail-safe state into a
    // completion condition, so it is refused before the daemon is contacted.
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("unknown".to_string()),
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_rejects_a_target_status_that_is_not_a_window_status_value() {
    // The wire field is typed (`target_status: WindowStatus`), so a string
    // that is not a status value cannot be forwarded at all.
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("Idle".to_string()),
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_rejects_a_zero_timeout_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("idle".to_string()),
        timeout_sec: Some(0),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_rejects_a_missing_timeout_without_calling_the_transport() {
    // `timeout_sec` is a bare `u64` on the wire, so this layer has no default
    // to apply: a missing value is invalid, not a silently invented timeout.
    let transport = MockTransport::never_called();
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("idle".to_string()),
        timeout_sec: None,
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn wait_for_status_forwards_the_parsed_target_status_and_timeout() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WaitResult {
        window_id: "w1".to_string(),
        outcome: "reached".to_string(),
        status: WindowStatus::WaitingInput,
    }));
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("waiting_input".to_string()),
        timeout_sec: Some(45),
    };

    let result = wait_for_status(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::WaitForStatus {
            window_id: "w1".to_string(),
            target_status: WindowStatus::WaitingInput,
            timeout_sec: 45,
        }
    );
    assert_eq!(result["window_id"], "w1");
    assert_eq!(result["outcome"], "reached");
    assert_eq!(result["status"], "waiting_input");
}

#[tokio::test]
async fn wait_for_status_returns_a_timeout_outcome_as_a_successful_result() {
    // A timeout is a normal response, not an error: `status` is the one
    // observed when the wait gave up and need not equal the target.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WaitResult {
        window_id: "w1".to_string(),
        outcome: "timeout".to_string(),
        status: WindowStatus::Running,
    }));
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("idle".to_string()),
        timeout_sec: Some(5),
    };

    let result = wait_for_status(&transport, args)
        .await
        .expect("a timeout outcome must not be reported as a tool error");

    assert_eq!(result["outcome"], "timeout");
    assert_eq!(result["status"], "running");
}

#[tokio::test]
async fn wait_for_status_relays_every_daemon_error_code_unchanged() {
    for code in CONTROL_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = WaitForStatusArgs {
            window_id: Some("w1".to_string()),
            target_status: Some("idle".to_string()),
            timeout_sec: Some(30),
        };

        let err = wait_for_status(&transport, args)
            .await
            .expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn wait_for_status_relays_the_status_unknown_message_unchanged() {
    let message = "the window's status could not be detected (a rule or a status event reported \
                    unknown), so this operation is refused";
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "status_unknown".to_string(),
        message: message.to_string(),
        rejected_argument: None,
    }));
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("idle".to_string()),
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "status_unknown");
    assert_eq!(
        err.to_string(),
        format!("the window's status is unknown: {message}")
    );
}

#[tokio::test]
async fn wait_for_status_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = WaitForStatusArgs {
        window_id: Some("w1".to_string()),
        target_status: Some("idle".to_string()),
        timeout_sec: Some(30),
    };

    let err = wait_for_status(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// ---------------------------------------------------------------------
// read_output
// ---------------------------------------------------------------------

#[tokio::test]
async fn read_output_rejects_a_missing_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = ReadOutputArgs {
        window_id: None,
        lines: None,
    };

    let err = read_output(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn read_output_rejects_an_empty_window_id_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let args = ReadOutputArgs {
        window_id: Some(String::new()),
        lines: None,
    };

    let err = read_output(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn read_output_forwards_an_omitted_lines_as_none() {
    // The `lines` default and its upper bound both live in the daemon; the
    // bridge forwards absence as absence.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOutput {
        window_id: "w1".to_string(),
        text: "line-a\nline-b".to_string(),
        truncated: false,
    }));
    let args = ReadOutputArgs {
        window_id: Some("w1".to_string()),
        lines: None,
    };

    read_output(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadOutput {
            window_id: "w1".to_string(),
            lines: None,
        }
    );
}

#[tokio::test]
async fn read_output_forwards_an_explicit_lines_value() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOutput {
        window_id: "w1".to_string(),
        text: "line-a".to_string(),
        truncated: false,
    }));
    let args = ReadOutputArgs {
        window_id: Some("w1".to_string()),
        lines: Some(25),
    };

    read_output(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadOutput {
            window_id: "w1".to_string(),
            lines: Some(25),
        }
    );
}

#[tokio::test]
async fn read_output_projects_the_text_and_a_false_truncated_flag() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOutput {
        window_id: "w1".to_string(),
        text: "line-a\nline-b".to_string(),
        truncated: false,
    }));
    let args = ReadOutputArgs {
        window_id: Some("w1".to_string()),
        lines: None,
    };

    let result = read_output(&transport, args).await.expect("must succeed");

    assert_eq!(result["window_id"], "w1");
    assert_eq!(result["text"], "line-a\nline-b");
    assert_eq!(result["truncated"].as_bool(), Some(false));
}

#[tokio::test]
async fn read_output_exposes_a_true_truncated_flag_to_the_caller() {
    // `truncated` tells the caller that older output was dropped; hiding it
    // would let an agent mistake a partial tail for the whole output.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOutput {
        window_id: "w1".to_string(),
        text: "tail-only".to_string(),
        truncated: true,
    }));
    let args = ReadOutputArgs {
        window_id: Some("w1".to_string()),
        lines: Some(10),
    };

    let result = read_output(&transport, args).await.expect("must succeed");

    assert_eq!(result["truncated"].as_bool(), Some(true));
}

#[tokio::test]
async fn read_output_relays_every_daemon_error_code_unchanged() {
    for code in CONTROL_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = ReadOutputArgs {
            window_id: Some("w1".to_string()),
            lines: None,
        };

        let err = read_output(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn read_output_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = ReadOutputArgs {
        window_id: Some("w1".to_string()),
        lines: None,
    };

    let err = read_output(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// ---------------------------------------------------------------------
// list_detections
// ---------------------------------------------------------------------

#[tokio::test]
async fn list_detections_maps_detection_list_response_to_a_detections_array() {
    let names = vec![
        "claude-code".to_string(),
        "codex-cli".to_string(),
        "generic-shell".to_string(),
    ];
    let transport = MockTransport::with_response(Ok(DaemonMessage::DetectionList {
        names: names.clone(),
        bundled_names: vec!["generic-shell".to_string()],
        directory_names: vec!["claude-code".to_string(), "codex-cli".to_string()],
        warning: None,
    }));

    let result = list_detections(&transport).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(transport.last_sent(), ClientMessage::ListDetections);
    let expected_names: Vec<serde_json::Value> = names
        .iter()
        .map(|name| serde_json::Value::String(name.clone()))
        .collect();
    assert_eq!(
        result["detections"],
        serde_json::Value::Array(expected_names)
    );
    assert_eq!(
        result["bundled_names"],
        serde_json::json!(["generic-shell"]),
        "bundled_names must be carried into the response, got: {result}"
    );
    assert_eq!(
        result["directory_names"],
        serde_json::json!(["claude-code", "codex-cli"]),
        "directory_names must be carried into the response, got: {result}"
    );
    assert!(
        result.get("warning").is_none(),
        "the warning key must be absent when the daemon reported no warning, got: {result}"
    );
}

#[tokio::test]
async fn list_detections_passes_a_daemon_warning_through_without_failing() {
    // A directory the daemon could not enumerate is reported as data, not as
    // an error: the caller still gets a (possibly empty) list and can tell
    // "no definitions" apart from "the daemon is unreachable".
    let warning = "failed to read the detection directory: access denied".to_string();
    let transport = MockTransport::with_response(Ok(DaemonMessage::DetectionList {
        names: Vec::new(),
        bundled_names: Vec::new(),
        directory_names: Vec::new(),
        warning: Some(warning.clone()),
    }));

    let result = list_detections(&transport)
        .await
        .expect("a warning must not turn the response into an error");

    assert_eq!(transport.last_sent(), ClientMessage::ListDetections);
    assert_eq!(result["detections"], serde_json::Value::Array(Vec::new()));
    assert_eq!(
        result["bundled_names"],
        serde_json::Value::Array(Vec::new()),
        "bundled_names must be present, empty, even under a directory-scan warning, got: {result}"
    );
    assert_eq!(
        result["directory_names"],
        serde_json::Value::Array(Vec::new()),
        "directory_names must be present, empty, even under a directory-scan warning, got: {result}"
    );
    assert_eq!(result["warning"], serde_json::Value::String(warning));
}

#[tokio::test]
async fn list_detections_maps_transport_unavailable_to_daemon_unavailable() {
    let transport = MockTransport::with_response(Err(TransportError::Unavailable(
        "pipe not connected".to_string(),
    )));

    let err = list_detections(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "daemon_unavailable");
}

#[tokio::test]
async fn list_detections_maps_transport_protocol_error_to_internal() {
    let transport = MockTransport::with_response(Err(TransportError::Protocol(
        "undecodable frame".to_string(),
    )));

    let err = list_detections(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn list_detections_maps_an_unexpected_daemon_message_variant_to_internal() {
    // A well-formed reply of the wrong variant is a daemon contract
    // violation: it must map to `internal`, not panic.
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));

    let err = list_detections(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// ---------------------------------------------------------------------
// ToolError::code(): the control-tool error codes
// ---------------------------------------------------------------------

#[test]
fn tool_error_code_covers_every_control_tool_error_code() {
    let cases: [(ToolError, &str); 4] = [
        (
            ToolError::ApprovalRequired("x".to_string()),
            "approval_required",
        ),
        (ToolError::StatusUnknown("x".to_string()), "status_unknown"),
        (ToolError::RateLimited("x".to_string()), "rate_limited"),
        (ToolError::AuthFailed("x".to_string()), "auth_failed"),
    ];
    for (err, expected_code) in cases {
        assert_eq!(err.code(), expected_code);
    }
}

// =====================================================================
// Launch profiles
// =====================================================================

/// A profile whose every optional field carries a value, so that each test
/// below can blank exactly the field it is about.
fn sample_profile(id: &str, display_name: &str) -> ProfileInfo {
    ProfileInfo {
        id: id.to_string(),
        display_name: display_name.to_string(),
        detection: Some("claude-code".to_string()),
        detection_warning: None,
        cwd: Some(r"C:\work".to_string()),
        available: true,
        unavailable_reason: None,
        command: vec!["claude".to_string()],
        model: None,
        effort: None,
        enabled: true,
        unavailable_kind: None,
        unavailable_guidance: None,
        limits: Vec::new(),
    }
}

/// The sorted key set of a JSON object, so an assertion can pin the whole
/// projection instead of only the keys it remembered to look up.
fn sorted_keys(value: &serde_json::Value) -> Vec<String> {
    let mut keys: Vec<String> = value
        .as_object()
        .expect("expected a JSON object")
        .keys()
        .cloned()
        .collect();
    keys.sort();
    keys
}

// ---------------------------------------------------------------------
// list_profiles
// ---------------------------------------------------------------------

#[tokio::test]
async fn list_profiles_maps_profile_list_response_to_a_profiles_array() {
    let profiles = vec![
        sample_profile("claude-code", "Claude Code"),
        ProfileInfo {
            id: "codex-cli".to_string(),
            display_name: "Codex CLI".to_string(),
            detection: Some("codex-cli".to_string()),
            detection_warning: None,
            cwd: None,
            available: false,
            unavailable_reason: Some("codex was not found on PATH".to_string()),
            command: vec!["codex".to_string()],
            model: None,
            effort: None,
            enabled: true,
            unavailable_kind: Some(ProfileUnavailableKind::NotFound),
            unavailable_guidance: None,
            limits: Vec::new(),
        },
    ];
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles,
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(transport.last_sent(), ClientMessage::ListProfiles);
    let entries = result["profiles"]
        .as_array()
        .expect("profiles must be a JSON array");
    // The daemon's order is the caller's order: the bridge neither re-sorts
    // nor drops entries.
    let ids: Vec<&str> = entries
        .iter()
        .map(|entry| entry["id"].as_str().expect("id must be a string"))
        .collect();
    assert_eq!(ids, vec!["claude-code", "codex-cli"]);
    assert_eq!(entries[0]["display_name"], "Claude Code");
    assert_eq!(entries[0]["detection"], "claude-code");
    assert_eq!(entries[0]["cwd"], r"C:\work");
    // A boolean, not the string "true": the caller gates its UI on this.
    assert_eq!(entries[0]["available"], serde_json::Value::Bool(true));
    assert_eq!(entries[0]["command"], serde_json::json!(["claude"]));
    assert_eq!(entries[1]["display_name"], "Codex CLI");
    assert_eq!(entries[1]["available"], serde_json::Value::Bool(false));
    assert_eq!(
        entries[1]["unavailable_reason"],
        "codex was not found on PATH"
    );
    assert_eq!(
        entries[1]["command"],
        serde_json::json!(["codex"]),
        "the command is the argv a launch would carry regardless of availability: an \
         entry unavailable because the executable was not found still names it"
    );
}

#[tokio::test]
async fn list_profiles_omits_every_key_the_daemon_left_empty() {
    // "No value" must be an absent key, never `null`, so that a caller never
    // has to tell `null` apart from "the daemon said nothing" -- the same
    // omission rule `list_detections` and `open_window` already follow.
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![ProfileInfo {
            id: "generic-shell".to_string(),
            display_name: "Shell".to_string(),
            detection: None,
            detection_warning: None,
            cwd: None,
            available: true,
            unavailable_reason: None,
            command: Vec::new(),
            model: None,
            effort: None,
            enabled: true,
            unavailable_kind: None,
            unavailable_guidance: None,
            limits: Vec::new(),
        }],
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    let entry = &result["profiles"][0];
    assert_eq!(
        sorted_keys(entry),
        vec![
            "available".to_string(),
            "display_name".to_string(),
            "id".to_string(),
        ],
        "a profile with no optional value must carry its always-present keys only -- an \
         empty command is left out the same way, got: {entry}"
    );
}

#[tokio::test]
async fn list_profiles_reports_the_command_the_daemon_selected_for_this_operating_system() {
    let mut profile = sample_profile("claude-code", "Claude Code");
    profile.command = vec!["claude".to_string(), "--resume".to_string()];
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![profile],
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    let entry = &result["profiles"][0];
    assert_eq!(
        entry["command"],
        serde_json::json!(["claude", "--resume"]),
        "the unresolved argv the daemon selected for this operating system is surfaced \
         verbatim, got: {entry}"
    );
}

#[tokio::test]
async fn list_profiles_omits_the_command_key_when_the_daemon_reported_an_empty_command() {
    let mut profile = sample_profile("no-argv-here", "No Argv Here");
    profile.command = Vec::new();
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![profile],
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    let entry = &result["profiles"][0];
    assert!(
        entry.get("command").is_none(),
        "an empty command means nothing to launch here, and (like every other optional \
         field this bridge maps) that is an absent key, not an empty array, got: {entry}"
    );
}

#[tokio::test]
async fn list_profiles_keeps_the_reason_of_an_unavailable_profile() {
    // Fail-safe: a profile that cannot be launched must say why, otherwise
    // the caller can only report a bare failure.
    let mut profile = sample_profile("missing-cli", "Missing CLI");
    profile.available = false;
    profile.unavailable_reason = Some("`missing-cli` was not found on PATH".to_string());
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![profile],
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    let entry = &result["profiles"][0];
    assert_eq!(entry["available"], serde_json::Value::Bool(false));
    assert_eq!(
        entry["unavailable_reason"],
        "`missing-cli` was not found on PATH"
    );
}

#[tokio::test]
async fn list_profiles_reports_a_detection_warning_on_the_profile_that_caused_it() {
    // The paired half of `list_profiles_omits_every_key_the_daemon_left_empty`:
    // a per-profile warning is data on that profile, not a failed call.
    let warning = "detection definition `claude-code` was not found".to_string();
    let mut profile = sample_profile("claude-code", "Claude Code");
    profile.detection_warning = Some(warning.clone());
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![profile],
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport)
        .await
        .expect("a per-profile warning must not turn the response into an error");

    let entry = &result["profiles"][0];
    assert_eq!(entry["detection"], "claude-code");
    assert_eq!(
        entry["detection_warning"],
        serde_json::Value::String(warning)
    );
}

#[tokio::test]
async fn list_profiles_passes_daemon_warnings_through_without_failing() {
    // An excluded file is reported as data: the caller still gets a (possibly
    // empty) list and can tell "no profiles" apart from "the daemon is
    // unreachable". Every line survives, in order.
    let warnings = vec![
        "skipped `Bad Name.toml`: the file name is not a valid profile id".to_string(),
        "skipped `broken.toml`: display_name is missing".to_string(),
    ];
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: Vec::new(),
        warnings: warnings.clone(),
    }));

    let result = list_profiles(&transport)
        .await
        .expect("warnings must not turn the response into an error");

    assert_eq!(transport.last_sent(), ClientMessage::ListProfiles);
    assert_eq!(result["profiles"], serde_json::Value::Array(Vec::new()));
    let expected: Vec<serde_json::Value> = warnings
        .iter()
        .map(|warning| serde_json::Value::String(warning.clone()))
        .collect();
    assert_eq!(result["warnings"], serde_json::Value::Array(expected));
}

#[tokio::test]
async fn list_profiles_omits_the_warnings_key_when_the_daemon_reported_none() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![sample_profile("claude-code", "Claude Code")],
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    assert!(
        result["profiles"]
            .as_array()
            .is_some_and(|profiles| !profiles.is_empty()),
        "the profiles key must still carry the list, got: {result}"
    );
    assert!(
        result.get("warnings").is_none(),
        "the warnings key must be absent when the daemon reported none, got: {result}"
    );
}

#[tokio::test]
async fn list_profiles_writes_model_and_effort_aliases_in_the_profile_files_own_order() {
    let mut profile = sample_profile("claude-code", "Claude Code");
    profile.model = Some(LaunchOptionInfo {
        aliases: vec![
            LaunchOptionAlias {
                alias: "fast".to_string(),
                value: "vendor-model-alpha".to_string(),
            },
            LaunchOptionAlias {
                alias: "default".to_string(),
                value: "vendor-model-beta".to_string(),
            },
            LaunchOptionAlias {
                alias: "strong".to_string(),
                value: "vendor-model-gamma".to_string(),
            },
        ],
    });
    profile.effort = Some(LaunchOptionInfo {
        aliases: Vec::new(),
    });
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![profile],
        warnings: Vec::new(),
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    let entry = &result["profiles"][0];
    assert_eq!(
        entry["model"],
        serde_json::json!({
            "aliases": [
                { "alias": "fast", "value": "vendor-model-alpha" },
                { "alias": "default", "value": "vendor-model-beta" },
                { "alias": "strong", "value": "vendor-model-gamma" },
            ]
        }),
        "aliases must survive verbatim, in the profile file's own order, got: {entry}"
    );
    assert_eq!(
        entry["effort"],
        serde_json::json!({ "aliases": [] }),
        "an axis the profile accepts but maps no alias for must still carry the key with an empty array, not be omitted like a `None` axis, got: {entry}"
    );
}

#[tokio::test]
async fn list_profiles_relays_a_daemon_error_code_unchanged() {
    let transport = MockTransport::with_response(Ok(daemon_error("auth_failed")));

    let err = list_profiles(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "auth_failed");
}

#[tokio::test]
async fn list_profiles_maps_transport_unavailable_to_daemon_unavailable() {
    let transport = MockTransport::with_response(Err(TransportError::Unavailable(
        "pipe not connected".to_string(),
    )));

    let err = list_profiles(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "daemon_unavailable");
}

#[tokio::test]
async fn list_profiles_maps_transport_protocol_error_to_internal() {
    let transport = MockTransport::with_response(Err(TransportError::Protocol(
        "undecodable frame".to_string(),
    )));

    let err = list_profiles(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn list_profiles_maps_an_unexpected_daemon_message_variant_to_internal() {
    // A well-formed reply of the wrong variant is a daemon contract
    // violation: it must map to `internal`, not panic.
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));

    let err = list_profiles(&transport).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// ---------------------------------------------------------------------
// open_window: the profile argument
// ---------------------------------------------------------------------

#[tokio::test]
async fn open_window_sends_the_profile_variant_when_a_profile_is_given() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w7".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: Some(r"C:\work".to_string()),
        name: Some("agent".to_string()),
        vendor: None,
        detection: None,
        profile: Some("claude-code".to_string()),
        model: None,
        effort: None,
    };

    let result = open_window(&transport, args).await.expect("must succeed");

    assert_eq!(result["window_id"], "w7");
    match transport.last_sent() {
        ClientMessage::OpenWindowFromProfile {
            profile,
            cwd,
            name,
            model,
            effort,
            ..
        } => {
            assert_eq!(profile, "claude-code");
            assert_eq!(cwd, Some(r"C:\work".to_string()));
            assert_eq!(name, Some("agent".to_string()));
            assert_eq!(model, None);
            assert_eq!(effort, None);
        }
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_forwards_an_omitted_cwd_and_name_of_a_profile_launch_as_none() {
    // The profile's own defaults are the daemon's to apply: an omitted
    // override must stay `None` on the wire rather than becoming an empty
    // string the daemon would treat as a value.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w7".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some("claude-code".to_string()),
        model: None,
        effort: None,
    };

    open_window(&transport, args).await.expect("must succeed");

    match transport.last_sent() {
        ClientMessage::OpenWindowFromProfile {
            profile,
            cwd,
            name,
            model,
            effort,
            ..
        } => {
            assert_eq!(profile, "claude-code");
            assert_eq!(cwd, None);
            assert_eq!(name, None);
            assert_eq!(model, None);
            assert_eq!(effort, None);
        }
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_rejects_a_command_and_a_profile_given_together() {
    // The two are mutually exclusive at this entry point: no message on the
    // wire can express both, so the bridge must refuse rather than silently
    // pick one.
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some("claude-code".to_string()),
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn open_window_rejects_a_request_with_neither_a_command_nor_a_profile() {
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: Some(r"C:\work".to_string()),
        name: Some("agent".to_string()),
        vendor: None,
        detection: None,
        profile: None,
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn open_window_rejects_an_empty_profile_string_without_calling_the_transport() {
    // "Missing" and "empty" are both constructible and both invalid, as for
    // every other required argument in this crate.
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some(String::new()),
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn open_window_rejects_an_empty_profile_string_even_alongside_a_command() {
    // An empty profile must not decay into "no profile was given": that would
    // launch a command the caller did not mean to launch on its own.
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some(String::new()),
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn open_window_rejects_a_detection_override_combined_with_a_profile() {
    // A profile carries its own detection definition and the profile message
    // has no field to override it with, so accepting one would silently drop
    // the caller's choice.
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: Some("codex-cli".to_string()),
        profile: Some("claude-code".to_string()),
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn open_window_rejects_a_vendor_override_combined_with_a_profile() {
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: Some("anthropic".to_string()),
        detection: None,
        profile: Some("claude-code".to_string()),
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn open_window_relays_a_daemon_refusal_of_a_profile_launch_unchanged() {
    // The profile id is the daemon's to validate: an unknown one comes back
    // as its own refusal, not as an `internal` error from the bridge.
    let transport = MockTransport::with_response(Ok(DaemonMessage::Error {
        code: "invalid_argument".to_string(),
        message: "unknown profile: nope".to_string(),
        rejected_argument: None,
    }));
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some("nope".to_string()),
        model: None,
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 1);
    match transport.last_sent() {
        ClientMessage::OpenWindowFromProfile { profile, .. } => assert_eq!(profile, "nope"),
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_passes_a_detection_warning_of_a_profile_launch_through() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w7".to_string(),
        detection_warning: Some("detection definition `claude-code` was not found".to_string()),
        name: None,
    }));
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some("claude-code".to_string()),
        model: None,
        effort: None,
    };

    let result = open_window(&transport, args).await.expect("must succeed");

    assert_eq!(result["window_id"], "w7");
    assert_eq!(
        result["detection_warning"],
        "detection definition `claude-code` was not found"
    );
}

#[tokio::test]
async fn open_window_forwards_model_and_effort_of_a_profile_launch() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w8".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some("claude-code".to_string()),
        model: Some("strong".to_string()),
        effort: Some("high".to_string()),
    };

    open_window(&transport, args).await.expect("must succeed");

    match transport.last_sent() {
        ClientMessage::OpenWindowFromProfile { model, effort, .. } => {
            assert_eq!(model, Some("strong".to_string()));
            assert_eq!(effort, Some("high".to_string()));
        }
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_accepts_a_model_only_specification_of_a_profile_launch() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w8".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some("claude-code".to_string()),
        model: Some("strong".to_string()),
        effort: None,
    };

    open_window(&transport, args).await.expect("must succeed");

    match transport.last_sent() {
        ClientMessage::OpenWindowFromProfile { model, effort, .. } => {
            assert_eq!(model, Some("strong".to_string()));
            assert_eq!(effort, None);
        }
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_accepts_an_effort_only_specification_of_a_profile_launch() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w8".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some("claude-code".to_string()),
        model: None,
        effort: Some("high".to_string()),
    };

    open_window(&transport, args).await.expect("must succeed");

    match transport.last_sent() {
        ClientMessage::OpenWindowFromProfile { model, effort, .. } => {
            assert_eq!(model, None);
            assert_eq!(effort, Some("high".to_string()));
        }
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_rejects_a_command_and_a_model_given_together() {
    // A launch option only means anything once a profile maps it to argv, so
    // supplying one without a profile -- even alongside a command -- is
    // refused before the transport is contacted, and with the same wording
    // the shared protocol validator carries.
    let transport = MockTransport::never_called();
    let args = OpenWindowArgs {
        command: vec!["pwsh.exe".to_string()],
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: None,
        model: Some("strong".to_string()),
        effort: None,
    };

    let err = open_window(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
    assert_eq!(
        err.to_string(),
        format!(
            "invalid argument: {}",
            OpenWindowExclusivityError::LaunchOptionWithoutProfile
        )
    );
}

// =====================================================================
// Requester identity and the codes a held request answers with
// =====================================================================

/// The daemon error codes a destructive request can only answer with once it
/// may be held for a human decision. Each must reach the caller as the *same*
/// MCP code: an agent that cannot tell a refusal from an expiry cannot decide
/// whether retrying is meaningful.
const APPROVAL_DAEMON_ERROR_CODES: [&str; 4] = [
    "approval_denied",
    "approval_timeout",
    "approval_not_pending",
    "invalid_state",
];

/// The window id a transport carries when the daemon accepted its claim.
const IDENTIFIED_WINDOW_ID: &str = "w-self";

/// The warning a transport reports for a claim the daemon refused. Its exact
/// text belongs to the transport, so the tool layer must pass it through
/// rather than compose one of its own.
const REFUSED_IDENTITY_WARNING: &str =
    "the daemon refused this window identity; continuing as an external client";

/// Every error variant a held request can end in, paired with the code it
/// must expose and a word its message must carry, so that a client rendering
/// only the text can still tell the four outcomes apart.
fn held_request_error_cases() -> [(ToolError, &'static str, &'static str); 4] {
    [
        (
            ToolError::ApprovalDenied(DAEMON_REFUSAL_MESSAGE.to_string()),
            "approval_denied",
            "denied",
        ),
        (
            ToolError::ApprovalTimeout(DAEMON_REFUSAL_MESSAGE.to_string()),
            "approval_timeout",
            "time",
        ),
        (
            ToolError::ApprovalNotPending(DAEMON_REFUSAL_MESSAGE.to_string()),
            "approval_not_pending",
            "pending",
        ),
        (
            ToolError::InvalidState(DAEMON_REFUSAL_MESSAGE.to_string()),
            "invalid_state",
            "state",
        ),
    ]
}

#[test]
fn tool_error_code_covers_every_code_a_held_request_can_end_in() {
    for (err, expected_code, _) in held_request_error_cases() {
        assert_eq!(err.code(), expected_code);
    }
}

#[test]
fn every_held_request_error_renders_a_distinct_english_message_carrying_the_daemons_own() {
    let mut rendered: Vec<(&'static str, String)> = Vec::new();
    for (err, code, keyword) in held_request_error_cases() {
        let text = err.to_string();
        assert!(
            text.is_ascii(),
            "{code} must render an English message, got {text:?}"
        );
        assert!(
            text.contains(DAEMON_REFUSAL_MESSAGE),
            "{code} must keep the daemon's own message, got {text:?}"
        );
        assert!(
            text.to_ascii_lowercase().contains(keyword),
            "{code} must name its outcome ({keyword:?}) in the message, got {text:?}"
        );
        rendered.push((code, text));
    }
    for (index, (code, text)) in rendered.iter().enumerate() {
        for (other_code, other_text) in rendered.iter().skip(index + 1) {
            assert_ne!(
                text, other_text,
                "{code} and {other_code} must not render the same sentence"
            );
        }
    }
}

/// `invalid_state` carries refusals from two unrelated daemon sources: a
/// connection whose origin claim conflicts with one it already made, and (via
/// the plan ledger) a task whose bound window is still alive or has no
/// resolvable assignment. `ToolError::InvalidState`'s fixed wording must not
/// tell the plan-ledger caller its *connection* is broken, since it is not.
#[test]
fn invalid_state_does_not_claim_the_connection_itself_is_invalid() {
    let err = ToolError::InvalidState("the task already has a live window bound to it".to_string());
    let text = err.to_string();

    assert!(
        !text
            .to_ascii_lowercase()
            .contains("the connection is in an invalid state"),
        "invalid_state's rendering must read as a state refusal a plan-ledger \
         caller can recognize, not a connection-origin claim, got {text:?}"
    );
    assert!(
        text.contains("the task already has a live window bound to it"),
        "must still carry the daemon's own message, got {text:?}"
    );
}

#[tokio::test]
async fn send_message_relays_every_held_request_error_code_unchanged() {
    for code in APPROVAL_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = SendMessageArgs {
            window_id: Some("w1".to_string()),
            text: Some("hello".to_string()),
            mode: None,
        };

        let err = send_message(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn interrupt_window_relays_every_held_request_error_code_unchanged() {
    for code in APPROVAL_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = InterruptWindowArgs {
            window_id: Some("w1".to_string()),
        };

        let err = interrupt_window(&transport, args)
            .await
            .expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn close_window_relays_every_held_request_error_code_unchanged() {
    for code in APPROVAL_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = CloseWindowArgs {
            window_id: Some("w1".to_string()),
            force: None,
        };

        let err = close_window(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

/// Runs each of the three gated tools against a transport reporting
/// `identity` and returns their success JSON, paired with the tool's name.
async fn gated_tool_results(
    identity: &RequesterIdentity,
) -> Vec<(&'static str, serde_json::Value)> {
    let send = MockTransport::with_identity(
        Ok(DaemonMessage::TextSent {
            window_id: "w1".to_string(),
        }),
        identity.clone(),
    );
    let sent = send_message(
        &send,
        SendMessageArgs {
            window_id: Some("w1".to_string()),
            text: Some("hello".to_string()),
            mode: None,
        },
    )
    .await
    .expect("send_message must succeed");

    let interrupt = MockTransport::with_identity(
        Ok(DaemonMessage::WindowInterrupted {
            window_id: "w1".to_string(),
        }),
        identity.clone(),
    );
    let interrupted = interrupt_window(
        &interrupt,
        InterruptWindowArgs {
            window_id: Some("w1".to_string()),
        },
    )
    .await
    .expect("interrupt_window must succeed");

    let close = MockTransport::with_identity(
        Ok(DaemonMessage::WindowClosed {
            window_id: "w1".to_string(),
        }),
        identity.clone(),
    );
    let closed = close_window(
        &close,
        CloseWindowArgs {
            window_id: Some("w1".to_string()),
            force: None,
        },
    )
    .await
    .expect("close_window must succeed");

    vec![
        ("send_message", sent),
        ("interrupt_window", interrupted),
        ("close_window", closed),
    ]
}

/// The key each gated tool answered with before it also reported a
/// requester, asserted alongside every identity case: a projection that
/// gained a requester but lost its own result would be a silent regression.
fn assert_tool_keeps_its_own_result(tool: &str, result: &serde_json::Value) {
    if tool == "send_message" {
        assert_eq!(
            result.get("ok").and_then(serde_json::Value::as_bool),
            Some(true),
            "send_message must still report ok:true"
        );
    } else {
        assert_eq!(
            result.get("window_id").and_then(serde_json::Value::as_str),
            Some("w1"),
            "{tool} must still echo back the window_id"
        );
    }
}

#[tokio::test]
async fn every_gated_tool_reports_a_window_requester_once_the_daemon_accepted_the_identity() {
    let results =
        gated_tool_results(&RequesterIdentity::Window(IDENTIFIED_WINDOW_ID.to_string())).await;

    assert_eq!(results.len(), 3, "all three gated tools must be measured");
    for (tool, result) in &results {
        assert_tool_keeps_its_own_result(tool, result);
        assert_eq!(
            result.get("requester").and_then(serde_json::Value::as_str),
            Some("window"),
            "{tool} must report that the daemon knows this connection as a window, got {result}"
        );
        assert!(
            result.get("identity_warning").is_none(),
            "{tool} must not warn about an identity the daemon accepted, got {result}"
        );
    }
}

#[tokio::test]
async fn every_gated_tool_reports_an_external_requester_when_no_identity_was_claimed() {
    let results = gated_tool_results(&RequesterIdentity::External).await;

    assert_eq!(results.len(), 3, "all three gated tools must be measured");
    for (tool, result) in &results {
        assert_tool_keeps_its_own_result(tool, result);
        assert_eq!(
            result.get("requester").and_then(serde_json::Value::as_str),
            Some("external"),
            "{tool} must report an external requester without a claim, got {result}"
        );
        assert!(
            result.get("identity_warning").is_none(),
            "{tool} must not warn when nothing was claimed, got {result}"
        );
    }
}

#[tokio::test]
async fn every_gated_tool_reports_an_external_requester_and_the_warning_for_a_refused_claim() {
    // A refused claim is not a failure of the call: the request runs as an
    // external client, and the caller is told why its own window did not
    // unlock anything.
    let results = gated_tool_results(&RequesterIdentity::Refused(
        REFUSED_IDENTITY_WARNING.to_string(),
    ))
    .await;

    assert_eq!(results.len(), 3, "all three gated tools must be measured");
    for (tool, result) in &results {
        assert_tool_keeps_its_own_result(tool, result);
        assert_eq!(
            result.get("requester").and_then(serde_json::Value::as_str),
            Some("external"),
            "{tool} must not claim to be a window the daemon refused, got {result}"
        );
        assert_eq!(
            result
                .get("identity_warning")
                .and_then(serde_json::Value::as_str),
            Some(REFUSED_IDENTITY_WARNING),
            "{tool} must pass the transport's warning through verbatim, got {result}"
        );
    }
}

#[tokio::test]
async fn the_read_only_tools_report_no_requester_at_all() {
    // The requester governs what a connection may *do*. Stamping it onto a
    // listing would change responses no gate ever consults.
    let identity = RequesterIdentity::Window(IDENTIFIED_WINDOW_ID.to_string());

    let list = MockTransport::with_identity(
        Ok(DaemonMessage::WindowList {
            windows: vec![sample_window_info("w1")],
        }),
        identity.clone(),
    );
    let listed = list_windows(&list)
        .await
        .expect("list_windows must succeed");

    let status_transport = MockTransport::with_identity(
        Ok(DaemonMessage::WindowStatus {
            window_id: "w1".to_string(),
            status: WindowStatus::Idle,
            since_ms: 0,
            last_output_summary: None,
            exit_code: None,
            unknown_reason: None,
            detection_warning: None,
            mcp_connected: None,
        }),
        identity.clone(),
    );
    let status = get_window_status(
        &status_transport,
        GetWindowStatusArgs {
            window_id: Some("w1".to_string()),
        },
    )
    .await
    .expect("get_window_status must succeed");

    let output_transport = MockTransport::with_identity(
        Ok(DaemonMessage::WindowOutput {
            window_id: "w1".to_string(),
            text: "line-a".to_string(),
            truncated: false,
        }),
        identity.clone(),
    );
    let output = read_output(
        &output_transport,
        ReadOutputArgs {
            window_id: Some("w1".to_string()),
            lines: None,
        },
    )
    .await
    .expect("read_output must succeed");

    for (tool, result) in [
        ("list_windows", listed),
        ("get_window_status", status),
        ("read_output", output),
    ] {
        assert!(
            result.get("requester").is_none(),
            "{tool} must stay unchanged, got {result}"
        );
        assert!(
            result.get("identity_warning").is_none(),
            "{tool} must stay unchanged, got {result}"
        );
    }
}

// =====================================================================
// send_handoff
// =====================================================================

/// A minimal valid `send_handoff` targeting an existing window: every
/// argument-validation test below starts here and blanks or corrupts exactly
/// the field it is about.
fn valid_window_handoff_args() -> SendHandoffArgs {
    SendHandoffArgs {
        window_id: Some("w1".to_string()),
        profile: None,
        cwd: None,
        name: None,
        reference: Some("src/foo.rs".to_string()),
        note: Some("Please review the error handling.".to_string()),
        model: None,
        effort: None,
    }
}

/// A minimal valid `send_handoff` opening a new window from a launch
/// profile, exercising the fields only that form accepts.
fn valid_profile_handoff_args() -> SendHandoffArgs {
    SendHandoffArgs {
        window_id: None,
        profile: Some("claude-code".to_string()),
        cwd: Some(r"C:\work".to_string()),
        name: Some("agent".to_string()),
        reference: Some("src/foo.rs".to_string()),
        note: Some("Please review the error handling.".to_string()),
        model: None,
        effort: None,
    }
}

#[tokio::test]
async fn send_handoff_rejects_a_request_with_neither_window_id_nor_profile() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.window_id = None;

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_request_with_both_window_id_and_profile() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.profile = Some("claude-code".to_string());

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_cwd_given_alongside_window_id() {
    // `cwd` only means anything for a window this handoff is about to open.
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.cwd = Some(r"C:\work".to_string());

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_name_given_alongside_window_id() {
    // Renaming an existing window is `rename_window`'s business, not a
    // handoff's.
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.name = Some("agent".to_string());

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_model_given_alongside_window_id() {
    // A launch option only means anything for a window this handoff is
    // about to open from a profile.
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.model = Some("strong".to_string());

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_missing_reference_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.reference = None;

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_an_empty_reference_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.reference = Some(String::new());

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_missing_note_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.note = None;

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_an_empty_note_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.note = Some(String::new());

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_control_character_in_reference_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.reference = Some("one\ntwo".to_string());

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_rejects_a_bidi_control_character_in_note_and_names_it_in_the_message() {
    // U+202E (RIGHT-TO-LEFT OVERRIDE) can reorder how the rest of the note
    // renders in a terminal, so it is refused like every other forbidden
    // label character, and the violation is reported by code point and byte
    // offset rather than by echoing the character itself.
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    let prefix = "please look here";
    let note = format!("{prefix}\u{202E}now");
    args.note = Some(note);

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
    let message = err.to_string();
    assert!(
        message.contains("U+202E"),
        "message must name the forbidden code point, got {message:?}"
    );
    assert!(
        message.contains(&format!("byte offset {}", prefix.len())),
        "message must report the violation's byte offset, got {message:?}"
    );
}

#[tokio::test]
async fn send_handoff_rejects_a_reference_exceeding_the_512_byte_limit_without_calling_the_transport()
 {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.reference = Some("a".repeat(513));

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_accepts_a_reference_exactly_at_the_512_byte_limit() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::HandoffSent {
        window_id: "w1".to_string(),
        opened: false,
        delivery: delivery("taken"),
        name: None,
    }));
    let mut args = valid_window_handoff_args();
    args.reference = Some("a".repeat(512));

    send_handoff(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
}

#[tokio::test]
async fn send_handoff_rejects_a_note_exceeding_the_1024_byte_limit_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_window_handoff_args();
    args.note = Some("a".repeat(1025));

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_handoff_accepts_a_note_exactly_at_the_1024_byte_limit() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::HandoffSent {
        window_id: "w1".to_string(),
        opened: false,
        delivery: delivery("taken"),
        name: None,
    }));
    let mut args = valid_window_handoff_args();
    args.note = Some("a".repeat(1024));

    send_handoff(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
}

#[tokio::test]
async fn send_handoff_sends_a_send_handoff_message_and_maps_a_window_target_response() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::HandoffSent {
        window_id: "w1".to_string(),
        opened: false,
        delivery: delivery("taken"),
        name: None,
    }));
    let args = valid_window_handoff_args();

    let result = send_handoff(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::SendHandoff {
            window_id: Some("w1".to_string()),
            profile: None,
            cwd: None,
            name: None,
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "Please review the error handling.".to_string(),
        }
    );
    assert_eq!(result["window_id"], "w1");
    assert_eq!(result["opened"], serde_json::Value::Bool(false));
    assert_eq!(result["delivery"], "taken");
}

#[tokio::test]
async fn send_handoff_sends_a_send_handoff_message_and_maps_a_profile_target_response() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::HandoffSent {
        window_id: "w9".to_string(),
        opened: true,
        delivery: delivery("taken"),
        name: None,
    }));
    let args = valid_profile_handoff_args();

    let result = send_handoff(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::SendHandoff {
            window_id: None,
            profile: Some("claude-code".to_string()),
            cwd: Some(r"C:\work".to_string()),
            name: Some("agent".to_string()),
            model: None,
            effort: None,
            reference: "src/foo.rs".to_string(),
            note: "Please review the error handling.".to_string(),
        }
    );
    assert_eq!(result["window_id"], "w9");
    assert_eq!(result["opened"], serde_json::Value::Bool(true));
    assert_eq!(result["delivery"], "taken");
}

#[tokio::test]
async fn send_handoff_forwards_the_model_launch_option_of_a_profile_target() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::HandoffSent {
        window_id: "w9".to_string(),
        opened: true,
        delivery: delivery("taken"),
        name: None,
    }));
    let mut args = valid_profile_handoff_args();
    args.model = Some("strong".to_string());

    send_handoff(&transport, args).await.expect("must succeed");

    match transport.last_sent() {
        ClientMessage::SendHandoff { model, effort, .. } => {
            assert_eq!(model, Some("strong".to_string()));
            assert_eq!(effort, None);
        }
        other => panic!("expected ClientMessage::SendHandoff, got {other:?}"),
    }
}

/// Every `HandoffDelivery` the daemon can answer with, by its wire spelling. The
/// three are listed together because the bridge's job here is to relay, not to
/// judge: a reply that dropped the outcome, or that reported the same outcome
/// whatever the daemon answered, would tell the caller a handoff landed that
/// nobody took.
const HANDOFF_DELIVERY_WIRE_SPELLINGS: [&str; 3] =
    ["taken", "undelivered_timeout", "undelivered_no_delivery"];

/// The `HandoffDelivery` whose wire spelling is `wire`.
fn delivery(wire: &str) -> HandoffDelivery {
    serde_json::from_value(serde_json::Value::String(wire.to_string()))
        .unwrap_or_else(|err| panic!("{wire:?} must be a handoff delivery ({err})"))
}

#[tokio::test]
async fn send_handoff_relays_every_delivery_outcome_the_daemon_can_report() {
    for spelling in HANDOFF_DELIVERY_WIRE_SPELLINGS {
        let transport = MockTransport::with_response(Ok(DaemonMessage::HandoffSent {
            window_id: "w1".to_string(),
            opened: false,
            delivery: delivery(spelling),
            name: None,
        }));

        let result = send_handoff(&transport, valid_window_handoff_args())
            .await
            .expect("must succeed");

        assert_eq!(
            result["delivery"], spelling,
            "the reply must carry the delivery outcome the daemon reported ({spelling}), got {result}"
        );
    }
}

#[tokio::test]
async fn send_handoff_reports_a_window_requester_once_the_daemon_accepted_the_identity() {
    let transport = MockTransport::with_identity(
        Ok(DaemonMessage::HandoffSent {
            window_id: "w1".to_string(),
            opened: false,
            delivery: delivery("taken"),
            name: None,
        }),
        RequesterIdentity::Window(IDENTIFIED_WINDOW_ID.to_string()),
    );
    let args = valid_window_handoff_args();

    let result = send_handoff(&transport, args).await.expect("must succeed");

    assert_eq!(
        result.get("requester").and_then(serde_json::Value::as_str),
        Some("window"),
        "send_handoff must report that the daemon knows this connection as a window, \
         got {result}"
    );
    assert!(
        result.get("identity_warning").is_none(),
        "send_handoff must not warn about an identity the daemon accepted, got {result}"
    );
}

/// Every daemon error code a `send_handoff` request can come back with. Each
/// must reach the caller as the *same* MCP code: the bridge must never
/// collapse a specific refusal into `internal`.
const SEND_HANDOFF_DAEMON_ERROR_CODES: [&str; 7] = [
    "approval_denied",
    "approval_timeout",
    "status_unknown",
    "child_exited",
    "invalid_argument",
    "resource_exhausted",
    "spawn_failed",
];

#[tokio::test]
async fn send_handoff_relays_every_daemon_error_code_unchanged() {
    for code in SEND_HANDOFF_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = valid_window_handoff_args();

        let err = send_handoff(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn send_handoff_maps_an_unexpected_daemon_message_variant_to_internal() {
    // A well-formed reply of the wrong variant is a daemon contract
    // violation: it must map to `internal`, not be treated as success.
    let transport = MockTransport::with_response(Ok(DaemonMessage::WindowOpened {
        window_id: "w1".to_string(),
        detection_warning: None,
        name: None,
    }));
    let args = valid_window_handoff_args();

    let err = send_handoff(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// =====================================================================
// send_chat
// =====================================================================

/// A minimal valid `send_chat` post: every argument-validation test below
/// starts here and corrupts exactly the field it is about.
fn valid_send_chat_args() -> SendChatArgs {
    SendChatArgs {
        room_id: None,
        body: Some("hello room".to_string()),
    }
}

fn sample_chat_posted() -> DaemonMessage {
    DaemonMessage::ChatPosted {
        room_id: "main".to_string(),
        seq: 1,
        targets: vec!["w1".to_string()],
        user: false,
        unresolved: Vec::new(),
        halted: false,
    }
}

#[tokio::test]
async fn send_chat_rejects_a_missing_body_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_send_chat_args();
    args.body = None;

    let err = send_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_chat_rejects_an_empty_body_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_send_chat_args();
    args.body = Some(String::new());

    let err = send_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_chat_rejects_a_body_exceeding_the_768_byte_limit_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_send_chat_args();
    args.body = Some("a".repeat(769));

    let err = send_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_chat_accepts_a_body_exactly_at_the_768_byte_limit() {
    let transport = MockTransport::with_response(Ok(sample_chat_posted()));
    let mut args = valid_send_chat_args();
    args.body = Some("a".repeat(768));

    send_chat(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
}

#[tokio::test]
async fn send_chat_rejects_a_c0_control_character_in_the_body_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_send_chat_args();
    args.body = Some(format!("hi{}there", '\u{0001}'));

    let err = send_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_chat_rejects_a_c1_control_character_in_the_body_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_send_chat_args();
    args.body = Some(format!("hi{}there", '\u{0080}'));

    let err = send_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_chat_rejects_a_del_character_in_the_body_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_send_chat_args();
    args.body = Some(format!("hi{}there", '\u{007F}'));

    let err = send_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn send_chat_sends_a_send_chat_message_and_maps_a_chat_posted_response() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::ChatPosted {
        room_id: "main".to_string(),
        seq: 7,
        targets: vec!["w1".to_string(), "w2".to_string()],
        user: true,
        unresolved: vec!["nobody".to_string()],
        halted: true,
    }));
    let args = valid_send_chat_args();

    let result = send_chat(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::SendChat {
            room_id: None,
            body: "hello room".to_string(),
        }
    );
    assert_eq!(result["room_id"], "main");
    assert_eq!(result["seq"], 7);
    assert_eq!(result["targets"], serde_json::json!(["w1", "w2"]));
    assert_eq!(result["user"], serde_json::Value::Bool(true));
    assert_eq!(result["unresolved"], serde_json::json!(["nobody"]));
    assert_eq!(result["halted"], serde_json::Value::Bool(true));
}

#[tokio::test]
async fn send_chat_forwards_an_explicit_room_id_without_substituting_a_default() {
    let transport = MockTransport::with_response(Ok(sample_chat_posted()));
    let mut args = valid_send_chat_args();
    args.room_id = Some("side-room".to_string());

    send_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::SendChat {
            room_id: Some("side-room".to_string()),
            body: "hello room".to_string(),
        }
    );
}

#[tokio::test]
async fn send_chat_reports_a_window_requester_once_the_daemon_accepted_the_identity() {
    let transport = MockTransport::with_identity(
        Ok(sample_chat_posted()),
        RequesterIdentity::Window(IDENTIFIED_WINDOW_ID.to_string()),
    );
    let args = valid_send_chat_args();

    let result = send_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        result.get("requester").and_then(serde_json::Value::as_str),
        Some("window"),
        "send_chat must report that the daemon knows this connection as a window, got {result}"
    );
    assert!(
        result.get("identity_warning").is_none(),
        "send_chat must not warn about an identity the daemon accepted, got {result}"
    );
}

/// Every daemon error code a `send_chat` request can come back with. Each
/// must reach the caller as the *same* MCP code.
const SEND_CHAT_DAEMON_ERROR_CODES: [&str; 6] = [
    "unsupported",
    "invalid_argument",
    "approval_required",
    "approval_denied",
    "approval_timeout",
    "resource_exhausted",
];

#[tokio::test]
async fn send_chat_relays_every_daemon_error_code_unchanged() {
    for code in SEND_CHAT_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = valid_send_chat_args();

        let err = send_chat(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn send_chat_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = valid_send_chat_args();

    let err = send_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// =====================================================================
// broadcast_chat
// =====================================================================

#[tokio::test]
async fn broadcast_chat_rejects_an_empty_body_without_calling_the_transport() {
    let transport = MockTransport::never_called();
    let mut args = valid_send_chat_args();
    args.body = Some(String::new());

    let err = broadcast_chat(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "invalid_argument");
    assert_eq!(transport.call_count(), 0);
}

#[tokio::test]
async fn broadcast_chat_sends_a_broadcast_chat_message_and_maps_a_chat_posted_response() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::ChatPosted {
        room_id: "main".to_string(),
        seq: 9,
        targets: vec!["w1".to_string(), "w2".to_string(), "w3".to_string()],
        user: false,
        unresolved: Vec::new(),
        halted: false,
    }));
    let args = valid_send_chat_args();

    let result = broadcast_chat(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::BroadcastChat {
            room_id: None,
            body: "hello room".to_string(),
        }
    );
    assert_eq!(result["room_id"], "main");
    assert_eq!(result["seq"], 9);
    assert_eq!(result["targets"], serde_json::json!(["w1", "w2", "w3"]));
    assert_eq!(result["halted"], serde_json::Value::Bool(false));
}

#[tokio::test]
async fn broadcast_chat_reports_a_window_requester_once_the_daemon_accepted_the_identity() {
    let transport = MockTransport::with_identity(
        Ok(sample_chat_posted()),
        RequesterIdentity::Window(IDENTIFIED_WINDOW_ID.to_string()),
    );
    let args = valid_send_chat_args();

    let result = broadcast_chat(&transport, args)
        .await
        .expect("must succeed");

    assert_eq!(
        result.get("requester").and_then(serde_json::Value::as_str),
        Some("window"),
        "broadcast_chat must report that the daemon knows this connection as a window, got \
         {result}"
    );
    assert!(
        result.get("identity_warning").is_none(),
        "broadcast_chat must not warn about an identity the daemon accepted, got {result}"
    );
}

#[tokio::test]
async fn broadcast_chat_relays_every_daemon_error_code_unchanged() {
    for code in SEND_CHAT_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = valid_send_chat_args();

        let err = broadcast_chat(&transport, args)
            .await
            .expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn broadcast_chat_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = valid_send_chat_args();

    let err = broadcast_chat(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

// =====================================================================
// read_chat
// =====================================================================

fn sample_chat_message(seq: u64) -> ChatMessage {
    ChatMessage {
        seq,
        at_ms: 1_000 + seq,
        from_kind: "window".to_string(),
        from_window_id: Some("w1".to_string()),
        from_name: "claude".to_string(),
        body: "hi there".to_string(),
        mentions: Vec::new(),
        broadcast: false,
        deliveries: vec![ChatDeliveryInfo {
            window_id: "w1".to_string(),
            state: "taken".to_string(),
            unknown_reason: None,
        }],
    }
}

fn sample_chat_messages_response() -> DaemonMessage {
    DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: vec![sample_chat_message(1)],
        next_after_seq: 1,
        halted: None,
        needs_user: false,
        roster: vec![ChatParticipant {
            window_id: "w1".to_string(),
            name: "claude".to_string(),
        }],
    }
}

#[tokio::test]
async fn read_chat_sends_a_read_chat_message_and_maps_a_chat_messages_response() {
    let messages = vec![sample_chat_message(1), sample_chat_message(2)];
    let roster = vec![
        ChatParticipant {
            window_id: "w1".to_string(),
            name: "claude".to_string(),
        },
        ChatParticipant {
            window_id: "w2".to_string(),
            name: "codex".to_string(),
        },
    ];
    let transport = MockTransport::with_response(Ok(DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: messages.clone(),
        next_after_seq: 2,
        halted: None,
        needs_user: true,
        roster: roster.clone(),
    }));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: Some(50),
    };

    let result = read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(transport.call_count(), 1);
    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: Some(0),
            limit: Some(50),
        }
    );
    assert_eq!(result["room_id"], "main");
    let expected_messages: Vec<serde_json::Value> = messages
        .iter()
        .map(|m| serde_json::to_value(m).expect("ChatMessage must serialize"))
        .collect();
    assert_eq!(
        result["messages"],
        serde_json::Value::Array(expected_messages)
    );
    assert_eq!(result["next_after_seq"], 2);
    assert_eq!(result["needs_user"], serde_json::Value::Bool(true));
    let expected_roster: Vec<serde_json::Value> = roster
        .iter()
        .map(|p| serde_json::to_value(p).expect("ChatParticipant must serialize"))
        .collect();
    assert_eq!(result["roster"], serde_json::Value::Array(expected_roster));
}

#[tokio::test]
async fn read_chat_forwards_a_missing_after_seq_and_limit_as_none() {
    let transport = MockTransport::with_response(Ok(sample_chat_messages_response()));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: None,
        limit: None,
    };

    read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        transport.last_sent(),
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: None,
            limit: None,
        }
    );
}

#[tokio::test]
async fn read_chat_maps_the_room_halt_when_present() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: Vec::new(),
        next_after_seq: 5,
        halted: Some(ChatHalt {
            reason: "monologue".to_string(),
            since_ms: 4242,
        }),
        needs_user: true,
        roster: Vec::new(),
    }));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: None,
    };

    let result = read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(result["halted"]["reason"], "monologue");
    assert_eq!(result["halted"]["since_ms"], 4242);
}

#[tokio::test]
async fn read_chat_omits_the_halted_key_when_the_room_is_not_halted() {
    let transport = MockTransport::with_response(Ok(sample_chat_messages_response()));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: None,
    };

    let result = read_chat(&transport, args).await.expect("must succeed");

    assert!(
        result.get("halted").is_none(),
        "read_chat must omit the halted key rather than write null when the room is not \
         halted, got {result}"
    );
}

const READ_CHAT_DAEMON_ERROR_CODES: [&str; 3] = ["unsupported", "invalid_argument", "internal"];

#[tokio::test]
async fn read_chat_relays_every_daemon_error_code_unchanged() {
    for code in READ_CHAT_DAEMON_ERROR_CODES {
        let transport = MockTransport::with_response(Ok(daemon_error(code)));
        let args = ReadChatArgs {
            room_id: None,
            after_seq: Some(0),
            limit: None,
        };

        let err = read_chat(&transport, args).await.expect_err("must fail");

        assert_eq!(err.code(), code);
    }
}

#[tokio::test]
async fn read_chat_maps_an_unexpected_daemon_message_variant_to_internal() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::Pong));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: None,
    };

    let err = read_chat(&transport, args).await.expect_err("must fail");

    assert_eq!(err.code(), "internal");
}

#[tokio::test]
async fn read_chat_passes_the_delivery_unknown_reason_through() {
    let mut message = sample_chat_message(1);
    message.deliveries = vec![ChatDeliveryInfo {
        window_id: "w1".to_string(),
        state: "undelivered_unknown".to_string(),
        unknown_reason: Some(UnknownReason::ScreenLost),
    }];
    let transport = MockTransport::with_response(Ok(DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: vec![message],
        next_after_seq: 1,
        halted: None,
        needs_user: false,
        roster: Vec::new(),
    }));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: None,
    };

    let result = read_chat(&transport, args).await.expect("must succeed");

    assert_eq!(
        result["messages"][0]["deliveries"][0]["unknown_reason"],
        serde_json::json!("screen_lost")
    );
}

#[tokio::test]
async fn read_chat_omits_the_delivery_unknown_reason_key_when_the_daemon_reports_none() {
    let transport = MockTransport::with_response(Ok(sample_chat_messages_response()));
    let args = ReadChatArgs {
        room_id: None,
        after_seq: Some(0),
        limit: None,
    };

    let result = read_chat(&transport, args).await.expect("must succeed");

    assert!(
        result["messages"][0]["deliveries"][0]
            .get("unknown_reason")
            .is_none(),
        "the unknown_reason key must be absent (not null) when the daemon reports none, got: {}",
        result["messages"][0]
    );
}

// =====================================================================
// Disabled launch profiles
// =====================================================================

/// The daemon's fixed refusal for a launch profile the user has disabled.
const DISABLED_PROFILE_MESSAGE: &str = "the launch profile is disabled";

fn profile_disabled_error() -> DaemonMessage {
    DaemonMessage::Error {
        code: "profile_disabled".to_string(),
        message: DISABLED_PROFILE_MESSAGE.to_string(),
        rejected_argument: None,
    }
}

fn disabled_profile(id: &str, display_name: &str) -> ProfileInfo {
    ProfileInfo {
        enabled: false,
        ..sample_profile(id, display_name)
    }
}

fn profile_open_window_args(profile: &str) -> OpenWindowArgs {
    OpenWindowArgs {
        command: Vec::new(),
        cwd: None,
        name: None,
        vendor: None,
        detection: None,
        profile: Some(profile.to_string()),
        model: None,
        effort: None,
    }
}

#[tokio::test]
async fn list_profiles_leaves_out_a_disabled_profile_and_keeps_the_others_in_order_with_their_keys()
{
    let bare_shell = ProfileInfo {
        id: "generic-shell".to_string(),
        display_name: "Shell".to_string(),
        detection: None,
        detection_warning: None,
        cwd: None,
        available: true,
        unavailable_reason: None,
        command: Vec::new(),
        model: None,
        effort: None,
        enabled: true,
        unavailable_kind: None,
        unavailable_guidance: None,
        limits: Vec::new(),
    };
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![
            sample_profile("agent-a", "Agent A"),
            disabled_profile("agent-b", "Agent B"),
            bare_shell,
            disabled_profile("agent-c", "Agent C"),
        ],
        warnings: vec!["skipped `broken.toml`: display_name is missing".to_string()],
    }));

    let result = list_profiles(&transport).await.expect("must succeed");

    let entries = result["profiles"]
        .as_array()
        .expect("profiles must be a JSON array");
    let ids: Vec<&str> = entries
        .iter()
        .map(|entry| entry["id"].as_str().expect("id must be a string"))
        .collect();
    assert_eq!(
        ids,
        vec!["agent-a", "generic-shell"],
        "a disabled profile must not be listed, and the enabled ones keep the daemon's order"
    );
    assert_eq!(
        sorted_keys(&entries[0]),
        vec![
            "available".to_string(),
            "command".to_string(),
            "cwd".to_string(),
            "detection".to_string(),
            "display_name".to_string(),
            "id".to_string(),
        ],
        "a listed profile carries the same keys as before and no enabled key, got: {}",
        entries[0]
    );
    assert_eq!(
        sorted_keys(&entries[1]),
        vec![
            "available".to_string(),
            "display_name".to_string(),
            "id".to_string(),
        ],
        "a listed profile with no optional value carries its always-present keys only, got: {}",
        entries[1]
    );
    assert_eq!(
        result["warnings"],
        serde_json::json!(["skipped `broken.toml`: display_name is missing"])
    );
}

#[tokio::test]
async fn list_profiles_reports_an_empty_profiles_array_when_every_profile_is_disabled() {
    let transport = MockTransport::with_response(Ok(DaemonMessage::ProfileList {
        profiles: vec![
            disabled_profile("agent-a", "Agent A"),
            disabled_profile("agent-b", "Agent B"),
        ],
        warnings: vec!["skipped `broken.toml`: display_name is missing".to_string()],
    }));

    let result = list_profiles(&transport)
        .await
        .expect("disabled profiles must not turn the response into an error");

    assert_eq!(result["profiles"], serde_json::Value::Array(Vec::new()));
    assert_eq!(
        result["warnings"],
        serde_json::json!(["skipped `broken.toml`: display_name is missing"])
    );
}

#[tokio::test]
async fn a_daemon_profile_disabled_refusal_becomes_profile_disabled_carrying_the_daemons_message() {
    let transport = MockTransport::with_response(Ok(profile_disabled_error()));

    let err = open_window(&transport, profile_open_window_args("agent-b"))
        .await
        .expect_err("must fail");

    match err {
        ToolError::ProfileDisabled(message) => assert_eq!(message, DISABLED_PROFILE_MESSAGE),
        other => panic!("expected ToolError::ProfileDisabled, got {other:?}"),
    }
}

#[tokio::test]
async fn open_window_relays_a_disabled_profile_refusal_as_profile_disabled() {
    let transport = MockTransport::with_response(Ok(profile_disabled_error()));

    let err = open_window(&transport, profile_open_window_args("agent-b"))
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "profile_disabled");
    match transport.last_sent() {
        ClientMessage::OpenWindowFromProfile { profile, .. } => assert_eq!(profile, "agent-b"),
        other => panic!("expected ClientMessage::OpenWindowFromProfile, got {other:?}"),
    }
}

#[tokio::test]
async fn send_handoff_relays_a_disabled_profile_refusal_as_profile_disabled() {
    let transport = MockTransport::with_response(Ok(profile_disabled_error()));

    let err = send_handoff(&transport, valid_profile_handoff_args())
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "profile_disabled");
    match transport.last_sent() {
        ClientMessage::SendHandoff { profile, .. } => {
            assert_eq!(profile, Some("claude-code".to_string()));
        }
        other => panic!("expected ClientMessage::SendHandoff, got {other:?}"),
    }
}

#[tokio::test]
async fn open_task_window_relays_a_disabled_profile_refusal_as_profile_disabled() {
    let transport = MockTransport::with_response(Ok(profile_disabled_error()));
    let args = OpenTaskWindowArgs {
        plan_id: Some("p1".to_string()),
        task_id: Some("t1".to_string()),
        name: None,
    };

    let err = open_task_window(&transport, args)
        .await
        .expect_err("must fail");

    assert_eq!(err.code(), "profile_disabled");
    assert_eq!(transport.call_count(), 1);
}

// =====================================================================
// Windows that declare no composer
// =====================================================================

/// A daemon refusal for a window whose definition declares no composer. The
/// text is the daemon's guidance to the caller, so the bridge must carry it
/// through as-is.
const NO_COMPOSER_REFUSAL: &str = "this window declares no composer";

fn no_composer_error() -> DaemonMessage {
    DaemonMessage::Error {
        code: "no_composer".to_string(),
        message: NO_COMPOSER_REFUSAL.to_string(),
        rejected_argument: None,
    }
}

/// A caller can only choose another route (a command window, `send_keys`)
/// when the refusal keeps both its own code and the daemon's guidance.
fn assert_no_composer_relayed(err: ToolError) {
    assert_eq!(err.code(), "no_composer");
    match err {
        ToolError::NoComposer(message) => assert_eq!(message, NO_COMPOSER_REFUSAL),
        other => panic!("expected ToolError::NoComposer, got {other:?}"),
    }
}

#[test]
fn tool_error_code_covers_no_composer() {
    let err: ToolError = ToolError::NoComposer("x".to_string());
    assert_eq!(err.code(), "no_composer");
}

#[tokio::test]
async fn send_message_relays_a_no_composer_refusal_with_the_daemons_message() {
    let transport = MockTransport::with_response(Ok(no_composer_error()));
    let args = SendMessageArgs {
        window_id: Some("w1".to_string()),
        text: Some("hello".to_string()),
        mode: None,
    };

    let err = send_message(&transport, args).await.expect_err("must fail");

    assert_no_composer_relayed(err);
}

#[tokio::test]
async fn send_handoff_relays_a_no_composer_refusal_with_the_daemons_message_for_either_target() {
    let targets: [SendHandoffArgs; 2] = [valid_window_handoff_args(), valid_profile_handoff_args()];
    for args in targets {
        let transport = MockTransport::with_response(Ok(no_composer_error()));

        let err = send_handoff(&transport, args).await.expect_err("must fail");

        assert_no_composer_relayed(err);
    }
}

#[tokio::test]
async fn open_task_window_relays_a_no_composer_refusal_with_the_daemons_message() {
    let transport = MockTransport::with_response(Ok(no_composer_error()));
    let args = OpenTaskWindowArgs {
        plan_id: Some("p1".to_string()),
        task_id: Some("t1".to_string()),
        name: None,
    };

    let err = open_task_window(&transport, args)
        .await
        .expect_err("must fail");

    assert_no_composer_relayed(err);
    assert_eq!(transport.call_count(), 1);
}
