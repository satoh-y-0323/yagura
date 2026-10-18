#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests for IPC protocol v2 (multi-window support).
//!
//! Defines the wire-level contract for v2 messages and their round-trip
//! invariants.

use yagura_protocol::{
    ClientMessage, DaemonMessage, MessageError, RoomRef, WindowInfo, WindowStatus,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
};

// =============================================================================
// WindowInfo: the optional keys are absent when None, and a room travels as
// its two named fields.
// =============================================================================

fn sample_window_info(room: Option<RoomRef>) -> WindowInfo {
    WindowInfo {
        window_id: "w1".to_string(),
        name: "shell".to_string(),
        vendor: None,
        cwd: "/home".to_string(),
        status: WindowStatus::Running,
        exit_code: None,
        layout_json: None,
        room,
        unknown_reason: None,
        detection_warning: None,
        status_rev: None,
        mcp_connected: None,
        status_reports_missing: None,
    }
}

#[test]
fn window_info_with_no_optional_value_carries_only_its_always_present_keys() {
    // A peer predating any optional field keeps decoding what this crate emits.
    let value = serde_json::to_value(sample_window_info(None)).expect("must serialize");
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("WindowInfo must serialize as an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["cwd", "exit_code", "name", "status", "vendor", "window_id"],
        "got: {value}"
    );
}

#[test]
fn window_info_room_travels_as_room_id_and_name() {
    let info = sample_window_info(Some(RoomRef {
        room_id: "r-0123456789abcdef".to_string(),
        name: "yagura".to_string(),
    }));
    let value = serde_json::to_value(&info).expect("must serialize");
    assert_eq!(
        value.get("room"),
        Some(&serde_json::json!({ "room_id": "r-0123456789abcdef", "name": "yagura" }))
    );
    let back: WindowInfo = serde_json::from_value(value).expect("must deserialize");
    assert_eq!(back, info);
}

// =============================================================================
// ClientMessage v2: new variants and Attach changes
// =============================================================================

#[test]
fn client_message_attach_view_requires_window_id_field() {
    let body = br#"{"type":"attach_view","window_id":"w1"}"#;
    let decoded = decode_client_message(body).expect("attach_view with window_id must decode");
    let _ = decoded;
}

#[test]
fn client_message_attach_view_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"attach_view"}"#;
    let result = decode_client_message(body);
    assert_eq!(result, Err(MessageError::InvalidJson));
}

#[test]
fn client_message_open_window_encodes_and_decodes() {
    let body = br#"{"type":"open_window","command":["pwsh.exe"],"cwd":"/home/user","name":"shell1","vendor":"claude-code","detection":"bash"}"#;
    let decoded = decode_client_message(body).expect("open_window must decode with all fields");
    let _ = decoded;
}

#[test]
fn client_message_open_window_with_minimal_fields() {
    let body = br#"{"type":"open_window","command":["sh","-c","echo hello"]}"#;
    let decoded = decode_client_message(body).expect("open_window with only command must decode");
    let _ = decoded;
}

#[test]
fn client_message_open_window_round_trip() {
    let body = br#"{"type":"open_window","command":["bash","-i"],"cwd":"/tmp","name":"term"}"#;
    let decoded = decode_client_message(body).expect("round trip must succeed");
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("second decode must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_list_windows_decodes() {
    let body = br#"{"type":"list_windows"}"#;
    let decoded = decode_client_message(body).expect("list_windows must decode");
    let _ = decoded;
}

#[test]
fn client_message_get_window_status_encodes_and_decodes() {
    let body = br#"{"type":"get_window_status","window_id":"w2"}"#;
    let decoded = decode_client_message(body).expect("get_window_status must decode");
    let _ = decoded;
}

#[test]
fn client_message_get_window_status_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"get_window_status"}"#;
    let result = decode_client_message(body);
    assert_eq!(result, Err(MessageError::InvalidJson));
}

#[test]
fn client_message_send_text_encodes_and_decodes() {
    let body = br#"{"type":"send_text","window_id":"w1","text":"echo hello\n"}"#;
    let decoded = decode_client_message(body).expect("send_text must decode");
    let _ = decoded;
}

