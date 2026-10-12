#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! The window status generation counter (`status_rev`) as it appears on the
//! wire: an optional, monotonically-assigned `u64` the daemon attaches to the
//! two message shapes that both report a window's status pair *and* update a
//! reader's registry (`WindowInfo`, `DaemonMessage::StatusChanged`).
//!
//! The wire key is `status_rev` on every carrier, the field decodes from a
//! body that omits it entirely (a peer predating this field), and it is
//! omitted from the wire (not sent as `null`) when the value is absent, so a
//! peer predating the field keeps decoding every message this crate emits.
//! For `WindowInfo` those two are measured with its other optional keys, in
//! `protocol_v2.rs` and `restore_protocol.rs`.
//!
//! `DaemonMessage::WindowStatus` (the request/response answer to
//! `get_window_status`) does not carry a generation at all: the field belongs
//! only to the push/list paths that update a reader's registry, not to a
//! point-in-time query. The exhaustive struct literals below, which name
//! every field and take no `..` update, double as the enforcement of that
//! omission: adding a `status_rev` field to `WindowStatus` would make them
//! fail to compile.

use yagura_protocol::{
    DaemonMessage, WindowInfo, WindowStatus, decode_daemon_message, encode_daemon_message,
};

fn sample_window_info(status_rev: Option<u64>) -> WindowInfo {
    WindowInfo {
        window_id: "w1".to_string(),
        name: "shell".to_string(),
        vendor: None,
        cwd: "/home".to_string(),
        status: WindowStatus::Running,
        exit_code: None,
        layout_json: None,
        room: None,
        unknown_reason: None,
        detection_warning: None,
        status_rev,
        mcp_connected: None,
        status_reports_missing: None,
    }
}

// ---------------------------------------------------------------------
// WindowInfo.status_rev
// ---------------------------------------------------------------------

#[test]
fn window_info_status_rev_round_trips_under_the_status_rev_key() {
    let info = sample_window_info(Some(3));
    let value = serde_json::to_value(&info).expect("must serialize");
    assert_eq!(
        value.get("status_rev"),
        Some(&serde_json::json!(3)),
        "the wire key must be exactly \"status_rev\", got: {value}"
    );
    let back: WindowInfo = serde_json::from_value(value).expect("must deserialize");
    assert_eq!(back, info);
}

// ---------------------------------------------------------------------
// DaemonMessage::StatusChanged.status_rev
// ---------------------------------------------------------------------

fn status_changed(status_rev: Option<u64>) -> DaemonMessage {
    DaemonMessage::StatusChanged {
        window_id: "w1".to_string(),
        status: WindowStatus::WaitingInput,
        unknown_reason: None,
        status_rev,
    }
}

#[test]
fn status_changed_decodes_a_body_missing_the_status_rev_key_as_none() {
    let body = br#"{"type":"status_changed","window_id":"w1","status":"waiting_input"}"#;
    let decoded = decode_daemon_message(body).expect("status_changed must decode");
    assert_eq!(decoded, status_changed(None));
}

#[test]
fn status_changed_omits_status_rev_from_the_wire_when_none() {
    let body = encode_daemon_message(&status_changed(None));
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    assert!(
        value.get("status_rev").is_none(),
        "status_rev: None must be omitted from the wire, got: {value}"
    );
}

#[test]
fn status_changed_status_rev_round_trips_when_present() {
    let msg = status_changed(Some(42));
    let body = encode_daemon_message(&msg);
    let decoded = decode_daemon_message(&body).expect("round trip decode must succeed");
    assert_eq!(decoded, msg);
}

#[test]
fn status_changed_wire_key_for_status_rev_is_literally_status_rev() {
    let body = encode_daemon_message(&status_changed(Some(5)));
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    assert_eq!(
        value.get("status_rev"),
        Some(&serde_json::json!(5)),
        "the wire key must be exactly \"status_rev\", got: {value}"
    );
}

// ---------------------------------------------------------------------
// DaemonMessage::WindowStatus: no generation on the request/response path
// ---------------------------------------------------------------------

#[test]
fn window_status_response_has_no_status_rev_field() {
    // An exhaustive literal (no `..` update): if a future change adds
    // `status_rev` to this variant, this construction stops compiling.
    let msg = DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Running,
        since_ms: 0,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected: None,
    };
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    assert!(
        value.get("status_rev").is_none(),
        "the request/response status query must never carry a generation, got: {value}"
    );
}
