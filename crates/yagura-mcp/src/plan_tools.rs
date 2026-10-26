//! The seven MCP plan tools: argument validation, the `ClientMessage` each
//! sends over [`crate::transport::DaemonTransport`], and the mapping of the
//! daemon's reply onto the MCP tool contract.
//!
//! This module never re-implements the daemon's plan validation (cycles,
//! self-dependencies, duplicate `task_id`s, unordered writers of the same
//! path, size limits): every task list is forwarded to the daemon as given,
//! and the daemon's own `invalid_argument` refusal is relayed unchanged. The
//! only checks made here are for a required argument being absent or empty,
//! before any daemon round trip.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{
    ToolError, map_daemon_error, map_transport_error, require_non_empty, unexpected_response,
};
use crate::transport::DaemonTransport;
use yagura_protocol::{ClientMessage, DaemonMessage, PlanInfo, PlanTaskSpec};

/// Rejects a missing `tasks` argument. A present, empty list is a plan the
/// daemon itself refuses (`invalid_argument`), not a locally missing
/// argument, so it is passed through unchanged.
fn require_tasks(value: Option<Vec<PlanTaskSpec>>) -> Result<Vec<PlanTaskSpec>, ToolError> {
    value.ok_or_else(|| ToolError::InvalidArgument("tasks must be provided".to_string()))
}

/// Normalizes every task's `instruction` to LF line endings before it is
/// sent: the daemon's own validation
/// (`validate_task_instruction`) forbids a bare `\r`, so a caller that wrote
/// its request on Windows must not have it refused for that alone.
fn normalize_task_instructions(tasks: Vec<PlanTaskSpec>) -> Vec<PlanTaskSpec> {
    tasks
        .into_iter()
        .map(|mut task| {
            task.instruction = task
                .instruction
                .map(|text| text.replace("\r\n", "\n").replace('\r', "\n"));
            task
        })
        .collect()
}

/// Serializes a [`PlanInfo`] into the flat JSON object `create_plan`,
/// `update_plan` and `get_plan` all reply with: the struct's own fields at
/// the top level, an absent optional field carried as `null` (the same rule
/// the wire itself uses).
fn plan_detail_json(plan: PlanInfo) -> Result<Value, ToolError> {
    serde_json::to_value(plan).map_err(|err| ToolError::Internal(err.to_string()))
}

/// Arguments for the `create_plan` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct CreatePlanArgs {
    /// The plan's display title (required, non-empty).
    pub title: Option<String>,
    /// Every task of the plan, in the caller's order (required; an empty
    /// list is forwarded to the daemon, which refuses it).
    pub tasks: Option<Vec<PlanTaskSpec>>,
}

/// Arguments for the `update_plan` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct UpdatePlanArgs {
    /// The plan to replace (required, non-empty).
    pub plan_id: Option<String>,
    /// The new title. Omitted keeps the stored one; forwarded verbatim, with
    /// no local emptiness check (the daemon's own call).
    pub title: Option<String>,
    /// The full replacement task list, not a delta (required; an empty list
    /// is forwarded to the daemon, which refuses it).
    pub tasks: Option<Vec<PlanTaskSpec>>,
}

/// Arguments for the `get_plan` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct GetPlanArgs {
    /// The plan to read (required, non-empty).
    pub plan_id: Option<String>,
    /// Narrows the reply to this one task, applied on the bridge's own side
    /// after the same round trip an unnarrowed `get_plan` makes: the wire
    /// itself carries no `task_id`. Omitted keeps every task.
    pub task_id: Option<String>,
}

/// Arguments for the `delete_plan` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct DeletePlanArgs {
    /// The plan to delete (required, non-empty).
    pub plan_id: Option<String>,
}

/// Arguments for the `open_task_window` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenTaskWindowArgs {
    /// The plan holding the task (required, non-empty).
    pub plan_id: Option<String>,
    /// The task to open a window for (required, non-empty).
    pub task_id: Option<String>,
    /// Display name for the new window. Omitted keeps the task's own title.
    pub name: Option<String>,
}

