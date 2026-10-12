//! How `reply_for` classifies the approval-settings daemon messages.

use yagura_protocol::{ApprovalSettingsInfo, DaemonMessage, PlaybookState, ToolAutoApprove};

use crate::commands::{ApprovalSettingsReply, ToolAutoApproveReply};
use crate::ipc_client::{PendingKind, Reply};

use super::reply_for;

/// Tool order deliberately not alphabetical, so a classifier that sorts
/// instead of preserving the daemon's order still fails this test.
fn sample_tools() -> Vec<ToolAutoApprove> {
    vec![
        ToolAutoApprove {
            tool: "send_chat".to_owned(),
            auto_approve: true,
        },
        ToolAutoApprove {
            tool: "interrupt_window".to_owned(),
            auto_approve: false,
        },
        ToolAutoApprove {
            tool: "send_message".to_owned(),
            auto_approve: false,
        },
    ]
}

fn sample_tool_replies() -> Vec<ToolAutoApproveReply> {
    vec![
        ToolAutoApproveReply {
            tool: "send_chat".to_owned(),
            auto_approve: true,
        },
        ToolAutoApproveReply {
            tool: "interrupt_window".to_owned(),
            auto_approve: false,
        },
        ToolAutoApproveReply {
            tool: "send_message".to_owned(),
            auto_approve: false,
        },
    ]
}

#[test]
fn reply_for_classifies_approval_settings_as_a_get_approval_settings_reply() {
    let settings = ApprovalSettingsInfo {
        playbook: PlaybookState::Loaded,
        tools: sample_tools(),
        plans_require_approval: true,
    };
    let result = reply_for(DaemonMessage::ApprovalSettings { settings });
    match result {
        Some((PendingKind::GetApprovalSettings, Reply::ApprovalSettings(got))) => {
            let got: ApprovalSettingsReply = got;
            assert_eq!(got.playbook, "loaded");
            let expected: Vec<ToolAutoApproveReply> = sample_tool_replies();
            assert_eq!(
                got.tools, expected,
                "tool order and values must be carried exactly as the daemon sent them"
            );
            assert!(got.plans_require_approval);
        }
        other => panic!("expected an ApprovalSettings reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_auto_approve_set_as_a_set_auto_approve_reply() {
    let settings = ApprovalSettingsInfo {
        playbook: PlaybookState::NotConfigured,
        tools: vec![ToolAutoApprove {
            tool: "close_window".to_owned(),
            auto_approve: true,
        }],
        plans_require_approval: false,
    };
    let result = reply_for(DaemonMessage::AutoApproveSet { settings });
    match result {
        Some((PendingKind::SetAutoApprove, Reply::AutoApproveSet(got))) => {
            let got: ApprovalSettingsReply = got;
            assert_eq!(got.playbook, "not_configured");
            let expected: Vec<ToolAutoApproveReply> = vec![ToolAutoApproveReply {
                tool: "close_window".to_owned(),
                auto_approve: true,
            }];
            assert_eq!(got.tools, expected);
            assert!(!got.plans_require_approval);
        }
        other => panic!("expected an AutoApproveSet reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_plan_approval_required_set_as_a_set_plan_approval_required_reply() {
    let settings = ApprovalSettingsInfo {
        playbook: PlaybookState::Failed,
        tools: Vec::new(),
        plans_require_approval: true,
    };
    let result = reply_for(DaemonMessage::PlanApprovalRequiredSet { settings });
    match result {
        Some((PendingKind::SetPlanApprovalRequired, Reply::PlanApprovalRequiredSet(got))) => {
            let got: ApprovalSettingsReply = got;
            assert_eq!(got.playbook, "failed");
            assert!(got.tools.is_empty());
            assert!(got.plans_require_approval);
        }
        other => panic!("expected a PlanApprovalRequiredSet reply, got {other:?}"),
    }
}

#[test]
fn reply_for_maps_an_unrecognized_playbook_state_to_the_unknown_string() {
    let settings = ApprovalSettingsInfo {
        playbook: PlaybookState::Unknown,
        tools: Vec::new(),
        plans_require_approval: false,
    };
    let result = reply_for(DaemonMessage::ApprovalSettings { settings });
    match result {
        Some((PendingKind::GetApprovalSettings, Reply::ApprovalSettings(got))) => {
            let got: ApprovalSettingsReply = got;
            assert_eq!(got.playbook, "unknown");
        }
        other => panic!("expected an ApprovalSettings reply, got {other:?}"),
    }
}
