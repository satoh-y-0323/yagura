#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the MCP registration messages
//! (`list_mcp_registrations`, `run_mcp_registration`) and their answers and
//! pushes, plus the types they carry.
//!
//! - The two requests and the four daemon messages round trip under their
//!   snake_case tags (the frozen shape in `wire_inventory.rs` pins each
//!   request's key set).
//! - `action` is one of `register` / `update` / `remove` and has no
//!   catch-all: an unrecognized value, or a missing `request_id` / `profile`,
//!   fails the whole frame rather than being filled in with a default.
//! - `state` has a catch-all (`McpRegistrationState::Unknown`): an
//!   unrecognized value decodes to it without failing the frame.
//! - Every optional field accepts both `null` and an omitted key as "not
//!   carried".
//! - A row may carry no `add` / `remove` command (a registration that only
//!   writes files); what it writes travels under `entries`.
//! - Both requests require `MCP_REGISTRATION_MIN_REVISION` (also see
//!   `required_revision.rs`'s per-variant corpus).

use yagura_protocol::{
    ClientMessage, DaemonMessage, MCP_REGISTRATION_MIN_REVISION, McpRegistrationAction,
    McpRegistrationCommand, McpRegistrationRow, McpRegistrationState, McpRegistrationStatusInfo,
    McpRegistrationStep, McpRegistrationTomlSet, McpRegistrationTomlValue, MessageError,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
    required_protocol_revision,
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

fn sample_command(verb: &str) -> McpRegistrationCommand {
    McpRegistrationCommand {
        argv: vec![
            "launcher".to_string(),
            "cli".to_string(),
            "mcp".to_string(),
            verb.to_string(),
            "yagura".to_string(),
        ],
        program_at: 1,
    }
}

fn sample_toml_set() -> McpRegistrationTomlSet {
    McpRegistrationTomlSet {
        file: ".cli/config.toml".to_string(),
        table: vec!["mcp_servers".to_string(), "yagura".to_string()],
        values: vec![McpRegistrationTomlValue {
            key: "tool_timeout_sec".to_string(),
            value: "86400".to_string(),
        }],
    }
}

fn sample_row() -> McpRegistrationRow {
    McpRegistrationRow {
        profile: "cli-host".to_string(),
        display_name: "CLI".to_string(),
        group: Some("host".to_string()),
        add: Some(sample_command("add")),
        remove: Some(sample_command("remove")),
        toml_set: Some(sample_toml_set()),
        entries: Vec::new(),
    }
}

fn sample_status(state: McpRegistrationState) -> McpRegistrationStatusInfo {
    McpRegistrationStatusInfo {
        profile: "cli-host".to_string(),
        state,
        differs_in: vec!["tool_timeout_sec".to_string()],
        reason: Some("a fixed reason".to_string()),
        add: Some(sample_command("add")),
        remove: Some(sample_command("remove")),
        refused_by_claim: Some("/managed_hooks_path".to_string()),
    }
}

fn sample_step() -> McpRegistrationStep {
    McpRegistrationStep {
        argv: sample_command("add").argv,
        program_at: 1,
        exit_code: Some(0),
        output: "added".to_string(),
    }
}

fn sample_listed() -> DaemonMessage {
    DaemonMessage::McpRegistrationsListed {
        request_id: 7,
        bridge: Some("/opt/yagura/yagura-mcp".to_string()),
        rows: vec![sample_row()],
    }
}

fn sample_status_push() -> DaemonMessage {
    DaemonMessage::McpRegistrationStatus {
        request_id: 7,
        status: sample_status(McpRegistrationState::Differs),
    }
}

fn sample_result() -> DaemonMessage {
    DaemonMessage::McpRegistrationResult {
        request_id: 8,
        profile: "cli-host".to_string(),
        action: McpRegistrationAction::Register,
        steps: vec![sample_step()],
        error: Some("a fixed failure".to_string()),
        status: sample_status(McpRegistrationState::Registered),
    }
}

fn assert_daemon_round_trip(decoded: DaemonMessage, tag: &str) {
    assert_wire_tag(&as_json(&decoded), tag);
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded)
            .unwrap_or_else(|err| panic!("{tag} must round trip, got {err:?}")),
        decoded
    );
}

// =============================================================================
// The two requests and four daemon messages round trip.
// =============================================================================

#[test]
fn list_mcp_registrations_round_trips() {
    let decoded = ClientMessage::ListMcpRegistrations { request_id: 3 };
    assert_wire_tag(&as_json(&decoded), "list_mcp_registrations");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("list_mcp_registrations must round trip"),
        decoded
    );
}

