//! The pure MCP tool logic: argument validation, the `ClientMessage` this
//! crate sends over [`crate::transport::DaemonTransport`], and the mapping
//! of the daemon's reply onto the MCP tool contract.
//!
//! It has no I/O of its own (it is generic over `T: DaemonTransport`), so it
//! is unit-testable against a scripted transport double.

use serde::Deserialize;
use serde_json::{Value, json};

use crate::transport::{
    DaemonTransport, RequesterIdentity, TransportError, UNKNOWN_TYPE_CODE, message_kind,
};
use yagura_protocol::{
    ClientMessage, ControlKey, DaemonMessage, WindowStatus, validate_control_keys,
};

/// The fixed MCP error-code vocabulary, exposed via
/// [`ToolError::code`]. Mapping it onto the MCP `content` / `isError` JSON
/// shape is `main.rs`'s job.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ToolError {
    /// The daemon could not be reached (`TransportError::Unavailable`).
    #[error("the yagura daemon is unavailable: {0}")]
    DaemonUnavailable(String),
    /// A required argument was missing, empty, or otherwise malformed.
    #[error("invalid argument: {0}")]
    InvalidArgument(String),
    /// The daemon reported that the referenced window is not known to it.
    #[error("window not found: {0}")]
    WindowNotFound(String),
    /// The request used a feature this bridge/daemon does not implement yet,
    /// or one the running daemon's wire revision is too old to carry.
    #[error("unsupported: {0}")]
    Unsupported(String),
    /// The daemon reported that the target window's child process has
    /// already exited.
    #[error("the window's child process has exited: {0}")]
    ChildExited(String),
    /// The daemon refused the request because a resource limit (e.g.
    /// `max_windows`) was reached.
    #[error("resource exhausted: {0}")]
    ResourceExhausted(String),
    /// The daemon failed to spawn the requested child process.
    #[error("failed to spawn the window's process: {0}")]
    SpawnFailed(String),
    /// The daemon refused a destructive operation because it is not
    /// approved by the active playbook (fail-closed default).
    #[error("approval required: {0}")]
    ApprovalRequired(String),
    /// The daemon refused an interrupt because the target window's status
    /// is `Unknown` (fail-safe; not relaxable by a playbook).
    #[error("the window's status is unknown: {0}")]
    StatusUnknown(String),
    /// The daemon judged the target window's composer and never wrote the
    /// text there, so the submit key was never pressed for it.
    #[error("the text was not written into the window: {0}")]
    NotWritten(String),
    /// The daemon refused to write text into the target window because the
    /// window's definition declares no composer, so the text could never be
    /// typed there as input.
    #[error("the window has no composer: {0}")]
    NoComposer(String),
    /// The daemon refused the request because a rate limit was exceeded.
    #[error("rate limited: {0}")]
    RateLimited(String),
    /// The daemon rejected the request's authentication token.
    #[error("authentication failed: {0}")]
    AuthFailed(String),
    /// A human approver denied the held request.
    #[error("the approval request was denied: {0}")]
    ApprovalDenied(String),
    /// No decision arrived before the approval deadline.
    #[error("the approval request timed out: {0}")]
    ApprovalTimeout(String),
    /// The request was already settled or withdrawn when a decision arrived
    /// for it.
    #[error("the approval request is no longer pending: {0}")]
    ApprovalNotPending(String),
    /// The daemon refused the operation in the current state: a conflicting
    /// connection origin claim, or a plan ledger state that rules the call out.
    #[error("invalid state: {0}")]
    InvalidState(String),
    /// The daemon reported that the referenced plan does not exist, or is
    /// not visible to the calling window.
    #[error("plan not found: {0}")]
    PlanNotFound(String),
    /// The daemon reported that the calling window is not the plan's living
    /// owner.
    #[error("not the plan's owner: {0}")]
    NotPlanOwner(String),
    /// The daemon refused to open a task window because the plan is still
    /// waiting for an approver to approve it.
    #[error("plan not approved: {0}")]
    PlanNotApproved(String),
    /// The named launch profile has been disabled in the user's settings.
    /// Not an argument error: repeating the same request reworded will not help.
    #[error("profile disabled: {0}")]
    ProfileDisabled(String),
    /// A contract violation by the daemon (undecodable/unexpected
    /// response), or any other failure that is not the caller's fault.
    #[error("internal error: {0}")]
    Internal(String),
}

