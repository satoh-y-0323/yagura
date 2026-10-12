#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the approval flow: the `identify` /
//! `register_approver` / `approval_decision` client messages, the `identified`
//! / `approver_registered` / `approval_requested` / `approval_withdrawn` /
//! `approval_resolved` daemon messages, and the shared `ApprovalRequest` /
//! `ApprovalRequester` / `ApprovalTarget` payload types.
//!
//! The properties pinned here are the ones both sides of the wire depend on:
//! the `type` tag is `snake_case`, `requester.kind` and `reason` are `serde`
//! enums with an `other` fallback (so a future value the running build does
//! not know about still decodes, as a forward-compatibility guarantee rather
//! than a schema change), `text` and `name` may be omitted, and no expiry is
//! carried in the payload (the deadline is the daemon's, and a wire-side
//! `expires_in_ms` would go stale the moment it is queued).

use yagura_protocol::{
    ApprovalRequest, ApprovalRequester, ApprovalRequesterKind, ApprovalTarget, ClientMessage,
    DaemonMessage, MessageError, WindowStatus, WithdrawReason, decode_client_message,
    decode_daemon_message, encode_client_message, encode_daemon_message,
};

/// A requester that is a window: `kind = window` with both optional fields
/// present, which is what the daemon always sends for a window-originated
/// request.
fn window_requester() -> ApprovalRequester {
    ApprovalRequester {
        kind: ApprovalRequesterKind::Window,
        window_id: Some("w1-abc".to_string()),
        name: Some("agent A".to_string()),
    }
}

/// A requester that is not a window (an MCP or CLI client that never
/// identified): `kind = external` with both optional fields absent.
fn external_requester() -> ApprovalRequester {
    ApprovalRequester {
        kind: ApprovalRequesterKind::External,
        window_id: None,
        name: None,
    }
}

fn target() -> ApprovalTarget {
    ApprovalTarget {
        window_id: "w2-abc".to_string(),
        name: "agent B".to_string(),
        status: WindowStatus::Idle,
    }
}

/// A `close_window` request from a window: no `text`.
fn close_request() -> ApprovalRequest {
    ApprovalRequest {
        request_id: "a1-abc".to_string(),
        tool: "close_window".to_string(),
        requester: window_requester(),
        target: Some(target()),
        text: None,
        // close_window is not addressed to a chat room.
        room: None,
    }
}

/// A `send_message` request from an external caller: `text` present.
fn send_request() -> ApprovalRequest {
    ApprovalRequest {
        request_id: "a2-abc".to_string(),
        tool: "send_message".to_string(),
        requester: external_requester(),
        target: Some(target()),
        text: Some("please run the tests".to_string()),
        // send_message is not addressed to a chat room.
        room: None,
    }
}

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("the approval payload types must serialize")
}

// =============================================================================
// ClientMessage::Identify
// =============================================================================

