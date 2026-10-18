//! The WebView boundary: the canonical Tauri event set and the return
//! type of the `attach` command.
//!
//! Every field name here is `snake_case` and is mirrored verbatim by the
//! TypeScript narrowing layer (`gui/src/`), per the cross-boundary naming
//! rule (Rust is the source of truth).

use serde::{Deserialize, Serialize};
use yagura_protocol::{
    ApprovalRequest, McpRegistrationAction, McpRegistrationStatusInfo, McpRegistrationStep,
    ModelChoice, ModelChoiceSource, UnknownReason, WindowInfo, WindowStatus, WithdrawReason,
};

use crate::recovery_state::RecoveryStatus;

/// The child process exited. Payload: [`ExitPayload`].
///
/// Re-attaching to an already exited child (`attached.child_exited == true`)
/// is reported through this same event, so the WebView has a single
/// termination path.
pub const EVENT_EXIT: &str = "session://exit";

/// The connection was lost and could not be recovered. Payload:
/// [`DisconnectedPayload`].
pub const EVENT_DISCONNECTED: &str = "session://disconnected";

/// Another GUI window took over the session; this one must not reconnect.
/// Payload: [`SupersededPayload`].
pub const EVENT_SUPERSEDED: &str = "session://superseded";

/// The daemon had to be started with the weaker detachment fallback
/// (`DetachedSpawn::used_fallback`). On Windows this means
/// `CREATE_BREAKAWAY_FROM_JOB` was refused; on Unix `setsid` detachment has
/// no such fallback, so this event never fires there. Payload:
/// [`DaemonSpawnWarningPayload`].
pub const EVENT_DAEMON_SPAWN_WARNING: &str = "session://daemon-spawn-warning";

/// A confirmed status transition of the child process. Payload:
/// [`StatusPayload`].
pub const EVENT_STATUS: &str = "session://status";

/// The daemon's current window list (polling). Payload:
/// [`WindowListPayload`].
pub const EVENT_WINDOW_LIST: &str = "session://window-list";

/// The approver connection's pending queue must be replaced wholesale: sent
/// once, before the per-item [`EVENT_APPROVAL_REQUESTED`] events, whenever
/// approver registration completes (fresh or after a reconnect). Payload:
/// [`ApprovalResetPayload`].
pub const EVENT_APPROVAL_RESET: &str = "session://approval-reset";

/// A destructive operation needs a human decision. Sent once per pending
/// request right after [`EVENT_APPROVAL_RESET`], and again, individually,
/// whenever a new request arrives while already registered. Payload:
/// [`ApprovalRequestedPayload`].
pub const EVENT_APPROVAL_REQUESTED: &str = "session://approval-requested";

/// A pending request is no longer actionable (decided elsewhere, timed out,
/// or its requester disconnected). Payload: [`ApprovalWithdrawnPayload`].
pub const EVENT_APPROVAL_WITHDRAWN: &str = "session://approval-withdrawn";

/// This connection's `register_approver` attempt was refused (the
/// peer executable did not match `--approver-exe`, or the daemon has none
/// configured). Payload: [`ApprovalRegistrationRefusedPayload`].
pub const EVENT_APPROVAL_REGISTRATION_REFUSED: &str = "session://approval-registration-refused";

/// A chat room changed in a way a human should see. Payload:
/// [`ChatChangedPayload`].
pub const EVENT_CHAT_CHANGED: &str = "session://chat-changed";

/// The chat rooms, or who takes part in one, changed, so a displayed roster
/// should be read again. Payload: [`ChatParticipantsChangedPayload`].
pub const EVENT_CHAT_PARTICIPANTS_CHANGED: &str = "session://chat-participants-changed";

/// A plan's stored content changed, so a displayed copy should be read
/// again. Payload: [`PlanChangedPayload`].
pub const EVENT_PLAN_CHANGED: &str = "session://plan-changed";

/// The model list a `list_model_choices` request asked for is ready or could
/// not be taken. Payload: [`ModelChoicesPayload`].
pub const EVENT_MODEL_CHOICES: &str = "session://model-choices";

/// One profile's MCP registration state, read for a `list_mcp_registrations`
/// request. Payload: [`McpRegistrationStatusPayload`].
pub const EVENT_MCP_REGISTRATION_STATUS: &str = "session://mcp-registration-status";

/// The outcome of a `run_mcp_registration` request. Payload:
/// [`McpRegistrationResultPayload`].
pub const EVENT_MCP_REGISTRATION_RESULT: &str = "session://mcp-registration-result";

/// Daemon recovery moved to a new status. Payload:
/// [`DaemonRecoveryPayload`].
///
/// Only status *changes* are pushed, so a WebView that starts listening
/// after one has gone out reads the current status with its own request
/// rather than waiting for the next change.
pub const EVENT_DAEMON_RECOVERY: &str = "session://daemon-recovery";

