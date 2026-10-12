#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the plan review gate (wire revision 9):
//! the closed enums `PlanReview` and `ReviewMark`, the fields they
//! and `default_role` add to existing plan/role payload types, and the two
//! new approver-only `ClientMessage` variants that settle a plan's review.
//!
//! Four properties are measured:
//!
//! - `PlanReview` and `ReviewMark` round-trip their documented wire strings
//!   and carry no catch-all: any other string fails the whole field's decode
//!   rather than falling back to a guessed value.
//! - `PlanInfo.review` / `PlanTaskInfo.review_mark` / `PlanSummary.review`
//!   are absent from the wire (not `null`) when `None`, and a frame missing
//!   the key -- an older peer's frame -- decodes to `None` rather than
//!   failing.
//! - `PlanInfo.version` / `PlanSummary.unseen_changes` default to `0` /
//!   `false` when their key is missing from the frame, the same older-peer
//!   case, rather than failing to decode.
//! - `approve_plan` / `mark_plan_seen` round trip through their documented
//!   wire tags, and `SaveRoles` / `RoleList`'s `default_role` follows the
//!   same absent-not-null rule as the fields above.

use yagura_protocol::{
    ClientMessage, DaemonMessage, PlanInfo, PlanReview, PlanSummary, PlanTaskInfo,
    ResolvedAssignment, ReviewMark, RoleEntry, decode_client_message, decode_daemon_message,
    encode_client_message, encode_daemon_message,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a plan review payload type must serialize")
}

fn assert_wire_tag(value: &serde_json::Value, expected: &str) {
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some(expected),
        "the wire tag must be snake_case, got: {value}"
    );
}

fn sample_plan_task_info(review_mark: Option<ReviewMark>) -> PlanTaskInfo {
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
            source: yagura_protocol::AssignmentSource::Unassigned,
            assignment: None,
        },
        window_id: None,
        report: None,
        instruction: None,
        instruction_notice: None,
        review_mark,
    }
}

fn sample_plan_info(review: Option<PlanReview>, version: u64) -> PlanInfo {
    PlanInfo {
        plan_id: "p-sample".to_string(),
        title: "ship the plan review gate".to_string(),
        cwd: "/work/yagura".to_string(),
        owner_window: None,
        waves: vec![],
        tasks: vec![],
        roles_warning: None,
        review,
        version,
    }
}

fn sample_plan_summary(review: Option<PlanReview>, unseen_changes: bool) -> PlanSummary {
    PlanSummary {
        plan_id: "p-sample".to_string(),
        title: "ship the plan review gate".to_string(),
        owner_window: None,
        task_count: 0,
        review,
        unseen_changes,
    }
}

// =============================================================================
// PlanInfo.review / PlanInfo.version
// =============================================================================

#[test]
fn plan_info_review_some_serializes_and_round_trips() {
    let decoded = sample_plan_info(Some(PlanReview::Pending), 3);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("review"),
        Some(&serde_json::json!("pending")),
        "review: Some(_) must serialize as its wire string, got: {value}"
    );
    let reencoded: PlanInfo =
        serde_json::from_value(value).expect("PlanInfo with review: Some(_) must decode");
    assert_eq!(reencoded, decoded);
}

#[test]
fn plan_info_review_none_is_omitted_from_the_wire() {
    let value = as_json(&sample_plan_info(None, 0));
    let object = value
        .as_object()
        .expect("PlanInfo must serialize as an object");
    assert!(
        !object.contains_key("review"),
        "PlanInfo.review: None must omit the key entirely, not serialize null, got: {value}"
    );
}

#[test]
fn plan_info_without_a_review_key_decodes_to_none() {
    let plan_json = serde_json::json!({
        "plan_id": "p-sample",
        "title": "ship the plan review gate",
        "cwd": "/work/yagura",
        "owner_window": null,
        "waves": [],
        "tasks": [],
        "roles_warning": null
    });
    let decoded: PlanInfo = serde_json::from_value(plan_json)
        .expect("a PlanInfo without a review key must still decode (older-daemon frame)");
    assert_eq!(decoded.review, None);
    assert_eq!(decoded.version, 0);
}

