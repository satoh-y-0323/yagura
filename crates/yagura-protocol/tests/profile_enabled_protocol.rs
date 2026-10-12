#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for switching a launch profile on and off: the
//! `set_profile_enabled` request, its `profile_enabled_set` answer, and the
//! `enabled` field every `ProfileInfo` carries.
//!
//! - The new request and answer round trip (the frozen shape in
//!   `wire_inventory.rs` pins the request's key set).
//! - The request's `enabled` must be a JSON boolean and must be present: a
//!   missing or mistyped value fails the whole frame rather than being filled
//!   in with a default.
//! - A `ProfileInfo` without `enabled` decodes as enabled (a peer that does not
//!   carry the field has no disabled state), `false` decodes as disabled, a
//!   mistyped value fails the frame, and encoding always writes the key.

use yagura_protocol::{
    ClientMessage, DaemonMessage, MessageError, ProfileInfo, decode_client_message,
    decode_daemon_message, encode_client_message, encode_daemon_message,
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

// =============================================================================
// The request and its answer
// =============================================================================

#[test]
fn set_profile_enabled_round_trips_for_both_values() {
    for enabled in [true, false] {
        let decoded = ClientMessage::SetProfileEnabled {
            profile: "p".to_string(),
            enabled,
        };
        assert_wire_tag(&as_json(&decoded), "set_profile_enabled");
        let encoded = encode_client_message(&decoded);
        assert_eq!(
            decode_client_message(&encoded).expect("set_profile_enabled must round trip"),
            decoded
        );
    }
}

#[test]
fn profile_enabled_set_round_trips_for_both_values() {
    for enabled in [true, false] {
        let decoded = DaemonMessage::ProfileEnabledSet {
            profile: "p".to_string(),
            enabled,
        };
        assert_wire_tag(&as_json(&decoded), "profile_enabled_set");
        let encoded = encode_daemon_message(&decoded);
        assert_eq!(
            decode_daemon_message(&encoded).expect("profile_enabled_set must round trip"),
            decoded
        );
    }
}

// =============================================================================
// The request's `enabled` is a required boolean
// =============================================================================

fn set_profile_enabled_frame(enabled: Option<serde_json::Value>) -> Vec<u8> {
    let mut body = serde_json::json!({
        "type": "set_profile_enabled",
        "profile": "p",
    });
    if let Some(enabled) = enabled {
        body.as_object_mut()
            .expect("the frame is an object")
            .insert("enabled".to_string(), enabled);
    }
    to_bytes(&body)
}

#[test]
fn a_well_formed_set_profile_enabled_frame_decodes() {
    let decoded = decode_client_message(&set_profile_enabled_frame(Some(serde_json::json!(false))))
        .expect("a set_profile_enabled frame with a boolean enabled must decode");
    assert_eq!(
        decoded,
        ClientMessage::SetProfileEnabled {
            profile: "p".to_string(),
            enabled: false,
        }
    );
}

#[test]
fn set_profile_enabled_with_a_non_boolean_enabled_fails_to_decode() {
    for enabled in [
        serde_json::json!("false"),
        serde_json::json!("true"),
        serde_json::json!(0),
        serde_json::json!(1),
        serde_json::Value::Null,
    ] {
        let result = decode_client_message(&set_profile_enabled_frame(Some(enabled.clone())));
        assert_eq!(
            result,
            Err(MessageError::InvalidJson),
            "a set_profile_enabled carrying enabled = {enabled} must fail the frame's decode"
        );
    }
}

#[test]
fn set_profile_enabled_without_enabled_fails_to_decode() {
    let result = decode_client_message(&set_profile_enabled_frame(None));
    assert_eq!(
        result,
        Err(MessageError::InvalidJson),
        "a set_profile_enabled missing enabled must fail rather than default the caller's intent"
    );
}

// =============================================================================
// `ProfileInfo.enabled`
// =============================================================================

fn profile_json(enabled: Option<serde_json::Value>) -> serde_json::Value {
    let mut profile = serde_json::json!({
        "id": "p",
        "display_name": "P",
        "detection": null,
        "detection_warning": null,
        "cwd": null,
        "command": ["cli"],
        "available": true,
        "unavailable_reason": null,
        "model": null,
        "effort": null,
    });
    if let Some(enabled) = enabled {
        profile
            .as_object_mut()
            .expect("the profile is an object")
            .insert("enabled".to_string(), enabled);
    }
    profile
}

fn profile_list_frame(profile: serde_json::Value) -> Vec<u8> {
    to_bytes(&serde_json::json!({
        "type": "profile_list",
        "profiles": [profile],
        "warnings": [],
    }))
}

fn decoded_single_profile(frame: &[u8]) -> ProfileInfo {
    match decode_daemon_message(frame).expect("the profile_list frame must decode") {
        DaemonMessage::ProfileList { mut profiles, .. } => {
            assert_eq!(profiles.len(), 1, "the frame carries exactly one profile");
            profiles.remove(0)
        }
        other => panic!("expected DaemonMessage::ProfileList, got {other:?}"),
    }
}

#[test]
fn a_profile_without_enabled_decodes_as_enabled() {
    let profile = decoded_single_profile(&profile_list_frame(profile_json(None)));
    assert!(
        profile.enabled,
        "a peer that does not carry enabled has no disabled state, so absence must read as true"
    );
}

#[test]
fn a_profile_with_enabled_false_decodes_as_disabled() {
    let profile = decoded_single_profile(&profile_list_frame(profile_json(Some(
        serde_json::json!(false),
    ))));
    assert!(!profile.enabled, "enabled = false must decode as disabled");
}

#[test]
fn a_profile_with_enabled_true_decodes_as_enabled() {
    let profile = decoded_single_profile(&profile_list_frame(profile_json(Some(
        serde_json::json!(true),
    ))));
    assert!(profile.enabled, "enabled = true must decode as enabled");
}

#[test]
fn a_profile_with_a_non_boolean_enabled_fails_the_frame() {
    for enabled in [
        serde_json::json!("false"),
        serde_json::json!(0),
        serde_json::Value::Null,
    ] {
        let result =
            decode_daemon_message(&profile_list_frame(profile_json(Some(enabled.clone()))));
        assert_eq!(
            result,
            Err(MessageError::InvalidJson),
            "a ProfileInfo carrying enabled = {enabled} must fail the whole frame's decode"
        );
    }
}

fn sample_profile(enabled: bool) -> ProfileInfo {
    ProfileInfo {
        id: "p".to_string(),
        display_name: "P".to_string(),
        detection: None,
        detection_warning: None,
        cwd: None,
        command: vec!["cli".to_string()],
        available: true,
        unavailable_reason: None,
        model: None,
        effort: None,
        enabled,
    }
}

#[test]
fn a_profile_always_writes_enabled_on_the_wire() {
    for enabled in [true, false] {
        let value = as_json(&sample_profile(enabled));
        assert_eq!(
            value.get("enabled"),
            Some(&serde_json::json!(enabled)),
            "ProfileInfo must always carry enabled as a boolean, got: {value}"
        );
    }
}

#[test]
fn a_profile_list_round_trips_a_disabled_profile() {
    let decoded = DaemonMessage::ProfileList {
        profiles: vec![sample_profile(false), sample_profile(true)],
        warnings: vec![],
    };
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("profile_list must round trip"),
        decoded
    );
}

// =============================================================================
// Revision
// =============================================================================
