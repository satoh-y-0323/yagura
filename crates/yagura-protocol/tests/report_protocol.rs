#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for task reports (wire revision 4): the two new
//! `ClientMessage` variants (`send_report` / `wait_for_report`), the two new
//! `DaemonMessage` variants (`report_sent` / `reports`), the `ReportOutcome`
//! enum, the payload types they nest (`ReportTaskRef`, `ReceivedReport`,
//! `TaskReport`), and `PlanTaskInfo.report`.

use yagura_protocol::{
    ClientMessage, DaemonMessage, PROTOCOL_REVISION_BASELINE, REPORT_PATH_MAX_BYTES,
    REPORT_SUMMARY_MAX_BYTES, ReceivedReport, ReportOutcome, ReportTaskRef, TaskReport,
    decode_client_message, decode_daemon_message, encode_client_message, encode_daemon_message,
    required_protocol_revision,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a report payload type must serialize")
}

// =============================================================================
// send_report / wait_for_report round trip through decode_client_message
// =============================================================================

#[test]
fn send_report_round_trips_with_a_path() {
    let decoded = ClientMessage::SendReport {
        outcome: ReportOutcome::Succeeded,
        summary: "finished the work".to_string(),
        path: Some("/tmp/result.txt".to_string()),
    };
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("send_report")
    );
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("send_report must round trip"),
        decoded
    );
}

#[test]
fn send_report_missing_path_decodes_to_none() {
    let body = br#"{"type":"send_report","outcome":"failed","summary":"it broke"}"#;
    let decoded = decode_client_message(body).expect("send_report without path must decode");
    assert_eq!(
        decoded,
        ClientMessage::SendReport {
            outcome: ReportOutcome::Failed,
            summary: "it broke".to_string(),
            path: None,
        },
        "an omitted path must decode to None rather than failing the frame"
    );
}

#[test]
fn wait_for_report_round_trips() {
    let decoded = ClientMessage::WaitForReport { timeout_sec: 30 };
    let value = as_json(&decoded);
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some("wait_for_report")
    );
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("wait_for_report must round trip"),
        decoded
    );
}

// =============================================================================
// report_sent always carries delivered_to and task, null when absent
// =============================================================================

#[test]
fn report_sent_carries_both_keys_present_when_absent() {
    let decoded = DaemonMessage::ReportSent {
        delivered_to: None,
        task: None,
    };
    let value = as_json(&decoded);
    let object = value
        .as_object()
        .expect("report_sent must serialize as an object");
    assert_eq!(
        object.get("delivered_to"),
        Some(&serde_json::Value::Null),
        "delivered_to must be present as null when absent, got: {value}"
    );
    assert_eq!(
        object.get("task"),
        Some(&serde_json::Value::Null),
        "task must be present as null when absent, got: {value}"
    );
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("report_sent must round trip"),
        decoded
    );
}

#[test]
fn report_sent_task_nests_plan_id_and_task_id() {
    let decoded = DaemonMessage::ReportSent {
        delivered_to: Some("w1-lead".to_string()),
        task: Some(ReportTaskRef {
            plan_id: "p-sample".to_string(),
            task_id: "t1".to_string(),
        }),
    };
    let value = as_json(&decoded);
    assert_eq!(
        value
            .pointer("/task/plan_id")
            .and_then(serde_json::Value::as_str),
        Some("p-sample"),
        "task must nest ReportTaskRef verbatim, got: {value}"
    );
    assert_eq!(
        value
            .pointer("/task/task_id")
            .and_then(serde_json::Value::as_str),
        Some("t1")
    );
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("report_sent must round trip"),
        decoded
    );
}

// =============================================================================
// Reports carries an array; every element always has its keys
// =============================================================================

#[test]
fn reports_elements_always_carry_their_keys_null_when_absent() {
    let decoded = DaemonMessage::Reports {
        reports: vec![ReceivedReport {
            from_window_id: "w2-child".to_string(),
            from_name: "child".to_string(),
            outcome: ReportOutcome::Succeeded,
            summary: "done".to_string(),
            path: None,
            task: None,
            at_ms: 1_700_000_000_000,
        }],
        awaiting: None,
    };
    let value = as_json(&decoded);
    let element = value
        .pointer("/reports/0")
        .and_then(serde_json::Value::as_object)
        .expect("reports/0 must be an object");
    for key in [
        "from_window_id",
        "from_name",
        "outcome",
        "summary",
        "path",
        "task",
        "at_ms",
    ] {
        assert!(
            element.contains_key(key),
            "reports element must always carry the key \"{key}\", got: {value}"
        );
    }
    assert_eq!(element.get("path"), Some(&serde_json::Value::Null));
    assert_eq!(element.get("task"), Some(&serde_json::Value::Null));
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("reports must round trip"),
        decoded
    );
}

// =============================================================================
// ReportOutcome wire values are exactly "succeeded" and "failed"; no other
// value decodes
// =============================================================================