#[test]
fn plan_info_rejects_an_invalid_review_value() {
    let plan_json = serde_json::json!({
        "plan_id": "p-sample",
        "title": "ship the plan review gate",
        "cwd": "/work/yagura",
        "owner_window": null,
        "waves": [],
        "tasks": [],
        "roles_warning": null,
        "review": "bogus"
    });
    let result: Result<PlanInfo, _> = serde_json::from_value(plan_json);
    assert!(
        result.is_err(),
        "a PlanInfo carrying a review value outside {{pending, approved}} must fail the whole \
         frame's decode, got {result:?}"
    );
}

// =============================================================================
// PlanTaskInfo.review_mark
// =============================================================================

#[test]
fn plan_task_info_review_mark_some_serializes_and_round_trips() {
    for (mark, wire) in [(ReviewMark::New, "new"), (ReviewMark::Changed, "changed")] {
        let decoded = sample_plan_task_info(Some(mark));
        let value = as_json(&decoded);
        assert_eq!(
            value.get("review_mark"),
            Some(&serde_json::json!(wire)),
            "PlanTaskInfo with Some(review_mark) must carry its wire string, got: {value}"
        );
        let reencoded: PlanTaskInfo =
            serde_json::from_value(value).expect("PlanTaskInfo must decode");
        assert_eq!(reencoded, decoded);
    }
}

#[test]
fn plan_task_info_review_mark_none_is_omitted_from_the_wire() {
    let value = as_json(&sample_plan_task_info(None));
    let object = value
        .as_object()
        .expect("PlanTaskInfo must serialize as an object");
    assert!(
        !object.contains_key("review_mark"),
        "PlanTaskInfo.review_mark: None must omit the key entirely, got: {value}"
    );
}

#[test]
fn plan_task_info_without_a_review_mark_key_decodes_to_none() {
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
    let decoded: PlanTaskInfo = serde_json::from_value(plan_task_json)
        .expect("a PlanTaskInfo without a review_mark key must still decode (older-daemon frame)");
    assert_eq!(decoded.review_mark, None);
}

#[test]
fn plan_task_info_rejects_an_invalid_review_mark_value() {
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
        "review_mark": "x"
    });
    let result: Result<PlanTaskInfo, _> = serde_json::from_value(plan_task_json);
    assert!(
        result.is_err(),
        "a PlanTaskInfo carrying a review_mark value outside {{new, changed}} must fail the \
         whole frame's decode, got {result:?}"
    );
}

// =============================================================================
// PlanSummary.review / PlanSummary.unseen_changes
// =============================================================================

#[test]
fn plan_summary_review_some_serializes_and_round_trips() {
    let decoded = sample_plan_summary(Some(PlanReview::Approved), true);
    let value = as_json(&decoded);
    assert_eq!(
        value.get("review"),
        Some(&serde_json::json!("approved")),
        "review: Some(_) must serialize as its wire string, got: {value}"
    );
    let reencoded: PlanSummary =
        serde_json::from_value(value).expect("PlanSummary with review: Some(_) must decode");
    assert_eq!(reencoded, decoded);
}

#[test]
fn plan_summary_review_none_is_omitted_from_the_wire() {
    let value = as_json(&sample_plan_summary(None, false));
    let object = value
        .as_object()
        .expect("PlanSummary must serialize as an object");
    assert!(
        !object.contains_key("review"),
        "PlanSummary.review: None must omit the key entirely, got: {value}"
    );
}

#[test]
fn plan_summary_without_a_review_or_unseen_changes_key_decodes_to_the_neutral_value() {
    let summary_json = serde_json::json!({
        "plan_id": "p-sample",
        "title": "ship the plan review gate",
        "owner_window": null,
        "task_count": 0
    });
    let decoded: PlanSummary = serde_json::from_value(summary_json).expect(
        "a PlanSummary without review/unseen_changes keys must still decode (older-daemon frame)",
    );
    assert_eq!(decoded.review, None);
    assert!(!decoded.unseen_changes);
}

