#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Round-trip coverage for the chat wire types, complementing
//! `chat_protocol.rs`'s single-representative cases: every
//! `ChatMessage::from_kind` value and every documented
//! `ChatDeliveryInfo::state` value, several mention multiplicities, and each
//! value of the `Option` fields of `ChatMessages` and `ApprovalRequest`.
//!
//! Each field serializes on its own, so every value of every axis appears in
//! some case rather than in every combination. Each case is round-tripped
//! (encode then decode) and checked for exact equality against the original
//! value, so a value is never silently normalized or truncated on the way
//! through the wire.

use yagura_protocol::{
    ApprovalRequest, ApprovalRequester, ApprovalRequesterKind, ApprovalTarget, CHAT_BODY_MAX_BYTES,
    ChatBodyError, ChatDeliveryInfo, ChatHalt, ChatMessage, ChatParticipant, DaemonMessage,
    RoomRef, WindowStatus, decode_daemon_message, encode_daemon_message, validate_chat_body,
};

/// Every `from_kind` value [`ChatMessage`] documents.
const FROM_KINDS: [&str; 4] = ["window", "user", "external", "system"];

/// Every delivery state a chat message can carry on the wire.
const DELIVERY_STATES: [&str; 6] = [
    "pending",
    "taken",
    "undelivered_unknown",
    "undelivered_halted",
    "undelivered_exhausted",
    "undelivered_restart",
];

/// Builds one [`ChatMessage`] for `from_kind` × `state`, with
/// `from_window_id` present only when `from_kind == "window"` (the field's
/// documented Option discipline) and a delivery list of length 1 carrying
/// `state`.
fn message_for(from_kind: &str, state: &str, mentions: Vec<String>) -> ChatMessage {
    ChatMessage {
        seq: 1,
        at_ms: 1_700_000_000_000,
        from_kind: from_kind.to_string(),
        from_window_id: if from_kind == "window" {
            Some("w1-abc".to_string())
        } else {
            None
        },
        from_name: format!("{from_kind}-name"),
        body: format!("body from {from_kind}"),
        mentions,
        broadcast: false,
        deliveries: vec![ChatDeliveryInfo {
            window_id: "w2-abc".to_string(),
            state: state.to_string(),
            unknown_reason: None,
        }],
    }
}

fn round_trip_via_chat_messages(message: ChatMessage) -> ChatMessage {
    let decoded = DaemonMessage::ChatMessages {
        room_id: "main".to_string(),
        messages: vec![message.clone()],
        next_after_seq: message.seq,
        halted: None,
        needs_user: false,
        roster: vec![],
    };
    let encoded = encode_daemon_message(&decoded);
    match decode_daemon_message(&encoded).expect("chat_messages must round trip") {
        DaemonMessage::ChatMessages { mut messages, .. } => {
            assert_eq!(messages.len(), 1);
            messages.remove(0)
        }
        other => panic!("expected chat_messages, got {other:?}"),
    }
}

// =============================================================================
// Every from_kind value and every delivery state
// =============================================================================

#[test]
fn every_from_kind_and_every_delivery_state_round_trips_exactly() {
    for (index, &state) in DELIVERY_STATES.iter().enumerate() {
        let from_kind = FROM_KINDS[index % FROM_KINDS.len()];
        let original = message_for(from_kind, state, vec!["user".to_string()]);
        let round_tripped = round_trip_via_chat_messages(original.clone());
        assert_eq!(
            round_tripped, original,
            "from_kind={from_kind} state={state} must round trip exactly"
        );
    }
}

#[test]
fn from_window_id_is_some_only_for_the_window_kind_after_a_round_trip() {
    for &from_kind in &FROM_KINDS {
        let original = message_for(from_kind, "taken", vec![]);
        let round_tripped = round_trip_via_chat_messages(original);
        if from_kind == "window" {
            assert!(
                round_tripped.from_window_id.is_some(),
                "from_kind=window must carry from_window_id"
            );
        } else {
            assert_eq!(
                round_tripped.from_window_id, None,
                "from_kind={from_kind} must not carry from_window_id"
            );
        }
    }
}

// =============================================================================
// Mention multiplicity: none, one, and several including a literal duplicate
// (the protocol type carries whatever list it is given, verbatim -- dedup is
// room::parse_mentions's job, not the wire type's).
// =============================================================================

#[test]
fn mentions_of_every_multiplicity_round_trip_verbatim_including_a_duplicate() {
    let cases: Vec<Vec<String>> = vec![
        vec![],
        vec!["user".to_string()],
        vec![
            "w2-abc".to_string(),
            "w3-abc".to_string(),
            "w2-abc".to_string(),
        ],
    ];
    for mentions in cases {
        let original = message_for("window", "taken", mentions.clone());
        let round_tripped = round_trip_via_chat_messages(original);
        assert_eq!(
            round_tripped.mentions, mentions,
            "mentions list must round trip verbatim (including duplicates), got {:?}",
            round_tripped.mentions
        );
    }
}

