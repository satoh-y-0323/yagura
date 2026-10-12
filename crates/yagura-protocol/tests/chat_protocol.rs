#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the chat room: the `send_chat` /
//! `broadcast_chat` / `read_chat` client messages, the `chat_posted` /
//! `chat_messages` / `chat_changed` daemon messages, the shared
//! `ChatMessage` / `ChatDeliveryInfo` / `ChatHalt` / `ChatParticipant` payload
//! types, `validate_chat_body` and `CHAT_READ_LIMIT_MAX`.
//!
//! Also covers the direction-neutral fallout of making
//! [`ApprovalRequest::target`] optional: a request with no `target` key must
//! decode (`chat_protocol_matrix.rs` measures that encoding `None` omits the
//! key rather than writing `null`).

use yagura_protocol::{
    ApprovalRequest, CHAT_BODY_MAX_BYTES, CHAT_READ_LIMIT_MAX, ChatBodyError, ChatDeliveryInfo,
    ChatHalt, ChatMessage, ChatParticipant, ChatRoomInfo, ClientMessage, DaemonMessage,
    MessageError, decode_client_message, decode_daemon_message, encode_client_message,
    encode_daemon_message, validate_chat_body,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("the chat payload types must serialize")
}

// =============================================================================
// ClientMessage::SendChat / BroadcastChat
// =============================================================================