#[test]
fn run_mcp_registration_round_trips_for_every_action() {
    for action in [
        McpRegistrationAction::Register,
        McpRegistrationAction::Update,
        McpRegistrationAction::Remove,
    ] {
        let decoded = ClientMessage::RunMcpRegistration {
            profile: "cli-host".to_string(),
            action,
            request_id: 4,
        };
        assert_wire_tag(&as_json(&decoded), "run_mcp_registration");
        let encoded = encode_client_message(&decoded);
        assert_eq!(
            decode_client_message(&encoded).expect("run_mcp_registration must round trip"),
            decoded
        );
    }
}

#[test]
fn mcp_registrations_listed_round_trips() {
    assert_daemon_round_trip(sample_listed(), "mcp_registrations_listed");
}

#[test]
fn mcp_registration_status_round_trips() {
    assert_daemon_round_trip(sample_status_push(), "mcp_registration_status");
}

#[test]
fn mcp_registration_requested_round_trips() {
    assert_daemon_round_trip(
        DaemonMessage::McpRegistrationRequested {
            request_id: 8,
            profile: "cli-host".to_string(),
            action: McpRegistrationAction::Update,
        },
        "mcp_registration_requested",
    );
}

#[test]
fn mcp_registration_result_round_trips() {
    assert_daemon_round_trip(sample_result(), "mcp_registration_result");
}

// =============================================================================
// `action` is a closed set; missing request fields fail the frame.
// =============================================================================

fn run_frame(action: Option<&str>, profile: Option<&str>, request_id: Option<u64>) -> Vec<u8> {
    let mut body = serde_json::json!({ "type": "run_mcp_registration" });
    let object = body.as_object_mut().expect("the frame is an object");
    if let Some(action) = action {
        object.insert("action".to_string(), serde_json::json!(action));
    }
    if let Some(profile) = profile {
        object.insert("profile".to_string(), serde_json::json!(profile));
    }
    if let Some(request_id) = request_id {
        object.insert("request_id".to_string(), serde_json::json!(request_id));
    }
    to_bytes(&body)
}

#[test]
fn each_documented_action_value_decodes_to_its_own_variant() {
    for (wire, expected) in [
        ("register", McpRegistrationAction::Register),
        ("update", McpRegistrationAction::Update),
        ("remove", McpRegistrationAction::Remove),
    ] {
        let context = format!("the documented action value {wire:?} must decode to {expected:?}");
        let decoded = decode_client_message(&run_frame(Some(wire), Some("cli-host"), Some(1)))
            .unwrap_or_else(|err| panic!("action {wire:?} must decode, got {err:?}"));
        assert_eq!(
            decoded,
            ClientMessage::RunMcpRegistration {
                profile: "cli-host".to_string(),
                action: expected,
                request_id: 1,
            },
            "{context}"
        );
    }
}

#[test]
fn an_unrecognized_action_fails_to_decode() {
    for action in ["reinstall", "unknown", "Register", ""] {
        let result = decode_client_message(&run_frame(Some(action), Some("cli-host"), Some(1)));
        assert_eq!(
            result,
            Err(MessageError::InvalidJson),
            "a run_mcp_registration carrying action = {action:?} must fail the frame's decode \
             rather than guess the caller's intent"
        );
    }
}

#[test]
fn run_mcp_registration_missing_a_field_fails_to_decode() {
    for (frame, missing) in [
        (run_frame(None, Some("cli-host"), Some(1)), "action"),
        (run_frame(Some("register"), None, Some(1)), "profile"),
        (
            run_frame(Some("register"), Some("cli-host"), None),
            "request_id",
        ),
    ] {
        assert_eq!(
            decode_client_message(&frame),
            Err(MessageError::InvalidJson),
            "a run_mcp_registration missing {missing} must fail rather than default it"
        );
    }
}

#[test]
fn list_mcp_registrations_missing_request_id_fails_to_decode() {
    let frame = to_bytes(&serde_json::json!({ "type": "list_mcp_registrations" }));
    assert_eq!(
        decode_client_message(&frame),
        Err(MessageError::InvalidJson),
        "a list_mcp_registrations missing request_id must fail rather than default it"
    );
}

// =============================================================================
// `state` has a catch-all: an unrecognized value does not fail the frame.
// =============================================================================

fn status_frame(state: &str) -> Vec<u8> {
    to_bytes(&serde_json::json!({
        "type": "mcp_registration_status",
        "request_id": 1,
        "status": {
            "profile": "cli-host",
            "state": state,
            "differs_in": [],
            "reason": null,
            "add": null,
            "remove": null,
        },
    }))
}

fn decoded_status(frame: &[u8]) -> McpRegistrationStatusInfo {
    match decode_daemon_message(frame).expect("the mcp_registration_status frame must decode") {
        DaemonMessage::McpRegistrationStatus { status, .. } => status,
        other => panic!("expected DaemonMessage::McpRegistrationStatus, got {other:?}"),
    }
}

