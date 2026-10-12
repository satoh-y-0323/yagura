#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire contracts for the MCP channel of a window (wire revision 8): whether
//! a window's MCP bridge has identified itself (`mcp_connected`), the windows
//! a caller is still waiting on when `wait_for_report` returns no report
//! (`awaiting`), and the environment variable names the bridge reads. The
//! launch provenance of a restore candidate is measured with its other
//! optional keys in `restore_protocol.rs`.
//!
//! Every new field is optional and follows R2 in the crate docs: a frame
//! from a peer predating the field decodes to `None`, never to a value.

use yagura_protocol::{
    AwaitingWindow, DaemonMessage, INSTANCE_ENV, MCP_BRIDGE_ENV, MCP_CHANNEL_ENV_VARS,
    STATUS_TOKEN_ENV, WINDOW_ID_ENV, WindowInfo, WindowStatus, decode_daemon_message,
    encode_daemon_message,
};

fn to_value(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a wire type must serialize")
}

fn encoded_value(message: &DaemonMessage) -> serde_json::Value {
    serde_json::from_slice(&encode_daemon_message(message)).expect("encoded frame must be JSON")
}

// =============================================================================
// WindowInfo.mcp_connected / DaemonMessage::WindowStatus.mcp_connected
// =============================================================================

fn window_info(mcp_connected: Option<bool>) -> WindowInfo {
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
        status_rev: None,
        mcp_connected,
        status_reports_missing: None,
    }
}

fn window_status_response(mcp_connected: Option<bool>) -> DaemonMessage {
    DaemonMessage::WindowStatus {
        window_id: "w1".to_string(),
        status: WindowStatus::Running,
        since_ms: 10,
        last_output_summary: None,
        exit_code: None,
        unknown_reason: None,
        detection_warning: None,
        mcp_connected,
    }
}

#[test]
fn window_info_mcp_connected_round_trips_true_and_false() {
    for flag in [true, false] {
        let info = window_info(Some(flag));
        let value = to_value(&info);
        assert_eq!(
            value.get("mcp_connected"),
            Some(&serde_json::json!(flag)),
            "the wire key must be exactly \"mcp_connected\", got: {value}"
        );
        let back: WindowInfo = serde_json::from_value(value).expect("must decode");
        assert_eq!(back, info);
    }
}

#[test]
fn window_status_response_omits_mcp_connected_when_none() {
    let value = encoded_value(&window_status_response(None));
    assert!(
        value.get("mcp_connected").is_none(),
        "mcp_connected: None must be omitted from the wire, got: {value}"
    );
}

#[test]
fn window_status_response_mcp_connected_round_trips_true_and_false() {
    for flag in [true, false] {
        let message = window_status_response(Some(flag));
        let value = encoded_value(&message);
        assert_eq!(
            value.get("mcp_connected"),
            Some(&serde_json::json!(flag)),
            "the wire key must be exactly \"mcp_connected\", got: {value}"
        );
        let decoded = decode_daemon_message(&encode_daemon_message(&message))
            .expect("window_status must round trip");
        assert_eq!(decoded, message);
    }
}

#[test]
fn window_status_response_decodes_a_frame_without_mcp_connected_as_none_not_false() {
    let body = br#"{"type":"window_status","window_id":"w1","status":"running","since_ms":10,"last_output_summary":null,"exit_code":null}"#;
    let decoded = decode_daemon_message(body).expect("a frame from an older daemon must decode");
    let DaemonMessage::WindowStatus { mcp_connected, .. } = decoded else {
        panic!("expected window_status, got: {decoded:?}");
    };
    assert_eq!(mcp_connected, None);
}

// =============================================================================
// DaemonMessage::Reports.awaiting and AwaitingWindow
// =============================================================================

fn awaiting_window(mcp_connected: Option<bool>) -> AwaitingWindow {
    AwaitingWindow {
        window_id: "w2".to_string(),
        name: "child".to_string(),
        status: WindowStatus::WaitingInput,
        mcp_connected,
    }
}