#[test]
fn client_message_send_chat_round_trips_with_an_explicit_room_id() {
    let body = br#"{"type":"send_chat","room_id":"main","body":"hello everyone"}"#;
    let decoded = decode_client_message(body).expect("send_chat must decode");
    assert_eq!(
        decoded,
        ClientMessage::SendChat {
            room_id: Some("main".to_string()),
            body: "hello everyone".to_string(),
        }
    );

    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("send_chat"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_send_chat_room_id_may_be_omitted() {
    let body = br#"{"type":"send_chat","body":"hi"}"#;
    let decoded = decode_client_message(body).expect("send_chat without room_id must decode");
    assert_eq!(
        decoded,
        ClientMessage::SendChat {
            room_id: None,
            body: "hi".to_string(),
        },
        "an omitted room_id must decode to None rather than failing the frame"
    );
}

#[test]
fn client_message_broadcast_chat_round_trips() {
    let body = br#"{"type":"broadcast_chat","room_id":"main","body":"status update"}"#;
    let decoded = decode_client_message(body).expect("broadcast_chat must decode");
    assert_eq!(
        decoded,
        ClientMessage::BroadcastChat {
            room_id: Some("main".to_string()),
            body: "status update".to_string(),
        }
    );

    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("broadcast_chat"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// ClientMessage::ReadChat
// =============================================================================

#[test]
fn client_message_read_chat_round_trips_with_every_field() {
    let body = br#"{"type":"read_chat","room_id":"main","after_seq":41,"limit":10}"#;
    let decoded = decode_client_message(body).expect("read_chat must decode");
    assert_eq!(
        decoded,
        ClientMessage::ReadChat {
            room_id: Some("main".to_string()),
            after_seq: Some(41),
            limit: Some(10),
        }
    );

    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("read_chat"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_read_chat_every_field_may_be_omitted() {
    let body = br#"{"type":"read_chat"}"#;
    let decoded = decode_client_message(body)
        .expect("read_chat with no fields at all must decode (every field is optional)");
    assert_eq!(
        decoded,
        ClientMessage::ReadChat {
            room_id: None,
            after_seq: None,
            limit: None,
        }
    );
}

// =============================================================================
// DaemonMessage::ChatPosted
// =============================================================================

#[test]
fn daemon_message_chat_posted_round_trips() {
    let decoded = DaemonMessage::ChatPosted {
        room_id: "main".to_string(),
        seq: 7,
        targets: vec!["w1-abc".to_string(), "w2-abc".to_string()],
        user: false,
        unresolved: vec!["nobody".to_string()],
        halted: false,
    };
    let encoded = encode_daemon_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("chat_posted"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        value
            .get("targets")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(2),
        "targets must be carried as a plain JSON array, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// DaemonMessage::ChatMessages (roster + nested ChatMessage payloads)
// =============================================================================

fn sample_chat_message() -> ChatMessage {
    ChatMessage {
        seq: 3,
        at_ms: 1_700_000_000_000,
        from_kind: "window".to_string(),
        from_window_id: Some("w1-abc".to_string()),
        from_name: "agent A".to_string(),
        body: "hello @user".to_string(),
        mentions: vec!["user".to_string()],
        broadcast: false,
        deliveries: vec![ChatDeliveryInfo {
            window_id: "w2-abc".to_string(),
            state: "confirmed".to_string(),
            unknown_reason: None,
        }],
    }
}

#[test]
fn daemon_message_chat_messages_round_trips_with_roster() {
    let decoded = DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: vec![sample_chat_message()],
        next_after_seq: 3,
        halted: Some(ChatHalt {
            reason: "monologue".to_string(),
            since_ms: 1_700_000_000_500,
        }),
        needs_user: true,
        roster: vec![ChatParticipant {
            window_id: "w2-abc".to_string(),
            name: "agent B".to_string(),
        }],
    };

    let encoded = encode_daemon_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("chat_messages"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        value
            .pointer("/messages/0/from_kind")
            .and_then(serde_json::Value::as_str),
        Some("window"),
        "each message must carry from_kind verbatim, got: {value}"
    );
    assert_eq!(
        value
            .pointer("/messages/0/deliveries/0/window_id")
            .and_then(serde_json::Value::as_str),
        Some("w2-abc"),
        "deliveries must nest ChatDeliveryInfo objects, got: {value}"
    );
    assert_eq!(
        value
            .pointer("/roster/0/window_id")
            .and_then(serde_json::Value::as_str),
        Some("w2-abc"),
        "roster must carry ChatParticipant objects, got: {value}"
    );
    assert_eq!(
        value
            .pointer("/halted/reason")
            .and_then(serde_json::Value::as_str),
        Some("monologue"),
        "halted must nest a ChatHalt object when present, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn daemon_message_chat_messages_carries_no_halt_when_the_room_is_not_halted() {
    let decoded = DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: vec![],
        next_after_seq: 0,
        halted: None,
        needs_user: false,
        roster: vec![],
    };
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// DaemonMessage::ChatChanged
// =============================================================================

#[test]
fn daemon_message_chat_changed_round_trips() {
    let decoded = DaemonMessage::ChatChanged {
        room_id: "main".to_string(),
        seq: 9,
        needs_user: true,
    };
    let encoded = encode_daemon_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("chat_changed"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// Direction: each new chat type belongs to exactly one enum, same discipline
// as the existing approval types (approval_protocol.rs).
// =============================================================================

#[test]
fn a_daemon_only_chat_type_is_unknown_to_the_client_decoder() {
    let body = br#"{"type":"chat_posted","room_id":"main","seq":1,"targets":[],"user":false,"unresolved":[],"halted":false}"#;
    let result = decode_client_message(body);
    assert!(
        matches!(result, Err(MessageError::UnknownType)),
        "chat_posted must be unknown to the client decoder, got {result:?}"
    );
}

#[test]
fn a_client_only_chat_type_is_unknown_to_the_daemon_decoder() {
    let body = br#"{"type":"send_chat","room_id":"main","body":"hi"}"#;
    let result = decode_daemon_message(body);
    assert!(
        matches!(result, Err(MessageError::UnknownType)),
        "send_chat must be unknown to the daemon decoder, got {result:?}"
    );
}

// =============================================================================
// validate_chat_body / CHAT_BODY_MAX_BYTES / CHAT_READ_LIMIT_MAX
// =============================================================================

#[test]
fn chat_body_max_bytes_is_768() {
    assert_eq!(CHAT_BODY_MAX_BYTES, 768);
}

#[test]
fn chat_read_limit_max_is_200() {
    assert_eq!(CHAT_READ_LIMIT_MAX, 200);
}

#[test]
fn validate_chat_body_rejects_an_empty_body() {
    assert_eq!(validate_chat_body(""), Err(ChatBodyError::Empty));
}

#[test]
fn validate_chat_body_rejects_a_body_with_no_visible_text() {
    for blank in [
        " ",
        "   ",
        "\n",
        " \r\n ",
        "\u{3000}",
        "\u{00A0}",
        "\u{200D}",
        " \u{200C} ",
    ] {
        assert_eq!(
            validate_chat_body(blank),
            Err(ChatBodyError::Empty),
            "a body of only {blank:?} carries nothing to read"
        );
    }
}

#[test]
fn validate_chat_body_keeps_whitespace_and_line_breaks_inside_visible_text() {
    for body in [
        " ok ",
        "line one\nline two",
        "\u{3000}はい",
        "\u{1F468}\u{200D}\u{1F4BB}",
    ] {
        assert_eq!(
            validate_chat_body(body),
            Ok(()),
            "{body:?} has visible text"
        );
    }
}

#[test]
fn validate_chat_body_accepts_exactly_the_byte_budget() {
    let body = "a".repeat(CHAT_BODY_MAX_BYTES);
    assert_eq!(validate_chat_body(&body), Ok(()));
}

#[test]
fn validate_chat_body_rejects_one_byte_past_the_budget() {
    let body = "a".repeat(CHAT_BODY_MAX_BYTES + 1);
    let result = validate_chat_body(&body);
    assert!(
        matches!(result, Err(ChatBodyError::TooLong { .. })),
        "a body one byte past the budget must be rejected, got {result:?}"
    );
}

#[test]
fn validate_chat_body_allows_newline_and_carriage_return() {
    assert_eq!(validate_chat_body("line one\nline two\r"), Ok(()));
}

#[test]
fn validate_chat_body_rejects_a_c0_control_character() {
    let result = validate_chat_body("\x1b[2Jerased");
    assert!(
        matches!(result, Err(ChatBodyError::ForbiddenChar { at: 0, .. })),
        "an ESC control character must be rejected, got {result:?}"
    );
}

#[test]
fn validate_chat_body_rejects_a_c1_control_character() {
    let result = validate_chat_body("before\u{0085}after");
    assert!(
        matches!(result, Err(ChatBodyError::ForbiddenChar { .. })),
        "a C1 control character must be rejected, got {result:?}"
    );
}

#[test]
fn validate_chat_body_rejects_del() {
    let result = validate_chat_body("before\u{007F}after");
    assert!(
        matches!(result, Err(ChatBodyError::ForbiddenChar { .. })),
        "DEL must be rejected, got {result:?}"
    );
}

// =============================================================================
// ApprovalRequest's optional keys decode from a request that omits them.
// chat_protocol_matrix.rs measures that encoding omits them (never null).
// =============================================================================

#[test]
fn approval_request_decodes_with_no_target_key_at_all() {
    let body = br#"{"request_id":"a1-abc","tool":"send_chat","requester":{"kind":"external"}}"#;
    let decoded: ApprovalRequest =
        serde_json::from_slice(body).expect("a target-less approval request must decode");
    assert_eq!(decoded.target, None);
    assert_eq!(decoded.text, None);
    assert_eq!(
        decoded.room, None,
        "a request with no room key at all must decode to None, same          discipline as target"
    );
}

// =============================================================================
// ClientMessage::ListChatRooms / DaemonMessage::ChatRooms / ChatRoomInfo
// =============================================================================

#[test]
fn client_message_list_chat_rooms_round_trips() {
    let body = br#"{"type":"list_chat_rooms"}"#;
    let decoded = decode_client_message(body).expect("list_chat_rooms must decode");
    assert_eq!(decoded, ClientMessage::ListChatRooms);

    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("list_chat_rooms"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

fn sample_chat_room_info(halted: Option<ChatHalt>) -> ChatRoomInfo {
    ChatRoomInfo {
        room_id: "r-0123456789abcdef".to_string(),
        name: "yagura".to_string(),
        participant_count: 2,
        needs_user: false,
        halted,
        last_at_ms: Some(1_700_000_000_000),
    }
}

#[test]
fn daemon_message_chat_rooms_round_trips_with_multiple_rooms() {
    let decoded = DaemonMessage::ChatRooms {
        rooms: vec![
            ChatRoomInfo {
                room_id: "main".to_string(),
                name: "main".to_string(),
                participant_count: 3,
                needs_user: true,
                halted: None,
                last_at_ms: None,
            },
            sample_chat_room_info(Some(ChatHalt {
                reason: "monologue".to_string(),
                since_ms: 1_700_000_000_500,
            })),
        ],
    };
    let encoded = encode_daemon_message(&decoded);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("chat_rooms"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        value
            .pointer("/rooms/0/room_id")
            .and_then(serde_json::Value::as_str),
        Some("main")
    );
    assert_eq!(
        value
            .pointer("/rooms/1/halted/reason")
            .and_then(serde_json::Value::as_str),
        Some("monologue"),
        "halted must nest a ChatHalt object when present, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn chat_room_info_carries_a_null_halted_and_last_at_ms_rather_than_omitting_the_keys() {
    let decoded = sample_chat_room_info(None);
    let value = as_json(&DaemonMessage::ChatRooms {
        rooms: vec![ChatRoomInfo {
            last_at_ms: None,
            ..decoded
        }],
    });
    let room = value
        .pointer("/rooms/0")
        .and_then(serde_json::Value::as_object)
        .expect("rooms/0 must be an object");
    assert!(
        room.contains_key("halted"),
        "halted must be present as null, not omitted, when absent: {value}"
    );
    assert_eq!(room.get("halted"), Some(&serde_json::Value::Null));
    assert!(
        room.contains_key("last_at_ms"),
        "last_at_ms must be present as null, not omitted, when absent: {value}"
    );
    assert_eq!(room.get("last_at_ms"), Some(&serde_json::Value::Null));
}

#[test]
fn a_daemon_only_room_listing_type_is_unknown_to_the_client_decoder() {
    let body = br#"{"type":"chat_rooms","rooms":[]}"#;
    let result = decode_client_message(body);
    assert!(
        matches!(result, Err(MessageError::UnknownType)),
        "chat_rooms must be unknown to the client decoder, got {result:?}"
    );
}

#[test]
fn a_client_only_room_listing_type_is_unknown_to_the_daemon_decoder() {
    let body = br#"{"type":"list_chat_rooms"}"#;
    let result = decode_daemon_message(body);
    assert!(
        matches!(result, Err(MessageError::UnknownType)),
        "list_chat_rooms must be unknown to the daemon decoder, got {result:?}"
    );
}