#[test]
fn client_message_identify_round_trips() {
    let body =
        br#"{"type":"identify","window_id":"w1-abc","token":"0123456789abcdef0123456789abcdef"}"#;
    let decoded = decode_client_message(body).expect("identify must decode");
    assert_eq!(
        decoded,
        ClientMessage::Identify {
            window_id: "w1-abc".to_string(),
            token: "0123456789abcdef0123456789abcdef".to_string(),
        }
    );

    let encoded = encode_client_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("identify"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_identify_without_a_token_is_invalid_json() {
    // The token is required: the pre-token shape must never decode into an
    // unauthenticated identify (fail-closed, same rule as status_event).
    let body = br#"{"type":"identify","window_id":"w1-abc"}"#;
    let result = decode_client_message(body);
    assert!(
        matches!(result, Err(MessageError::InvalidJson)),
        "identify without a token must not decode, got {result:?}"
    );
}

#[test]
fn client_message_identify_without_a_window_id_is_invalid_json() {
    let body = br#"{"type":"identify","token":"0123456789abcdef0123456789abcdef"}"#;
    let result = decode_client_message(body);
    assert!(
        matches!(result, Err(MessageError::InvalidJson)),
        "identify without a window_id must not decode, got {result:?}"
    );
}

// =============================================================================
// ClientMessage::RegisterApprover
// =============================================================================

#[test]
fn client_message_register_approver_round_trips() {
    let body = br#"{"type":"register_approver"}"#;
    let decoded = decode_client_message(body).expect("register_approver must decode");
    assert_eq!(
        decoded,
        ClientMessage::RegisterApprover,
        "register_approver carries no fields: the registration is about the \
         connection, not about a window"
    );

    let encoded = encode_client_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("register_approver"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// ClientMessage::ApprovalDecision
// =============================================================================

#[test]
fn client_message_approval_decision_round_trips() {
    let body = br#"{"type":"approval_decision","request_id":"a1-abc","approved":true}"#;
    let decoded = decode_client_message(body).expect("approval_decision must decode");
    assert_eq!(
        decoded,
        ClientMessage::ApprovalDecision {
            request_id: "a1-abc".to_string(),
            approved: true,
        }
    );

    let encoded = encode_client_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("approval_decision"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_approval_decision_carries_a_denial() {
    let body = br#"{"type":"approval_decision","request_id":"a1-abc","approved":false}"#;
    let decoded = decode_client_message(body).expect("a denial must decode");
    assert_eq!(
        decoded,
        ClientMessage::ApprovalDecision {
            request_id: "a1-abc".to_string(),
            approved: false,
        },
        "approve and deny travel in the same message, distinguished only by the \
         boolean, so a client cannot deny by omission"
    );
}

#[test]
fn client_message_approval_decision_without_approved_is_invalid_json() {
    // No default: a decision that does not say which way it went must be
    // rejected rather than read as an approval.
    let body = br#"{"type":"approval_decision","request_id":"a1-abc"}"#;
    let result = decode_client_message(body);
    assert!(
        matches!(result, Err(MessageError::InvalidJson)),
        "approval_decision without `approved` must not decode, got {result:?}"
    );
}

// =============================================================================
// DaemonMessage::Identified / ApproverRegistered / ApprovalResolved
// =============================================================================

#[test]
fn daemon_message_identified_round_trips() {
    let body = br#"{"type":"identified","window_id":"w1-abc"}"#;
    let decoded = decode_daemon_message(body).expect("identified must decode");
    assert_eq!(
        decoded,
        DaemonMessage::Identified {
            window_id: "w1-abc".to_string(),
        }
    );

    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("identified"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn daemon_message_approver_registered_carries_an_empty_pending_list() {
    let body = br#"{"type":"approver_registered","pending":[]}"#;
    let decoded = decode_daemon_message(body).expect("approver_registered must decode");
    assert_eq!(
        decoded,
        DaemonMessage::ApproverRegistered { pending: vec![] },
        "an approver joining with nothing pending must still get the list, so the \
         GUI can clear its queue rather than guess"
    );
}

#[test]
fn daemon_message_approver_registered_round_trips_with_pending_requests() {
    let decoded = DaemonMessage::ApproverRegistered {
        pending: vec![close_request(), send_request()],
    };
    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("approver_registered"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        value
            .get("pending")
            .and_then(serde_json::Value::as_array)
            .map(Vec::len),
        Some(2),
        "the pending list must be carried as a JSON array of requests, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn daemon_message_approval_resolved_round_trips() {
    let body = br#"{"type":"approval_resolved","request_id":"a1-abc"}"#;
    let decoded = decode_daemon_message(body).expect("approval_resolved must decode");
    assert_eq!(
        decoded,
        DaemonMessage::ApprovalResolved {
            request_id: "a1-abc".to_string(),
        },
        "the reply to a decision names the request it settled, so an approver \
         with several dialogs open can match it up"
    );

    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("approval_resolved"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// DaemonMessage::ApprovalRequested / ApprovalWithdrawn
// =============================================================================

#[test]
fn daemon_message_approval_requested_nests_the_request() {
    let decoded = DaemonMessage::ApprovalRequested {
        request: close_request(),
    };
    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("approval_requested"),
        "the wire tag must be snake_case, got: {value}"
    );
    assert_eq!(
        value
            .pointer("/request/request_id")
            .and_then(serde_json::Value::as_str),
        Some("a1-abc"),
        "the request must travel as one nested object rather than flattened \
         fields, got: {value}"
    );
    assert_eq!(
        decode_daemon_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn daemon_message_approval_withdrawn_round_trips_every_reason() {
    // The three reasons a pending request ends without a decision, each
    // wire-tagged as its snake_case spelling.
    for (reason, wire) in [
        (WithdrawReason::Resolved, "resolved"),
        (WithdrawReason::Timeout, "timeout"),
        (
            WithdrawReason::RequesterDisconnected,
            "requester_disconnected",
        ),
    ] {
        let decoded = DaemonMessage::ApprovalWithdrawn {
            request_id: "a1-abc".to_string(),
            reason,
        };
        let encoded = encode_daemon_message(&decoded);
        let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
        assert_eq!(
            value.get("type").and_then(serde_json::Value::as_str),
            Some("approval_withdrawn"),
            "the wire tag must be snake_case, got: {value}"
        );
        assert_eq!(
            value.get("reason").and_then(serde_json::Value::as_str),
            Some(wire),
            "the withdrawal reason must be carried verbatim as its snake_case string, \
             got: {value}"
        );
        assert_eq!(
            decode_daemon_message(&encoded).expect("round trip must succeed"),
            decoded
        );
    }
}

#[test]
fn daemon_message_approval_withdrawn_falls_back_to_unknown_for_an_unrecognized_reason() {
    // A future daemon build may send a reason this build does not know about
    // yet; the frame must still decode (forward compatibility) rather than
    // be dropped as malformed.
    let body = br#"{"type":"approval_withdrawn","request_id":"a1-abc","reason":"a_future_reason"}"#;
    let decoded = decode_daemon_message(body)
        .expect("an unrecognized reason must still decode via #[serde(other)]");
    match decoded {
        DaemonMessage::ApprovalWithdrawn { reason, .. } => {
            assert_eq!(
                reason,
                WithdrawReason::Unknown,
                "an unrecognized reason string must fall back to Unknown"
            );
        }
        other => panic!("expected approval_withdrawn, got {other:?}"),
    }
}

// =============================================================================
// Direction: each new type belongs to exactly one enum
// =============================================================================

#[test]
fn a_daemon_only_approval_type_is_unknown_to_the_client_decoder() {
    let body = br#"{"type":"approval_requested","request":{}}"#;
    let result = decode_client_message(body);
    assert!(
        matches!(result, Err(MessageError::UnknownType)),
        "approval_requested is daemon-to-client only; the client decoder must \
         reject it as an unknown type, got {result:?}"
    );
}

#[test]
fn a_client_only_approval_type_is_unknown_to_the_daemon_decoder() {
    let body = br#"{"type":"approval_decision","request_id":"a1-abc","approved":true}"#;
    let result = decode_daemon_message(body);
    assert!(
        matches!(result, Err(MessageError::UnknownType)),
        "approval_decision is client-to-daemon only; the daemon decoder must \
         reject it as an unknown type, got {result:?}"
    );
}

// =============================================================================
// ApprovalRequest / ApprovalRequester / ApprovalTarget
// =============================================================================

#[test]
fn approval_requester_kind_falls_back_to_unknown_for_an_unrecognized_value() {
    // A requester kind this build does not know about must still decode
    // (forward compatibility) rather than fail the whole request.
    let requester: ApprovalRequester = serde_json::from_str(r#"{"kind":"a_future_kind"}"#)
        .expect("an unrecognized kind must still decode via #[serde(other)]");
    assert_eq!(
        requester.kind,
        ApprovalRequesterKind::Unknown,
        "an unrecognized kind string must fall back to Unknown"
    );
}

#[test]
fn a_window_requester_carries_its_window_id_and_name() {
    let value = as_json(&window_requester());
    assert_eq!(
        value.get("window_id").and_then(serde_json::Value::as_str),
        Some("w1-abc")
    );
    assert_eq!(
        value.get("name").and_then(serde_json::Value::as_str),
        Some("agent A"),
        "the approver is shown who is asking, so a window requester must carry \
         its display name"
    );
}

#[test]
fn an_external_requester_omits_the_name() {
    // `name` is optional on the wire: an external caller has none, and the key
    // must be absent rather than present as null.
    let value = as_json(&external_requester());
    assert!(
        value.get("name").is_none(),
        "an absent name must be omitted from the wire form, got: {value}"
    );
}

#[test]
fn a_requester_decodes_without_the_optional_fields() {
    let requester: ApprovalRequester =
        serde_json::from_str(r#"{"kind":"external"}"#).expect("kind alone must decode");
    assert_eq!(requester.window_id, None);
    assert_eq!(requester.name, None);
}

#[test]
fn approval_target_carries_the_status_snapshot_as_a_snake_case_string() {
    let value = as_json(&target());
    assert_eq!(
        value.get("status").and_then(serde_json::Value::as_str),
        Some("idle"),
        "the target status must use the same snake_case spelling as everywhere \
         else in the protocol, got: {value}"
    );
    assert_eq!(
        value.get("window_id").and_then(serde_json::Value::as_str),
        Some("w2-abc")
    );
    assert_eq!(
        value.get("name").and_then(serde_json::Value::as_str),
        Some("agent B"),
        "the approver is shown which window is affected, so the target name is \
         not optional"
    );
}