#[test]
fn an_unrecognized_state_decodes_as_unknown_without_failing_the_frame() {
    let status: McpRegistrationStatusInfo = decoded_status(&status_frame("migrating"));
    assert_eq!(
        status.state,
        McpRegistrationState::Unknown,
        "an unrecognized state string must decode to McpRegistrationState::Unknown rather than \
         failing the whole frame"
    );
}

#[test]
fn each_documented_state_value_decodes_to_its_own_variant_and_round_trips() {
    for (wire, expected) in [
        ("registered", McpRegistrationState::Registered),
        ("differs", McpRegistrationState::Differs),
        ("not_registered", McpRegistrationState::NotRegistered),
        ("not_installed", McpRegistrationState::NotInstalled),
        ("unknown", McpRegistrationState::Unknown),
    ] {
        let status: McpRegistrationStatusInfo = decoded_status(&status_frame(wire));
        assert_eq!(
            status.state, expected,
            "the documented state value {wire:?} must decode to {expected:?}"
        );
        assert_eq!(
            as_json(&expected),
            serde_json::json!(wire),
            "{expected:?} must encode back to {wire:?}"
        );
    }
}

// =============================================================================
// Optional fields: `null` and an omitted key both mean "not carried".
// =============================================================================

#[derive(Debug, Clone, Copy)]
enum Absent {
    Null,
    Omitted,
}

/// Encodes `message`, makes `key` of the object at `pointer` absent, and
/// decodes the result.
fn decode_with_absent(
    message: &DaemonMessage,
    pointer: &str,
    key: &str,
    absent: Absent,
) -> DaemonMessage {
    let mut value = as_json(message);
    let object = value
        .pointer_mut(pointer)
        .and_then(serde_json::Value::as_object_mut)
        .unwrap_or_else(|| panic!("{pointer:?} must name an object in {message:?}"));
    assert!(
        object.contains_key(key),
        "the sample must carry {key:?} at {pointer:?} before it is made absent"
    );
    match absent {
        Absent::Null => {
            object.insert(key.to_string(), serde_json::Value::Null);
        }
        Absent::Omitted => {
            object.remove(key);
        }
    }
    decode_daemon_message(&to_bytes(&value)).unwrap_or_else(|err| {
        panic!("a frame with {key:?} at {pointer:?} made {absent:?} must decode, got {err:?}")
    })
}

fn assert_absent_is_carried_as_none(
    message: &DaemonMessage,
    pointer: &str,
    key: &str,
    is_none: impl Fn(&DaemonMessage) -> bool,
) {
    for absent in [Absent::Null, Absent::Omitted] {
        let decoded = decode_with_absent(message, pointer, key, absent);
        assert!(
            is_none(&decoded),
            "{key:?} at {pointer:?} made {absent:?} must decode as None, got {decoded:?}"
        );
    }
}

fn listed_rows(message: &DaemonMessage) -> &[McpRegistrationRow] {
    match message {
        DaemonMessage::McpRegistrationsListed { rows, .. } => rows,
        other => panic!("expected DaemonMessage::McpRegistrationsListed, got {other:?}"),
    }
}

fn pushed_status(message: &DaemonMessage) -> &McpRegistrationStatusInfo {
    match message {
        DaemonMessage::McpRegistrationStatus { status, .. } => status,
        other => panic!("expected DaemonMessage::McpRegistrationStatus, got {other:?}"),
    }
}

#[test]
fn an_absent_bridge_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_listed(), "", "bridge", |message| {
        matches!(
            message,
            DaemonMessage::McpRegistrationsListed { bridge: None, .. }
        )
    });
}

#[test]
fn an_absent_row_group_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_listed(), "/rows/0", "group", |message| {
        let row: &McpRegistrationRow = &listed_rows(message)[0];
        row.group.is_none()
    });
}

#[test]
fn an_absent_row_toml_set_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_listed(), "/rows/0", "toml_set", |message| {
        let row: &McpRegistrationRow = &listed_rows(message)[0];
        row.toml_set.is_none()
    });
}

#[test]
fn an_absent_status_reason_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_status_push(), "/status", "reason", |message| {
        pushed_status(message).reason.is_none()
    });
}

#[test]
fn an_absent_status_add_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_status_push(), "/status", "add", |message| {
        pushed_status(message).add.is_none()
    });
}

#[test]
fn an_absent_status_remove_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_status_push(), "/status", "remove", |message| {
        pushed_status(message).remove.is_none()
    });
}

