#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

//! Wire-protocol contracts for the plan ledger: the eleven new
//! `ClientMessage` variants and six new `DaemonMessage` variants that carry
//! plans, tasks and the role-to-assignment table, plus the shared payload
//! types they nest (`PlanTaskSpec`, `TaskAssignment`, `AssignmentSource`,
//! `ResolvedAssignment`, `PlanTaskInfo`, `PlanInfo`, `PlanSummary`,
//! `RoleEntry`).
//!
//! Five properties are measured:
//!
//! - every new message round-trips through its documented wire tag
//! - `AssignmentSource` carries exactly five wire strings and rejects any
//!   other string outright, rather than falling back to a default
//! - every new struct's `Option` field serializes with its key present and
//!   the value `null` when absent, rather than omitting the key
//! - every new `ClientMessage` variant requires no more than the baseline
//!   wire revision
//! - every new wire tag is present in the decoder's own tag tables

use yagura_protocol::{
    AssignmentSource, ClientMessage, DaemonMessage, PROTOCOL_REVISION_BASELINE, PlanInfo,
    PlanSummary, PlanTaskInfo, PlanTaskSpec, ResolvedAssignment, RoleEntry, TaskAssignment,
    client_message_wire_tags, daemon_message_wire_tags, decode_client_message,
    decode_daemon_message, encode_client_message, encode_daemon_message,
    required_protocol_revision,
};

fn as_json(value: &impl serde::Serialize) -> serde_json::Value {
    serde_json::to_value(value).expect("a plan ledger payload type must serialize")
}

fn assert_wire_tag(value: &serde_json::Value, expected: &str) {
    assert_eq!(
        value.get("type").and_then(serde_json::Value::as_str),
        Some(expected),
        "the wire tag must be snake_case, got: {value}"
    );
}

// =============================================================================
// Sample payload builders
// =============================================================================

fn sample_plan_task_spec() -> PlanTaskSpec {
    PlanTaskSpec {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: Some("tester".to_string()),
        profile: Some("claude-code".to_string()),
        model: Some("opus".to_string()),
        effort: Some("high".to_string()),
        depends_on: vec!["t0".to_string()],
        writes: vec!["crates/yagura-protocol/tests/plan_protocol.rs".to_string()],
        instruction: None,
    }
}

fn sample_task_assignment() -> TaskAssignment {
    TaskAssignment {
        profile: "claude-code".to_string(),
        model: Some("opus".to_string()),
        effort: Some("high".to_string()),
    }
}

fn sample_resolved_assignment() -> ResolvedAssignment {
    ResolvedAssignment {
        source: AssignmentSource::Role,
        assignment: Some(sample_task_assignment()),
    }
}

fn sample_plan_task_info() -> PlanTaskInfo {
    PlanTaskInfo {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: Some("tester".to_string()),
        depends_on: vec!["t0".to_string()],
        writes: vec!["crates/yagura-protocol/tests/plan_protocol.rs".to_string()],
        wave: 1,
        parent: Some(sample_task_assignment()),
        user: Some(sample_task_assignment()),
        resolved: sample_resolved_assignment(),
        window_id: Some("w1-abc".to_string()),
        report: None,
        instruction: None,
        instruction_notice: None,
        review_mark: None,
    }
}

fn sample_plan_info() -> PlanInfo {
    PlanInfo {
        plan_id: "p-sample".to_string(),
        title: "ship the plan ledger".to_string(),
        cwd: "/work/yagura".to_string(),
        owner_window: Some("w1-abc".to_string()),
        waves: vec![vec!["t1".to_string()]],
        tasks: vec![sample_plan_task_info()],
        roles_warning: Some("roles.toml is malformed".to_string()),
        // Same note as `PlanTaskInfo::review_mark` above.
        review: None,
        version: 0,
        ticket: None,
    }
}

fn sample_plan_summary() -> PlanSummary {
    PlanSummary {
        plan_id: "p-sample".to_string(),
        title: "ship the plan ledger".to_string(),
        owner_window: Some("w1-abc".to_string()),
        task_count: 1,
        // Same note as `PlanTaskInfo::review_mark` above.
        review: None,
        unseen_changes: false,
    }
}

