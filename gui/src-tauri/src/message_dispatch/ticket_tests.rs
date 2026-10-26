//! How `reply_for` and `dispatch` handle the ticket-board daemon messages.
//!
//! `ticket_detail` answers four different requests (get / create / update /
//! add-result) but is one wire message, so classifying it once covers all of
//! them. `ticket_changed` is a push sent to every approver, never an answer.

use std::collections::BTreeMap;

use serde_json::json;
use yagura_protocol::{
    DaemonMessage, MilestoneInfo, TicketConfigInfo, TicketFieldInfo, TicketInfo, TicketResultInfo,
    TicketStateInfo, TicketSummary, TicketWorkspaceInfo,
};

use crate::commands::TicketListReply;
use crate::events::EVENT_TICKET_CHANGED;
use crate::host::fake::FakeHost;
use crate::ipc_client::{PendingKind, Reply};

use super::*;

fn sample_workspace() -> TicketWorkspaceInfo {
    TicketWorkspaceInfo {
        workspace_id: 4,
        kind: "git".to_owned(),
        path: "C:\\repo".to_owned(),
    }
}

fn sample_summary() -> TicketSummary {
    TicketSummary {
        number: 12,
        title: "Add the ticket board".to_owned(),
        state: "todo".to_owned(),
        milestone: Some("v1".to_owned()),
        updated_at: 1_790_000_000,
    }
}

fn sample_ticket() -> TicketInfo {
    TicketInfo {
        number: 12,
        title: "Add the ticket board".to_owned(),
        body: "line one\nline two".to_owned(),
        state: "on-hold".to_owned(),
        milestone: Some("v1".to_owned()),
        fields: BTreeMap::from([("resume_trigger".to_owned(), "when ready".to_owned())]),
        results: vec![TicketResultInfo {
            seq: 1,
            at: 1_790_000_100,
            text: "merged".to_owned(),
        }],
        plans: vec!["p-abc123".to_owned()],
        created_at: 1_790_000_000,
        updated_at: 1_790_000_200,
    }
}

fn sample_config() -> TicketConfigInfo {
    TicketConfigInfo {
        guide: "One ticket, one purpose.".to_owned(),
        states: vec![TicketStateInfo {
            name: "on-hold".to_owned(),
            description: "Paused.".to_owned(),
            requires: vec!["resume_trigger".to_owned()],
        }],
        fields: vec![TicketFieldInfo {
            name: "resume_trigger".to_owned(),
            description: "What brings it back.".to_owned(),
        }],
    }
}

fn sample_milestone() -> MilestoneInfo {
    MilestoneInfo {
        name: "v1".to_owned(),
        position: 1,
        title: "First release".to_owned(),
        description: None,
    }
}

#[test]
fn reply_for_classifies_a_workspace_list_as_a_workspace_list_reply() {
    let workspaces = vec![sample_workspace()];
    let result = reply_for(DaemonMessage::TicketWorkspaceList {
        workspaces: workspaces.clone(),
    });
    match result {
        Some((PendingKind::TicketWorkspaceList, Reply::TicketWorkspaceList(got))) => {
            assert_eq!(got, workspaces);
        }
        other => panic!("expected a TicketWorkspaceList reply, got {other:?}"),
    }
}

#[test]
fn reply_for_keeps_every_part_of_a_ticket_list() {
    let result = reply_for(DaemonMessage::TicketList {
        workspace_id: Some(4),
        config: Some(sample_config()),
        config_error: None,
        milestones: vec![sample_milestone()],
        tickets: vec![sample_summary()],
        truncated: true,
    });
    match result {
        Some((PendingKind::TicketList, Reply::TicketList(got))) => {
            let got: TicketListReply = got;
            assert_eq!(got.workspace_id, Some(4));
            assert_eq!(got.config, Some(sample_config()));
            assert_eq!(got.config_error, None);
            assert_eq!(got.milestones, vec![sample_milestone()]);
            assert_eq!(got.tickets, vec![sample_summary()]);
            assert!(got.truncated);
        }
        other => panic!("expected a TicketList reply, got {other:?}"),
    }
}

#[test]
fn reply_for_keeps_the_reason_of_a_ticket_list_with_no_settings() {
    let result = reply_for(DaemonMessage::TicketList {
        workspace_id: None,
        config: None,
        config_error: Some("tickets.toml is missing".to_owned()),
        milestones: Vec::new(),
        tickets: Vec::new(),
        truncated: false,
    });
    match result {
        Some((PendingKind::TicketList, Reply::TicketList(got))) => {
            let got: TicketListReply = got;
            assert_eq!(got.config, None);
            assert_eq!(got.config_error.as_deref(), Some("tickets.toml is missing"));
        }
        other => panic!("expected a TicketList reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_a_ticket_detail_as_a_ticket_detail_reply() {
    let ticket = sample_ticket();
    let result = reply_for(DaemonMessage::TicketDetail {
        ticket: ticket.clone(),
    });
    match result {
        Some((PendingKind::TicketDetail, Reply::TicketDetail(got))) => assert_eq!(got, ticket),
        other => panic!("expected a TicketDetail reply, got {other:?}"),
    }
}

#[test]
fn reply_for_classifies_ticket_changed_as_answering_no_request_of_this_client() {
    let result = reply_for(DaemonMessage::TicketChanged { workspace_id: 4 });
    assert!(result.is_none());
}

#[tokio::test]
async fn a_ticket_changed_push_becomes_the_event_naming_its_workspace() {
    let host = FakeHost::new("dispatch-ticket-changed");
    let client = Arc::new(IpcClient::new());

    let superseded = dispatch(
        &host,
        &client,
        DaemonMessage::TicketChanged { workspace_id: 4 },
        None,
    )
    .await;

    assert!(!superseded);
    assert_eq!(EVENT_TICKET_CHANGED, "session://ticket-changed");
    assert_eq!(
        host.next_event(EVENT_TICKET_CHANGED).await,
        json!({ "workspace_id": 4 })
    );
}