/// `create_plan`: `title` and `tasks` are required; `title` must be
/// non-empty. `tasks` may be an empty list -- that is a plan the daemon
/// itself refuses, not a locally missing argument.
pub async fn create_plan<T: DaemonTransport>(
    transport: &T,
    args: CreatePlanArgs,
) -> Result<Value, ToolError> {
    let title = require_non_empty(args.title, "title")?;
    let tasks = normalize_task_instructions(require_tasks(args.tasks)?);
    let response = transport
        .round_trip(ClientMessage::CreatePlan {
            title,
            tasks,
            ticket: None,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::PlanDetail { plan } => plan_detail_json(plan),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `update_plan`: `plan_id` and `tasks` are required; `plan_id` must be
/// non-empty. `title` is forwarded verbatim (omitted keeps the stored
/// title). `tasks` may be an empty list, forwarded to the daemon unchanged.
pub async fn update_plan<T: DaemonTransport>(
    transport: &T,
    args: UpdatePlanArgs,
) -> Result<Value, ToolError> {
    let plan_id = require_non_empty(args.plan_id, "plan_id")?;
    let tasks = normalize_task_instructions(require_tasks(args.tasks)?);
    let response = transport
        .round_trip(ClientMessage::UpdatePlan {
            plan_id,
            title: args.title,
            tasks,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::PlanDetail { plan } => plan_detail_json(plan),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `get_plan`: `plan_id` is required and non-empty. `task_id`, when given,
/// narrows the reply to that one task on the bridge's own side, after the
/// same round trip an unnarrowed call makes: a task
/// window reads only its own instruction through this, not every other
/// task's.
pub async fn get_plan<T: DaemonTransport>(
    transport: &T,
    args: GetPlanArgs,
) -> Result<Value, ToolError> {
    let plan_id = require_non_empty(args.plan_id, "plan_id")?;
    let response = transport
        .round_trip(ClientMessage::GetPlan { plan_id })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::PlanDetail { mut plan } => {
            if let Some(task_id) = args.task_id {
                plan.tasks.retain(|task| task.task_id == task_id);
                if plan.tasks.is_empty() {
                    return Err(ToolError::InvalidArgument(format!(
                        "no task with task_id {task_id:?} exists in this plan"
                    )));
                }
            }
            plan_detail_json(plan)
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `list_plans`: no arguments. Maps `DaemonMessage::PlanList` to
/// `{ "plans": [...] }`.
pub async fn list_plans<T: DaemonTransport>(transport: &T) -> Result<Value, ToolError> {
    let response = transport
        .round_trip(ClientMessage::ListPlans)
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::PlanList { plans } => {
            let plans_json =
                serde_json::to_value(plans).map_err(|err| ToolError::Internal(err.to_string()))?;
            Ok(json!({ "plans": plans_json }))
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `delete_plan`: `plan_id` is required and non-empty.
pub async fn delete_plan<T: DaemonTransport>(
    transport: &T,
    args: DeletePlanArgs,
) -> Result<Value, ToolError> {
    let plan_id = require_non_empty(args.plan_id, "plan_id")?;
    let response = transport
        .round_trip(ClientMessage::DeletePlan { plan_id })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::PlanDeleted { plan_id } => Ok(json!({ "plan_id": plan_id })),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `list_roles`: no arguments. Maps `DaemonMessage::RoleList` to
/// `{ "roles": [...] }`, plus `"default_role"` when the table names one.
pub async fn list_roles<T: DaemonTransport>(transport: &T) -> Result<Value, ToolError> {
    let response = transport
        .round_trip(ClientMessage::ListRoles)
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::RoleList {
            roles,
            default_role,
        } => {
            let roles_json =
                serde_json::to_value(roles).map_err(|err| ToolError::Internal(err.to_string()))?;
            let mut reply = json!({ "roles": roles_json });
            if let (Some(default_role), Some(object)) = (default_role, reply.as_object_mut()) {
                object.insert("default_role".to_string(), Value::String(default_role));
            }
            Ok(reply)
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `open_task_window`: `plan_id` and `task_id` are required and non-empty.
/// `name` is forwarded verbatim (omitted keeps the task's own title).
pub async fn open_task_window<T: DaemonTransport>(
    transport: &T,
    args: OpenTaskWindowArgs,
) -> Result<Value, ToolError> {
    let plan_id = require_non_empty(args.plan_id, "plan_id")?;
    let task_id = require_non_empty(args.task_id, "task_id")?;
    let response = transport
        .round_trip(ClientMessage::OpenTaskWindow {
            plan_id,
            task_id,
            name: args.name,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::TaskWindowOpened {
            plan_id,
            task_id,
            window_id,
            name,
            assignment,
        } => {
            let assignment_json = serde_json::to_value(assignment)
                .map_err(|err| ToolError::Internal(err.to_string()))?;
            let mut result = json!({
                "plan_id": plan_id,
                "task_id": task_id,
                "window_id": window_id,
                "assignment": assignment_json,
            });
            if let (Some(name), Some(object)) = (name, result.as_object_mut()) {
                object.insert("name".to_string(), Value::String(name));
            }
            Ok(result)
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}
