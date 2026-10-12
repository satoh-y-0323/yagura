#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests for the state-detection extension of the yagura wire
//! protocol: StatusEvent ingress (shared validation helpers and constants)
//! and StatusChanged egress (`WindowStatus`).

use yagura_protocol::{
    ClientMessage, DaemonMessage, MessageError, WindowStatus, decode_client_message,
    decode_daemon_message, encode_client_message, encode_daemon_message, is_valid_source,
};

/// The seven fixed states and their `snake_case` wire representations.
const ALL_STATUSES: &[(WindowStatus, &str)] = &[
    (WindowStatus::Running, "running"),
    (WindowStatus::WaitingInput, "waiting_input"),
    (WindowStatus::WaitingApproval, "waiting_approval"),
    (WindowStatus::Interrupted, "interrupted"),
    (WindowStatus::Idle, "idle"),
    (WindowStatus::Exited, "exited"),
    (WindowStatus::Unknown, "unknown"),
];

// ---------------------------------------------------------------------
// WindowStatus: the seven fixed values, snake_case serde representation
// ---------------------------------------------------------------------

#[test]
fn window_status_round_trips_through_its_snake_case_wire_string() {
    for (status, wire) in ALL_STATUSES {
        let value = serde_json::to_value(status).expect("status must serialize");
        assert_eq!(value, serde_json::json!(wire));
        let decoded: WindowStatus = serde_json::from_value(value).expect("status must deserialize");
        assert_eq!(decoded, *status);
    }
}

#[test]
fn window_status_deserialize_rejects_anything_but_a_snake_case_wire_string() {
    // Fail-loud: an unrecognized state name must not silently degrade into a
    // known variant (not even into `Unknown`, which is a decided state).
    for wire in [r#""bogus_status""#, r#""WaitingInput""#] {
        let result: Result<WindowStatus, _> = serde_json::from_str(wire);
        assert!(result.is_err(), "{wire} must not decode, got {result:?}");
    }
}

// ---------------------------------------------------------------------
// Unknown gate: is_actionable()
// ---------------------------------------------------------------------

#[test]
fn is_actionable_returns_false_for_unknown() {
    assert!(!WindowStatus::Unknown.is_actionable());
}

#[test]
fn is_actionable_returns_false_for_exited() {
    assert!(!WindowStatus::Exited.is_actionable());
}

#[test]
fn is_actionable_returns_true_for_every_other_status() {
    let actionable = [
        WindowStatus::Running,
        WindowStatus::WaitingInput,
        WindowStatus::WaitingApproval,
        WindowStatus::Interrupted,
        WindowStatus::Idle,
    ];
    for status in actionable {
        assert!(status.is_actionable(), "{status:?} must be actionable");
    }
}

// ---------------------------------------------------------------------
// ClientMessage::StatusEvent (C->D)
// ---------------------------------------------------------------------

/// A `StatusEvent` frame as a hook sends it, decoded; built from JSON so the
/// tests below do not depend on which fields the Rust variant carries.
fn status_event_from_json(value: serde_json::Value) -> ClientMessage {
    decode_client_message(value.to_string().as_bytes()).expect("status_event must decode")
}

#[test]
fn status_event_decodes_from_its_snake_case_tagged_json() {
    let body = br#"{"type":"status_event","source":"claude-code","status":"waiting_approval","token":"0123456789abcdef0123456789abcdef"}"#;
    match decode_client_message(body).expect("status_event must decode") {
        ClientMessage::StatusEvent {
            source,
            status,
            window_id,
            token,
            ..
        } => {
            assert_eq!(source, "claude-code");
            assert_eq!(status, WindowStatus::WaitingApproval);
            assert_eq!(window_id, None);
            assert_eq!(token, "0123456789abcdef0123456789abcdef");
        }
        other => panic!("expected status_event, got {other:?}"),
    }
}

#[test]
fn status_event_round_trip_preserves_all_fields() {
    for (source, status, token) in [
        ("cli", "idle", "0123456789abcdef0123456789abcdef"),
        (
            "hook_source-1",
            "unknown",
            "fedcba9876543210fedcba9876543210",
        ),
    ] {
        let msg = status_event_from_json(serde_json::json!({
            "type": "status_event", "source": source, "status": status, "token": token,
        }));
        let body = encode_client_message(&msg);
        let decoded = decode_client_message(&body).expect("round trip decode must succeed");
        assert_eq!(decoded, msg);
    }
}

#[test]
fn status_event_encodes_with_the_snake_case_type_tag_and_no_ttl_ms() {
    // A report is a fact about the turn, not a value with a lifetime, so the
    // frame carries no `ttl_ms` -- not even a null one.
    let msg = status_event_from_json(serde_json::json!({
        "type": "status_event",
        "source": "cli",
        "status": "waiting_input",
        "ttl_ms": 30_000,
        "token": "0123456789abcdef0123456789abcdef",
    }));
    let body = encode_client_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    let expected = serde_json::json!({
        "type": "status_event",
        "source": "cli",
        "status": "waiting_input",
        "token": "0123456789abcdef0123456789abcdef",
    });
    assert_eq!(value, expected);
}

