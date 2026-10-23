//! How `reply_for` and `dispatch` treat the MCP registration daemon messages.
//!
//! The list and run acknowledgements answer a request this client is waiting
//! on. The per-row status and the operation result are pushes that arrive
//! afterward, so they complete no request and become WebView events instead.

use serde_json::json;
use yagura_protocol::{
    DaemonMessage, McpRegistrationAction, McpRegistrationCommand, McpRegistrationRow,
    McpRegistrationState, McpRegistrationStatusInfo, McpRegistrationStep, McpRegistrationTomlSet,
    McpRegistrationTomlValue,
};

use crate::commands::{McpRegistrationRequestedReply, McpRegistrationsListedReply};
use crate::events::{
    EVENT_MCP_REGISTRATION_RESULT, EVENT_MCP_REGISTRATION_STATUS, McpRegistrationResultPayload,
    McpRegistrationStatusPayload,
};
use crate::ipc_client::{PendingKind, Reply};

use super::{McpRegistrationEvent, mcp_registration_event_for, reply_for};

fn sample_command(argv: &[&str], program_at: u32) -> McpRegistrationCommand {
    McpRegistrationCommand {
        argv: argv.iter().map(|part| (*part).to_owned()).collect(),
        program_at,
    }
}

fn sample_rows() -> Vec<McpRegistrationRow> {
    vec![
        McpRegistrationRow {
            profile: "sample-cli".to_owned(),
            display_name: "Sample CLI".to_owned(),
            group: None,
            add: Some(sample_command(
                &["sample-cli", "mcp", "add", "yagura", "/bridge"],
                0,
            )),
            remove: Some(sample_command(
                &["sample-cli", "mcp", "remove", "yagura"],
                0,
            )),
            toml_set: None,
            entries: Vec::new(),
        },
        McpRegistrationRow {
            profile: "sample-cli-wsl".to_owned(),
            display_name: "Sample CLI (WSL)".to_owned(),
            group: Some("wsl".to_owned()),
            add: Some(sample_command(
                &["wsl", "--", "sample-cli", "mcp", "add", "{bridge}"],
                2,
            )),
            remove: Some(sample_command(
                &["wsl", "--", "sample-cli", "mcp", "remove"],
                2,
            )),
            toml_set: Some(McpRegistrationTomlSet {
                file: ".sample/config.toml".to_owned(),
                table: vec!["mcp_servers".to_owned(), "yagura".to_owned()],
                values: vec![McpRegistrationTomlValue {
                    key: "tool_timeout_sec".to_owned(),
                    value: "600".to_owned(),
                }],
            }),
            entries: Vec::new(),
        },
    ]
}

fn sample_status() -> McpRegistrationStatusInfo {
    McpRegistrationStatusInfo {
        profile: "sample-cli-wsl".to_owned(),
        state: McpRegistrationState::Differs,
        differs_in: vec!["command".to_owned(), "tool_timeout_sec".to_owned()],
        reason: Some("Registered with a different bridge.".to_owned()),
        add: Some(sample_command(
            &["wsl", "--", "sample-cli", "mcp", "add", "/mnt/c/bridge"],
            2,
        )),
        remove: Some(sample_command(
            &["wsl", "--", "sample-cli", "mcp", "remove"],
            2,
        )),
        refused_by_claim: None,
    }
}

/// The JSON the WebView narrows `sample_status` from.
fn sample_status_json() -> serde_json::Value {
    json!({
        "profile": "sample-cli-wsl",
        "state": "differs",
        "differs_in": ["command", "tool_timeout_sec"],
        "reason": "Registered with a different bridge.",
        "add": {
            "argv": ["wsl", "--", "sample-cli", "mcp", "add", "/mnt/c/bridge"],
            "program_at": 2,
        },
        "remove": {
            "argv": ["wsl", "--", "sample-cli", "mcp", "remove"],
            "program_at": 2,
        },
        "refused_by_claim": null,
    })
}

fn sample_steps() -> Vec<McpRegistrationStep> {
    vec![
        McpRegistrationStep {
            argv: vec![
                "sample-cli".to_owned(),
                "mcp".to_owned(),
                "remove".to_owned(),
            ],
            program_at: 0,
            exit_code: Some(0),
            output: "removed".to_owned(),
        },
        McpRegistrationStep {
            argv: vec!["sample-cli".to_owned(), "mcp".to_owned(), "add".to_owned()],
            program_at: 0,
            exit_code: Some(1),
            output: "add failed".to_owned(),
        },
        McpRegistrationStep {
            argv: Vec::new(),
            program_at: 0,
            exit_code: None,
            output: "set tool_timeout_sec".to_owned(),
        },
    ]
}

