#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration test: yagura-protocol's length-prefixed JSON framing contract
//! (schema, JSON examples and malformed-frame table).

use yagura_protocol::{
    ClientMessage, DaemonMessage, FrameError, MAX_FRAME_BODY_LEN, MessageError,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
    frame_decode, frame_encode,
};

// ---------------------------------------------------------------------
// Frame envelope: round trip and encode/decode, independent of either enum
// ---------------------------------------------------------------------

#[test]
fn frame_round_trip_returns_original_body_and_consumed_length() {
    let body = br#"{"type":"ping"}"#;
    let framed = frame_encode(body);
    assert_eq!(framed.len(), 4 + body.len());

    let decoded = frame_decode(&framed)
        .expect("frame_decode should succeed for a well-formed frame")
        .expect("a complete frame must be available");
    assert_eq!(decoded.0, body);
    assert_eq!(decoded.1, framed.len());
}

#[test]
fn frame_decode_returns_none_when_length_prefix_is_incomplete() {
    // The little-endian u32 length prefix itself has not arrived in full.
    let partial_prefix = [0x05, 0x00];
    let result = frame_decode(&partial_prefix).expect("incomplete prefix must not be an error");
    assert!(result.is_none());
}

#[test]
fn frame_decode_returns_none_when_body_is_truncated() {
    let body = br#"{"type":"attach","window_id":"w1"}"#;
    let full = frame_encode(body);
    // Drop the tail of the body to simulate a mid-frame cut.
    let truncated = &full[..full.len() - 3];
    let result = frame_decode(truncated).expect("truncated body must not be an error");
    assert!(result.is_none());
}

#[test]
fn frame_decode_accepts_body_exactly_at_the_size_limit() {
    // A length exactly at the limit is accepted (only exceeding it is
    // frame_too_large), and the verdict comes from the prefix alone, so no
    // body is supplied here.
    let mut prefix_only = Vec::new();
    prefix_only.extend_from_slice(&(MAX_FRAME_BODY_LEN as u32).to_le_bytes());
    let result =
        frame_decode(&prefix_only).expect("a length equal to the limit must not be rejected");
    assert!(
        result.is_none(),
        "should be waiting for the body, not erroring"
    );
}

#[test]
fn frame_decode_rejects_length_prefix_exceeding_the_size_limit() {
    let mut prefix_only = Vec::new();
    prefix_only.extend_from_slice(&(MAX_FRAME_BODY_LEN as u32 + 1).to_le_bytes());
    let result = frame_decode(&prefix_only);
    assert_eq!(result, Err(FrameError::TooLarge));
}