#[test]
fn status_event_ignores_a_ttl_ms_an_older_hook_still_sends() {
    // Hooks written before `ttl_ms` was removed keep sending it; it is an
    // unknown field now, ignored whatever its value.
    for ttl in [
        serde_json::json!(5000),
        serde_json::json!(null),
        serde_json::json!("5000"),
    ] {
        let body = serde_json::json!({
            "type": "status_event", "source": "cli", "status": "running",
            "ttl_ms": ttl, "token": "0123456789abcdef0123456789abcdef",
        })
        .to_string();
        let decoded = decode_client_message(body.as_bytes())
            .unwrap_or_else(|err| panic!("a legacy ttl_ms {ttl} must not fail the frame: {err:?}"));
        assert!(
            matches!(decoded, ClientMessage::StatusEvent { .. }),
            "got {decoded:?}"
        );
    }
}

#[test]
fn status_event_round_trips_for_every_status_value() {
    for (status, wire) in ALL_STATUSES {
        let msg = status_event_from_json(serde_json::json!({
            "type": "status_event", "source": "cli", "status": wire,
            "token": "0123456789abcdef0123456789abcdef",
        }));
        let body = encode_client_message(&msg);
        let decoded = decode_client_message(&body).expect("round trip decode must succeed");
        assert_eq!(decoded, msg);
        assert!(
            matches!(&decoded, ClientMessage::StatusEvent { status: s, .. } if s == status),
            "{decoded:?} must carry {status:?}"
        );
    }
}

#[test]
fn decode_client_message_rejects_status_event_with_an_unknown_status_string() {
    // A known `type` with a malformed field is InvalidJson, not a silent
    // fallback. The token is present so the `status` value is the body's only
    // defect (makes the token required).
    let body = br#"{"type":"status_event","source":"cli","status":"bogus_status","token":"0123456789abcdef0123456789abcdef"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn decode_client_message_rejects_status_event_missing_the_status_field() {
    let body =
        br#"{"type":"status_event","source":"cli","token":"0123456789abcdef0123456789abcdef"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn decode_client_message_rejects_status_event_missing_the_source_field() {
    let body =
        br#"{"type":"status_event","status":"running","token":"0123456789abcdef0123456789abcdef"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn decode_daemon_message_treats_status_event_as_unknown_type() {
    // `status_event` is C->D only; the daemon-side enum must reject it the same
    // way it rejects any other direction-violating type.
    let body = br#"{"type":"status_event","source":"cli","status":"running","token":"0123456789abcdef0123456789abcdef"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::UnknownType));
}

// ---------------------------------------------------------------------
// DaemonMessage::StatusChanged (D->C)
// ---------------------------------------------------------------------

#[test]
fn status_changed_decodes_from_its_snake_case_tagged_json() {
    let body = br#"{"type":"status_changed","window_id":"w1","status":"waiting_input"}"#;
    let decoded = decode_daemon_message(body).expect("status_changed must decode");
    assert_eq!(
        decoded,
        DaemonMessage::StatusChanged {
            window_id: "w1".to_string(),
            status: WindowStatus::WaitingInput,
            unknown_reason: None,
            status_rev: None,
        }
    );
}

#[test]
fn status_changed_encodes_with_the_snake_case_type_tag_and_field_names() {
    let msg = DaemonMessage::StatusChanged {
        window_id: "w2".to_string(),
        status: WindowStatus::Unknown,
        unknown_reason: None,
        status_rev: None,
    };
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    let expected =
        serde_json::json!({"type": "status_changed", "window_id": "w2", "status": "unknown"});
    assert_eq!(value, expected);
}

#[test]
fn status_changed_round_trips_for_every_status_value() {
    for (status, _) in ALL_STATUSES {
        let msg = DaemonMessage::StatusChanged {
            window_id: "w1".to_string(),
            status: *status,
            unknown_reason: None,
            status_rev: None,
        };
        let body = encode_daemon_message(&msg);
        let decoded = decode_daemon_message(&body).expect("round trip decode must succeed");
        assert_eq!(decoded, msg);
    }
}

#[test]
fn decode_daemon_message_rejects_status_changed_with_an_unknown_status_string() {
    let body = br#"{"type":"status_changed","window_id":"w1","status":"bogus_status"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn decode_daemon_message_rejects_status_changed_missing_the_status_field() {
    let body = br#"{"type":"status_changed","window_id":"w1"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn decode_daemon_message_rejects_status_changed_missing_the_window_id_field() {
    let body = br#"{"type":"status_changed","status":"running"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn decode_client_message_treats_status_changed_as_unknown_type() {
    // `status_changed` is D->C only.
    let body = br#"{"type":"status_changed","window_id":"w1","status":"running"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::UnknownType));
}