#[test]
fn an_absent_step_exit_code_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_result(), "/steps/0", "exit_code", |message| {
        match message {
            DaemonMessage::McpRegistrationResult { steps, .. } => {
                let step: &McpRegistrationStep = &steps[0];
                step.exit_code.is_none()
            }
            other => panic!("expected DaemonMessage::McpRegistrationResult, got {other:?}"),
        }
    });
}

#[test]
fn an_absent_result_error_is_carried_as_none() {
    assert_absent_is_carried_as_none(&sample_result(), "", "error", |message| {
        matches!(
            message,
            DaemonMessage::McpRegistrationResult { error: None, .. }
        )
    });
}

#[test]
fn an_absent_status_refused_by_claim_is_carried_as_none() {
    assert_absent_is_carried_as_none(
        &sample_status_push(),
        "/status",
        "refused_by_claim",
        |message| pushed_status(message).refused_by_claim.is_none(),
    );
}

#[test]
fn a_status_refused_by_a_claim_carries_the_claim_pointer_through_a_round_trip() {
    let decoded = decoded_status(&to_bytes(&serde_json::json!({
        "type": "mcp_registration_status",
        "request_id": 1,
        "status": {
            "profile": "cli-host",
            "state": "differs",
            "differs_in": ["/managed_hooks_path"],
            "reason": null,
            "add": null,
            "remove": null,
            "refused_by_claim": "/managed_hooks_path",
        },
    })));
    assert_eq!(
        decoded.refused_by_claim.as_deref(),
        Some("/managed_hooks_path"),
        "a status Register / Update would refuse must name the claim that refuses it"
    );
    assert_eq!(
        as_json(&decoded)["refused_by_claim"],
        serde_json::json!("/managed_hooks_path"),
        "the claim pointer must be written back under the same key"
    );
}

// =============================================================================
// A registration that has no add / remove command of its own.
// =============================================================================

fn commandless_listed_frame() -> Vec<u8> {
    to_bytes(&serde_json::json!({
        "type": "mcp_registrations_listed",
        "request_id": 7,
        "bridge": null,
        "rows": [{
            "profile": "file-host",
            "display_name": "File host",
            "group": null,
            "toml_set": null,
            "entries": [
                {
                    "file": ".cli/settings.json",
                    "name": "/mcpServers/yagura",
                    "content": "{\"command\":\"bridge\"}",
                },
                {
                    "file": ".cli/hooks.json",
                    "name": "/hooks",
                    "content": "{}",
                },
            ],
        }],
    }))
}

#[test]
fn a_row_without_add_and_remove_decodes_and_writes_neither_key_back() {
    let decoded = decode_daemon_message(&commandless_listed_frame())
        .expect("a row with no add / remove command must decode");
    let row: &McpRegistrationRow = &listed_rows(&decoded)[0];
    assert!(
        row.add.is_none(),
        "an absent add must decode as none, got {row:?}"
    );
    assert!(
        row.remove.is_none(),
        "an absent remove must decode as none, got {row:?}"
    );

    let written = as_json(&decoded);
    let object = written
        .pointer("/rows/0")
        .and_then(serde_json::Value::as_object)
        .expect("the row is an object");
    for key in ["add", "remove", "hook_entries"] {
        assert!(
            !object.contains_key(key),
            "a row without a command must not write {key:?}, got {written}"
        );
    }
    assert_eq!(
        decode_daemon_message(&encode_daemon_message(&decoded)).expect("must round trip"),
        decoded
    );
}

#[test]
fn a_row_carries_what_registration_writes_under_entries() {
    let decoded =
        decode_daemon_message(&commandless_listed_frame()).expect("a row with entries must decode");
    let written = as_json(&decoded);
    assert_eq!(
        written.pointer("/rows/0/entries"),
        Some(&serde_json::json!([
            {
                "file": ".cli/settings.json",
                "name": "/mcpServers/yagura",
                "content": "{\"command\":\"bridge\"}",
            },
            {
                "file": ".cli/hooks.json",
                "name": "/hooks",
                "content": "{}",
            },
        ])),
        "every file, pointer and value registration writes must survive under entries, \
         got {written}"
    );
}

// =============================================================================
// Revision (also see required_revision.rs).
// =============================================================================

#[test]
fn both_requests_require_the_mcp_registration_threshold() {
    let messages = [
        ClientMessage::ListMcpRegistrations { request_id: 1 },
        ClientMessage::RunMcpRegistration {
            profile: "cli-host".to_string(),
            action: McpRegistrationAction::Register,
            request_id: 2,
        },
    ];
    for msg in messages {
        assert_eq!(
            required_protocol_revision(&msg),
            MCP_REGISTRATION_MIN_REVISION,
            "{msg:?} must require the MCP registration threshold"
        );
    }
}
