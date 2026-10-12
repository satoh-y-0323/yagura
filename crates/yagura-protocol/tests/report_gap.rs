#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire revision 14: the mark that a window's status reports stopped arriving
//! (`WindowInfo.status_reports_missing`), the hook entries a registration
//! writes (`McpRegistrationRow.hook_entries`), and the removal of
//! `StatusEvent.ttl_ms`.
//!
//! Every property is read off the serialized JSON, because the wire is the
//! contract: the mark and the entries must reach a peer under exactly these
//! keys, and a peer that does not carry them must still decode the frame.

use serde_json::{Value, json};

use yagura_protocol::{
    ClientMessage, DaemonMessage, MCP_REGISTRATION_MIN_REVISION, McpRegistrationRow,
    PROTOCOL_REVISION_BASELINE, decode_client_message, decode_daemon_message,
    encode_daemon_message, required_protocol_revision,
};

fn window_info_json(extra: Value) -> Value {
    let mut base = json!({
        "window_id": "w1",
        "name": "shell",
        "vendor": null,
        "cwd": "/home",
        "status": "running",
        "exit_code": null,
    });
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

#[test]
fn the_new_fields_and_the_removed_one_need_no_message_gate() {
    // Nothing a peer must establish before sending: the mark and the entries
    // are fields of answers, and `ttl_ms` is only ignored.
    let status_event = decode_client_message(
        br#"{"type":"status_event","source":"cli","status":"running","token":"0123456789abcdef0123456789abcdef"}"#,
    )
    .expect("status_event must decode");
    assert_eq!(
        required_protocol_revision(&status_event),
        PROTOCOL_REVISION_BASELINE
    );
    assert_eq!(
        required_protocol_revision(&ClientMessage::ListWindows),
        PROTOCOL_REVISION_BASELINE
    );
    assert_eq!(MCP_REGISTRATION_MIN_REVISION, 13);
}

#[test]
fn a_status_event_frame_never_carries_ttl_ms() {
    let legacy = decode_client_message(
        br#"{"type":"status_event","source":"cli","status":"running","ttl_ms":5000,"token":"0123456789abcdef0123456789abcdef"}"#,
    )
    .expect("a frame from a hook that still sends ttl_ms must decode");
    let value: Value = serde_json::from_slice(&yagura_protocol::encode_client_message(&legacy))
        .expect("valid json");
    assert!(
        value.get("ttl_ms").is_none(),
        "the frame must not carry ttl_ms, got: {value}"
    );
}

#[test]
fn a_window_list_keeps_the_mark_of_each_window() {
    let body = json!({
        "type": "window_list",
        "windows": [
            window_info_json(json!({ "window_id": "w1", "status_reports_missing": true })),
            window_info_json(json!({ "window_id": "w2", "status_reports_missing": false })),
        ],
    })
    .to_string();
    let decoded = decode_daemon_message(body.as_bytes()).expect("window_list must decode");
    assert!(matches!(&decoded, DaemonMessage::WindowList { .. }));
    let value: Value =
        serde_json::from_slice(&encode_daemon_message(&decoded)).expect("valid json");
    let marks: Vec<Option<&Value>> = value["windows"]
        .as_array()
        .expect("windows is an array")
        .iter()
        .map(|window| window.get("status_reports_missing"))
        .collect();
    assert_eq!(marks, vec![Some(&json!(true)), Some(&json!(false))]);
}

fn row_json(extra: Value) -> Value {
    let mut base = json!({
        "profile": "p",
        "display_name": "P",
        "add": { "argv": ["p", "mcp", "add"], "program_at": 0 },
        "remove": { "argv": ["p", "mcp", "remove"], "program_at": 0 },
    });
    if let (Some(base), Some(extra)) = (base.as_object_mut(), extra.as_object()) {
        for (key, value) in extra {
            base.insert(key.clone(), value.clone());
        }
    }
    base
}

fn reserialized_row(value: &Value) -> Value {
    let row: McpRegistrationRow =
        serde_json::from_value(value.clone()).expect("a registration row must decode");
    serde_json::to_value(&row).expect("a registration row must serialize")
}

#[test]
fn a_registration_row_without_hook_entries_decodes_as_having_none() {
    // A daemon older than the field, or a profile that writes no hooks.
    let value = reserialized_row(&row_json(json!({})));
    let none = match value.get("hook_entries") {
        None => true,
        Some(entries) => entries == &json!([]),
    };
    assert!(none, "no hook entries must read as none, got: {value}");
}

#[test]
fn a_registration_row_with_a_hook_entry_survives_a_listing_round_trip() {
    let body = json!({
        "type": "mcp_registrations_listed",
        "request_id": 7,
        "bridge": null,
        "rows": [row_json(json!({
            "hook_entries": [{ "file": "f", "name": "n", "content": "c" }],
        }))],
    })
    .to_string();
    let decoded = decode_daemon_message(body.as_bytes()).expect("the listing must decode");
    let value: Value =
        serde_json::from_slice(&encode_daemon_message(&decoded)).expect("valid json");
    assert_eq!(
        value["rows"][0]["hook_entries"],
        json!([{ "file": "f", "name": "n", "content": "c" }])
    );
}