fn status_push() -> DaemonMessage {
    DaemonMessage::McpRegistrationStatus {
        request_id: 41,
        status: sample_status(),
    }
}

fn result_push() -> DaemonMessage {
    DaemonMessage::McpRegistrationResult {
        request_id: 42,
        profile: "sample-cli-wsl".to_owned(),
        action: McpRegistrationAction::Update,
        steps: sample_steps(),
        error: Some("The add command failed.".to_owned()),
        status: sample_status(),
    }
}

#[test]
fn reply_for_classifies_mcp_registrations_listed_as_a_list_mcp_registrations_reply() {
    let rows = sample_rows();
    let result = reply_for(DaemonMessage::McpRegistrationsListed {
        request_id: 17,
        bridge: Some("/opt/yagura/yagura-mcp".to_owned()),
        rows: rows.clone(),
    });
    match result {
        Some((PendingKind::ListMcpRegistrations, Reply::McpRegistrationsListed(got))) => {
            let got: McpRegistrationsListedReply = got;
            assert_eq!(got.request_id, 17);
            assert_eq!(got.bridge.as_deref(), Some("/opt/yagura/yagura-mcp"));
            assert_eq!(got.rows, rows);
        }
        Some(_) => panic!("expected a ListMcpRegistrations reply, got another classification"),
        None => panic!("expected a ListMcpRegistrations reply, got no classification"),
    }
}

#[test]
fn reply_for_carries_a_missing_bridge_on_the_list_reply() {
    let result = reply_for(DaemonMessage::McpRegistrationsListed {
        request_id: 18,
        bridge: None,
        rows: Vec::new(),
    });
    match result {
        Some((PendingKind::ListMcpRegistrations, Reply::McpRegistrationsListed(got))) => {
            let got: McpRegistrationsListedReply = got;
            assert_eq!(got.request_id, 18);
            assert_eq!(got.bridge, None);
            assert!(got.rows.is_empty());
        }
        Some(_) => panic!("expected a ListMcpRegistrations reply, got another classification"),
        None => panic!("expected a ListMcpRegistrations reply, got no classification"),
    }
}

#[test]
fn list_reply_serializes_to_the_snake_case_shape_the_webview_narrows() {
    let result = reply_for(DaemonMessage::McpRegistrationsListed {
        request_id: 19,
        bridge: Some("/bridge".to_owned()),
        rows: sample_rows(),
    });
    let Some((PendingKind::ListMcpRegistrations, Reply::McpRegistrationsListed(got))) = result
    else {
        panic!("expected a ListMcpRegistrations reply");
    };
    let got: McpRegistrationsListedReply = got;
    let value: serde_json::Value = serde_json::to_value(&got).unwrap();
    assert_eq!(value["request_id"], json!(19));
    assert_eq!(value["bridge"], json!("/bridge"));
    assert_eq!(value["rows"][1]["display_name"], json!("Sample CLI (WSL)"));
    assert_eq!(value["rows"][1]["group"], json!("wsl"));
    assert_eq!(value["rows"][1]["add"]["program_at"], json!(2));
    assert_eq!(
        value["rows"][1]["toml_set"]["values"][0]["key"],
        json!("tool_timeout_sec")
    );
    assert_eq!(value["rows"][0]["toml_set"], serde_json::Value::Null);
}

#[test]
fn reply_for_classifies_each_action_of_mcp_registration_requested_as_a_run_reply() {
    let cases: [(McpRegistrationAction, &str); 3] = [
        (McpRegistrationAction::Register, "register"),
        (McpRegistrationAction::Update, "update"),
        (McpRegistrationAction::Remove, "remove"),
    ];
    for (action, expected) in cases {
        let result = reply_for(DaemonMessage::McpRegistrationRequested {
            request_id: 23,
            profile: "sample-cli".to_owned(),
            action,
        });
        match result {
            Some((PendingKind::RunMcpRegistration, Reply::McpRegistrationRequested(got))) => {
                let got: McpRegistrationRequestedReply = got;
                assert_eq!(got.request_id, 23);
                assert_eq!(got.profile, "sample-cli");
                let got_action: String = got.action;
                assert_eq!(
                    got_action, expected,
                    "the action must travel as its wire string"
                );
            }
            Some(_) => panic!("expected a RunMcpRegistration reply, got another classification"),
            None => panic!("expected a RunMcpRegistration reply, got no classification"),
        }
    }
}