// =============================================================================
// ClientMessage::ApprovePlan / ClientMessage::MarkPlanSeen
// =============================================================================

#[test]
fn client_message_approve_plan_round_trips() {
    let decoded = ClientMessage::ApprovePlan {
        plan_id: "p-sample".to_string(),
        version: 3,
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "approve_plan");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("approve_plan must round trip"),
        decoded
    );
}

#[test]
fn client_message_mark_plan_seen_round_trips() {
    let decoded = ClientMessage::MarkPlanSeen {
        plan_id: "p-sample".to_string(),
        version: 3,
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "mark_plan_seen");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("mark_plan_seen must round trip"),
        decoded
    );
}

// =============================================================================
// SaveRoles / RoleList default_role
// =============================================================================

fn sample_role_entry() -> RoleEntry {
    RoleEntry {
        name: "tester".to_string(),
        profile: "claude-code".to_string(),
        model: None,
        effort: None,
    }
}

#[test]
fn client_message_save_roles_default_role_some_round_trips() {
    let decoded = ClientMessage::SaveRoles {
        roles: vec![sample_role_entry()],
        default_role: Some("tester".to_string()),
    };
    let value = as_json(&decoded);
    assert_eq!(
        value.get("default_role"),
        Some(&serde_json::json!("tester")),
        "default_role: Some(_) must serialize its value, got: {value}"
    );
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("save_roles with a default_role must round trip"),
        decoded
    );
}

#[test]
fn client_message_save_roles_default_role_none_is_omitted_from_the_wire() {
    let decoded = ClientMessage::SaveRoles {
        roles: vec![],
        default_role: None,
    };
    let value = as_json(&decoded);
    let object = value
        .as_object()
        .expect("save_roles must serialize as an object");
    assert!(
        !object.contains_key("default_role"),
        "SaveRoles.default_role: None must omit the key entirely, got: {value}"
    );
}

#[test]
fn client_message_save_roles_without_a_default_role_key_decodes_to_none() {
    let body = br#"{"type":"save_roles","roles":[]}"#;
    let decoded = decode_client_message(body)
        .expect("a save_roles frame without a default_role key must still decode");
    assert_eq!(
        decoded,
        ClientMessage::SaveRoles {
            roles: vec![],
            default_role: None,
        },
        "an omitted default_role must decode to None rather than failing the frame"
    );
}

#[test]
fn daemon_message_role_list_default_role_some_round_trips() {
    let decoded = DaemonMessage::RoleList {
        roles: vec![sample_role_entry()],
        default_role: Some("tester".to_string()),
    };
    let value = as_json(&decoded);
    assert_eq!(
        value.get("default_role"),
        Some(&serde_json::json!("tester")),
        "default_role: Some(_) must serialize its value, got: {value}"
    );
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("role_list with a default_role must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_role_list_default_role_none_is_omitted_from_the_wire() {
    let decoded = DaemonMessage::RoleList {
        roles: vec![],
        default_role: None,
    };
    let value = as_json(&decoded);
    let object = value
        .as_object()
        .expect("role_list must serialize as an object");
    assert!(
        !object.contains_key("default_role"),
        "RoleList.default_role: None must omit the key entirely, got: {value}"
    );
}

#[test]
fn daemon_message_role_list_without_a_default_role_key_decodes_to_none() {
    let body = br#"{"type":"role_list","roles":[]}"#;
    let decoded = decode_daemon_message(body)
        .expect("a role_list frame without a default_role key must still decode");
    assert_eq!(
        decoded,
        DaemonMessage::RoleList {
            roles: vec![],
            default_role: None,
        },
        "an omitted default_role must decode to None rather than failing the frame"
    );
}
