//! The six MCP ticket tools: argument checks, the `ClientMessage` each sends
//! over [`crate::transport::DaemonTransport`], and the mapping of the
//! daemon's reply onto the MCP tool contract.
//!
//! The board's rules belong to the daemon and the user's `tickets.toml`:
//! states, fields, milestones and size limits are forwarded as given and the
//! daemon's own refusal is relayed unchanged. The only checks made here are
//! for a required argument being absent or empty, before any daemon round
//! trip, and every multi-line text (a body, a field value, a result, a
//! milestone description) is sent with LF line endings. A window never names
//! a workspace: the daemon reads it from the window's own registration.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::tools::{
    ToolError, map_daemon_error, map_transport_error, normalize_to_lf, require_non_empty,
    require_present, to_json, unexpected_response,
};
use crate::transport::DaemonTransport;
use yagura_protocol::{
    ClientMessage, DaemonMessage, MilestoneInfo, TicketConfigInfo, TicketSummary,
};

/// Arguments for the `list_tickets` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct ListTicketsArgs {
    /// Only tickets in this state.
    pub state: Option<String>,
    /// Only tickets in this milestone.
    pub milestone: Option<String>,
    /// The most summaries to return. Its default and upper bound are the
    /// daemon's, so an omitted value is forwarded as `None`.
    pub limit: Option<u32>,
}

/// Arguments for the `get_ticket` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct GetTicketArgs {
    /// The ticket's number in the calling window's workspace (required).
    pub ticket: Option<u32>,
}

/// Arguments for the `create_ticket` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct CreateTicketArgs {
    /// The ticket's title (required, non-empty).
    pub title: Option<String>,
    /// The ticket's body (required, non-empty).
    pub body: Option<String>,
    /// The milestone the ticket belongs to.
    pub milestone: Option<String>,
    /// The state to start in. Omitted leaves it to the daemon.
    pub state: Option<String>,
    /// Field values by field name.
    pub fields: Option<BTreeMap<String, String>>,
}

/// Arguments for the `update_ticket` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdateTicketArgs {
    /// The ticket's number in the calling window's workspace (required).
    pub ticket: Option<u32>,
    /// The new title.
    pub title: Option<String>,
    /// The new body.
    pub body: Option<String>,
    /// The new milestone; `""` takes the ticket out of its milestone.
    pub milestone: Option<String>,
    /// The new state.
    pub state: Option<String>,
    /// The fields to change; a value of `""` removes that field.
    pub fields: Option<BTreeMap<String, String>>,
}

/// Arguments for the `add_ticket_result` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct AddTicketResultArgs {
    /// The ticket's number in the calling window's workspace (required).
    pub ticket: Option<u32>,
    /// The result to record (required, non-empty).
    pub text: Option<String>,
}

/// Arguments for the `set_milestone` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SetMilestoneArgs {
    /// The milestone's name (required, non-empty).
    pub name: Option<String>,
    /// The milestone's display title (required, non-empty).
    pub title: Option<String>,
    /// The milestone's description.
    pub description: Option<String>,
    /// Where the milestone sorts (required).
    pub position: Option<i64>,
}

/// The JSON `list_tickets` replies with: the [`DaemonMessage::TicketList`]
/// fields at the top level, an absent optional one left out.
#[derive(Serialize)]
struct TicketListReply {
    #[serde(skip_serializing_if = "Option::is_none")]
    workspace_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config: Option<TicketConfigInfo>,
    #[serde(skip_serializing_if = "Option::is_none")]
    config_error: Option<String>,
    milestones: Vec<MilestoneInfo>,
    tickets: Vec<TicketSummary>,
    truncated: bool,
}

fn normalize_fields(fields: Option<BTreeMap<String, String>>) -> Option<BTreeMap<String, String>> {
    fields.map(|fields| {
        fields
            .into_iter()
            .map(|(name, value)| (name, normalize_to_lf(&value)))
            .collect()
    })
}

async fn round_trip<T: DaemonTransport>(
    transport: &T,
    msg: ClientMessage,
) -> Result<DaemonMessage, ToolError> {
    transport.round_trip(msg).await.map_err(map_transport_error)
}