impl ToolError {
    /// The machine-readable, snake_case MCP error code.
    pub fn code(&self) -> &'static str {
        match self {
            Self::DaemonUnavailable(_) => "daemon_unavailable",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::WindowNotFound(_) => "window_not_found",
            Self::Unsupported(_) => "unsupported",
            Self::ChildExited(_) => "child_exited",
            Self::ResourceExhausted(_) => "resource_exhausted",
            Self::SpawnFailed(_) => "spawn_failed",
            Self::ApprovalRequired(_) => "approval_required",
            Self::StatusUnknown(_) => "status_unknown",
            Self::NotWritten(_) => "not_written",
            Self::NoComposer(_) => "no_composer",
            Self::RateLimited(_) => "rate_limited",
            Self::AuthFailed(_) => "auth_failed",
            Self::ApprovalDenied(_) => "approval_denied",
            Self::ApprovalTimeout(_) => "approval_timeout",
            Self::ApprovalNotPending(_) => "approval_not_pending",
            Self::InvalidState(_) => "invalid_state",
            Self::PlanNotFound(_) => "plan_not_found",
            Self::NotPlanOwner(_) => "not_plan_owner",
            Self::PlanNotApproved(_) => "plan_not_approved",
            Self::ProfileDisabled(_) => "profile_disabled",
            Self::Internal(_) => "internal",
        }
    }
}

/// Arguments for the `get_window_status` tool.
///
/// `window_id` is an `Option` so that a missing argument and an empty string
/// stay distinct, constructible inputs; both are rejected as
/// `invalid_argument`.
#[derive(Debug, Clone, Deserialize)]
pub struct GetWindowStatusArgs {
    /// The window ID to query.
    pub window_id: Option<String>,
}

/// Arguments for the `send_message` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SendMessageArgs {
    /// The window ID to send text to.
    pub window_id: Option<String>,
    /// The text to send.
    pub text: Option<String>,
    /// Delivery mode. Only `"immediate"` (the default when omitted) is
    /// implemented; any other value is `unsupported`.
    pub mode: Option<String>,
}

/// Arguments for the `send_keys` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SendKeysArgs {
    /// The window ID to send keys to.
    pub window_id: Option<String>,
    /// Named control keys, in the order they should be written.
    pub keys: Option<Vec<ControlKey>>,
}

/// Arguments for the `open_window` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct OpenWindowArgs {
    /// Command and arguments to execute (required, non-empty).
    #[serde(default)]
    pub command: Vec<String>,
    /// Optional working directory.
    pub cwd: Option<String>,
    /// Optional display name for the window.
    pub name: Option<String>,
    /// Optional vendor identifier.
    pub vendor: Option<String>,
    /// Optional detection definition name (one of the names `list_detections`
    /// reports, from either of the daemon's two layers).
    pub detection: Option<String>,
    /// Optional profile identifier. Mutually exclusive with `command`, `detection`, and `vendor`.
    #[serde(default)]
    pub profile: Option<String>,
    /// Model launch option. Only valid with `profile`; the daemon resolves
    /// the value against the profile's own alias table.
    pub model: Option<String>,
    /// Effort launch option. Only valid with `profile`; the daemon resolves
    /// the value against the profile's own alias table.
    pub effort: Option<String>,
}

/// Arguments for the `interrupt_window` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct InterruptWindowArgs {
    /// The window ID to interrupt.
    pub window_id: Option<String>,
}

/// Arguments for the `close_window` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct CloseWindowArgs {
    /// The window ID to close.
    pub window_id: Option<String>,
    /// Skip the graceful grace period. The default (`false`) is owned by
    /// the daemon, so an omitted value is forwarded as `None`.
    pub force: Option<bool>,
}

/// Arguments for the `wait_for_status` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct WaitForStatusArgs {
    /// The window ID to wait on.
    pub window_id: Option<String>,
    /// The status to wait for, as its wire (snake_case) spelling. Parsed
    /// into the typed wire field; `"unknown"` is refused.
    pub target_status: Option<String>,
    /// Maximum time to wait, in seconds. The wire field is a bare `u64`,
    /// so this bridge has no default of its own to apply.
    pub timeout_sec: Option<u64>,
}

/// Arguments for the `read_output` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadOutputArgs {
    /// The window ID to read output from.
    pub window_id: Option<String>,
    /// Number of trailing lines. The default and the upper bound are owned
    /// by the daemon, so an omitted value is forwarded as `None`.
    pub lines: Option<u32>,
}

/// Arguments for the `send_handoff` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct SendHandoffArgs {
    /// The existing window to hand the work to. Mutually exclusive with
    /// `profile`.
    pub window_id: Option<String>,
    /// The launch profile to open a new window from. Mutually exclusive with
    /// `window_id`.
    pub profile: Option<String>,
    /// Working directory for the window `profile` opens. Rejected alongside
    /// `window_id`.
    pub cwd: Option<String>,
    /// Display name for the window `profile` opens. Rejected alongside
    /// `window_id`.
    pub name: Option<String>,
    /// What the receiving window is asked to look at.
    pub reference: Option<String>,
    /// The requester's own words about the work being handed over.
    pub note: Option<String>,
    /// Model launch option for a window `profile` opens. Rejected alongside
    /// `window_id`.
    pub model: Option<String>,
    /// Effort launch option for a window `profile` opens. Rejected alongside
    /// `window_id`.
    pub effort: Option<String>,
}