// ---------------------------------------------------------------------
// Shared StatusEvent validation helpers and constants
// ---------------------------------------------------------------------

#[test]
fn is_valid_source_accepts_lowercase_alphanumeric_underscore_and_hyphen() {
    for source in ["a", "0", "cli", "claude-code", "hook_source", "a-b_c-9"] {
        assert!(is_valid_source(source), "{source:?} must be accepted");
    }
}

#[test]
fn is_valid_source_accepts_a_source_of_exactly_the_maximum_length() {
    let source = "a".repeat(64);
    assert!(is_valid_source(&source));
}

#[test]
fn is_valid_source_rejects_an_empty_source() {
    assert!(!is_valid_source(""));
}

#[test]
fn is_valid_source_rejects_a_source_longer_than_the_maximum_length() {
    let source = "a".repeat(65);
    assert!(!is_valid_source(&source));
}

#[test]
fn is_valid_source_rejects_uppercase_characters() {
    for source in ["CLI", "Cli", "claude-Code"] {
        assert!(!is_valid_source(source), "{source:?} must be rejected");
    }
}

#[test]
fn is_valid_source_rejects_path_separators_and_traversal() {
    for source in ["a/b", r"a\b", "..", "../etc", "a.b", "/cli"] {
        assert!(!is_valid_source(source), "{source:?} must be rejected");
    }
}

#[test]
fn is_valid_source_rejects_whitespace_and_control_characters() {
    for source in ["a b", " cli", "cli ", "a\tb", "a\nb", "a\0b"] {
        assert!(!is_valid_source(source), "{source:?} must be rejected");
    }
}

#[test]
fn is_valid_source_rejects_non_ascii_characters() {
    for source in ["クライアント", "clí"] {
        assert!(!is_valid_source(source), "{source:?} must be rejected");
    }
}

// ---------------------------------------------------------------------
// Backward compatibility: adding variants must not break existing messages
// ---------------------------------------------------------------------

#[test]
fn existing_client_messages_still_decode_after_the_status_extension() {
    let cases: [(&[u8], ClientMessage); 5] = [
        (
            br#"{"type":"attach_view","window_id":"w1"}"#,
            ClientMessage::AttachView {
                window_id: "w1".to_string(),
                size: None,
            },
        ),
        (
            br#"{"type":"text","text":"ls"}"#,
            ClientMessage::Text {
                text: "ls".to_string(),
            },
        ),
        (
            br#"{"type":"resize","cols":80,"rows":24}"#,
            ClientMessage::Resize { cols: 80, rows: 24 },
        ),
        (br#"{"type":"ping"}"#, ClientMessage::Ping),
        (br#"{"type":"shutdown"}"#, ClientMessage::Shutdown),
    ];
    for (body, expected) in cases {
        let decoded = decode_client_message(body).expect("existing type must still decode");
        assert_eq!(decoded, expected);
    }
}

#[test]
fn existing_daemon_messages_still_decode_after_the_status_extension() {
    let cases: [(&[u8], DaemonMessage); 5] = [
        (
            br#"{"type":"attached","child_exited":false,"exit_code":null}"#,
            DaemonMessage::Attached {
                child_exited: false,
                exit_code: None,
            },
        ),
        (
            br#"{"type":"selection_text","text":null}"#,
            DaemonMessage::SelectionText { text: None },
        ),
        (
            br#"{"type":"exit","exit_code":-1}"#,
            DaemonMessage::Exit { exit_code: -1 },
        ),
        (br#"{"type":"pong"}"#, DaemonMessage::Pong),
        (
            br#"{"type":"error","code":"internal","message":"boom"}"#,
            DaemonMessage::Error {
                code: "internal".to_string(),
                message: "boom".to_string(),
                rejected_argument: None,
            },
        ),
    ];
    for (body, expected) in cases {
        let decoded = decode_daemon_message(body).expect("existing type must still decode");
        assert_eq!(decoded, expected);
    }
}

#[test]
fn an_unrelated_unknown_type_is_still_reported_as_unknown_type() {
    assert_eq!(
        decode_client_message(br#"{"type":"bogus"}"#),
        Err(MessageError::UnknownType)
    );
    assert_eq!(
        decode_daemon_message(br#"{"type":"bogus"}"#),
        Err(MessageError::UnknownType)
    );
}

#[test]
fn status_event_ignores_unknown_fields_for_forward_compatibility() {
    let body = br#"{"type":"status_event","source":"cli","status":"idle","token":"0123456789abcdef0123456789abcdef","unexpected_field":123}"#;
    match decode_client_message(body).expect("unknown fields must be ignored") {
        ClientMessage::StatusEvent {
            source,
            status,
            window_id,
            token,
            ..
        } => {
            assert_eq!(source, "cli");
            assert_eq!(status, WindowStatus::Idle);
            assert_eq!(window_id, None);
            assert_eq!(token, "0123456789abcdef0123456789abcdef");
        }
        other => panic!("expected status_event, got {other:?}"),
    }
}