fn sample_role_entry() -> RoleEntry {
    RoleEntry {
        name: "tester".to_string(),
        profile: "claude-code".to_string(),
        model: Some("opus".to_string()),
        effort: Some("high".to_string()),
    }
}

// =============================================================================
// ClientMessage: create_plan / update_plan / get_plan / list_plans /
// delete_plan
// =============================================================================

#[test]
fn client_message_create_plan_round_trips() {
    let decoded = ClientMessage::CreatePlan {
        title: "ship the plan ledger".to_string(),
        tasks: vec![sample_plan_task_spec()],
        ticket: None,
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "create_plan");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("create_plan must round trip"),
        decoded
    );
}

#[test]
fn client_message_update_plan_round_trips_with_title() {
    let decoded = ClientMessage::UpdatePlan {
        plan_id: "p-sample".to_string(),
        title: Some("renamed plan".to_string()),
        tasks: vec![sample_plan_task_spec()],
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "update_plan");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("update_plan must round trip"),
        decoded
    );
}

#[test]
fn client_message_update_plan_title_may_be_omitted() {
    let body = br#"{"type":"update_plan","plan_id":"p-sample","tasks":[]}"#;
    let decoded = decode_client_message(body).expect("update_plan without title must decode");
    assert_eq!(
        decoded,
        ClientMessage::UpdatePlan {
            plan_id: "p-sample".to_string(),
            title: None,
            tasks: vec![],
        },
        "an omitted title must decode to None rather than failing the frame"
    );
}

