#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire contract for `get_daemon_health` and its `daemon_health` answer.

use yagura_protocol::{
    ClientMessage, DAEMON_HEALTH_MIN_REVISION, DaemonMessage, DaemonProblem, DaemonProblemKind,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
    required_protocol_revision,
};

#[test]
fn get_daemon_health_round_trips_under_its_snake_case_tag() {
    let bytes = encode_client_message(&ClientMessage::GetDaemonHealth);
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["type"], "get_daemon_health");
    assert_eq!(
        decode_client_message(&bytes).unwrap(),
        ClientMessage::GetDaemonHealth
    );
    assert_eq!(
        required_protocol_revision(&ClientMessage::GetDaemonHealth),
        DAEMON_HEALTH_MIN_REVISION
    );
}

#[test]
fn daemon_health_round_trips_with_snake_case_kinds() {
    let message = DaemonMessage::DaemonHealth {
        problems: vec![
            DaemonProblem {
                kind: DaemonProblemKind::WindowsUnavailable,
                detail: "missing".to_string(),
            },
            DaemonProblem {
                kind: DaemonProblemKind::InitialWindowFailed,
                detail: "boom".to_string(),
            },
        ],
    };
    let bytes = encode_daemon_message(&message);
    let value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(value["type"], "daemon_health");
    assert_eq!(value["problems"][0]["kind"], "windows_unavailable");
    assert_eq!(value["problems"][1]["kind"], "initial_window_failed");
    assert_eq!(decode_daemon_message(&bytes).unwrap(), message);
}

#[test]
fn an_unknown_problem_kind_fails_the_frame() {
    let bytes = br#"{"type":"daemon_health","problems":[{"kind":"nope","detail":""}]}"#;
    assert!(decode_daemon_message(bytes).is_err());
}
