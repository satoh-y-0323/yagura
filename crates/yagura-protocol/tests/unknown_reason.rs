#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Integration tests for the `UnknownReason` / `DetectedStatus` types and the
//! wire fields that carry a reason alongside a status.

use yagura_protocol::{
    ChatDeliveryInfo, DaemonMessage, DetectedStatus, UnknownReason, WindowInfo, WindowStatus,
    decode_daemon_message, encode_daemon_message,
};

/// The six fixed reasons, their `snake_case` wire strings and the phrase
/// `UnknownReason::phrase()` returns for each.
const ALL_UNKNOWN_REASONS: &[(UnknownReason, &str, &str)] = &[
    (
        UnknownReason::NoDefinition,
        "no_definition",
        "no usable detection definition",
    ),
    (
        UnknownReason::NoRuleMatched,
        "no_rule_matched",
        "the screen matches no detection rule",
    ),
    (
        UnknownReason::ScreenLost,
        "screen_lost",
        "the screen model was lost",
    ),
    (
        UnknownReason::DetectorStopped,
        "detector_stopped",
        "the detection task stopped",
    ),
    (
        UnknownReason::NotYetJudged,
        "not_yet_judged",
        "nothing has been judged yet",
    ),
    (
        UnknownReason::DeclaredUnknown,
        "declared_unknown",
        "a rule or a status event reported unknown",
    ),
];

/// The six non-`Unknown` `DetectedStatus` variants paired with the
/// `WindowStatus` they project to.
const ALL_NON_UNKNOWN: &[(DetectedStatus, WindowStatus)] = &[
    (DetectedStatus::Running, WindowStatus::Running),
    (DetectedStatus::WaitingInput, WindowStatus::WaitingInput),
    (
        DetectedStatus::WaitingApproval,
        WindowStatus::WaitingApproval,
    ),
    (DetectedStatus::Interrupted, WindowStatus::Interrupted),
    (DetectedStatus::Idle, WindowStatus::Idle),
    (DetectedStatus::Exited, WindowStatus::Exited),
];

// ---------------------------------------------------------------------
// UnknownReason <-> wire string
// ---------------------------------------------------------------------

#[test]
fn unknown_reason_round_trips_through_its_snake_case_wire_string() {
    for (reason, wire, _) in ALL_UNKNOWN_REASONS {
        let value = serde_json::to_value(reason).expect("reason must serialize");
        assert_eq!(value, serde_json::json!(wire));
        let decoded: UnknownReason =
            serde_json::from_value(value).expect("reason must deserialize");
        assert_eq!(decoded, *reason);
    }
}

#[test]
fn unknown_reason_deserialize_rejects_an_unknown_wire_string() {
    let result: Result<UnknownReason, _> = serde_json::from_str(r#""bogus_reason""#);
    assert!(result.is_err());
}

#[test]
fn unknown_reason_as_wire_matches_the_serde_representation_for_every_variant() {
    for (reason, wire, _) in ALL_UNKNOWN_REASONS {
        assert_eq!(reason.as_wire(), *wire);
    }
}

#[test]
fn unknown_reason_from_wire_str_round_trips_for_every_variant() {
    for (reason, wire, _) in ALL_UNKNOWN_REASONS {
        assert_eq!(UnknownReason::from_wire_str(wire), Some(*reason));
    }
}

#[test]
fn unknown_reason_from_wire_str_rejects_an_unknown_string() {
    assert_eq!(UnknownReason::from_wire_str("bogus_reason"), None);
}

// ---------------------------------------------------------------------
// DetectedStatus projections
// ---------------------------------------------------------------------

#[test]
fn detected_status_projects_to_its_window_status_through_status_and_from() {
    let mut pairs: Vec<(DetectedStatus, WindowStatus)> = ALL_NON_UNKNOWN.to_vec();
    pairs.extend(
        ALL_UNKNOWN_REASONS
            .iter()
            .map(|(reason, _, _)| (DetectedStatus::Unknown(*reason), WindowStatus::Unknown)),
    );
    for (detected, expected) in pairs {
        assert_eq!(detected.status(), expected, "{detected:?}");
        let via_from: WindowStatus = detected.into();
        assert_eq!(via_from, expected, "{detected:?}");
    }
}

#[test]
fn detected_status_unknown_reason_is_some_and_matches_the_carried_reason() {
    for (reason, _, _) in ALL_UNKNOWN_REASONS {
        assert_eq!(
            DetectedStatus::Unknown(*reason).unknown_reason(),
            Some(*reason)
        );
    }
}

#[test]
fn detected_status_known_returns_none_for_window_status_unknown() {
    assert_eq!(DetectedStatus::known(WindowStatus::Unknown), None);
}

#[test]
fn detected_status_known_returns_some_for_every_other_window_status() {
    for (detected, status) in ALL_NON_UNKNOWN {
        assert_eq!(DetectedStatus::known(*status), Some(*detected));
    }
}

// ---------------------------------------------------------------------
// Emit-side consistency (a reason is present iff the status is Unknown)
// ---------------------------------------------------------------------

#[test]
fn every_variant_carries_a_reason_if_and_only_if_its_status_is_unknown() {
    let mut all: Vec<DetectedStatus> = ALL_NON_UNKNOWN.iter().map(|(d, _)| *d).collect();
    all.extend(
        ALL_UNKNOWN_REASONS
            .iter()
            .map(|(reason, _, _)| DetectedStatus::Unknown(*reason)),
    );
    for detected in all {
        assert_eq!(
            detected.unknown_reason().is_some(),
            detected.status() == WindowStatus::Unknown
        );
    }
}

// ---------------------------------------------------------------------
// UnknownReason::phrase()
// ---------------------------------------------------------------------

#[test]
fn unknown_reason_phrase_matches_the_designed_text_for_every_variant() {
    for (reason, _, phrase) in ALL_UNKNOWN_REASONS {
        assert_eq!(reason.phrase(), *phrase);
    }
}

// ---------------------------------------------------------------------
// The four wire types' new optional fields
// ---------------------------------------------------------------------

fn sample_window_info(
    unknown_reason: Option<UnknownReason>,
    detection_warning: Option<String>,
) -> WindowInfo {
    WindowInfo {
        window_id: "w1".to_string(),
        name: "shell".to_string(),
        vendor: None,
        cwd: "/home".to_string(),
        status: WindowStatus::Unknown,
        exit_code: None,
        layout_json: None,
        room: None,
        unknown_reason,
        detection_warning,
        status_rev: None,
        mcp_connected: None,
        status_reports_missing: None,
    }
}

#[test]
fn window_info_round_trips_unknown_reason_and_detection_warning_when_present() {
    let info = sample_window_info(
        Some(UnknownReason::NoRuleMatched),
        Some("no directory resolved".to_string()),
    );
    let json = serde_json::to_string(&info).expect("window info must serialize");
    let decoded: WindowInfo = serde_json::from_str(&json).expect("window info must deserialize");
    assert_eq!(decoded, info);
}

fn sample_window_status_message(
    unknown_reason: Option<UnknownReason>,
    detection_warning: Option<String>,
) -> DaemonMessage {
    DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Unknown,
        since_ms: 100,
        last_output_summary: None,
        exit_code: None,
        unknown_reason,
        detection_warning,
        mcp_connected: None,
    }
}