#[test]
fn reports_omits_awaiting_when_none() {
    let value = encoded_value(&DaemonMessage::Reports {
        reports: Vec::new(),
        awaiting: None,
    });
    assert!(
        value.get("awaiting").is_none(),
        "awaiting: None must be omitted from the wire, got: {value}"
    );
}

#[test]
fn reports_sends_an_empty_awaiting_list_as_an_empty_array() {
    let message = DaemonMessage::Reports {
        reports: Vec::new(),
        awaiting: Some(Vec::new()),
    };
    let value = encoded_value(&message);
    assert_eq!(
        value.get("awaiting"),
        Some(&serde_json::json!([])),
        "Some(empty) must travel as an empty array, got: {value}"
    );
    let decoded =
        decode_daemon_message(&encode_daemon_message(&message)).expect("reports must round trip");
    assert_eq!(decoded, message);
}

#[test]
fn reports_decodes_a_frame_without_awaiting_as_none_not_empty() {
    let body = br#"{"type":"reports","reports":[]}"#;
    let decoded = decode_daemon_message(body).expect("a frame from an older daemon must decode");
    let DaemonMessage::Reports { awaiting, .. } = decoded else {
        panic!("expected reports, got: {decoded:?}");
    };
    assert_eq!(awaiting, None);
}

#[test]
fn reports_awaiting_round_trips_with_every_mcp_connected_state() {
    let message = DaemonMessage::Reports {
        reports: Vec::new(),
        awaiting: Some(vec![
            awaiting_window(Some(true)),
            awaiting_window(Some(false)),
            awaiting_window(None),
        ]),
    };
    let value = encoded_value(&message);
    let elements = value
        .get("awaiting")
        .and_then(serde_json::Value::as_array)
        .expect("awaiting must be an array");
    assert_eq!(elements.len(), 3);
    assert_eq!(
        elements[0],
        serde_json::json!({"window_id":"w2","name":"child","status":"waiting_input","mcp_connected":true})
    );
    assert_eq!(
        elements[1].get("mcp_connected"),
        Some(&serde_json::json!(false))
    );
    assert!(
        elements[2].get("mcp_connected").is_none(),
        "mcp_connected: None must be omitted from an awaiting element, got: {}",
        elements[2]
    );
    let decoded =
        decode_daemon_message(&encode_daemon_message(&message)).expect("reports must round trip");
    assert_eq!(decoded, message);
}

#[test]
fn awaiting_window_decodes_an_element_without_mcp_connected_as_none() {
    let element: AwaitingWindow =
        serde_json::from_str(r#"{"window_id":"w2","name":"child","status":"idle"}"#)
            .expect("an element without mcp_connected must decode");
    assert_eq!(element.mcp_connected, None);
    assert_eq!(element.status, WindowStatus::Idle);
}

#[test]
fn awaiting_window_rejects_an_element_missing_a_required_key() {
    for missing in ["window_id", "name", "status"] {
        let mut element = serde_json::json!({"window_id":"w2","name":"child","status":"idle"});
        element
            .as_object_mut()
            .expect("literal is an object")
            .remove(missing);
        assert!(
            serde_json::from_value::<AwaitingWindow>(element).is_err(),
            "an awaiting element without {missing} must fail to decode"
        );
    }
}

// =============================================================================
// Environment variable names of the MCP channel
// =============================================================================

#[test]
fn mcp_channel_env_vars_are_window_id_status_token_and_instance_in_order() {
    assert_eq!(
        MCP_CHANNEL_ENV_VARS,
        ["YAGURA_WINDOW_ID", "YAGURA_STATUS_TOKEN", "YAGURA_INSTANCE"]
    );
    assert_eq!(
        MCP_CHANNEL_ENV_VARS,
        [WINDOW_ID_ENV, STATUS_TOKEN_ENV, INSTANCE_ENV]
    );
}

#[test]
fn mcp_bridge_env_names_the_bridge_path_variable() {
    assert_eq!(MCP_BRIDGE_ENV, "YAGURA_MCP_BRIDGE");
}
