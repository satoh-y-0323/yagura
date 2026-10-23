//! The two MCP report tools: argument validation, the `ClientMessage` each
//! sends over [`crate::transport::DaemonTransport`], and the mapping of the
//! daemon's reply onto the MCP tool contract.
//!
//! The size and shape rules for `summary` and `path` belong to the daemon:
//! they are forwarded as given and the daemon's own `invalid_argument`
//! refusal is relayed unchanged. The only checks made here are for a required
//! argument being absent or empty, and for an `outcome` outside the wire's
//! vocabulary, before any daemon round trip.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::tools::{
    ToolError, map_daemon_error, map_transport_error, require_non_empty, unexpected_response,
};
use crate::transport::DaemonTransport;
use yagura_protocol::{ClientMessage, DaemonMessage, ReportOutcome};

/// Parses the caller's `outcome` string into the typed [`ReportOutcome`],
/// reusing serde's snake_case vocabulary instead of a second parser.
fn parse_outcome(value: Option<String>) -> Result<ReportOutcome, ToolError> {
    let raw = require_non_empty(value, "outcome")?;
    serde_json::from_value(Value::String(raw.clone())).map_err(|_| {
        ToolError::InvalidArgument(format!(
            "outcome {raw:?} is not one of \"succeeded\" or \"failed\""
        ))
    })
}

fn to_json<S: serde::Serialize>(value: S) -> Result<Value, ToolError> {
    serde_json::to_value(value).map_err(|err| ToolError::Internal(err.to_string()))
}

/// Arguments for the `send_report` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SendReportArgs {
    /// `"succeeded"` or `"failed"` (required). Kept as a string so an
    /// unknown value is refused as `invalid_argument` rather than failing to
    /// deserialize.
    pub outcome: Option<String>,
    /// The reporting window's account of the work (required, non-empty).
    pub summary: Option<String>,
    /// Where a longer result was written. Forwarded verbatim when present.
    pub path: Option<String>,
}

/// Arguments for the `wait_for_report` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct WaitForReportArgs {
    /// How long to wait, in seconds (required). `0` returns at once.
    pub timeout_sec: Option<u64>,
}

/// `send_report`: `outcome` and `summary` are required; `outcome` must be
/// `succeeded` or `failed` and `summary` non-empty. Replies with
/// `{ "delivered_to", "task" }`, each `null` when absent.
///
/// # Errors
///
/// `invalid_argument` for a missing or malformed argument (checked before any
/// round trip) and every daemon refusal, mapped by [`map_daemon_error`];
/// transport failures are mapped by [`map_transport_error`].
pub async fn send_report<T: DaemonTransport>(
    transport: &T,
    args: SendReportArgs,
) -> Result<Value, ToolError> {
    let outcome = parse_outcome(args.outcome)?;
    let summary = require_non_empty(args.summary, "summary")?;
    let response = transport
        .round_trip(ClientMessage::SendReport {
            outcome,
            summary,
            path: args.path,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::ReportSent { delivered_to, task } => Ok(json!({
            "delivered_to": delivered_to,
            "task": to_json(task)?,
        })),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `wait_for_report`: `timeout_sec` is required; `0` returns at once. Replies
/// with `{ "reports": [...] }`, each report carrying the wire's keys with an
/// absent `path` or `task` as `null`. An `awaiting` list the daemon sent is
/// passed through; without one the key is left out.
///
/// # Errors
///
/// `invalid_argument` for a missing `timeout_sec` (checked before any round
/// trip); daemon refusals and transport failures are mapped by
/// [`map_daemon_error`] and [`map_transport_error`].
pub async fn wait_for_report<T: DaemonTransport>(
    transport: &T,
    args: WaitForReportArgs,
) -> Result<Value, ToolError> {
    let timeout_sec = args
        .timeout_sec
        .ok_or_else(|| ToolError::InvalidArgument("timeout_sec must be provided".to_string()))?;
    let response = transport
        .round_trip(ClientMessage::WaitForReport { timeout_sec })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::Reports { reports, awaiting } => {
            let mut result = json!({ "reports": to_json(reports)? });
            // A daemon that sent no list leaves the key out, so an absent key
            // never reads as "waiting on nobody".
            if let (Some(awaiting), Value::Object(fields)) = (awaiting, &mut result) {
                fields.insert("awaiting".to_string(), to_json(awaiting)?);
            }
            Ok(result)
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}