/// Payload of [`EVENT_EXIT`].
#[derive(Debug, Clone, Serialize)]
pub struct ExitPayload {
    /// The window this exit event belongs to (protocol v2).
    pub window_id: String,
    /// Exit code of the child process (`-1` when it could not be determined).
    pub exit_code: i32,
}

/// Payload of [`EVENT_DISCONNECTED`].
#[derive(Debug, Clone, Serialize)]
pub struct DisconnectedPayload {
    /// The window this disconnection belongs to (protocol v2).
    pub window_id: String,
    /// Machine-readable reason, currently always `"reconnect_failed"`.
    pub reason: String,
}

/// Payload of [`EVENT_SUPERSEDED`].
#[derive(Debug, Clone, Serialize)]
pub struct SupersededPayload {
    /// The window that was superseded (protocol v2).
    pub window_id: String,
    /// Machine-readable reason, currently always `"superseded"`.
    pub reason: String,
}

/// Payload of [`EVENT_DAEMON_SPAWN_WARNING`].
#[derive(Debug, Clone, Serialize)]
pub struct DaemonSpawnWarningPayload {
    /// English text the WebView shows to the user verbatim, so it names the
    /// consequence rather than only the condition.
    pub message: String,
}

/// Payload of [`EVENT_STATUS`].
///
/// `Deserialize` is derived alongside `Serialize` even though nothing in the
/// desktop layer decodes this payload: it is what lets this crate's own tests
/// read back a body in the shape a WebView receives, including one produced
/// before an optional key existed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StatusPayload {
    /// The window this status transition belongs to (protocol v2).
    pub window_id: String,
    /// The new (confirmed) status of the child process.
    pub status: WindowStatus,
    /// Why `status` is `Unknown`, if it is. Omitted from the wire when
    /// `None`, so a peer predating this field still decodes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unknown_reason: Option<UnknownReason>,
    /// The generation of the status pair this event reports, forwarded
    /// verbatim from `DaemonMessage::StatusChanged`. `None` when the daemon
    /// predates the field, which the WebView treats as "always apply".
    /// Omitted from the wire when `None`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_rev: Option<u64>,
}

/// Payload of [`EVENT_WINDOW_LIST`].
#[derive(Debug, Clone, Serialize)]
pub struct WindowListPayload {
    /// Every window the daemon reported on this poll.
    pub windows: Vec<WindowInfo>,
    /// The wire revision the connected daemon settled on, `None` while this
    /// connection has not settled one yet for its current generation.
    /// Information for the banner shown to the user, not
    /// a gate: [`crate::ipc_client::IpcClient::send`] is the sole authority
    /// on what may go out.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub protocol_revision: Option<u32>,
}

/// Payload of [`EVENT_APPROVAL_RESET`].
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalResetPayload {
    /// Every request pending at the moment registration completed.
    pub requests: Vec<ApprovalRequest>,
}

/// Payload of [`EVENT_APPROVAL_REQUESTED`].
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRequestedPayload {
    /// The request awaiting a decision.
    pub request: ApprovalRequest,
}

/// Payload of [`EVENT_APPROVAL_WITHDRAWN`].
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalWithdrawnPayload {
    /// The request that is no longer pending.
    pub request_id: String,
    /// Why the request ended. The protocol's own enum: an unrecognized wire
    /// value decodes as `WithdrawReason::Unknown` (forward compatibility)
    /// rather than failing the frame.
    pub reason: WithdrawReason,
}

/// Payload of [`EVENT_APPROVAL_REGISTRATION_REFUSED`].
///
/// Fieldless on purpose: a unit struct or `()` would serialize to JSON
/// `null`, which the TypeScript boundary's object narrowing rejects. This
/// struct serializes to `{}` instead.
#[derive(Debug, Clone, Serialize)]
pub struct ApprovalRegistrationRefusedPayload {}

/// Payload of [`EVENT_CHAT_CHANGED`].
#[derive(Debug, Clone, Serialize)]
pub struct ChatChangedPayload {
    /// The room whose state changed.
    pub room_id: String,
    /// The sequence number of the message that triggered the change.
    pub seq: u64,
    /// The room's `needs_user` state at the moment of this push.
    pub needs_user: bool,
}

/// Payload of [`EVENT_CHAT_PARTICIPANTS_CHANGED`].
///
/// Fieldless on purpose, serializing to `{}` rather than `null` for the same
/// reason as [`ApprovalRegistrationRefusedPayload`].
#[derive(Debug, Clone, Serialize)]
pub struct ChatParticipantsChangedPayload {}

/// Payload of [`EVENT_PLAN_CHANGED`].
#[derive(Debug, Clone, Serialize)]
pub struct PlanChangedPayload {
    /// The plan that changed.
    pub plan_id: String,
}

