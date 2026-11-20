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
    DaemonMessage, ReportState, TaskReportMark, WindowInfo, WindowStatus, decode_daemon_message,
    encode_daemon_message,
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
        status_boot: None,
        mcp_connected: None,
        status_reports_missing: None,
        queued_count: None,
        task_report: None,
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
        status_boot: None,
        task_report: None,
        exit_code: None,
    }
}

#[test]
fn status_changed_status_rev_round_trips_when_present() {
    let msg = status_changed(Some(42));
    let body = encode_daemon_message(&msg);
    let decoded = decode_daemon_message(&body).expect("round trip decode must succeed");
    assert_eq!(decoded, msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    assert_eq!(
        value.get("status_rev"),
        Some(&serde_json::json!(42)),
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
        queued_count: None,
        queued_ids: None,
    };
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    assert!(
        value.get("status_rev").is_none(),
        "the request/response status query must never carry a generation, got: {value}"
    );
}

// ---------------------------------------------------------------------
// task_report: the daemon's report-mark fact on the two registry-updating
// carriers (`WindowInfo`, `StatusChanged`)
// ---------------------------------------------------------------------

fn mark(seq: u64, state: ReportState) -> TaskReportMark {
    TaskReportMark { seq, state }
}

#[test]
fn window_info_task_report_round_trips_as_a_seq_and_state_object() {
    for (state, spelled) in [
        (ReportState::Standing, "standing"),
        (ReportState::Instruction, "instruction"),
        (ReportState::NoReader, "no_reader"),
    ] {
        let mut info = sample_window_info(None);
        info.task_report = Some(mark(2, state));
        let value = serde_json::to_value(&info).expect("must serialize");
        assert_eq!(
            value.get("task_report"),
            Some(&serde_json::json!({ "seq": 2, "state": spelled })),
            "got: {value}"
        );
        let back: WindowInfo = serde_json::from_value(value).expect("must deserialize");
        assert_eq!(back, info);
    }
}

#[test]
fn window_info_omits_task_report_from_the_wire_when_none() {
    let value = serde_json::to_value(sample_window_info(None)).expect("must serialize");
    assert!(
        value.get("task_report").is_none(),
        "task_report: None must be omitted, not sent as null, got: {value}"
    );
}

#[test]
fn window_info_without_the_task_report_key_decodes_as_none() {
    // A revision 34 frame: the same body minus the new key.
    let mut value = serde_json::to_value(sample_window_info(None)).expect("must serialize");
    value.as_object_mut().expect("object").remove("task_report");
    let back: WindowInfo = serde_json::from_value(value).expect("must deserialize");
    assert_eq!(back.task_report, None);
}

#[test]
fn status_changed_task_report_round_trips_when_present() {
    let msg = DaemonMessage::StatusChanged {
        window_id: "w1".to_string(),
        status: WindowStatus::WaitingInput,
        unknown_reason: None,
        status_rev: None,
        status_boot: None,
        task_report: Some(mark(1, ReportState::NoReader)),
        exit_code: None,
    };
    let body = encode_daemon_message(&msg);
    let value: serde_json::Value = serde_json::from_slice(&body).expect("valid json");
    assert_eq!(
        value.get("task_report"),
        Some(&serde_json::json!({ "seq": 1, "state": "no_reader" })),
        "got: {value}"
    );
    assert_eq!(decode_daemon_message(&body).expect("decode"), msg);
}

/// A revision 35 daemon sends `{ "seq", "standing" }`: the frame still decodes,
/// and the mark reads as a state this build cannot tell, not as one it can.
#[test]
fn a_revision_35_mark_decodes_on_both_carriers_with_an_unknown_state() {
    for standing in [true, false] {
        let push = format!(
            r#"{{"type":"status_changed","window_id":"w1","status":"waiting_input","task_report":{{"seq":4,"standing":{standing}}}}}"#
        );
        match decode_daemon_message(push.as_bytes()).expect("a revision 35 push decodes") {
            DaemonMessage::StatusChanged { task_report, .. } => {
                assert_eq!(task_report, Some(mark(4, ReportState::Unknown)));
            }
            other => panic!("expected status_changed, got {other:?}"),
        }

        let mut value = serde_json::to_value(sample_window_info(None)).expect("must serialize");
        value.as_object_mut().expect("object").insert(
            "task_report".to_string(),
            serde_json::json!({ "seq": 4, "standing": standing }),
        );
        let info: WindowInfo =
            serde_json::from_value(value).expect("a revision 35 window record decodes");
        assert_eq!(info.task_report, Some(mark(4, ReportState::Unknown)));
    }
}

/// A newer daemon may send a state this build does not know: the frame still
/// decodes and the state reads as unknown.
#[test]
fn a_report_state_this_build_does_not_know_decodes_as_unknown() {
    let push = br#"{"type":"status_changed","window_id":"w1","status":"waiting_input","task_report":{"seq":2,"state":"held_by_a_later_revision"}}"#;
    match decode_daemon_message(push).expect("an unknown state does not fail the frame") {
        DaemonMessage::StatusChanged { task_report, .. } => {
            assert_eq!(task_report, Some(mark(2, ReportState::Unknown)));
        }
        other => panic!("expected status_changed, got {other:?}"),
    }
}
