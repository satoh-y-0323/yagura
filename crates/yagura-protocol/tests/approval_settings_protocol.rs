#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the three approval-settings requests
//! (`get_approval_settings`, `set_auto_approve`, `set_plan_approval_required`)
//! and their answers, plus the `ApprovalSettingsInfo` / `ToolAutoApprove` /
//! `PlaybookState` types they carry.
//!
//! - The three requests and their three answers round trip (the frozen shape
//!   in `wire_inventory.rs` pins each request's key set).
//! - `set_auto_approve`'s `auto_approve` and `set_plan_approval_required`'s
//!   `required` must be a JSON boolean and must be present: a missing or
//!   mistyped value fails the whole frame rather than being filled in with a
//!   default.
//! - `ApprovalSettingsInfo.playbook` carries a `#[serde(other)]` catch-all
//!   (`PlaybookState::Unknown`): an unrecognized string decodes to it rather
//!   than failing the frame. `ToolAutoApprove.tool` is a plain string, so an
//!   unrecognized tool name decodes unchanged.
//! - The three requests require `APPROVAL_SETTINGS_MIN_REVISION` (also see
//!   `required_revision.rs`'s per-variant corpus).

use yagura_protocol::{
    APPROVAL_SETTINGS_MIN_REVISION, ApprovalSettingsInfo, ClientMessage, DaemonMessage,
    MessageError, PlaybookState, ToolAutoApprove, decode_client_message, decode_daemon_message,
    encode_client_message, encode_daemon_message, required_protocol_revision,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a wire type must serialize")
}

fn assert_wire_tag(value: &serde_json::Value, expected: &str) {
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some(expected),
        "the wire tag must be snake_case, got: {value}"
    );
}

fn to_bytes(value: &serde_json::Value) -> Vec<u8> {
    serde_json::to_vec(value).expect("json must encode")
}

fn sample_settings() -> ApprovalSettingsInfo {
    ApprovalSettingsInfo {
        playbook: PlaybookState::Loaded,
        tools: vec![
            ToolAutoApprove {
                tool: "send_message".to_string(),
                auto_approve: true,
            },
            ToolAutoApprove {
                tool: "close_window".to_string(),
                auto_approve: false,
            },
        ],
        plans_require_approval: true,
    }
}

// =============================================================================
// The three requests and three answers round trip.
// =============================================================================

#[test]
fn get_approval_settings_round_trips() {
    let decoded = ClientMessage::GetApprovalSettings {};
    assert_wire_tag(&as_json(&decoded), "get_approval_settings");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("get_approval_settings must round trip"),
        decoded
    );
}

#[test]
fn set_auto_approve_round_trips_for_both_values() {
    for auto_approve in [true, false] {
        let decoded = ClientMessage::SetAutoApprove {
            tool: "send_message".to_string(),
            auto_approve,
        };
        assert_wire_tag(&as_json(&decoded), "set_auto_approve");
        let encoded = encode_client_message(&decoded);
        assert_eq!(
            decode_client_message(&encoded).expect("set_auto_approve must round trip"),
            decoded
        );
    }
}

#[test]
fn set_plan_approval_required_round_trips_for_both_values() {
    for required in [true, false] {
        let decoded = ClientMessage::SetPlanApprovalRequired { required };
        assert_wire_tag(&as_json(&decoded), "set_plan_approval_required");
        let encoded = encode_client_message(&decoded);
        assert_eq!(
            decode_client_message(&encoded).expect("set_plan_approval_required must round trip"),
            decoded
        );
    }
}

#[test]
fn approval_settings_round_trips() {
    let decoded = DaemonMessage::ApprovalSettings {
        settings: sample_settings(),
    };
    assert_wire_tag(&as_json(&decoded), "approval_settings");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("approval_settings must round trip"),
        decoded
    );
}

#[test]
fn auto_approve_set_round_trips() {
    let decoded = DaemonMessage::AutoApproveSet {
        settings: sample_settings(),
    };
    assert_wire_tag(&as_json(&decoded), "auto_approve_set");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("auto_approve_set must round trip"),
        decoded
    );
}

#[test]
fn plan_approval_required_set_round_trips() {
    let decoded = DaemonMessage::PlanApprovalRequiredSet {
        settings: sample_settings(),
    };
    assert_wire_tag(&as_json(&decoded), "plan_approval_required_set");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("plan_approval_required_set must round trip"),
        decoded
    );
}

// =============================================================================
// `auto_approve` / `required` must be a JSON boolean and must be present.
// =============================================================================

fn set_auto_approve_frame(auto_approve: Option<serde_json::Value>) -> Vec<u8> {
    let mut body = serde_json::json!({
        "type": "set_auto_approve",
        "tool": "send_message",
    });
    if let Some(auto_approve) = auto_approve {
        body.as_object_mut()
            .expect("the frame is an object")
            .insert("auto_approve".to_string(), auto_approve);
    }
    to_bytes(&body)
}

fn set_plan_approval_required_frame(required: Option<serde_json::Value>) -> Vec<u8> {
    let mut body = serde_json::json!({
        "type": "set_plan_approval_required",
    });
    if let Some(required) = required {
        body.as_object_mut()
            .expect("the frame is an object")
            .insert("required".to_string(), required);
    }
    to_bytes(&body)
}