/// Arguments for the `send_chat` and `broadcast_chat` tools, which address
/// their post the same way and differ only in the wire message they send.
///
/// `body` is an `Option` so that a missing argument and an empty string stay
/// distinct, constructible inputs; both are rejected as `invalid_argument`.
#[derive(Debug, Clone, Deserialize)]
pub struct SendChatArgs {
    /// The room to post to. Forwarded verbatim -- the default room is the
    /// daemon's to choose.
    pub room_id: Option<String>,
    /// The message body.
    pub body: Option<String>,
}

/// Arguments for the `read_chat` tool.
#[derive(Debug, Clone, Deserialize)]
pub struct ReadChatArgs {
    /// The room to read from. Forwarded verbatim, like
    /// [`SendChatArgs::room_id`].
    pub room_id: Option<String>,
    /// Return only messages after this sequence number. What an omitted
    /// value means depends on the caller, which only the daemon knows, so it
    /// is forwarded as `None`.
    pub after_seq: Option<u64>,
    /// Maximum number of messages to return. Its default and upper bound are
    /// owned by the daemon, so an omitted value is forwarded as `None`.
    pub limit: Option<u32>,
}

/// The only `mode` value `send_message` implements; anything else is
/// `unsupported`.
const SEND_MESSAGE_MODE_IMMEDIATE: &str = "immediate";

pub(crate) fn map_transport_error(err: TransportError) -> ToolError {
    match err {
        TransportError::Unavailable(message) => ToolError::DaemonUnavailable(message),
        TransportError::Protocol(message) => ToolError::Internal(message),
        TransportError::Unsupported(message) => ToolError::Unsupported(message),
    }
}

/// Maps a daemon-reported `DaemonMessage::Error` code onto the corresponding
/// [`ToolError`] variant. An unrecognized code is a daemon contract violation
/// and maps to `Internal`.
pub(crate) fn map_daemon_error(code: &str, message: String) -> ToolError {
    match code {
        "invalid_argument" => ToolError::InvalidArgument(message),
        "window_not_found" => ToolError::WindowNotFound(message),
        "unsupported" => ToolError::Unsupported(message),
        UNKNOWN_TYPE_CODE => ToolError::Unsupported(message),
        "child_exited" => ToolError::ChildExited(message),
        "resource_exhausted" => ToolError::ResourceExhausted(message),
        "spawn_failed" => ToolError::SpawnFailed(message),
        "approval_required" => ToolError::ApprovalRequired(message),
        "status_unknown" => ToolError::StatusUnknown(message),
        "not_written" => ToolError::NotWritten(message),
        "no_composer" => ToolError::NoComposer(message),
        "rate_limited" => ToolError::RateLimited(message),
        "auth_failed" => ToolError::AuthFailed(message),
        "approval_denied" => ToolError::ApprovalDenied(message),
        "approval_timeout" => ToolError::ApprovalTimeout(message),
        "approval_not_pending" => ToolError::ApprovalNotPending(message),
        "invalid_state" => ToolError::InvalidState(message),
        "plan_not_found" => ToolError::PlanNotFound(message),
        "not_plan_owner" => ToolError::NotPlanOwner(message),
        "plan_not_approved" => ToolError::PlanNotApproved(message),
        "profile_disabled" => ToolError::ProfileDisabled(message),
        _ => ToolError::Internal(message),
    }
}

/// A daemon response of a variant the calling tool did not ask for is always
/// a contract violation, never a panic.
fn unexpected_response(response: &DaemonMessage) -> ToolError {
    ToolError::Internal(format!(
        "unexpected daemon response: {}",
        message_kind(response)
    ))
}

/// Adds `"requester"` -- and, only for a refused claim, `"identity_warning"`
/// -- to a gated tool's own success JSON, without disturbing any key already
/// there. Built key by key so an accepted or absent claim never writes a
/// `null` warning.
fn with_requester(mut result: Value, identity: RequesterIdentity) -> Value {
    let (requester, warning) = match identity {
        RequesterIdentity::Window(_) => ("window", None),
        RequesterIdentity::External => ("external", None),
        RequesterIdentity::Refused(warning) => ("external", Some(warning)),
    };
    if let Value::Object(map) = &mut result {
        map.insert(
            "requester".to_string(),
            Value::String(requester.to_string()),
        );
        if let Some(warning) = warning {
            map.insert("identity_warning".to_string(), Value::String(warning));
        }
    }
    result
}

/// A launch option only means anything alongside a profile, so `open_window`
/// and `send_handoff` both check `model` and `effort` for presence together
/// through this one function rather than re-deriving the `||` twice.
fn has_launch_option(model: Option<&str>, effort: Option<&str>) -> bool {
    model.is_some() || effort.is_some()
}

/// Rejects `None` and `Some(String::new())` alike as `invalid_argument`.
pub(crate) fn require_non_empty(value: Option<String>, field: &str) -> Result<String, ToolError> {
    match value {
        Some(value) if !value.is_empty() => Ok(value),
        _ => Err(ToolError::InvalidArgument(format!(
            "{field} must be a non-empty string"
        ))),
    }
}

