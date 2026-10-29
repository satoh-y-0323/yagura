#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire contracts for revision 30: a registration row carries an optional
//! `notice`, and `launch_context` is a known `ProfileCapability`.
//!
//! Frames are written as raw JSON and read back through the typed decoder, so
//! what is measured is what a peer on the wire observes.

use serde_json::{Value, json};
use yagura_protocol::{DaemonMessage, ProfileCapability, decode_daemon_message};

fn listed_frame(row_extra: Value) -> Vec<u8> {
    let mut row = json!({
        "profile": "cli-host",
        "display_name": "CLI",
        "group": null,
        "toml_set": null,
        "entries": [],
    });
    if let (Some(row), Some(extra)) = (row.as_object_mut(), row_extra.as_object()) {
        row.extend(extra.clone());
    }
    serde_json::to_vec(&json!({
        "type": "mcp_registrations_listed",
        "request_id": 1,
        "bridge": null,
        "rows": [row],
    }))
    .expect("json must encode")
}

fn reencoded_row(frame: &[u8]) -> Value {
    let message = decode_daemon_message(frame).expect("the frame must decode");
    assert!(matches!(
        message,
        DaemonMessage::McpRegistrationsListed { .. }
    ));
    let value = serde_json::to_value(&message).expect("a wire type must serialize");
    value["rows"][0].clone()
}

#[test]
fn a_notice_on_a_row_survives_a_decode_and_encode() {
    let row = reencoded_row(&listed_frame(
        json!({ "notice": "Trust applies to folders." }),
    ));
    assert_eq!(row["notice"], json!("Trust applies to folders."));
}

#[test]
fn a_row_from_an_older_daemon_without_a_notice_reads_as_none() {
    let row = reencoded_row(&listed_frame(json!({})));
    assert!(
        row.get("notice").is_some_and(Value::is_null),
        "an absent notice must be carried as an explicit None, got: {row}"
    );
}

#[test]
fn launch_context_is_a_known_capability_and_the_old_ones_are_unchanged() {
    for name in ["model", "effort", "launch_prompt", "launch_context"] {
        let capability: ProfileCapability =
            serde_json::from_value(json!(name)).expect("a capability name must decode");
        assert_eq!(
            serde_json::to_value(capability).unwrap(),
            json!(name),
            "{name:?} must decode to its own value, not the catch-all"
        );
    }
    let unknown: ProfileCapability = serde_json::from_value(json!("from_the_future")).unwrap();
    assert_eq!(unknown, ProfileCapability::Unknown);
}