#[test]
fn a_well_formed_set_auto_approve_frame_decodes() {
    let decoded = decode_client_message(&set_auto_approve_frame(Some(serde_json::json!(true))))
        .expect("a set_auto_approve frame with a boolean auto_approve must decode");
    assert_eq!(
        decoded,
        ClientMessage::SetAutoApprove {
            tool: "send_message".to_string(),
            auto_approve: true,
        }
    );
}

#[test]
fn set_auto_approve_with_a_non_boolean_auto_approve_fails_to_decode() {
    for auto_approve in [
        serde_json::json!("true"),
        serde_json::json!(0),
        serde_json::json!(1),
        serde_json::Value::Null,
    ] {
        let result = decode_client_message(&set_auto_approve_frame(Some(auto_approve.clone())));
        assert_eq!(
            result,
            Err(MessageError::InvalidJson),
            "a set_auto_approve carrying auto_approve = {auto_approve} must fail the frame's \
             decode"
        );
    }
}

#[test]
fn set_auto_approve_without_auto_approve_fails_to_decode() {
    let result = decode_client_message(&set_auto_approve_frame(None));
    assert_eq!(
        result,
        Err(MessageError::InvalidJson),
        "a set_auto_approve missing auto_approve must fail rather than default the caller's intent"
    );
}

#[test]
fn a_well_formed_set_plan_approval_required_frame_decodes() {
    let decoded = decode_client_message(&set_plan_approval_required_frame(Some(
        serde_json::json!(false),
    )))
    .expect("a set_plan_approval_required frame with a boolean required must decode");
    assert_eq!(
        decoded,
        ClientMessage::SetPlanApprovalRequired { required: false }
    );
}

#[test]
fn set_plan_approval_required_with_a_non_boolean_required_fails_to_decode() {
    for required in [
        serde_json::json!("false"),
        serde_json::json!(0),
        serde_json::Value::Null,
    ] {
        let result =
            decode_client_message(&set_plan_approval_required_frame(Some(required.clone())));
        assert_eq!(
            result,
            Err(MessageError::InvalidJson),
            "a set_plan_approval_required carrying required = {required} must fail the frame's \
             decode"
        );
    }
}

#[test]
fn set_plan_approval_required_without_required_fails_to_decode() {
    let result = decode_client_message(&set_plan_approval_required_frame(None));
    assert_eq!(
        result,
        Err(MessageError::InvalidJson),
        "a set_plan_approval_required missing required must fail rather than default the \
         caller's intent"
    );
}

// =============================================================================
// An unrecognized `playbook` value decodes to `PlaybookState::Unknown`
// without failing the frame; an unrecognized `tools[].tool` decodes unchanged.
// =============================================================================

fn approval_settings_frame(playbook: &str, tool: &str) -> Vec<u8> {
    to_bytes(&serde_json::json!({
        "type": "approval_settings",
        "settings": {
            "playbook": playbook,
            "tools": [{ "tool": tool, "auto_approve": true }],
            "plans_require_approval": true,
        },
    }))
}

fn decoded_settings(frame: &[u8]) -> ApprovalSettingsInfo {
    match decode_daemon_message(frame).expect("the approval_settings frame must decode") {
        DaemonMessage::ApprovalSettings { settings } => settings,
        other => panic!("expected DaemonMessage::ApprovalSettings, got {other:?}"),
    }
}

#[test]
fn an_unrecognized_playbook_value_decodes_as_unknown_without_failing_the_frame() {
    let settings = decoded_settings(&approval_settings_frame("reloading", "send_message"));
    assert_eq!(
        settings.playbook,
        PlaybookState::Unknown,
        "an unrecognized playbook string must decode to PlaybookState::Unknown rather than \
         failing the whole frame"
    );
}

#[test]
fn each_documented_playbook_value_decodes_to_its_own_variant() {
    for (wire, expected) in [
        ("loaded", PlaybookState::Loaded),
        ("not_configured", PlaybookState::NotConfigured),
        ("failed", PlaybookState::Failed),
    ] {
        let settings = decoded_settings(&approval_settings_frame(wire, "send_message"));
        assert_eq!(
            settings.playbook, expected,
            "the documented playbook value {wire:?} must decode to {expected:?}"
        );
    }
}

#[test]
fn an_unrecognized_tool_name_decodes_unchanged() {
    let settings = decoded_settings(&approval_settings_frame("loaded", "a_future_tool"));
    assert_eq!(
        settings.tools.len(),
        1,
        "the frame carries exactly one tool"
    );
    assert_eq!(
        settings.tools[0].tool, "a_future_tool",
        "an unrecognized tool name must be carried as a plain string rather than rejected"
    );
}

// =============================================================================
// Revision (also see required_revision.rs)
// =============================================================================

#[test]
fn the_three_requests_require_the_approval_settings_threshold() {
    let messages = [
        ClientMessage::GetApprovalSettings {},
        ClientMessage::SetAutoApprove {
            tool: "send_message".to_string(),
            auto_approve: true,
        },
        ClientMessage::SetPlanApprovalRequired { required: true },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            APPROVAL_SETTINGS_MIN_REVISION,
            "{msg:?} must require the approval-settings threshold"
        );
    }
}