#[test]
fn reply_for_classifies_the_two_mcp_registration_pushes_as_answering_no_request() {
    assert!(reply_for(status_push()).is_none());
    assert!(reply_for(result_push()).is_none());
}

#[test]
fn status_push_becomes_the_status_event_with_the_whole_status() {
    let event = mcp_registration_event_for(status_push());
    let Some(event) = event else {
        panic!("a status push must become an event");
    };
    let event: McpRegistrationEvent = event;
    assert_eq!(event.name(), EVENT_MCP_REGISTRATION_STATUS);
    match event {
        McpRegistrationEvent::Status(payload) => {
            let payload: McpRegistrationStatusPayload = payload;
            let value: serde_json::Value = serde_json::to_value(&payload).unwrap();
            assert_eq!(
                value,
                json!({
                    "request_id": 41,
                    "status": sample_status_json(),
                })
            );
        }
        McpRegistrationEvent::Result(_) => panic!("a status push must not become a result event"),
    }
}

#[test]
fn result_push_becomes_the_result_event_with_its_steps_in_order() {
    let event = mcp_registration_event_for(result_push());
    let Some(event) = event else {
        panic!("a result push must become an event");
    };
    let event: McpRegistrationEvent = event;
    assert_eq!(event.name(), EVENT_MCP_REGISTRATION_RESULT);
    match event {
        McpRegistrationEvent::Result(payload) => {
            let payload: McpRegistrationResultPayload = payload;
            let value: serde_json::Value = serde_json::to_value(&payload).unwrap();
            assert_eq!(
                value,
                json!({
                    "request_id": 42,
                    "profile": "sample-cli-wsl",
                    "action": "update",
                    "steps": [
                        {
                            "argv": ["sample-cli", "mcp", "remove"],
                            "program_at": 0,
                            "exit_code": 0,
                            "output": "removed",
                        },
                        {
                            "argv": ["sample-cli", "mcp", "add"],
                            "program_at": 0,
                            "exit_code": 1,
                            "output": "add failed",
                        },
                        {
                            "argv": [],
                            "program_at": 0,
                            "exit_code": null,
                            "output": "set tool_timeout_sec",
                        },
                    ],
                    "error": "The add command failed.",
                    "status": sample_status_json(),
                })
            );
        }
        McpRegistrationEvent::Status(_) => panic!("a result push must not become a status event"),
    }
}

#[test]
fn result_push_without_an_error_carries_a_null_error() {
    let event = mcp_registration_event_for(DaemonMessage::McpRegistrationResult {
        request_id: 43,
        profile: "sample-cli".to_owned(),
        action: McpRegistrationAction::Register,
        steps: Vec::new(),
        error: None,
        status: sample_status(),
    });
    let Some(McpRegistrationEvent::Result(payload)) = event else {
        panic!("a result push must become a result event");
    };
    let payload: McpRegistrationResultPayload = payload;
    let value: serde_json::Value = serde_json::to_value(&payload).unwrap();
    assert_eq!(value["error"], serde_json::Value::Null);
    assert_eq!(value["action"], json!("register"));
    assert_eq!(value["steps"], json!([]));
}

#[test]
fn immediate_replies_and_unrelated_messages_become_no_mcp_registration_event() {
    assert!(
        mcp_registration_event_for(DaemonMessage::McpRegistrationsListed {
            request_id: 1,
            bridge: None,
            rows: Vec::new(),
        })
        .is_none()
    );
    assert!(
        mcp_registration_event_for(DaemonMessage::McpRegistrationRequested {
            request_id: 2,
            profile: "sample-cli".to_owned(),
            action: McpRegistrationAction::Remove,
        })
        .is_none()
    );
    assert!(mcp_registration_event_for(DaemonMessage::Pong).is_none());
}

#[test]
fn mcp_registration_event_names_match_the_webview_listeners() {
    assert_eq!(
        EVENT_MCP_REGISTRATION_STATUS,
        "session://mcp-registration-status"
    );
    assert_eq!(
        EVENT_MCP_REGISTRATION_RESULT,
        "session://mcp-registration-result"
    );
}