/// Parses the caller's `target_status` string into the typed
/// [`WindowStatus`] the wire field requires, reusing serde's snake_case
/// vocabulary instead of introducing a second status parser.
///
/// `Unknown` is refused rather than forwarded: waiting for the fail-safe
/// status would turn a detection failure into a completion condition.
fn parse_wait_target(value: Option<String>) -> Result<WindowStatus, ToolError> {
    let raw = require_non_empty(value, "target_status")?;
    let parsed: WindowStatus =
        serde_json::from_value(Value::String(raw.clone())).map_err(|_| {
            ToolError::InvalidArgument(format!(
                "target_status {raw:?} is not a known window status"
            ))
        })?;
    if parsed == WindowStatus::Unknown {
        return Err(ToolError::InvalidArgument(
            "target_status \"unknown\" is not a valid wait target".to_string(),
        ));
    }
    Ok(parsed)
}

/// Rejects a missing or zero `timeout_sec`. The upper bound is validated by
/// the daemon, which owns it.
fn require_positive_timeout(value: Option<u64>) -> Result<u64, ToolError> {
    match value {
        Some(timeout_sec) if timeout_sec > 0 => Ok(timeout_sec),
        _ => Err(ToolError::InvalidArgument(
            "timeout_sec must be a positive number of seconds".to_string(),
        )),
    }
}

/// Evaluates `mode` against the fixed `"immediate"` vocabulary locally,
/// before any daemon round trip. A value check rather than a presence check,
/// so it maps to `unsupported` rather than `invalid_argument`.
fn validate_send_mode(mode: Option<&str>) -> Result<(), ToolError> {
    match mode {
        None | Some(SEND_MESSAGE_MODE_IMMEDIATE) => Ok(()),
        Some(other) => Err(ToolError::Unsupported(format!(
            "mode {other:?} is not supported; only \"immediate\" is implemented"
        ))),
    }
}

