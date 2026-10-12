#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the restore flow (present/select restart): the
//! `ListRestoreCandidates` / `UpdateWindowLayout` client messages, the
//! `RestoreCandidateList` / `WindowLayoutUpdated` daemon messages, the
//! `RestoreCandidate` struct, and `WindowInfo`'s `layout_json` field.

use yagura_protocol::{
    ClientMessage, DaemonMessage, MessageError, RestoreCandidate, WindowInfo, WindowStatus,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
};

// =============================================================================
// ClientMessage::ListRestoreCandidates
// =============================================================================

#[test]
fn client_message_list_restore_candidates_decodes() {
    let body = br#"{"type":"list_restore_candidates"}"#;
    let decoded = decode_client_message(body).expect("list_restore_candidates must decode");
    assert_eq!(decoded, ClientMessage::ListRestoreCandidates);
}

#[test]
fn client_message_list_restore_candidates_round_trips() {
    let decoded = ClientMessage::ListRestoreCandidates;
    let encoded = encode_client_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("list_restore_candidates"),
        "the wire tag must be snake_case, got: {value}"
    );
    let decoded2 = decode_client_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

// =============================================================================
// ClientMessage::UpdateWindowLayout
// =============================================================================

#[test]
fn client_message_update_window_layout_round_trips() {
    let body = br#"{"type":"update_window_layout","window_id":"w1","layout_json":"{\"v\":1}"}"#;
    let decoded = decode_client_message(body).expect("update_window_layout must decode");
    assert_eq!(
        decoded,
        ClientMessage::UpdateWindowLayout {
            window_id: "w1".to_string(),
            layout_json: "{\"v\":1}".to_string(),
        }
    );
    let encoded = encode_client_message(&decoded);
    let decoded2 = decode_client_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn client_message_update_window_layout_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"update_window_layout","layout_json":"{}"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_update_window_layout_without_layout_json_returns_invalid_json() {
    let body = br#"{"type":"update_window_layout","window_id":"w1"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn client_message_update_window_layout_is_registered_but_unknown_to_the_daemon_decoder() {
    let body = br#"{"type":"update_window_layout","window_id":"w1","layout_json":"{}"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::UnknownType));
}

// =============================================================================
// DaemonMessage::RestoreCandidateList
// =============================================================================

#[test]
fn daemon_message_restore_candidate_list_round_trips_with_windows() {
    let body = br#"{"type":"restore_candidate_list","windows":[{"position":0,"command":["cmd.exe"],"cwd":"C:/work","name":"alpha","vendor":null,"detection":null}],"taken_at_epoch_s":1000}"#;
    let decoded = decode_daemon_message(body).expect("restore_candidate_list must decode");
    assert_eq!(
        decoded,
        DaemonMessage::RestoreCandidateList {
            windows: vec![RestoreCandidate {
                position: 0,
                command: vec!["cmd.exe".to_string()],
                cwd: "C:/work".to_string(),
                name: "alpha".to_string(),
                vendor: None,
                detection: None,
                layout_json: None,
                effort: None,
                model: None,
                profile: None,
            }],
            taken_at_epoch_s: Some(1000),
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_restore_candidate_list_with_no_windows_and_null_taken_at_round_trips() {
    let body = br#"{"type":"restore_candidate_list","windows":[],"taken_at_epoch_s":null}"#;
    let decoded = decode_daemon_message(body)
        .expect("restore_candidate_list with an empty candidate list must decode");
    assert_eq!(
        decoded,
        DaemonMessage::RestoreCandidateList {
            windows: vec![],
            taken_at_epoch_s: None,
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let value: serde_json::Value = serde_json::from_slice(&encoded).expect("valid json");
    assert_eq!(
        value.get("taken_at_epoch_s"),
        Some(&serde_json::Value::Null),
        "taken_at_epoch_s must be present as null on the wire when None (no \
         skip_serializing_if), matching the Tauri reply's wire-form-equal contract, got: {value}"
    );
}

#[test]
fn daemon_message_restore_candidate_list_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"restore_candidate_list","taken_at_epoch_s":null}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

// =============================================================================
// DaemonMessage::WindowLayoutUpdated
// =============================================================================

#[test]
fn daemon_message_window_layout_updated_round_trips() {
    let body = br#"{"type":"window_layout_updated","window_id":"w1"}"#;
    let decoded = decode_daemon_message(body).expect("window_layout_updated must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowLayoutUpdated {
            window_id: "w1".to_string(),
        }
    );
    let encoded = encode_daemon_message(&decoded);
    let decoded2 = decode_daemon_message(&encoded).expect("round trip must succeed");
    assert_eq!(decoded, decoded2);
}

#[test]
fn daemon_message_window_layout_updated_without_window_id_returns_invalid_json() {
    let body = br#"{"type":"window_layout_updated"}"#;
    assert_eq!(decode_daemon_message(body), Err(MessageError::InvalidJson));
}

#[test]
fn daemon_message_window_layout_updated_is_registered_but_unknown_to_the_client_decoder() {
    let body = br#"{"type":"window_layout_updated","window_id":"w1"}"#;
    assert_eq!(decode_client_message(body), Err(MessageError::UnknownType));
}

// =============================================================================
// RestoreCandidate: optional keys are omitted when None and carried when Some
// =============================================================================

fn sample_candidate(layout_json: Option<&str>) -> RestoreCandidate {
    RestoreCandidate {
        position: 2,
        command: vec!["pwsh.exe".to_string(), "-NoLogo".to_string()],
        cwd: "C:/work/beta".to_string(),
        name: "beta".to_string(),
        vendor: Some("neutral".to_string()),
        detection: Some("generic-shell".to_string()),
        layout_json: layout_json.map(str::to_string),
        effort: None,
        model: None,
        profile: None,
    }
}

#[test]
fn restore_candidate_omits_every_absent_optional_key() {
    let value = serde_json::to_value(sample_candidate(None)).expect("must serialize to a Value");
    let mut keys: Vec<&str> = value
        .as_object()
        .expect("RestoreCandidate must serialize as an object")
        .keys()
        .map(String::as_str)
        .collect();
    keys.sort_unstable();
    assert_eq!(
        keys,
        ["command", "cwd", "detection", "name", "position", "vendor"],
        "layout_json, profile, model and effort must be omitted when None, got: {value}"
    );
}

#[test]
fn restore_candidate_layout_and_provenance_round_trip_under_their_own_keys() {
    let candidate = RestoreCandidate {
        profile: Some("assistant".to_string()),
        model: Some("model-a".to_string()),
        effort: Some("high".to_string()),
        ..sample_candidate(Some("{\"v\":1}"))
    };
    let value = serde_json::to_value(&candidate).expect("must serialize to a Value");
    assert_eq!(
        value.get("layout_json"),
        Some(&serde_json::json!("{\"v\":1}"))
    );
    assert_eq!(value.get("profile"), Some(&serde_json::json!("assistant")));
    assert_eq!(value.get("model"), Some(&serde_json::json!("model-a")));
    assert_eq!(value.get("effort"), Some(&serde_json::json!("high")));
    let back: RestoreCandidate = serde_json::from_value(value).expect("must deserialize");
    assert_eq!(candidate, back);
}

// =============================================================================
// WindowInfo: optional fields, backward compatibility
// =============================================================================

fn sample_window_info(layout_json: Option<&str>) -> WindowInfo {
    WindowInfo {
        window_id: "w1".to_string(),
        name: "shell".to_string(),
        vendor: None,
        cwd: "/home".to_string(),
        status: WindowStatus::Running,
        exit_code: None,
        layout_json: layout_json.map(str::to_string),
        room: None,
        unknown_reason: None,
        detection_warning: None,
        status_rev: None,
        mcp_connected: None,
        status_reports_missing: None,
    }
}

#[test]
fn window_list_decodes_a_body_missing_every_optional_window_key() {
    // A peer predating the optional WindowInfo fields sends none of their
    // keys (not even null); each must default to None.
    let body = br#"{"type":"window_list","windows":[{"window_id":"w1","name":"shell","vendor":null,"cwd":"/home","status":"running","exit_code":null}]}"#;
    let decoded = decode_daemon_message(body)
        .expect("the window_list form missing the optional keys entirely must still decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowList {
            windows: vec![sample_window_info(None)],
        }
    );
}

#[test]
fn window_list_decodes_an_explicit_layout_json_value() {
    let body = br#"{"type":"window_list","windows":[{"window_id":"w1","name":"shell","vendor":null,"cwd":"/home","status":"running","exit_code":null,"layout_json":"{\"v\":1}"}]}"#;
    let decoded = decode_daemon_message(body)
        .expect("window_list with an explicit layout_json value must decode");
    assert_eq!(
        decoded,
        DaemonMessage::WindowList {
            windows: vec![sample_window_info(Some("{\"v\":1}"))],
        }
    );
}