#[test]
fn client_message_send_text_round_trip() {
    let body = br#"{"type":"send_text","window_id":"w3","text":"input"}"#;
    let decoded = decode_client_message(body).expect("decode must succeed");
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("second decode must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_status_event_now_accepts_optional_window_id() {
    let body = br#"{"type":"status_event","source":"cli","status":"running","window_id":"w2","token":"0123456789abcdef0123456789abcdef"}"#;
    let decoded = decode_client_message(body).expect("status_event with window_id must decode");
    let _ = decoded;
}

#[test]
fn client_message_status_event_without_window_id_still_decodes_for_backward_compat() {
    let body = br#"{"type":"status_event","source":"cli","status":"idle","token":"0123456789abcdef0123456789abcdef"}"#;
    let decoded =
        decode_client_message(body).expect("status_event without window_id must still decode");
    let _ = decoded;
}

#[test]
fn client_message_status_event_with_window_id_round_trip() {
    let body = br#"{"type":"status_event","source":"hook","status":"waiting_input","window_id":"w1","ttl_ms":15000,"token":"0123456789abcdef0123456789abcdef"}"#;
    let decoded = decode_client_message(body).expect("decode must succeed");
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("round trip decode must succeed");
    assert_eq!(decoded, decoded2);
}

// =============================================================================
// DaemonMessage v2: new variants and StatusChanged change
// =============================================================================

#[test]
fn daemon_message_window_opened_encodes_and_decodes() {
    let body = br#"{"type":"window_opened","window_id":"w4"}"#;
    let decoded = decode_daemon_message(body).expect("window_opened must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_window_opened_round_trip() {
    let body = br#"{"type":"window_opened","window_id":"w1"}"#;
    let decoded = decode_daemon_message(body).expect("decode must succeed");
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_list_encodes_and_decodes() {
    let body = br#"{"type":"window_list","windows":[{"window_id":"w1","name":"shell","vendor":null,"cwd":"/home","status":"running","exit_code":null}]}"#;
    let decoded = decode_daemon_message(body).expect("window_list must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_window_list_with_multiple_windows() {
    let body = br#"{"type":"window_list","windows":[{"window_id":"w1","name":"main","vendor":"claude-code","cwd":"/work","status":"running","exit_code":null},{"window_id":"w2","name":"aux","vendor":null,"cwd":"/tmp","status":"idle","exit_code":null}]}"#;
    let decoded =
        decode_daemon_message(body).expect("window_list with multiple entries must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_window_list_with_exited_window() {
    let body = br#"{"type":"window_list","windows":[{"window_id":"w1","name":"old","vendor":null,"cwd":"/tmp","status":"exited","exit_code":0}]}"#;
    let decoded = decode_daemon_message(body).expect("exited window in list must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_window_status_encodes_and_decodes() {
    let body = br#"{"type":"window_status","window_id":"w1","status":"running","since_ms":1234567,"last_output_summary":null,"exit_code":null}"#;
    let decoded = decode_daemon_message(body).expect("window_status must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_window_status_with_output_summary_and_exit_code() {
    let body = br#"{"type":"window_status","window_id":"w2","status":"exited","since_ms":5000,"last_output_summary":"hello world","exit_code":1}"#;
    let decoded =
        decode_daemon_message(body).expect("window_status with summary and exit_code must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_window_status_round_trip() {
    let body = br#"{"type":"window_status","window_id":"w3","status":"waiting_input","since_ms":999,"last_output_summary":null,"exit_code":null}"#;
    let decoded = decode_daemon_message(body).expect("decode must succeed");
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_status_changed_now_includes_window_id() {
    let body = br#"{"type":"status_changed","window_id":"w1","status":"waiting_approval"}"#;
    let decoded = decode_daemon_message(body).expect("status_changed with window_id must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_status_changed_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"status_changed","status":"running"}"#;
    let result = decode_daemon_message(body);
    assert_eq!(result, Err(MessageError::InvalidJson));
}

#[test]
fn daemon_message_status_changed_round_trip_with_window_id() {
    let body = br#"{"type":"status_changed","window_id":"w2","status":"idle"}"#;
    let decoded = decode_daemon_message(body).expect("decode must succeed");
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_text_sent_round_trip() {
    // The success response to send_text.
    let body = br#"{"type":"text_sent","window_id":"w1"}"#;
    let decoded = decode_daemon_message(body).expect("text_sent must decode");
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

// =============================================================================
// Error codes: new v2 error variants
// =============================================================================

#[test]
fn daemon_message_error_with_resource_exhausted_code() {
    let body = br#"{"type":"error","code":"resource_exhausted","message":"too many windows"}"#;
    let decoded = decode_daemon_message(body).expect("resource_exhausted error must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_error_with_window_not_found_code() {
    let body = br#"{"type":"error","code":"window_not_found","message":"window w99 not found"}"#;
    let decoded = decode_daemon_message(body).expect("window_not_found error must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_error_with_unsupported_code() {
    let body = br#"{"type":"error","code":"unsupported","message":"mode not supported"}"#;
    let decoded = decode_daemon_message(body).expect("unsupported error must decode");
    let _ = decoded;
}

#[test]
fn daemon_message_error_with_spawn_failed_code() {
    let body = br#"{"type":"error","code":"spawn_failed","message":"failed to spawn process"}"#;
    let decoded = decode_daemon_message(body).expect("spawn_failed error must decode");
    let _ = decoded;
}

// =============================================================================
// Backward compatibility: existing message types must still work in v2
// =============================================================================

#[test]
fn existing_client_messages_still_decode_in_v2_protocol() {
    let cases: &[(&[u8], &str)] = &[
        (br#"{"type":"text","text":"ls"}"#, "Text"),
        (br#"{"type":"resize","cols":80,"rows":24}"#, "Resize"),
        (br#"{"type":"ping"}"#, "Ping"),
        (br#"{"type":"shutdown"}"#, "Shutdown"),
    ];
    for (body, name) in cases {
        let decoded = decode_client_message(body)
            .unwrap_or_else(|_| panic!("existing {name} must still decode in v2"));
        let _ = decoded;
    }
}

#[test]
fn existing_daemon_messages_still_decode_in_v2_protocol() {
    let cases: &[(&[u8], &str)] = &[
        (
            br#"{"type":"attached","child_exited":false,"exit_code":null}"#,
            "Attached",
        ),
        (br#"{"type":"exit","exit_code":-1}"#, "Exit"),
        (br#"{"type":"pong"}"#, "Pong"),
    ];
    for (body, name) in cases {
        let decoded = decode_daemon_message(body)
            .unwrap_or_else(|_| panic!("existing {name} must still decode in v2"));
        let _ = decoded;
    }
}

// =============================================================================
// Unknown type and malformed message handling (same as v1, preserved for v2)
// =============================================================================

#[test]
fn decode_client_message_rejects_unknown_type_in_v2() {
    let result = decode_client_message(br#"{"type":"unknown_v2_type"}"#);
    assert_eq!(result, Err(MessageError::UnknownType));
}

#[test]
fn decode_daemon_message_rejects_unknown_type_in_v2() {
    let result = decode_daemon_message(br#"{"type":"unknown_v2_type"}"#);
    assert_eq!(result, Err(MessageError::UnknownType));
}

#[test]
fn client_message_ignores_unknown_fields_in_v2() {
    let body = br#"{"type":"open_window","command":["sh"],"future_field":"ignored"}"#;
    let decoded = decode_client_message(body)
        .expect("unknown fields must be ignored for forward compatibility");
    let _ = decoded;
}

// =============================================================================
// Control tools: ClientMessage additions
// =============================================================================

/// A syntactically well-formed status token as distributed by the daemon:
/// a 128-bit value rendered as 32 hex characters.
const TEST_STATUS_TOKEN: &str = "0123456789abcdef0123456789abcdef";

#[test]
fn client_message_interrupt_window_round_trips() {
    let body = br#"{"type":"interrupt_window","window_id":"w1"}"#;
    let decoded = decode_client_message(body).expect("interrupt_window must decode");
    assert_eq!(
        decoded,
        ClientMessage::InterruptWindow {
            window_id: "w1".to_string(),
        }
    );
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_interrupt_window_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"interrupt_window"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_close_window_round_trips_with_explicit_force() {
    let body = br#"{"type":"close_window","window_id":"w1","force":true}"#;
    let decoded = decode_client_message(body).expect("close_window must decode");
    assert_eq!(
        decoded,
        ClientMessage::CloseWindow {
            window_id: "w1".to_string(),
            force: Some(true),
        }
    );
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_close_window_without_force_decodes_as_none() {
    // `force` is optional on the wire; the daemon applies the `false` default.
    let body = br#"{"type":"close_window","window_id":"w2"}"#;
    let decoded = decode_client_message(body).expect("close_window without force must decode");
    assert_eq!(
        decoded,
        ClientMessage::CloseWindow {
            window_id: "w2".to_string(),
            force: None,
        }
    );
}

#[test]
fn client_message_close_window_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"close_window","force":false}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_wait_for_status_round_trips() {
    let body = br#"{"type":"wait_for_status","window_id":"w1","target_status":"waiting_approval","timeout_sec":30}"#;
    let decoded = decode_client_message(body).expect("wait_for_status must decode");
    assert_eq!(
        decoded,
        ClientMessage::WaitForStatus {
            window_id: "w1".to_string(),
            target_status: WindowStatus::WaitingApproval,
            timeout_sec: 30,
        }
    );
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_wait_for_status_decodes_unknown_as_a_valid_target_status() {
    // `unknown` is a well-formed WindowStatus on the wire. Rejecting it as a
    // wait target is an IPC-layer concern (invalid_argument), not a decode
    // concern, so the protocol layer must accept it.
    let body =
        br#"{"type":"wait_for_status","window_id":"w1","target_status":"unknown","timeout_sec":5}"#;
    let decoded = decode_client_message(body).expect("unknown target_status must decode");
    assert_eq!(
        decoded,
        ClientMessage::WaitForStatus {
            window_id: "w1".to_string(),
            target_status: WindowStatus::Unknown,
            timeout_sec: 5,
        }
    );
}

#[test]
fn client_message_wait_for_status_rejects_an_unknown_target_status_string() {
    // target_status is the WindowStatus type, not a free-form string: an
    // unrecognized name must fail loud rather than degrade to a variant.
    let body = br#"{"type":"wait_for_status","window_id":"w1","target_status":"bogus_status","timeout_sec":5}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_wait_for_status_without_timeout_sec_returns_invalid_json() {
    let body = br#"{"type":"wait_for_status","window_id":"w1","target_status":"idle"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_wait_for_status_with_a_non_numeric_timeout_sec_returns_invalid_json() {
    let body =
        br#"{"type":"wait_for_status","window_id":"w1","target_status":"idle","timeout_sec":"30"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_read_output_round_trips_with_explicit_lines() {
    let body = br#"{"type":"read_output","window_id":"w1","lines":50}"#;
    let decoded = decode_client_message(body).expect("read_output must decode");
    assert_eq!(
        decoded,
        ClientMessage::ReadOutput {
            window_id: "w1".to_string(),
            lines: Some(50),
        }
    );
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_read_output_without_lines_decodes_as_none() {
    // `lines` is optional on the wire; the daemon applies the default of 100.
    let body = br#"{"type":"read_output","window_id":"w2"}"#;
    let decoded = decode_client_message(body).expect("read_output without lines must decode");
    assert_eq!(
        decoded,
        ClientMessage::ReadOutput {
            window_id: "w2".to_string(),
            lines: None,
        }
    );
}

#[test]
fn client_message_read_output_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"read_output","lines":10}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

// =============================================================================
// StatusEvent authentication token: breaking change, fail-closed
// =============================================================================

#[test]
fn client_message_status_event_without_token_returns_invalid_json() {
    // Fail-closed: the pre-token wire form must be rejected outright rather
    // than decoded into an unauthenticated event.
    let body = br#"{"type":"status_event","source":"cli","status":"running"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_status_event_with_null_token_returns_invalid_json() {
    // `token` is a required String, not an Option: an explicit null must not
    // be accepted as "no token".
    let body = br#"{"type":"status_event","source":"cli","status":"running","token":null}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_status_event_with_token_round_trips() {
    let body = br#"{"type":"status_event","source":"cli","status":"running","token":"0123456789abcdef0123456789abcdef"}"#;
    let decoded = decode_client_message(body).expect("status_event with token must decode");
    match &decoded {
        ClientMessage::StatusEvent {
            source,
            status,
            window_id,
            token,
            ..
        } => {
            assert_eq!(source, "cli");
            assert_eq!(*status, WindowStatus::Running);
            assert_eq!(*window_id, None);
            assert_eq!(token, TEST_STATUS_TOKEN);
        }
        other => panic!("expected status_event, got {other:?}"),
    }
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_status_event_encodes_the_token_field() {
    let msg = decode_client_message(
        format!(
            r#"{{"type":"status_event","source":"cli","status":"idle","token":"{TEST_STATUS_TOKEN}"}}"#
        )
        .as_bytes(),
    )
    .expect("status_event with token must decode");
    let body = encode_client_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    assert_eq!(
        value.get("token").and_then(serde_json::Value::as_str),
        Some(TEST_STATUS_TOKEN)
    );
}

// =============================================================================
// Control tools: DaemonMessage additions
// =============================================================================

#[test]
fn daemon_message_window_interrupted_round_trips() {
    let body = br#"{"type":"window_interrupted","window_id":"w1"}"#;
    let decoded = decode_daemon_message(body).expect("window_interrupted must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowInterrupted {
            window_id: "w1".to_string(),
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_interrupted_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"window_interrupted"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn daemon_message_window_closed_round_trips() {
    let body = br#"{"type":"window_closed","window_id":"w1"}"#;
    let decoded = decode_daemon_message(body).expect("window_closed must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowClosed {
            window_id: "w1".to_string(),
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_closed_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"window_closed"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn daemon_message_wait_result_round_trips_for_a_reached_outcome() {
    let body = br#"{"type":"wait_result","window_id":"w1","outcome":"reached","status":"idle"}"#;
    let decoded = decode_daemon_message(body).expect("wait_result must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WaitResult {
            window_id: "w1".to_string(),
            outcome: "reached".to_string(),
            status: WindowStatus::Idle,
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_wait_result_carries_the_observed_status_on_timeout() {
    // A timeout is a normal response, and `status` is the status observed when
    // the response was produced, which need not equal the wait target.
    let body = br#"{"type":"wait_result","window_id":"w1","outcome":"timeout","status":"running"}"#;
    let decoded = decode_daemon_message(body).expect("timeout wait_result must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WaitResult {
            window_id: "w1".to_string(),
            outcome: "timeout".to_string(),
            status: WindowStatus::Running,
        }
    );
}

#[test]
fn daemon_message_wait_result_without_outcome_returns_invalid_json() {
    let body = br#"{"type":"wait_result","window_id":"w1","status":"idle"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn daemon_message_wait_result_with_an_unknown_status_string_returns_invalid_json() {
    let body = br#"{"type":"wait_result","window_id":"w1","outcome":"reached","status":"bogus"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn daemon_message_window_output_round_trips() {
    let body = br#"{"type":"window_output","window_id":"w1","text":"line one\nline two","truncated":false}"#;
    let decoded = decode_daemon_message(body).expect("window_output must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowOutput {
            window_id: "w1".to_string(),
            text: "line one\nline two".to_string(),
            truncated: false,
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_output_round_trips_when_truncated() {
    let body = br#"{"type":"window_output","window_id":"w2","text":"tail","truncated":true}"#;
    let decoded = decode_daemon_message(body).expect("truncated window_output must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowOutput {
            window_id: "w2".to_string(),
            text: "tail".to_string(),
            truncated: true,
        }
    );
}

#[test]
fn daemon_message_window_output_without_truncated_returns_invalid_json() {
    // `truncated` is required: a missing flag must not silently mean "false".
    let body = br#"{"type":"window_output","window_id":"w1","text":"tail"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

// =============================================================================
// Allow-list registration and direction enforcement for the new types
// =============================================================================

#[test]
fn new_control_client_types_are_registered_in_the_client_allow_list() {
    // A tag that is missing from CLIENT_MESSAGE_TYPES decodes to UnknownType
    // regardless of its fields, so a bare tag body distinguishes "registered
    // but incomplete" (InvalidJson) from "not registered" (UnknownType).
    for body in [
        r#"{"type":"interrupt_window"}"#,
        r#"{"type":"close_window"}"#,
        r#"{"type":"wait_for_status"}"#,
        r#"{"type":"read_output"}"#,
    ] {
        assert_eq!(
            decode_client_message(body.as_bytes()),
            Err(MessageError::InvalidJson),
            "{body} must be a registered client message type"
        );
    }
}

#[test]
fn new_control_daemon_types_are_registered_in_the_daemon_allow_list() {
    for body in [
        r#"{"type":"window_interrupted"}"#,
        r#"{"type":"window_closed"}"#,
        r#"{"type":"wait_result"}"#,
        r#"{"type":"window_output"}"#,
    ] {
        assert_eq!(
            decode_daemon_message(body.as_bytes()),
            Err(MessageError::InvalidJson),
            "{body} must be a registered daemon message type"
        );
    }
}

#[test]
fn new_control_client_types_are_unknown_to_the_daemon_decoder() {
    for body in [
        r#"{"type":"interrupt_window","window_id":"w1"}"#,
        r#"{"type":"close_window","window_id":"w1","force":true}"#,
        r#"{"type":"wait_for_status","window_id":"w1","target_status":"idle","timeout_sec":5}"#,
        r#"{"type":"read_output","window_id":"w1","lines":10}"#,
    ] {
        assert_eq!(
            decode_daemon_message(body.as_bytes()),
            Err(MessageError::UnknownType),
            "{body} is client-to-daemon only"
        );
    }
}

#[test]
fn new_control_daemon_types_are_unknown_to_the_client_decoder() {
    for body in [
        r#"{"type":"window_interrupted","window_id":"w1"}"#,
        r#"{"type":"window_closed","window_id":"w1"}"#,
        r#"{"type":"wait_result","window_id":"w1","outcome":"reached","status":"idle"}"#,
        r#"{"type":"window_output","window_id":"w1","text":"t","truncated":false}"#,
    ] {
        assert_eq!(
            decode_client_message(body.as_bytes()),
            Err(MessageError::UnknownType),
            "{body} is daemon-to-client only"
        );
    }
}

// =============================================================================
// Error codes added for the control tools
// =============================================================================

// =============================================================================
// Detection definition enumeration and open_window warning propagation
// =============================================================================

#[test]
fn client_message_list_detections_decodes() {
    let body = br#"{"type":"list_detections"}"#;
    let decoded = decode_client_message(body).expect("list_detections must decode");
    assert_eq!(decoded, ClientMessage::ListDetections);
}

#[test]
fn daemon_message_detection_list_round_trips_without_warning() {
    let body = br#"{"type":"detection_list","names":["claude-code","generic-shell"]}"#;
    let decoded = decode_daemon_message(body).expect("detection_list without warning must decode");
    assert_eq!(
        decoded,
        DaemonMessage::DetectionList {
            names: vec!["claude-code".to_string(), "generic-shell".to_string()],
            bundled_names: vec![],
            directory_names: vec![],
            warning: None,
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert!(
        value.get("warning").is_none(),
        "warning must be omitted from the wire when None (skip_serializing_if), got: {value}"
    );
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_detection_list_round_trips_with_warning() {
    let body = br#"{"type":"detection_list","names":[],"warning":"failed to enumerate detection definitions (os error)"}"#;
    let decoded = decode_daemon_message(body).expect("detection_list with warning must decode");
    assert_eq!(
        decoded,
        DaemonMessage::DetectionList {
            names: vec![],
            bundled_names: vec![],
            directory_names: vec![],
            warning: Some("failed to enumerate detection definitions (os error)".to_string()),
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_detection_list_decodes_the_old_form_missing_the_warning_key() {
    // Backward compatibility: a body with no "warning" key at all (not even
    // null) must still decode, with warning defaulting to None.
    let body = br#"{"type":"detection_list","names":["a"]}"#;
    let decoded = decode_daemon_message(body).expect("old-form detection_list must decode");
    assert_eq!(
        decoded,
        DaemonMessage::DetectionList {
            names: vec!["a".to_string()],
            bundled_names: vec![],
            directory_names: vec![],
            warning: None,
        }
    );
}

#[test]
fn daemon_message_detection_list_decodes_a_body_missing_both_layer_fields() {
    // A daemon that predates the two-layer split sends only "names" (and,
    // optionally, "warning"): both new fields must default to empty rather
    // than fail to decode.
    let body = br#"{"type":"detection_list","names":["claude-code"],"warning":null}"#;
    let decoded = decode_daemon_message(body).expect("body missing both layer fields must decode");
    assert_eq!(
        decoded,
        DaemonMessage::DetectionList {
            names: vec!["claude-code".to_string()],
            bundled_names: vec![],
            directory_names: vec![],
            warning: None,
        }
    );
}

#[test]
fn daemon_message_detection_list_round_trips_with_layer_fields() {
    let decoded = DaemonMessage::DetectionList {
        names: vec!["claude-code".to_string(), "generic-shell".to_string()],
        bundled_names: vec!["generic-shell".to_string()],
        directory_names: vec!["claude-code".to_string()],
        warning: None,
    };
    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value["bundled_names"],
        serde_json::json!(["generic-shell"]),
        "bundled_names must round trip on the wire, got: {value}"
    );
    assert_eq!(
        value["directory_names"],
        serde_json::json!(["claude-code"]),
        "directory_names must round trip on the wire, got: {value}"
    );
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_opened_round_trips_without_detection_warning() {
    let body = br#"{"type":"window_opened","window_id":"w5"}"#;
    let decoded = decode_daemon_message(body).expect("window_opened without warning must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowOpened {
            window_id: "w5".to_string(),
            detection_warning: None,
            name: None,
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert!(
        value.get("detection_warning").is_none(),
        "detection_warning must be omitted from the wire when None, got: {value}"
    );
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_opened_round_trips_with_detection_warning() {
    let body = br#"{"type":"window_opened","window_id":"w6","detection_warning":"failed to load the detection definition (os error)"}"#;
    let decoded = decode_daemon_message(body).expect("window_opened with warning must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowOpened {
            window_id: "w6".to_string(),
            detection_warning: Some(
                "failed to load the detection definition (os error)".to_string()
            ),
            name: None,
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_opened_decodes_the_old_form_missing_the_detection_warning_key() {
    // Backward compatibility: a window_opened body with no
    // detection_warning key at all must still decode.
    let body = br#"{"type":"window_opened","window_id":"w7"}"#;
    let decoded = decode_daemon_message(body).expect("old-form window_opened must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowOpened {
            window_id: "w7".to_string(),
            detection_warning: None,
            name: None,
        }
    );
}

#[test]
fn daemon_message_error_round_trips_for_the_new_control_error_codes() {
    for code in [
        "approval_required",
        "status_unknown",
        "rate_limited",
        "auth_failed",
    ] {
        let body = format!(r#"{{"type":"error","code":"{code}","message":"denied"}}"#);
        let decoded =
            decode_daemon_message(body.as_bytes()).expect("control error code must decode");
        assert_eq!(
            decoded,
            DaemonMessage::Error {
                code: code.to_string(),
                message: "denied".to_string(),
                rejected_argument: None,
            }
        );
        let encoded = encode_daemon_message(&decoded);
        let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
        assert_eq!(decoded, decoded2);
    }
}