/// Payload of [`EVENT_MODEL_CHOICES`].
#[derive(Debug, Clone, Serialize)]
pub struct ModelChoicesPayload {
    /// The id the request was sent with.
    pub request_id: u64,
    /// The profile whose models were listed.
    pub profile: String,
    /// Where the list came from, `None` when the profile declares no source.
    pub source: Option<ModelChoiceSource>,
    /// The models found, in the source's order.
    pub choices: Vec<ModelChoice>,
    /// Why the list could not be taken, `None` on success.
    pub error: Option<String>,
}

/// Payload of [`EVENT_MCP_REGISTRATION_STATUS`].
#[derive(Debug, Clone, Serialize)]
pub struct McpRegistrationStatusPayload {
    /// The id the list request was sent with.
    pub request_id: u64,
    /// The row's state as the daemon read it.
    pub status: McpRegistrationStatusInfo,
}

/// Payload of [`EVENT_MCP_REGISTRATION_RESULT`].
#[derive(Debug, Clone, Serialize)]
pub struct McpRegistrationResultPayload {
    /// The id the run request was sent with.
    pub request_id: u64,
    /// The profile whose registration was changed.
    pub profile: String,
    /// What was done.
    pub action: McpRegistrationAction,
    /// The steps taken, in the order they ran.
    pub steps: Vec<McpRegistrationStep>,
    /// Why the operation failed, `None` on success.
    pub error: Option<String>,
    /// The row's state read again after the operation.
    pub status: McpRegistrationStatusInfo,
}

/// Payload of [`EVENT_DAEMON_RECOVERY`].
#[derive(Debug, Clone, Serialize)]
pub struct DaemonRecoveryPayload {
    /// The status recovery has moved to.
    pub status: RecoveryStatus,
    /// The generation of this change, which orders it against every other
    /// delivery of a status -- including the one a fresh WebView reads.
    pub rev: u64,
}

/// Return value of the `attach` command. The window's screen follows as
/// frames, which the WebView pulls with `next_frames`.
///
/// Mirrors `DaemonMessage::Attached` field for field so the TypeScript side
/// can validate the command result with the same narrowing style it uses for
/// event payloads ("boundary validation on the TS side").
#[derive(Debug, Clone, Serialize)]
pub struct AttachResponse {
    /// Whether the child process has already exited.
    pub child_exited: bool,
    /// Exit code of the child process, present when `child_exited` is true.
    pub exit_code: Option<i32>,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `unknown_reason` is an `Option`, and `skip_serializing_if`
    /// omits the key entirely rather than emitting `null` when there is
    /// nothing to report, matching the TypeScript boundary's key-omission
    /// contract for every other optional field on this payload family.
    #[test]
    fn status_payload_omits_unknown_reason_key_when_none() {
        let payload = StatusPayload {
            window_id: "window-a".to_owned(),
            status: WindowStatus::Running,
            unknown_reason: None,
            status_rev: Some(1),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert!(!json.as_object().unwrap().contains_key("unknown_reason"));
    }

    #[test]
    fn status_payload_carries_unknown_reason_key_when_some() {
        let payload = StatusPayload {
            window_id: "window-a".to_owned(),
            status: WindowStatus::Unknown,
            unknown_reason: Some(UnknownReason::ScreenLost),
            status_rev: Some(1),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["unknown_reason"], "screen_lost");
    }

    /// The status generation as this layer forwards it: the same optional
    /// `status_rev` key the daemon's own wire shapes carry, so the WebView
    /// orders push and list observations of one window by a single name.
    #[test]
    fn status_payload_omits_status_rev_key_when_none() {
        let payload = StatusPayload {
            window_id: "window-a".to_owned(),
            status: WindowStatus::Running,
            unknown_reason: None,
            status_rev: None,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert!(!json.as_object().unwrap().contains_key("status_rev"));
    }

    #[test]
    fn status_payload_wire_key_for_the_generation_is_literally_status_rev() {
        let payload = StatusPayload {
            window_id: "window-a".to_owned(),
            status: WindowStatus::Running,
            unknown_reason: None,
            status_rev: Some(7),
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["status_rev"], 7);
    }

    #[test]
    fn status_payload_decodes_a_body_predating_the_status_rev_key() {
        let body = r#"{"window_id":"window-a","status":"running"}"#;
        let payload: StatusPayload = serde_json::from_str(body).unwrap();
        assert_eq!(
            payload,
            StatusPayload {
                window_id: "window-a".to_owned(),
                status: WindowStatus::Running,
                unknown_reason: None,
                status_rev: None,
            }
        );
    }

    #[test]
    fn status_payload_round_trips_with_a_generation_and_a_reason() {
        let payload = StatusPayload {
            window_id: "window-a".to_owned(),
            status: WindowStatus::Unknown,
            unknown_reason: Some(UnknownReason::ScreenLost),
            status_rev: Some(42),
        };
        let json = serde_json::to_string(&payload).unwrap();
        let back: StatusPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(back, payload);
    }
}
