//! How `reply_for` classifies the plan-ledger daemon messages.
//!
//! `plan_detail` answers six different requests (create / update / get /
//! set_task_assignment / clear_task_assignment / set_plan_owner) but is one
//! wire message, so classifying it once here covers all of them; `reply_for`
//! itself cannot distinguish which request produced a given `PlanDetail`
//! frame, since the protocol carries no correlation id. `plan_changed` is a
//! push rather than a reply to a request this client ever sends.

use yagura_protocol::{
    AssignmentSource, DaemonMessage, PlanInfo, PlanSummary, PlanTaskInfo, ResolvedAssignment,
    RoleEntry, TaskAssignment,
};

use crate::commands::{RoleListReply, TaskWindowOpenedReply};
use crate::ipc_client::{PendingKind, Reply};

use super::reply_for;

fn sample_task_assignment() -> TaskAssignment {
    TaskAssignment {
        profile: "claude-code".to_owned(),
        model: None,
        effort: None,
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
        task_id: "investigate".to_owned(),
        title: "Investigate".to_owned(),
        role: Some("review".to_owned()),
        depends_on: Vec::new(),
        writes: vec!["gui/src/App.svelte".to_owned()],
        wave: 0,
        parent: Some(sample_task_assignment()),
        user: None,
        resolved: sample_resolved_assignment(),
        window_id: Some("window-a".to_owned()),
        report: None,
        instruction: None,
        instruction_notice: None,
        review_mark: None,
    }
}

fn sample_plan_info() -> PlanInfo {
    PlanInfo {
        plan_id: "p-abc123".to_owned(),
        title: "Plan window launcher".to_owned(),
        cwd: "C:\\repo".to_owned(),
        owner_window: Some("window-a".to_owned()),
        waves: vec![vec!["investigate".to_owned()]],
        tasks: vec![sample_plan_task_info()],
        roles_warning: None,
        // Same note as `sample_plan_task_info` above.
        review: None,
        version: 0,
    }
}

fn sample_plan_summary() -> PlanSummary {
    PlanSummary {
        plan_id: "p-abc123".to_owned(),
        title: "Plan window launcher".to_owned(),
        owner_window: Some("window-a".to_owned()),
        task_count: 1,
        // Same note as `sample_plan_task_info` above.
        review: None,
        unseen_changes: false,
    }
}

fn sample_role_entry() -> RoleEntry {
    RoleEntry {
        name: "review".to_owned(),
        profile: "codex-cli".to_owned(),
        model: None,
        effort: None,
    }
}

#[test]
fn reply_for_classifies_plan_detail_as_a_plan_detail_reply() {
    let plan = sample_plan_info();
    let result = reply_for(DaemonMessage::PlanDetail { plan: plan.clone() });
    match result {
        Some((PendingKind::PlanDetail, Reply::PlanDetail(got))) => assert_eq!(got, plan),
        other => panic!("expected a PlanDetail reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_plan_list_as_a_plan_list_reply() {
    let plans = vec![sample_plan_summary()];
    let result = reply_for(DaemonMessage::PlanList {
        plans: plans.clone(),
    });
    match result {
        Some((PendingKind::PlanList, Reply::PlanList(got))) => assert_eq!(got, plans),
        other => panic!("expected a PlanList reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_plan_deleted_as_a_plan_deleted_reply() {
    let result = reply_for(DaemonMessage::PlanDeleted {
        plan_id: "p-abc123".to_owned(),
    });
    match result {
        Some((PendingKind::PlanDeleted, Reply::PlanDeleted(got))) => {
            assert_eq!(got, "p-abc123");
        }
        other => panic!("expected a PlanDeleted reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_role_list_as_a_role_list_reply() {
    let roles = vec![sample_role_entry()];
    let result = reply_for(DaemonMessage::RoleList {
        roles: roles.clone(),
        default_role: None,
    });
    match result {
        Some((PendingKind::RoleList, Reply::RoleList(got))) => {
            let got: RoleListReply = got;
            assert_eq!(got.roles, roles);
        }
        other => panic!("expected a RoleList reply, got {other:?}"),
    }
}

#[test]
fn reply_for_keeps_the_default_role_of_a_role_list() {
    let roles = vec![sample_role_entry()];
    let result = reply_for(DaemonMessage::RoleList {
        roles: roles.clone(),
        default_role: Some("review".to_owned()),
    });
    match result {
        Some((PendingKind::RoleList, Reply::RoleList(got))) => {
            let got: RoleListReply = got;
            assert_eq!(got.roles, roles);
            assert_eq!(got.default_role.as_deref(), Some("review"));
        }
        other => panic!("expected a RoleList reply, got {other:?}"),
    }
}

#[test]
fn reply_for_reports_no_default_role_when_the_table_has_none() {
    let result = reply_for(DaemonMessage::RoleList {
        roles: vec![sample_role_entry()],
        default_role: None,
    });
    match result {
        Some((PendingKind::RoleList, Reply::RoleList(got))) => {
            let got: RoleListReply = got;
            assert_eq!(got.default_role, None);
        }
        other => panic!("expected a RoleList reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_task_window_opened_as_a_task_window_opened_reply() {
    let assignment = sample_resolved_assignment();
    let result = reply_for(DaemonMessage::TaskWindowOpened {
        plan_id: "p-abc123".to_owned(),
        task_id: "investigate".to_owned(),
        window_id: "window-b".to_owned(),
        name: None,
        assignment: assignment.clone(),
    });
    match result {
        Some((PendingKind::TaskWindowOpened, Reply::TaskWindowOpened(got))) => {
            let got: TaskWindowOpenedReply = got;
            assert_eq!(got.plan_id, "p-abc123");
            assert_eq!(got.task_id, "investigate");
            assert_eq!(got.window_id, "window-b");
            assert_eq!(got.assignment, assignment);
        }
        other => panic!("expected a TaskWindowOpened reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_plan_changed_as_answering_no_request_of_this_client() {
    // `PlanChanged` is a push sent to every approver, not an answer to a
    // request this client made, so it must complete nothing.
    let result = reply_for(DaemonMessage::PlanChanged {
        plan_id: "p-abc123".to_owned(),
    });
    assert!(result.is_none());
}