#[test]
fn frame_decode_reads_first_frame_out_of_a_buffer_containing_two_frames() {
    let first = frame_encode(br#"{"type":"ping"}"#);
    let second = frame_encode(br#"{"type":"shutdown"}"#);
    let mut buf = Vec::new();
    buf.extend_from_slice(&first);
    buf.extend_from_slice(&second);

    let (body, consumed) = frame_decode(&buf)
        .expect("decode should succeed")
        .expect("first frame must be complete");
    assert_eq!(body, br#"{"type":"ping"}"#);
    assert_eq!(consumed, first.len());

    let (body2, consumed2) = frame_decode(&buf[consumed..])
        .expect("decode should succeed")
        .expect("second frame must be complete");
    assert_eq!(body2, br#"{"type":"shutdown"}"#);
    assert_eq!(consumed2, second.len());
}

// ---------------------------------------------------------------------
// ClientMessage / DaemonMessage: round trip and internally-tagged JSON shape
// ---------------------------------------------------------------------

#[test]
fn client_message_round_trip_for_every_variant() {
    let messages = vec![
        ClientMessage::AttachView {
            window_id: "w1".to_string(),
            size: None,
        },
        ClientMessage::Text {
            text: "ls".to_string(),
        },
        ClientMessage::FrameAck { seq: 3 },
        ClientMessage::CopySelection,
        ClientMessage::ScrollView { lines: -2 },
        ClientMessage::Resize { cols: 80, rows: 24 },
        ClientMessage::Ping,
        ClientMessage::Shutdown,
    ];

    for msg in messages {
        let body = encode_client_message(&msg);
        let decoded = decode_client_message(&body).expect("round trip decode must succeed");
        assert_eq!(decoded, msg);
    }
}

#[test]
fn daemon_message_round_trip_for_every_variant() {
    let messages = vec![
        DaemonMessage::Attached {
            child_exited: false,
            exit_code: None,
        },
        DaemonMessage::Attached {
            child_exited: true,
            exit_code: Some(0),
        },
        DaemonMessage::SelectionText {
            text: Some("hello".to_string()),
        },
        DaemonMessage::SelectionText { text: None },
        DaemonMessage::Exit { exit_code: -1 },
        DaemonMessage::Pong,
        DaemonMessage::Error {
            code: "unknown_type".to_string(),
            message: "unknown message type".to_string(),
            rejected_argument: None,
        },
    ];

    for msg in messages {
        let body = encode_daemon_message(&msg);
        let decoded = decode_daemon_message(&body).expect("round trip decode must succeed");
        assert_eq!(decoded, msg);
    }
}

#[test]
fn decode_client_message_refuses_raw_input_bytes() {
    // Input reaches a window only as structured messages the daemon encodes.
    let result = decode_client_message(br#"{"type":"input","data_b64":"bHM="}"#);
    assert_eq!(result, Err(MessageError::UnknownType));
}

#[test]
fn daemon_message_attached_not_exited_matches_adr2_json_example_shape() {
    let msg = DaemonMessage::Attached {
        child_exited: false,
        exit_code: None,
    };
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    let expected = serde_json::json!({
        "type": "attached",
        "child_exited": false,
        "exit_code": null,
    });
    assert_eq!(value, expected);
}

#[test]
fn daemon_message_attached_exited_matches_adr2_json_example_shape() {
    let msg = DaemonMessage::Attached {
        child_exited: true,
        exit_code: Some(0),
    };
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    let expected = serde_json::json!({
        "type": "attached",
        "child_exited": true,
        "exit_code": 0,
    });
    assert_eq!(value, expected);
}

#[test]
fn daemon_message_error_matches_adr2_json_example_shape() {
    let msg = DaemonMessage::Error {
        code: "unknown_type".to_string(),
        message: "unknown message type".to_string(),
        rejected_argument: None,
    };
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    let expected = serde_json::json!({
        "type": "error",
        "code": "unknown_type",
        "message": "unknown message type",
    });
    assert_eq!(value, expected);
}

// ---------------------------------------------------------------------
// Malformed-frame table: non-JSON, missing required field, unknown type,
// direction-violating type, ignored unknown field
// ---------------------------------------------------------------------

#[test]
fn decode_client_message_rejects_non_json_body() {
    let result = decode_client_message(b"this is not json");
    assert_eq!(result, Err(MessageError::InvalidJson));
    assert_eq!(
        decode_daemon_message(b"this is not json"),
        Err(MessageError::InvalidJson)
    );
}

#[test]
fn decode_client_message_rejects_known_type_missing_required_field() {
    // "text" requires text.
    let result = decode_client_message(br#"{"type":"text"}"#);
    assert_eq!(result, Err(MessageError::InvalidJson));
}

#[test]
fn decode_client_message_rejects_unknown_type() {
    let result = decode_client_message(br#"{"type":"bogus"}"#);
    assert_eq!(result, Err(MessageError::UnknownType));
}

#[test]
fn decode_client_message_treats_direction_violating_type_as_unknown_type() {
    // "frame" is daemon-to-client only, so the one-enum-per-direction design
    // makes it indistinguishable from an unknown type here.
    let result = decode_client_message(br#"{"type":"frame","seq":1}"#);
    assert_eq!(result, Err(MessageError::UnknownType));
}

#[test]
fn decode_daemon_message_treats_direction_violating_type_as_unknown_type() {
    // "text" is client-to-daemon only, unknown from the daemon enum's side.
    let result = decode_daemon_message(br#"{"type":"text","text":"hello"}"#);
    assert_eq!(result, Err(MessageError::UnknownType));
}

#[test]
fn decode_client_message_ignores_unknown_fields() {
    let result = decode_client_message(br#"{"type":"ping","unexpected_field":123}"#)
        .expect("unknown fields must be ignored for forward compatibility");
    assert_eq!(result, ClientMessage::Ping);
}