/// `list_windows`: no arguments. Maps `DaemonMessage::WindowList` to
/// `{ "windows": [...] }`.
pub async fn list_windows<T: DaemonTransport>(transport: &T) -> Result<Value, ToolError> {
    let response = transport
        .round_trip(ClientMessage::ListWindows)
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::WindowList { mut windows } => {
            // The layout and the missing-reports mark are the GUI's own
            // presentation state and mean nothing to an agent; clearing them
            // drops the keys from the response.
            for window in &mut windows {
                window.layout_json = None;
                window.status_reports_missing = None;
            }
            let windows_json = serde_json::to_value(&windows)
                .map_err(|err| ToolError::Internal(err.to_string()))?;
            Ok(json!({ "windows": windows_json }))
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `get_window_status`: `window_id` is required and non-empty.
pub async fn get_window_status<T: DaemonTransport>(
    transport: &T,
    args: GetWindowStatusArgs,
) -> Result<Value, ToolError> {
    let window_id = require_non_empty(args.window_id, "window_id")?;
    let response = transport
        .round_trip(ClientMessage::GetWindowStatus { window_id })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::WindowStatus {
            window_id,
            status,
            since_ms,
            last_output_summary,
            exit_code,
            unknown_reason,
            detection_warning,
            mcp_connected,
        } => {
            // Built key by key rather than with `json!` so that an absent
            // reason or warning leaves out the key entirely instead of
            // writing `null`.
            let mut result = serde_json::Map::new();
            result.insert("window_id".to_string(), Value::String(window_id));
            result.insert("status".to_string(), json!(status));
            result.insert("since_ms".to_string(), json!(since_ms));
            result.insert(
                "last_output_summary".to_string(),
                json!(last_output_summary),
            );
            result.insert("exit_code".to_string(), json!(exit_code));
            if let Some(reason) = unknown_reason {
                result.insert("unknown_reason".to_string(), json!(reason));
            }
            if let Some(warning) = detection_warning {
                result.insert("detection_warning".to_string(), Value::String(warning));
            }
            if let Some(connected) = mcp_connected {
                result.insert("mcp_connected".to_string(), Value::Bool(connected));
            }
            Ok(Value::Object(result))
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// Rewrites every line terminator -- both `\r\n` pairs and lone LFs -- as a
/// lone CR, leaving existing lone CRs untouched and never appending a
/// terminator the caller did not write, so text without a trailing newline
/// stays unsubmitted.
///
/// A console line editor (Windows PSReadLine in particular) submits on CR: a
/// trailing LF only opens a continuation prompt, and CRLF is read as two
/// keystrokes (submit, then an in-line newline). MCP clients cannot put a raw
/// CR in a JSON string argument, so the normalization is an MCP-layer policy.
///
/// The daemon rejects a normalized body carrying a line break anywhere but a
/// single trailing terminator (`SendTextError::EmbeddedLineBreak`): it writes
/// a body straight through with no per-line confirmation, so it cannot tell
/// whether an embedded line break reached the receiver as more than one
/// message. A single-line payload with one trailing terminator goes through
/// the target definition's input plan instead, which replaces that
/// terminator with the definition's own submit key. Normalizing here
/// therefore does not decide how the line is submitted, only that a caller
/// who wrote LF still gets a terminated line; a caller with several lines to
/// deliver sends them one at a time, or writes them into a plan task's
/// `instruction` (see [`crate::plan_tools::create_plan`]).
fn normalize_line_endings(text: &str) -> String {
    let mut normalized = String::with_capacity(text.len());
    let mut previous_was_cr = false;
    for character in text.chars() {
        match character {
            // The CR of this pair was already emitted; drop the LF.
            '\n' if previous_was_cr => {}
            '\n' => normalized.push('\r'),
            _ => normalized.push(character),
        }
        previous_was_cr = character == '\r';
    }
    normalized
}

/// `send_message`: `window_id` and `text` are required and non-empty;
/// `mode` (if present) must be `"immediate"`. All three are checked before
/// any daemon round trip, whatever the target window's status.
///
/// `text` is forwarded with its line endings normalized by
/// [`normalize_line_endings`] and is otherwise unmodified. The `text` size
/// limit is enforced solely by the daemon's `send_text` handler; this
/// function relays whatever error it sends back.
pub async fn send_message<T: DaemonTransport>(
    transport: &T,
    args: SendMessageArgs,
) -> Result<Value, ToolError> {
    let window_id = require_non_empty(args.window_id, "window_id")?;
    let text = normalize_line_endings(&require_non_empty(args.text, "text")?);
    validate_send_mode(args.mode.as_deref())?;

    let response = transport
        .round_trip(ClientMessage::SendText { window_id, text })
        .await
        .map_err(map_transport_error)?;
    match response {
        // Success is observed solely via `TextSent`: any other non-error reply
        // is a daemon contract violation, not a silent success.
        DaemonMessage::TextSent { .. } => Ok(with_requester(
            json!({ "ok": true }),
            transport.requester_identity(),
        )),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `send_keys`: validates a non-empty sequence of at most
/// [`yagura_protocol::SEND_KEYS_MAX_KEYS`] named keys
/// before crossing the daemon boundary.
pub async fn send_keys<T: DaemonTransport>(
    transport: &T,
    args: SendKeysArgs,
) -> Result<Value, ToolError> {
    let window_id = require_non_empty(args.window_id, "window_id")?;
    let keys = args
        .keys
        .ok_or_else(|| ToolError::InvalidArgument("keys is required".to_string()))?;
    validate_control_keys(&keys).map_err(|err| ToolError::InvalidArgument(err.to_string()))?;

    let response = transport
        .round_trip(ClientMessage::SendKeys { window_id, keys })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::KeysSent { .. } => Ok(with_requester(
            json!({ "ok": true }),
            transport.requester_identity(),
        )),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `open_window`: either `command` (as a non-empty array) or `profile` (as a
/// non-empty string) must be provided, but not both. When `profile` is
/// specified, `detection` and `vendor` are rejected as mutually exclusive.
/// `detection` is forwarded to the daemon as an optional per-window detection
/// definition selector.
///
/// A `detection_warning` the daemon reports is passed through as a key of the
/// same name; the key is absent (never `null`) when there is none, matching
/// the wire's own omission of the field.
pub async fn open_window<T: DaemonTransport>(
    transport: &T,
    args: OpenWindowArgs,
) -> Result<Value, ToolError> {
    yagura_protocol::validate_open_window_exclusivity(
        !args.command.is_empty(),
        args.profile.as_deref(),
        args.detection.as_deref(),
        args.vendor.as_deref(),
        has_launch_option(args.model.as_deref(), args.effort.as_deref()),
    )
    .map_err(|err| ToolError::InvalidArgument(err.to_string()))?;

    let response = if let Some(profile) = args.profile {
        transport
            .round_trip(ClientMessage::OpenWindowFromProfile {
                profile,
                cwd: args.cwd,
                name: args.name,
                model: args.model,
                effort: args.effort,
            })
            .await
            .map_err(map_transport_error)?
    } else {
        transport
            .round_trip(ClientMessage::OpenWindow {
                command: args.command,
                cwd: args.cwd,
                name: args.name,
                vendor: args.vendor,
                detection: args.detection,
            })
            .await
            .map_err(map_transport_error)?
    };

    match response {
        DaemonMessage::WindowOpened {
            window_id,
            detection_warning,
        } => {
            // Built key by key rather than with `json!` so that the absent
            // warning leaves out the key entirely instead of writing `null`.
            let mut result = serde_json::Map::new();
            result.insert("window_id".to_string(), Value::String(window_id));
            if let Some(warning) = detection_warning {
                result.insert("detection_warning".to_string(), Value::String(warning));
            }
            Ok(Value::Object(result))
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `interrupt_window`: `window_id` is required and non-empty. The approval
/// gate, the `Unknown` status gate and the interrupt byte sequence all live
/// in the daemon; this bridge relays its refusal codes unchanged.
///
/// Only `DaemonMessage::WindowInterrupted` counts as success: control
/// responses are not interchangeable.
pub async fn interrupt_window<T: DaemonTransport>(
    transport: &T,
    args: InterruptWindowArgs,
) -> Result<Value, ToolError> {
    let window_id = require_non_empty(args.window_id, "window_id")?;
    let response = transport
        .round_trip(ClientMessage::InterruptWindow { window_id })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::WindowInterrupted { window_id } => Ok(with_requester(
            json!({ "window_id": window_id }),
            transport.requester_identity(),
        )),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `close_window`: `window_id` is required and non-empty. `force` is
/// forwarded verbatim -- its default is owned by the daemon, so an omitted
/// value must stay `None` on the wire.
pub async fn close_window<T: DaemonTransport>(
    transport: &T,
    args: CloseWindowArgs,
) -> Result<Value, ToolError> {
    let window_id = require_non_empty(args.window_id, "window_id")?;
    let response = transport
        .round_trip(ClientMessage::CloseWindow {
            window_id,
            force: args.force,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::WindowClosed { window_id } => Ok(with_requester(
            json!({ "window_id": window_id }),
            transport.requester_identity(),
        )),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `wait_for_status`: `window_id`, a parsable non-`Unknown` `target_status`
/// and a positive `timeout_sec` are all required, and all are checked
/// before any daemon round trip.
///
/// A `"timeout"` outcome is a successful result, not an error: the returned
/// `status` is the one observed when the wait ended and need not equal the
/// target.
pub async fn wait_for_status<T: DaemonTransport>(
    transport: &T,
    args: WaitForStatusArgs,
) -> Result<Value, ToolError> {
    let window_id = require_non_empty(args.window_id, "window_id")?;
    let target_status = parse_wait_target(args.target_status)?;
    let timeout_sec = require_positive_timeout(args.timeout_sec)?;

    let response = transport
        .round_trip(ClientMessage::WaitForStatus {
            window_id,
            target_status,
            timeout_sec,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::WaitResult {
            window_id,
            outcome,
            status,
        } => Ok(json!({
            "window_id": window_id,
            "outcome": outcome,
            "status": status,
        })),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `list_detections`: no arguments. Maps `DaemonMessage::DetectionList` to
/// `{ "detections": [...], "bundled_names": [...], "directory_names": [...] }`.
///
/// `detections` is the union of the daemon's two layers; the per-layer keys
/// are always present (empty arrays included) so the caller can tell which
/// layer a name came from, a name in both being served from the directory.
///
/// A `warning` the daemon reports (the detection directory could not be
/// enumerated) is passed through as a key of the same name, present only
/// when `Some`, and does not turn the response into an error: the caller can
/// still distinguish "no definitions" from "the directory could not be
/// read" without either becoming a failure.
pub async fn list_detections<T: DaemonTransport>(transport: &T) -> Result<Value, ToolError> {
    let response = transport
        .round_trip(ClientMessage::ListDetections)
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::DetectionList {
            names,
            bundled_names,
            directory_names,
            warning,
        } => {
            // Built key by key rather than with `json!` so that the absent
            // warning leaves out the key entirely instead of writing `null`.
            let mut result = serde_json::Map::new();
            result.insert(
                "detections".to_string(),
                Value::Array(names.into_iter().map(Value::String).collect()),
            );
            result.insert(
                "bundled_names".to_string(),
                Value::Array(bundled_names.into_iter().map(Value::String).collect()),
            );
            result.insert(
                "directory_names".to_string(),
                Value::Array(directory_names.into_iter().map(Value::String).collect()),
            );
            if let Some(warning) = warning {
                result.insert("warning".to_string(), Value::String(warning));
            }
            Ok(Value::Object(result))
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `list_profiles`: no arguments. Maps `DaemonMessage::ProfileList` to
/// `{ "profiles": [...], "warnings": [...] }`.
///
/// A profile the user has disabled is left out, since it cannot be launched.
///
/// A `warnings` array the daemon reports (excluded files, or the reason the
/// profiles directory could not be enumerated) is passed through as a key of
/// the same name, present only when non-empty, and does not turn the response
/// into an error: the caller can still list available profiles even if some
/// files could not be validated or the directory could not be read.
pub async fn list_profiles<T: DaemonTransport>(transport: &T) -> Result<Value, ToolError> {
    let response = transport
        .round_trip(ClientMessage::ListProfiles)
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::ProfileList { profiles, warnings } => {
            // Built key by key rather than with `json!` so that empty
            // warnings leaves out the key entirely instead of writing an empty array.
            let mut result = serde_json::Map::new();

            // Map each ProfileInfo to an object, omitting keys where the value is None.
            let profiles_json: Vec<Value> = profiles
                .into_iter()
                .filter(|profile| profile.enabled)
                .map(|profile| -> Result<Value, ToolError> {
                    let mut obj = serde_json::Map::new();
                    obj.insert("id".to_string(), Value::String(profile.id));
                    obj.insert(
                        "display_name".to_string(),
                        Value::String(profile.display_name),
                    );
                    obj.insert("available".to_string(), Value::Bool(profile.available));
                    if let Some(detection) = profile.detection {
                        obj.insert("detection".to_string(), Value::String(detection));
                    }
                    if let Some(warning) = profile.detection_warning {
                        obj.insert("detection_warning".to_string(), Value::String(warning));
                    }
                    if let Some(cwd) = profile.cwd {
                        obj.insert("cwd".to_string(), Value::String(cwd));
                    }
                    // The daemon always carries `command`, empty when the
                    // profile has no argv to launch; an empty one is omitted
                    // here like every other absent value.
                    if !profile.command.is_empty() {
                        obj.insert(
                            "command".to_string(),
                            Value::Array(profile.command.into_iter().map(Value::String).collect()),
                        );
                    }
                    if let Some(reason) = profile.unavailable_reason {
                        obj.insert("unavailable_reason".to_string(), Value::String(reason));
                    }
                    // An axis the profile does not map is an absent key, not
                    // `null`; one it maps with no aliases is still a key,
                    // carrying an empty `aliases` array.
                    if let Some(model) = profile.model {
                        obj.insert(
                            "model".to_string(),
                            serde_json::to_value(model)
                                .map_err(|err| ToolError::Internal(err.to_string()))?,
                        );
                    }
                    if let Some(effort) = profile.effort {
                        obj.insert(
                            "effort".to_string(),
                            serde_json::to_value(effort)
                                .map_err(|err| ToolError::Internal(err.to_string()))?,
                        );
                    }
                    Ok(Value::Object(obj))
                })
                .collect::<Result<Vec<Value>, ToolError>>()?;

            result.insert("profiles".to_string(), Value::Array(profiles_json));
            if !warnings.is_empty() {
                result.insert(
                    "warnings".to_string(),
                    Value::Array(warnings.into_iter().map(Value::String).collect()),
                );
            }
            Ok(Value::Object(result))
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `read_output`: `window_id` is required and non-empty. `lines` is
/// forwarded verbatim -- its default and upper bound are owned by the daemon.
/// `truncated` is passed through so the caller can tell a partial tail from
/// the whole output.
pub async fn read_output<T: DaemonTransport>(
    transport: &T,
    args: ReadOutputArgs,
) -> Result<Value, ToolError> {
    let window_id = require_non_empty(args.window_id, "window_id")?;
    let response = transport
        .round_trip(ClientMessage::ReadOutput {
            window_id,
            lines: args.lines,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::WindowOutput {
            window_id,
            text,
            truncated,
        } => Ok(json!({
            "window_id": window_id,
            "text": text,
            "truncated": truncated,
        })),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// Applies the shared handoff text rule to one of the two free-text fields.
/// An omitted value is validated as the empty string so that a missing and an
/// empty argument are refused by the same rule rather than by two.
fn require_handoff_text(
    value: Option<String>,
    field: &str,
    max_bytes: usize,
) -> Result<String, ToolError> {
    let value = value.unwrap_or_default();
    yagura_protocol::validate_handoff_text(&value, max_bytes)
        .map_err(|err| ToolError::InvalidArgument(format!("{field}: {err}")))?;
    Ok(value)
}

/// `send_handoff`: hands work to another window, either an existing one
/// (`window_id`) or a new one opened from a launch profile (`profile`).
///
/// The target exclusivity rule and both text fields are checked with the
/// protocol's own validators before any daemon round trip, so a malformed
/// request never reaches the approval gate. The envelope's own size limit,
/// the profile's existence and the readiness wait all belong to the daemon;
/// this bridge relays its refusal codes unchanged.
///
/// Only `DaemonMessage::HandoffSent` counts as success: any other non-error
/// reply is a daemon contract violation, not a silent delivery.
pub async fn send_handoff<T: DaemonTransport>(
    transport: &T,
    args: SendHandoffArgs,
) -> Result<Value, ToolError> {
    yagura_protocol::validate_handoff_target(
        args.window_id.as_deref(),
        args.profile.as_deref(),
        args.cwd.as_deref(),
        args.name.as_deref(),
        has_launch_option(args.model.as_deref(), args.effort.as_deref()),
    )
    .map_err(|err| ToolError::InvalidArgument(err.to_string()))?;
    let reference = require_handoff_text(
        args.reference,
        "reference",
        yagura_protocol::HANDOFF_REFERENCE_MAX_BYTES,
    )?;
    let note = require_handoff_text(args.note, "note", yagura_protocol::HANDOFF_NOTE_MAX_BYTES)?;

    let response = transport
        .round_trip(ClientMessage::SendHandoff {
            window_id: args.window_id,
            profile: args.profile,
            cwd: args.cwd,
            name: args.name,
            model: args.model,
            effort: args.effort,
            reference,
            note,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::HandoffSent {
            window_id,
            opened,
            delivery,
        } => Ok(with_requester(
            json!({ "window_id": window_id, "opened": opened, "delivery": delivery }),
            transport.requester_identity(),
        )),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// Applies the protocol's chat body rule before any daemon round trip. An
/// omitted body is validated as the empty string, so a missing and an empty
/// argument are refused by the same rule rather than by two.
fn require_chat_body(value: Option<String>) -> Result<String, ToolError> {
    let value = value.unwrap_or_default();
    yagura_protocol::validate_chat_body(&value)
        .map_err(|err| ToolError::InvalidArgument(format!("body: {err}")))?;
    Ok(value)
}

/// Maps the reply both chat posting tools get back. Only
/// `DaemonMessage::ChatPosted` counts as success: any other non-error reply
/// is a daemon contract violation, not a silently recorded post.
fn chat_posted_result(
    response: DaemonMessage,
    identity: RequesterIdentity,
) -> Result<Value, ToolError> {
    match response {
        DaemonMessage::ChatPosted {
            room_id,
            seq,
            targets,
            user,
            unresolved,
            halted,
        } => Ok(with_requester(
            json!({
                "room_id": room_id,
                "seq": seq,
                "targets": targets,
                "user": user,
                "unresolved": unresolved,
                "halted": halted,
            }),
            identity,
        )),
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}

/// `send_chat`: posts a message to the shared room, addressed to whoever its
/// `@` mentions resolve to.
///
/// The body is checked with the protocol's own validator before any daemon
/// round trip, so a malformed post never reaches the approval gate. Mention
/// resolution, the room's halt state and the approval decision all belong to
/// the daemon; this bridge relays its refusal codes unchanged.
pub async fn send_chat<T: DaemonTransport>(
    transport: &T,
    args: SendChatArgs,
) -> Result<Value, ToolError> {
    let body = require_chat_body(args.body)?;
    let response = transport
        .round_trip(ClientMessage::SendChat {
            room_id: args.room_id,
            body,
        })
        .await
        .map_err(map_transport_error)?;
    chat_posted_result(response, transport.requester_identity())
}

/// `broadcast_chat`: posts to every participant in the room but the poster.
/// Same arguments and same reply as [`send_chat`]; only the wire message
/// differs.
pub async fn broadcast_chat<T: DaemonTransport>(
    transport: &T,
    args: SendChatArgs,
) -> Result<Value, ToolError> {
    let body = require_chat_body(args.body)?;
    let response = transport
        .round_trip(ClientMessage::BroadcastChat {
            room_id: args.room_id,
            body,
        })
        .await
        .map_err(map_transport_error)?;
    chat_posted_result(response, transport.requester_identity())
}

/// `read_chat`: reads a page of the room's history. `after_seq` and `limit`
/// are forwarded verbatim -- their defaults, the upper bound and what an
/// omitted cursor means per caller are all the daemon's.
///
/// This is a read, so the reply carries no `requester` key.
pub async fn read_chat<T: DaemonTransport>(
    transport: &T,
    args: ReadChatArgs,
) -> Result<Value, ToolError> {
    let response = transport
        .round_trip(ClientMessage::ReadChat {
            room_id: args.room_id,
            after_seq: args.after_seq,
            limit: args.limit,
        })
        .await
        .map_err(map_transport_error)?;
    match response {
        DaemonMessage::ChatMessages {
            room_id,
            messages,
            next_after_seq,
            halted,
            needs_user,
            roster,
        } => {
            let messages = serde_json::to_value(messages)
                .map_err(|err| ToolError::Internal(format!("undecodable chat messages: {err}")))?;
            let roster = serde_json::to_value(roster)
                .map_err(|err| ToolError::Internal(format!("undecodable chat roster: {err}")))?;
            // Built key by key rather than with `json!` so that a room that
            // is not halted leaves out the key entirely instead of writing
            // `null`.
            let mut result = serde_json::Map::new();
            result.insert("room_id".to_string(), Value::String(room_id));
            result.insert("messages".to_string(), messages);
            result.insert("next_after_seq".to_string(), json!(next_after_seq));
            result.insert("needs_user".to_string(), Value::Bool(needs_user));
            result.insert("roster".to_string(), roster);
            if let Some(halt) = halted {
                result.insert(
                    "halted".to_string(),
                    json!({ "reason": halt.reason, "since_ms": halt.since_ms }),
                );
            }
            Ok(Value::Object(result))
        }
        DaemonMessage::Error { code, message, .. } => Err(map_daemon_error(&code, message)),
        other => Err(unexpected_response(&other)),
    }
}