// =============================================================================
// ChatMessages' own fields: halted absent and present, roster empty and full,
// needs_user both values.
// =============================================================================

#[test]
fn chat_messages_every_option_field_value_round_trips() {
    let cases = [
        (None, vec![], false),
        (
            Some(ChatHalt {
                reason: "turn_budget".to_string(),
                since_ms: 42,
            }),
            vec![ChatParticipant {
                window_id: "w1-abc".to_string(),
                name: "agent A".to_string(),
            }],
            true,
        ),
    ];
    for (halted, roster, needs_user) in cases {
        let decoded = DaemonMessage::ChatMessages {
            room_id: "main".to_string(),
            messages: vec![],
            next_after_seq: 0,
            halted,
            needs_user,
            roster,
        };
        let encoded = encode_daemon_message(&decoded);
        assert_eq!(
            decode_daemon_message(&encoded).expect("round trip must succeed"),
            decoded
        );
    }
}

// =============================================================================
// ApprovalRequest: each Option field absent and present, both requester kinds.
// =============================================================================

fn window_requester() -> ApprovalRequester {
    ApprovalRequester {
        kind: ApprovalRequesterKind::Window,
        window_id: Some("w1-abc".to_string()),
        name: Some("agent A".to_string()),
    }
}

fn external_requester() -> ApprovalRequester {
    ApprovalRequester {
        kind: ApprovalRequesterKind::External,
        window_id: None,
        name: None,
    }
}

fn sample_target() -> ApprovalTarget {
    ApprovalTarget {
        window_id: "w2-abc".to_string(),
        name: "agent B".to_string(),
        status: WindowStatus::Idle,
    }
}

fn sample_room() -> RoomRef {
    RoomRef {
        room_id: "r-0123456789abcdef".to_string(),
        name: "yagura".to_string(),
    }
}

// =============================================================================
// 768-byte boundary crossed by a multi-byte character rather than an ASCII
// one: validate_chat_body must reject on total byte length without ever
// slicing mid-codepoint (which would panic in `char_indices`/byte-index
// logic), and must accept a body that lands exactly on the cap even when the
// last character is multi-byte.
// =============================================================================

#[test]
fn validate_chat_body_accepts_the_byte_cap_when_the_last_character_is_multi_byte() {
    // "あ" is 3 bytes in UTF-8.
    let ascii_len = CHAT_BODY_MAX_BYTES - "あ".len();
    let body = "x".repeat(ascii_len) + "あ";
    assert_eq!(body.len(), CHAT_BODY_MAX_BYTES);
    assert_eq!(validate_chat_body(&body), Ok(()));
}

#[test]
fn validate_chat_body_rejects_a_multi_byte_character_that_pushes_past_the_cap() {
    // One ASCII byte more than the exact-fit case above pushes the trailing
    // "あ" (3 bytes) one byte past the cap, without ever landing exactly on
    // a codepoint boundary at the cap itself.
    let ascii_len = CHAT_BODY_MAX_BYTES - "あ".len() + 1;
    let body = "x".repeat(ascii_len) + "あ";
    assert!(body.len() > CHAT_BODY_MAX_BYTES);
    let result = validate_chat_body(&body);
    assert!(
        matches!(result, Err(ChatBodyError::TooLong { .. })),
        "a body whose multi-byte tail pushes past the cap must be rejected, got {result:?}"
    );
}

#[test]
fn approval_request_option_keys_appear_exactly_when_some_and_round_trip() {
    // One case with every Option absent, one with every Option present; the
    // requester kind alternates so both kinds round trip.
    let cases = [
        (external_requester(), None, None, None),
        (
            window_requester(),
            Some(sample_target()),
            Some("[yagura chat] hello".to_string()),
            Some(sample_room()),
        ),
    ];
    for (requester, target, text, room) in cases {
        let request = ApprovalRequest {
            request_id: "a1-abc".to_string(),
            tool: "send_chat".to_string(),
            requester,
            target,
            text,
            room,
        };
        let value =
            serde_json::to_value(&request).expect("an ApprovalRequest must serialize to JSON");
        let object = value
            .as_object()
            .expect("an ApprovalRequest must serialize as a JSON object");
        assert_eq!(
            object.contains_key("target"),
            request.target.is_some(),
            "target key presence must match Option state, got {value}"
        );
        assert_eq!(
            object.contains_key("text"),
            request.text.is_some(),
            "text key presence must match Option state, got {value}"
        );
        assert_eq!(
            object.contains_key("room"),
            request.room.is_some(),
            "room key presence must match Option state, got {value}"
        );
        let round_tripped: ApprovalRequest =
            serde_json::from_value(value.clone()).expect("an ApprovalRequest must round trip");
        assert_eq!(round_tripped, request, "round trip mismatch for {value}");
    }
}