#[test]
fn client_message_get_plan_round_trips() {
    let body = br#"{"type":"get_plan","plan_id":"p-sample"}"#;
    let decoded = decode_client_message(body).expect("get_plan must decode");
    assert_eq!(
        decoded,
        ClientMessage::GetPlan {
            plan_id: "p-sample".to_string(),
        }
    );
    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_wire_tag(&value, "get_plan");
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_list_plans_round_trips() {
    let body = br#"{"type":"list_plans"}"#;
    let decoded = decode_client_message(body).expect("list_plans must decode");
    assert_eq!(decoded, ClientMessage::ListPlans);
    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_wire_tag(&value, "list_plans");
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_delete_plan_round_trips() {
    let body = br#"{"type":"delete_plan","plan_id":"p-sample"}"#;
    let decoded = decode_client_message(body).expect("delete_plan must decode");
    assert_eq!(
        decoded,
        ClientMessage::DeletePlan {
            plan_id: "p-sample".to_string(),
        }
    );
    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_wire_tag(&value, "delete_plan");
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// ClientMessage: list_roles / save_roles / set_task_assignment /
// clear_task_assignment / set_plan_owner
// =============================================================================

#[test]
fn client_message_list_roles_round_trips() {
    let body = br#"{"type":"list_roles"}"#;
    let decoded = decode_client_message(body).expect("list_roles must decode");
    assert_eq!(decoded, ClientMessage::ListRoles);
    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_wire_tag(&value, "list_roles");
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_save_roles_round_trips() {
    let decoded = ClientMessage::SaveRoles {
        roles: vec![sample_role_entry()],
        // Same note as `PlanTaskInfo::review_mark` above.
        default_role: None,
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "save_roles");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("save_roles must round trip"),
        decoded
    );
}

#[test]
fn client_message_set_task_assignment_round_trips() {
    let decoded = ClientMessage::SetTaskAssignment {
        plan_id: "p-sample".to_string(),
        task_id: "t1".to_string(),
        assignment: sample_task_assignment(),
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "set_task_assignment");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("set_task_assignment must round trip"),
        decoded
    );
}

#[test]
fn client_message_clear_task_assignment_round_trips() {
    let body = br#"{"type":"clear_task_assignment","plan_id":"p-sample","task_id":"t1"}"#;
    let decoded = decode_client_message(body).expect("clear_task_assignment must decode");
    assert_eq!(
        decoded,
        ClientMessage::ClearTaskAssignment {
            plan_id: "p-sample".to_string(),
            task_id: "t1".to_string(),
        }
    );
    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_wire_tag(&value, "clear_task_assignment");
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

#[test]
fn client_message_set_plan_owner_round_trips() {
    let body = br#"{"type":"set_plan_owner","plan_id":"p-sample","window_id":"w1-abc"}"#;
    let decoded = decode_client_message(body).expect("set_plan_owner must decode");
    assert_eq!(
        decoded,
        ClientMessage::SetPlanOwner {
            plan_id: "p-sample".to_string(),
            window_id: "w1-abc".to_string(),
        }
    );
    let encoded = encode_client_message(&decoded);
    let value = as_json(&decoded);
    assert_wire_tag(&value, "set_plan_owner");
    assert_eq!(
        decode_client_message(&encoded).expect("round trip must succeed"),
        decoded
    );
}

// =============================================================================
// ClientMessage: open_task_window
// =============================================================================

#[test]
fn client_message_open_task_window_round_trips_with_name() {
    let decoded = ClientMessage::OpenTaskWindow {
        plan_id: "p-sample".to_string(),
        task_id: "t1".to_string(),
        name: Some("tester window".to_string()),
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "open_task_window");
    let encoded = encode_client_message(&decoded);
    assert_eq!(
        decode_client_message(&encoded).expect("open_task_window must round trip"),
        decoded
    );
}

#[test]
fn client_message_open_task_window_name_may_be_omitted() {
    let body = br#"{"type":"open_task_window","plan_id":"p-sample","task_id":"t1"}"#;
    let decoded = decode_client_message(body).expect("open_task_window without name must decode");
    assert_eq!(
        decoded,
        ClientMessage::OpenTaskWindow {
            plan_id: "p-sample".to_string(),
            task_id: "t1".to_string(),
            name: None,
        },
        "an omitted name must decode to None rather than failing the frame"
    );
}

// =============================================================================
// DaemonMessage: plan_detail / plan_list / plan_deleted / role_list /
// task_window_opened / plan_changed
// =============================================================================

#[test]
fn daemon_message_plan_detail_round_trips() {
    let decoded = DaemonMessage::PlanDetail {
        plan: sample_plan_info(),
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "plan_detail");
    assert_eq!(
        value
            .pointer("/plan/tasks/0/task_id")
            .and_then(serde_json::Value::as_str),
        Some("t1"),
        "plan must nest PlanInfo verbatim, got: {value}"
    );
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("plan_detail must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_plan_list_round_trips() {
    let decoded = DaemonMessage::PlanList {
        plans: vec![sample_plan_summary()],
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "plan_list");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("plan_list must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_plan_deleted_round_trips() {
    let decoded = DaemonMessage::PlanDeleted {
        plan_id: "p-sample".to_string(),
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "plan_deleted");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("plan_deleted must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_role_list_round_trips() {
    let decoded = DaemonMessage::RoleList {
        roles: vec![sample_role_entry()],
        // Same note as `PlanTaskInfo::review_mark` above.
        default_role: None,
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "role_list");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("role_list must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_task_window_opened_round_trips() {
    let decoded = DaemonMessage::TaskWindowOpened {
        plan_id: "p-sample".to_string(),
        task_id: "t1".to_string(),
        window_id: "w1-abc".to_string(),
        assignment: sample_resolved_assignment(),
        name: None,
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "task_window_opened");
    assert_eq!(
        value
            .pointer("/assignment/source")
            .and_then(serde_json::Value::as_str),
        Some("role"),
        "assignment must nest ResolvedAssignment verbatim, got: {value}"
    );
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("task_window_opened must round trip"),
        decoded
    );
}

#[test]
fn daemon_message_plan_changed_round_trips() {
    let decoded = DaemonMessage::PlanChanged {
        plan_id: "p-sample".to_string(),
    };
    let value = as_json(&decoded);
    assert_wire_tag(&value, "plan_changed");
    let encoded = encode_daemon_message(&decoded);
    assert_eq!(
        decode_daemon_message(&encoded).expect("plan_changed must round trip"),
        decoded
    );
}

// =============================================================================
// AssignmentSource: exactly five wire values, no catch-all
// =============================================================================

#[test]
fn assignment_source_round_trips_all_five_wire_values() {
    let pairs = [
        (AssignmentSource::User, "user"),
        (AssignmentSource::Parent, "parent"),
        (AssignmentSource::Role, "role"),
        (AssignmentSource::Default, "default"),
        (AssignmentSource::Unassigned, "unassigned"),
    ];
    for (value, wire) in pairs {
        let encoded = serde_json::to_value(value).expect("AssignmentSource must serialize");
        assert_eq!(encoded, serde_json::json!(wire));
        let decoded: AssignmentSource =
            serde_json::from_value(encoded).expect("AssignmentSource must deserialize");
        assert_eq!(decoded, value);
    }
}

#[test]
fn assignment_source_rejects_an_unknown_wire_string() {
    let result = serde_json::from_str::<AssignmentSource>("\"bogus\"");
    assert!(
        result.is_err(),
        "an unrecognized AssignmentSource string must fail to decode rather than fall back to \
         a default, got {result:?}"
    );
}

// =============================================================================
// New struct Option fields: null on the wire, never an omitted key
// =============================================================================

#[test]
fn plan_task_spec_absent_optionals_serialize_as_null_not_omitted() {
    let value = as_json(&PlanTaskSpec {
        task_id: "t1".to_string(),
        title: "write the tests".to_string(),
        role: None,
        profile: None,
        model: None,
        effort: None,
        depends_on: vec![],
        writes: vec![],
        instruction: None,
    });
    let object = value
        .as_object()
        .expect("PlanTaskSpec must serialize as an object");
    for key in ["role", "profile", "model", "effort"] {
        assert_eq!(
            object.get(key),
            Some(&serde_json::Value::Null),
            "{key} must be present as null when absent, not omitted, got: {value}"
        );
    }
}

#[test]
fn task_assignment_absent_optionals_serialize_as_null_not_omitted() {
    let value = as_json(&TaskAssignment {
        profile: "claude-code".to_string(),
        model: None,
        effort: None,
    });
    let object = value
        .as_object()
        .expect("TaskAssignment must serialize as an object");
    for key in ["model", "effort"] {
        assert_eq!(
            object.get(key),
            Some(&serde_json::Value::Null),
            "{key} must be present as null when absent, not omitted, got: {value}"
        );
    }
}

#[test]
fn resolved_assignment_absent_assignment_serializes_as_null_not_omitted() {
    let value = as_json(&ResolvedAssignment {
        source: AssignmentSource::Unassigned,
        assignment: None,
    });
    let object = value
        .as_object()
        .expect("ResolvedAssignment must serialize as an object");
    assert_eq!(
        object.get("assignment"),
        Some(&serde_json::Value::Null),
        "assignment must be present as null when absent, not omitted, got: {value}"
    );
}

#[test]
fn plan_task_info_absent_optionals_serialize_as_null_not_omitted() {
    let value = as_json(&PlanTaskInfo {
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
        instruction_notice: None,
        // Same note as `sample_plan_task_info` above.
        review_mark: None,
    });
    let object = value
        .as_object()
        .expect("PlanTaskInfo must serialize as an object");
    for key in ["role", "parent", "user", "window_id"] {
        assert_eq!(
            object.get(key),
            Some(&serde_json::Value::Null),
            "{key} must be present as null when absent, not omitted, got: {value}"
        );
    }
}

#[test]
fn plan_info_absent_optionals_serialize_as_null_not_omitted() {
    let value = as_json(&PlanInfo {
        plan_id: "p-sample".to_string(),
        title: "ship the plan ledger".to_string(),
        cwd: "/work/yagura".to_string(),
        owner_window: None,
        waves: vec![],
        tasks: vec![],
        roles_warning: None,
        // Same note as `sample_plan_info` above.
        review: None,
        version: 0,
        ticket: None,
    });
    let object = value
        .as_object()
        .expect("PlanInfo must serialize as an object");
    for key in ["owner_window", "roles_warning"] {
        assert_eq!(
            object.get(key),
            Some(&serde_json::Value::Null),
            "{key} must be present as null when absent, not omitted, got: {value}"
        );
    }
}

#[test]
fn plan_summary_absent_owner_window_serializes_as_null_not_omitted() {
    let value = as_json(&PlanSummary {
        plan_id: "p-sample".to_string(),
        title: "ship the plan ledger".to_string(),
        owner_window: None,
        task_count: 0,
        // Same note as `sample_plan_summary` above.
        review: None,
        unseen_changes: false,
    });
    let object = value
        .as_object()
        .expect("PlanSummary must serialize as an object");
    assert_eq!(
        object.get("owner_window"),
        Some(&serde_json::Value::Null),
        "owner_window must be present as null when absent, not omitted, got: {value}"
    );
}

#[test]
fn role_entry_absent_optionals_serialize_as_null_not_omitted() {
    let value = as_json(&RoleEntry {
        name: "tester".to_string(),
        profile: "claude-code".to_string(),
        model: None,
        effort: None,
    });
    let object = value
        .as_object()
        .expect("RoleEntry must serialize as an object");
    for key in ["model", "effort"] {
        assert_eq!(
            object.get(key),
            Some(&serde_json::Value::Null),
            "{key} must be present as null when absent, not omitted, got: {value}"
        );
    }
}

// =============================================================================
// required_protocol_revision: every new ClientMessage variant requires no
// more than the baseline revision
// =============================================================================

fn one_instance_of_every_new_client_message() -> Vec<ClientMessage> {
    vec![
        ClientMessage::CreatePlan {
            title: "ship the plan ledger".to_string(),
            tasks: vec![sample_plan_task_spec()],
            ticket: None,
        },
        ClientMessage::UpdatePlan {
            plan_id: "p-sample".to_string(),
            title: Some("renamed plan".to_string()),
            tasks: vec![sample_plan_task_spec()],
        },
        ClientMessage::GetPlan {
            plan_id: "p-sample".to_string(),
        },
        ClientMessage::ListPlans,
        ClientMessage::DeletePlan {
            plan_id: "p-sample".to_string(),
        },
        ClientMessage::ListRoles,
        ClientMessage::SaveRoles {
            roles: vec![sample_role_entry()],
            default_role: None,
        },
        ClientMessage::SetTaskAssignment {
            plan_id: "p-sample".to_string(),
            task_id: "t1".to_string(),
            assignment: sample_task_assignment(),
        },
        ClientMessage::ClearTaskAssignment {
            plan_id: "p-sample".to_string(),
            task_id: "t1".to_string(),
        },
        ClientMessage::SetPlanOwner {
            plan_id: "p-sample".to_string(),
            window_id: "w1-abc".to_string(),
        },
        ClientMessage::OpenTaskWindow {
            plan_id: "p-sample".to_string(),
            task_id: "t1".to_string(),
            name: Some("tester window".to_string()),
        },
    ]
}

#[test]
fn every_new_client_message_requires_no_more_than_the_baseline_revision() {
    for message in one_instance_of_every_new_client_message() {
        assert_eq!(
            required_protocol_revision(&message),
            PROTOCOL_REVISION_BASELINE,
            "message must require no more than the baseline revision: {message:?}"
        );
    }
}

// =============================================================================
// Wire tag tables: every new tag is present in the decoder's own tables
// =============================================================================

#[test]
fn new_client_message_tags_are_present_in_the_client_wire_tag_table() {
    let tags = client_message_wire_tags();
    for expected in [
        "create_plan",
        "update_plan",
        "get_plan",
        "list_plans",
        "delete_plan",
        "list_roles",
        "save_roles",
        "set_task_assignment",
        "clear_task_assignment",
        "set_plan_owner",
        "open_task_window",
    ] {
        assert!(
            tags.contains(&expected),
            "{expected} must be in the client message tag table, got: {tags:?}"
        );
    }
}

#[test]
fn new_daemon_message_tags_are_present_in_the_daemon_wire_tag_table() {
    let tags = daemon_message_wire_tags();
    for expected in [
        "plan_detail",
        "plan_list",
        "plan_deleted",
        "role_list",
        "task_window_opened",
        "plan_changed",
    ] {
        assert!(
            tags.contains(&expected),
            "{expected} must be in the daemon message tag table, got: {tags:?}"
        );
    }
}