/// The reply of every tool that answers with one ticket in full.
fn ticket_detail_reply(response: DaemonMessage) -> Result<Value, ToolError> {
    match response {
        DaemonMessage::TicketDetail { ticket } => to_json(ticket),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `list_tickets`: no argument is required.
pub async fn list_tickets<T: DaemonTransport>(
    transport: &T,
    args: ListTicketsArgs,
) -> Result<Value, ToolError> {
    let response = round_trip(
        transport,
        ClientMessage::ListTickets {
            workspace_id: None,
            state: args.state,
            milestone: args.milestone,
            limit: args.limit,
        },
    )
    .await?;
    match response {
        DaemonMessage::TicketList {
            workspace_id,
            config,
            config_error,
            milestones,
            tickets,
            truncated,
        } => to_json(TicketListReply {
            workspace_id,
            config,
            config_error,
            milestones,
            tickets,
            truncated,
        }),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `get_ticket`: `ticket` is required.
pub async fn get_ticket<T: DaemonTransport>(
    transport: &T,
    args: GetTicketArgs,
) -> Result<Value, ToolError> {
    let ticket = require_present(args.ticket, "ticket")?;
    let response = round_trip(
        transport,
        ClientMessage::GetTicket {
            workspace_id: None,
            ticket,
        },
    )
    .await?;
    ticket_detail_reply(response)
}

/// `create_ticket`: `title` and `body` are required and non-empty. The body
/// and field values reach the daemon with LF line endings.
pub async fn create_ticket<T: DaemonTransport>(
    transport: &T,
    args: CreateTicketArgs,
) -> Result<Value, ToolError> {
    let title = require_non_empty(args.title, "title")?;
    let body = normalize_to_lf(&require_non_empty(args.body, "body")?);
    let response = round_trip(
        transport,
        ClientMessage::CreateTicket {
            title,
            body,
            milestone: args.milestone,
            state: args.state,
            fields: normalize_fields(args.fields),
        },
    )
    .await?;
    ticket_detail_reply(response)
}

/// `update_ticket`: `ticket` is required; only what is given is sent. An
/// empty milestone or field value is a removal, forwarded as given.
pub async fn update_ticket<T: DaemonTransport>(
    transport: &T,
    args: UpdateTicketArgs,
) -> Result<Value, ToolError> {
    let ticket = require_present(args.ticket, "ticket")?;
    let response = round_trip(
        transport,
        ClientMessage::UpdateTicket {
            ticket,
            title: args.title,
            body: args.body.as_deref().map(normalize_to_lf),
            milestone: args.milestone,
            state: args.state,
            fields: normalize_fields(args.fields),
        },
    )
    .await?;
    ticket_detail_reply(response)
}

/// `add_ticket_result`: `ticket` is required and `text` is required and
/// non-empty. The text reaches the daemon with LF line endings.
pub async fn add_ticket_result<T: DaemonTransport>(
    transport: &T,
    args: AddTicketResultArgs,
) -> Result<Value, ToolError> {
    let ticket = require_present(args.ticket, "ticket")?;
    let text = normalize_to_lf(&require_non_empty(args.text, "text")?);
    let response = round_trip(transport, ClientMessage::AddTicketResult { ticket, text }).await?;
    ticket_detail_reply(response)
}

/// `set_milestone`: `name` and `title` are required and non-empty, and
/// `position` is required. The description reaches the daemon with LF line
/// endings.
pub async fn set_milestone<T: DaemonTransport>(
    transport: &T,
    args: SetMilestoneArgs,
) -> Result<Value, ToolError> {
    let name = require_non_empty(args.name, "name")?;
    let title = require_non_empty(args.title, "title")?;
    let position = require_present(args.position, "position")?;
    let response = round_trip(
        transport,
        ClientMessage::SetMilestone {
            name,
            title,
            description: args.description.as_deref().map(normalize_to_lf),
            position,
        },
    )
    .await?;
    match response {
        DaemonMessage::MilestoneSet { milestone } => to_json(milestone),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}