#[test]
fn report_outcome_round_trips_both_wire_values() {
    let pairs = [
        (ReportOutcome::Succeeded, "succeeded"),
        (ReportOutcome::Failed, "failed"),
    ];
    for (value, wire) in pairs {
        let encoded = serde_json::to_value(value).expect("ReportOutcome must serialize");
        assert_eq!(encoded, serde_json::json!(wire));
        let decoded: ReportOutcome =
            serde_json::from_value(encoded).expect("ReportOutcome must deserialize");
        assert_eq!(decoded, value);
    }
}

#[test]
fn a_frame_carrying_an_unknown_outcome_value_fails_to_decode() {
    let body = br#"{"type":"send_report","outcome":"done","summary":"x"}"#;
    let result = decode_client_message(body);
    assert!(
        result.is_err(),
        "a send_report frame with an outcome outside {{succeeded, failed}} must fail the whole \
         frame's decode, got {result:?}"
    );
}

// =============================================================================
// PlanTaskInfo.report -- key absent (not null) when None
// =============================================================================

#[test]
fn plan_task_info_decodes_a_report_object_whose_path_is_null() {
    let plan_task_json = serde_json::json!({
        "task_id": "t1",
        "title": "write the tests",
        "role": null,
        "depends_on": [],
        "writes": [],
        "wave": 0,
        "parent": null,
        "user": null,
        "resolved": { "source": "unassigned", "assignment": null },
        "window_id": null,
        "report": {
            "outcome": "succeeded",
            "summary": "shipped",
            "path": null,
            "reported_at": 1_700_000_000
        }
    });
    let decoded: yagura_protocol::PlanTaskInfo = serde_json::from_value(plan_task_json)
        .expect("a PlanTaskInfo carrying a report object must decode");
    assert_eq!(
        decoded.report,
        Some(TaskReport {
            outcome: ReportOutcome::Succeeded,
            summary: "shipped".to_string(),
            path: None,
            reported_at: 1_700_000_000,
        })
    );

    let reencoded = as_json(&decoded);
    let object = reencoded
        .as_object()
        .expect("PlanTaskInfo must serialize as an object");
    assert!(
        object.contains_key("report"),
        "a PlanTaskInfo with Some(report) must carry the \"report\" key, got: {reencoded}"
    );
}

#[test]
fn plan_task_info_without_a_report_field_decodes_to_none() {
    // Simulates a pre-report daemon's frame: the "report" key is absent
    // entirely, not present with a null value.
    let plan_task_json = serde_json::json!({
        "task_id": "t1",
        "title": "write the tests",
        "role": null,
        "depends_on": [],
        "writes": [],
        "wave": 0,
        "parent": null,
        "user": null,
        "resolved": { "source": "unassigned", "assignment": null },
        "window_id": null
    });
    let decoded: yagura_protocol::PlanTaskInfo = serde_json::from_value(plan_task_json)
        .expect("a PlanTaskInfo without a report key must still decode (older-daemon frame)");
    assert_eq!(decoded.report, None);
}

#[test]
fn plan_task_info_report_none_key_is_absent_not_null_on_the_wire() {
    let task = yagura_protocol::PlanTaskInfo {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: None,
        depends_on: vec![],
        writes: vec![],
        wave: 0,
        parent: None,
        user: None,
        resolved: yagura_protocol::ResolvedAssignment {
            source: yagura_protocol::AssignmentSource::Unassigned,
            assignment: None,
        },
        window_id: None,
        report: None,
        instruction: None,
        instruction_notice: None,
        review_mark: None,
    };
    let value = as_json(&task);
    let object = value
        .as_object()
        .expect("PlanTaskInfo must serialize as an object");
    assert!(
        !object.contains_key("report"),
        "a PlanTaskInfo with report: None must omit the \"report\" key entirely (unlike this \
         struct's other Option fields, which serialize as null), got: {value}"
    );
}

// =============================================================================
// Public byte-limit constants
// =============================================================================

#[test]
fn report_summary_and_path_byte_limits_are_fixed() {
    assert_eq!(REPORT_SUMMARY_MAX_BYTES, 1024);
    assert_eq!(REPORT_PATH_MAX_BYTES, 512);
}

// =============================================================================
// The two new ClientMessage variants require no more than baseline
// =============================================================================

#[test]
fn send_report_and_wait_for_report_require_no_more_than_the_baseline_revision() {
    let messages = [
        ClientMessage::SendReport {
            outcome: ReportOutcome::Succeeded,
            summary: "done".to_string(),
            path: None,
        },
        ClientMessage::WaitForReport { timeout_sec: 0 },
    ];
    for message in messages {
        assert_eq!(
            required_protocol_revision(&message),
            PROTOCOL_REVISION_BASELINE,
            "message must require no more than the baseline revision: {message:?}"
        );
    }
}
