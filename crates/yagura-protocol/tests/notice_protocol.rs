#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for wire revision 7: the task-window
//! request-delivery notice -- `InstructionNotice` / `InstructionNoticeState`,
//! `PlanTaskInfo.instruction_notice`, `DaemonMessage::TaskWindowOpened`
//! losing `instruction_delivery`, and `PROTOCOL_REVISION` itself.
//!
//! The wire ledger in
//! `wire_inventory.rs` covers the corpus completeness, structure freeze and
//! frozen-file correspondence this revision requires separately; this file
//! covers the semantics `wire_inventory.rs` does not (skip-if-none, the
//! `at`/`state` pair, decode failure on an unknown state).

use yagura_protocol::{
    AssignmentSource, DaemonMessage, InstructionNotice, InstructionNoticeState, PlanTaskInfo,
    ResolvedAssignment, decode_daemon_message, encode_daemon_message,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a wire payload type must serialize")
}

fn sample_task_window_opened() -> DaemonMessage {
    DaemonMessage::TaskWindowOpened {
        plan_id: "p-sample".to_string(),
        task_id: "t1".to_string(),
        window_id: "w1-abc".to_string(),
        assignment: ResolvedAssignment {
            source: AssignmentSource::Unassigned,
            assignment: None,
        },
    }
}

fn sample_plan_task_info(instruction_notice: Option<InstructionNotice>) -> PlanTaskInfo {
    PlanTaskInfo {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: None,
        depends_on: vec![],
        writes: vec![],
        wave: 0,
        parent: None,
        user: None,
        resolved: ResolvedAssignment {
            source: AssignmentSource::Unassigned,
            assignment: None,
        },
        window_id: None,
        report: None,
        instruction: None,
        instruction_notice,
        review_mark: None,
    }
}

// =============================================================================
// TaskWindowOpened no longer carries an instruction_delivery key at
// all (the frozen shape in wire_inventory.rs pins its key set). An older (rev 5/6) daemon's frame that still carries that key decodes
// without failing: the existing unknown-key discipline (no
// deny_unknown_fields on this wire) covers it, not something this revision
// has to add.
// =============================================================================

#[test]
fn task_window_opened_round_trips_without_instruction_delivery() {
    let decoded = sample_task_window_opened();
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("task_window_opened must round trip"),
        decoded
    );
}

#[test]
fn a_pre_revision_seven_frame_carrying_instruction_delivery_still_decodes() {
    let body = br#"{"type":"task_window_opened","plan_id":"p-sample","task_id":"t1","window_id":"w1-abc","assignment":{"source":"unassigned","assignment":null},"instruction_delivery":"confirmed"}"#;
    let decoded = decode_daemon_message(body).expect(
        "a rev 5/6 daemon's task_window_opened frame carrying instruction_delivery must still \
         decode: an unknown key is dropped, not fatal",
    );
    match decoded {
        DaemonMessage::TaskWindowOpened {
            plan_id, task_id, ..
        } => {
            assert_eq!(plan_id, "p-sample");
            assert_eq!(task_id, "t1");
        }
        other => panic!("expected DaemonMessage::TaskWindowOpened, got {other:?}"),
    }
}

// =============================================================================
// PlanTaskInfo.instruction_notice -- None omits the key entirely; Some
// carries { "state": <value>, "at": <u64|null> } and round-trips ("at" holds
// a value for delivered/window_closed and is null for pending); a frame
// without the key decodes to None (older-daemon frame).
// =============================================================================

#[test]
fn plan_task_info_instruction_notice_none_omits_the_key() {
    let value = as_json(&sample_plan_task_info(None));
    let object = value
        .as_object()
        .expect("PlanTaskInfo must serialize as an object");
    assert!(
        !object.contains_key("instruction_notice"),
        "a PlanTaskInfo with instruction_notice: None must omit the key entirely, got: {value}"
    );
}

#[test]
fn plan_task_info_instruction_notice_round_trips_every_state_with_its_at() {
    for (state, wire, at, wire_at) in [
        (
            InstructionNoticeState::Pending,
            "pending",
            None,
            serde_json::Value::Null,
        ),
        (
            InstructionNoticeState::Delivered,
            "delivered",
            Some(1_700_000_000),
            serde_json::json!(1_700_000_000),
        ),
        (
            InstructionNoticeState::WindowClosed,
            "window_closed",
            Some(1_700_000_500),
            serde_json::json!(1_700_000_500),
        ),
    ] {
        let value = sample_plan_task_info(Some(InstructionNotice { state, at }));
        let json = as_json(&value);
        assert_eq!(
            json.pointer("/instruction_notice/state")
                .and_then(serde_json::Value::as_str),
            Some(wire)
        );
        assert_eq!(
            json.pointer("/instruction_notice/at"),
            Some(&wire_at),
            "a {wire} notice's at must travel as {wire_at}, got: {json}"
        );
        let decoded: PlanTaskInfo = serde_json::from_value(json).expect("PlanTaskInfo must decode");
        assert_eq!(decoded, value);
    }
}

#[test]
fn plan_task_info_without_an_instruction_notice_key_decodes_to_none() {
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
    let decoded: PlanTaskInfo = serde_json::from_value(plan_task_json).expect(
        "a PlanTaskInfo without an instruction_notice key must still decode (older-daemon frame)",
    );
    assert_eq!(decoded.instruction_notice, None);
}

// =============================================================================
// InstructionNoticeState has no catch-all (R3): an unrecognized string fails
// the whole PlanTaskInfo's decode.
// =============================================================================

#[test]
fn a_plan_task_info_carrying_an_unknown_instruction_notice_state_fails_to_decode() {
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
        "instruction_notice": { "state": "bogus", "at": null }
    });
    let result: Result<PlanTaskInfo, _> = serde_json::from_value(plan_task_json);
    assert!(
        result.is_err(),
        "a PlanTaskInfo carrying an instruction_notice.state outside {{pending, delivered, \
         window_closed}} must fail the whole field's decode, got {result:?}"
    );
}