#[test]
fn window_status_message_omits_unknown_reason_and_detection_warning_keys_when_none() {
    let msg = sample_window_status_message(None, None);
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    let object = value.as_object().expect("message serializes to an object");
    assert_eq!(
        object.get("type"),
        Some(&serde_json::Value::from("window_status"))
    );
    assert!(!object.contains_key("unknown_reason"));
    assert!(!object.contains_key("detection_warning"));
}

#[test]
fn window_status_message_decodes_an_old_form_json_without_the_two_new_keys() {
    let json = r#"{
        "type": "window_status",
        "window_id": "w1",
        "status": "unknown",
        "since_ms": 100,
        "last_output_summary": null,
        "exit_code": null
    }"#;
    let decoded =
        decode_daemon_message(json.as_bytes()).expect("old-form window_status must decode");
    match decoded {
        DaemonMessage::WindowStatus {
            unknown_reason,
            detection_warning,
            ..
        } => {
            assert_eq!(unknown_reason, None);
            assert_eq!(detection_warning, None);
        }
        other => panic!("expected WindowStatus, got {other:?}"),
    }
}

#[test]
fn window_status_message_round_trips_unknown_reason_and_detection_warning_when_present() {
    let msg = sample_window_status_message(
        Some(UnknownReason::ScreenLost),
        Some("no directory resolved".to_string()),
    );
    let body = encode_daemon_message(&msg);
    let decoded = decode_daemon_message(&body).expect("message must decode");
    assert_eq!(decoded, msg);
}

fn sample_status_changed(unknown_reason: Option<UnknownReason>) -> DaemonMessage {
    DaemonMessage::StatusChanged {
        window_id: "w1".to_string(),
        status: WindowStatus::Unknown,
        unknown_reason,
        status_rev: None,
    }
}

#[test]
fn status_changed_omits_unknown_reason_key_when_none() {
    let msg = sample_status_changed(None);
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    let object = value.as_object().expect("message serializes to an object");
    assert!(!object.contains_key("unknown_reason"));
}

#[test]
fn status_changed_decodes_an_old_form_json_without_the_new_key() {
    let json = r#"{"type": "status_changed", "window_id": "w1", "status": "unknown"}"#;
    let decoded =
        decode_daemon_message(json.as_bytes()).expect("old-form status_changed must decode");
    match decoded {
        DaemonMessage::StatusChanged { unknown_reason, .. } => {
            assert_eq!(unknown_reason, None);
        }
        other => panic!("expected StatusChanged, got {other:?}"),
    }
}

#[test]
fn status_changed_round_trips_unknown_reason_when_present() {
    let msg = sample_status_changed(Some(UnknownReason::DetectorStopped));
    let body = encode_daemon_message(&msg);
    let decoded = decode_daemon_message(&body).expect("message must decode");
    assert_eq!(decoded, msg);
}

fn sample_chat_delivery(unknown_reason: Option<UnknownReason>) -> ChatDeliveryInfo {
    ChatDeliveryInfo {
        window_id: "w2".to_string(),
        state: "undelivered_unknown".to_string(),
        unknown_reason,
    }
}

#[test]
fn chat_delivery_info_omits_unknown_reason_key_when_none() {
    let delivery = sample_chat_delivery(None);
    let value = serde_json::to_value(&delivery).expect("delivery info must serialize");
    let object = value
        .as_object()
        .expect("delivery info serializes to an object");
    assert!(!object.contains_key("unknown_reason"));
}

#[test]
fn chat_delivery_info_decodes_an_old_form_json_without_the_new_key() {
    let json = r#"{"window_id": "w2", "state": "undelivered_unknown"}"#;
    let decoded: ChatDeliveryInfo =
        serde_json::from_str(json).expect("old-form delivery info must decode");
    assert_eq!(decoded.unknown_reason, None);
}

#[test]
fn chat_delivery_info_round_trips_unknown_reason_when_present() {
    let delivery = sample_chat_delivery(Some(UnknownReason::NoDefinition));
    let json = serde_json::to_string(&delivery).expect("delivery info must serialize");
    let decoded: ChatDeliveryInfo =
        serde_json::from_str(&json).expect("delivery info must deserialize");
    assert_eq!(decoded, delivery);
}
