//! Tauri commands exposed to the WebView.
//!
//! Tauri 2 only permission-gates core/plugin commands, so application
//! commands defend themselves with Rust-side argument validation
//! (command arguments are untrusted input). Arguments keep their `snake_case`
//! names across the boundary, matching the event payloads and the daemon
//! protocol.

use std::sync::Arc;

use tauri::{AppHandle, LogicalSize, Manager, State, UserAttentionType};
use yagura_protocol::{
    CHAT_READ_LIMIT_MAX, ClientMessage, DEFAULT_PALETTES_MAX_VENDORS, DefaultPalettes, Frame,
    MAX_LAYOUT_JSON_BYTES, OnNameTaken, Palette, TerminalSize, check_input_bounds,
    validate_chat_body, validate_open_window_exclusivity, validate_window_name,
    validate_window_vendor,
};

use crate::attention::{AttentionState, MAIN_WINDOW_LABEL};
use crate::connection_pool::ConnectionPool;
use crate::constants::{MAX_PTY_DIMENSION, MIN_PTY_COLS, MIN_PTY_ROWS};
use crate::daemon_supervisor::RecoverySupervisorState;
use crate::events::{DaemonCompatibilityPayload, DaemonConnectedPayload};
use crate::instance::StartupContext;
use crate::ipc_client::IpcError;
use crate::pool_outcomes::{ApprovalSyncOutcome, CloseOutcome, DecideApprovalOutcome};
use crate::recovery_state::RecoveryStatusReport;

/// Connects to the daemon and attaches to a specific window, at the size the
/// view will draw it at (`None` keeps the window's size). Where the window's
/// link stands reaches the WebView as `session://window-link` and
/// `session://exit`, not as this result.
///
/// # Errors
/// Returns a [`CommandFailure`] when a dimension is outside the protocol
/// range, the daemon cannot be reached, the window does not exist, the
/// attach times out, or another GUI took the window over.
#[tauri::command(rename_all = "snake_case")]
pub async fn attach_window(
    app: AppHandle,
    window_id: String,
    size: Option<TerminalSize>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    validate_window_id(&window_id)?;
    if let Some(size) = size {
        validate_dimension(size.cols, MIN_PTY_COLS, "cols")?;
        validate_dimension(size.rows, MIN_PTY_ROWS, "rows")?;
    }
    pool.attach_window(&app, &window_id, size)
        .await
        .map_err(CommandFailure::from)
}

/// Lists all windows the daemon currently manages.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// the response is invalid.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_windows(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<Vec<yagura_protocol::WindowInfo>, CommandFailure> {
    pool.list_windows(&app).await.map_err(CommandFailure::from)
}

/// The reply of [`list_detections`]: the definition names to offer, plus the
/// reason the daemon could not enumerate them, if any.
///
/// The warning field is omitted rather than serialized as `null` when there
/// is none, symmetrically with [`OpenWindowReply`]: both fields are
/// optional on the daemon's own wire, and the WebView narrows a missing key
/// and a `null` to the same value.
#[derive(Debug, serde::Serialize)]
pub struct DetectionListReply {
    /// The available detection definition names, sorted in byte order: the
    /// union of the daemon's bundled definitions and its detection directory.
    pub names: Vec<String>,
    /// English reason text when the detection directory could not be read.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub warning: Option<String>,
}

/// The reply of [`list_profiles`]: the launch profiles the daemon's resolved
/// profiles directory holds, plus one line per file it had to exclude.
///
/// Unlike [`DetectionListReply`], the warnings are a list and are always
/// present (empty when there is nothing to report): the daemon reports one
/// line per excluded file, so "no directory" and "three unreadable files" are
/// the same kind of answer here.
#[derive(Debug, serde::Serialize)]
pub struct ProfileListReply {
    /// Every profile the resolved directory currently holds.
    pub profiles: Vec<yagura_protocol::ProfileInfo>,
    /// One English line per excluded file, or per reason nothing could be
    /// enumerated at all.
    pub warnings: Vec<String>,
}

/// The reply of [`open_window`]: the new window's id, plus the warning the
/// daemon reports when the window runs no detection definition.
///
/// A warning does not mean the window failed to open -- it opened, with its
/// status pinned to `Unknown`. See [`DetectionListReply`] for the shared serde
/// treatment of the warning field.
#[derive(Debug, serde::Serialize)]
pub struct OpenWindowReply {
    /// The id the daemon assigned to the new window.
    pub window_id: String,
    /// The name the daemon gave the window, which differs from the requested
    /// one when it was numbered. Absent when the daemon predates the field.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    /// English reason text when the window runs no detection definition: one
    /// failed to load, or no definition layer is configured at all.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detection_warning: Option<String>,
}

/// Why a command that reaches the daemon failed, in a shape the WebView can
/// branch on without reading the English sentence.
///
/// Every command that takes the [`ConnectionPool`] and returns a `Result`
/// fails with this. The sentence is serialized as `text` beside the kind's
/// own fields, and is the only copy of it: an [`IpcError`]'s
/// [`Display`](std::fmt::Display), or the Display of the argument check that
/// refused the request (see [`CommandFailure::from_reason`]). The fields are
/// private and there is no `Display`, so a command cannot turn a failure back
/// into a string of its own wording.
///
/// The daemon's own rejection and a refusal the GUI's Rust layer raised on
/// its own are different kinds, because only the former carries an
/// `error.code`. A failure Tauri itself raises before the command runs (an
/// argument that does not deserialize, an unknown command name) never takes
/// this shape: it reaches the WebView as a bare string.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct CommandFailure {
    #[serde(flatten)]
    kind: FailureKind,
    text: String,
}

/// The kind of a [`CommandFailure`], serialized as its `kind` key.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum FailureKind {
    /// The daemon answered the request with an error frame.
    Rejected {
        /// The `error.code` the daemon reported.
        code: String,
        /// The daemon's own English reason, empty when it gave none.
        message: String,
        /// The argument the rejection points at, when it points at one the
        /// caller itself sent. Omitted from the wire when absent.
        #[serde(skip_serializing_if = "Option::is_none")]
        rejected_argument: Option<String>,
    },
    /// The GUI's own Rust layer refused the request or could not carry it
    /// out, so no daemon rejection stands behind it.
    Local,
    /// The connected daemon speaks another wire revision than this build, so
    /// the GUI's Rust layer did not send the request: no byte of it was
    /// written.
    Unsupported {
        /// The wire tag of the request that was refused, one of
        /// [`yagura_protocol::client_message_wire_tags`].
        request_type: String,
    },
}

/// The reply of [`list_restore_candidates`]: the previous boot's window
/// configuration for the restore UI.
///
/// `taken_at_epoch_s` has no `skip_serializing_if`: it is always present on
/// the wire (as `null` when unknown), matching the daemon's own
/// `RestoreCandidateList` shape one for one.
#[derive(Debug, serde::Serialize)]
pub struct RestoreCandidateListReply {
    /// The previous boot's windows, in ascending `position` order.
    pub windows: Vec<yagura_protocol::RestoreCandidate>,
    /// When that configuration was recorded, in Unix epoch seconds.
    pub taken_at_epoch_s: Option<i64>,
}

/// The reply of [`get_daemon_health`]: the problems the daemon lists, empty
/// when it has none.
#[derive(Debug, serde::Serialize)]
pub struct DaemonHealthReply {
    /// Each problem's `kind` and `detail`, as the daemon states them.
    pub problems: Vec<yagura_protocol::DaemonProblem>,
}

/// The reply of [`send_chat`] / [`broadcast_chat`]: what the daemon recorded
/// for the post.
///
/// Every field is always present on the wire, mirroring the daemon's own
/// `ChatPosted` shape one for one.
#[derive(Debug, serde::Serialize)]
pub struct ChatPostedReply {
    /// The room the message was posted to.
    pub room_id: String,
    /// The message's sequence number within the room.
    pub seq: u64,
    /// The resolved window ids this message was addressed to.
    pub targets: Vec<String>,
    /// Whether this post counts the human user among its addressees.
    pub user: bool,
    /// Raw mention text that did not resolve to exactly one participant.
    pub unresolved: Vec<String>,
    /// Whether the room is halted as of this post.
    pub halted: bool,
}

/// The reply of [`read_chat`]: one page of the room's history, plus the room
/// state the panel renders around it.
///
/// `halted` has no `skip_serializing_if`: it is always present (as `null` when
/// the room is not halted), the same way [`RestoreCandidateListReply`] keeps
/// its nullable field on the wire.
#[derive(Debug, serde::Serialize)]
pub struct ChatMessagesReply {
    /// The room read from.
    pub room_id: String,
    /// The messages found, ascending by `seq`.
    pub messages: Vec<yagura_protocol::ChatMessage>,
    /// The cursor to pass as `after_seq` on the next call.
    pub next_after_seq: u64,
    /// The room's current halt, if any.
    pub halted: Option<yagura_protocol::ChatHalt>,
    /// Whether the room currently needs a human's attention.
    pub needs_user: bool,
    /// The room's current participants (never includes the user).
    pub roster: Vec<yagura_protocol::ChatParticipant>,
}

/// The reply of [`list_chat_rooms`]: every room the daemon knows of.
///
/// Wrapped in `rooms` rather than returned bare, mirroring the daemon's own
/// `ChatRooms` shape one for one: the WebView boundary narrows a bare array
/// and an envelope differently, and this command sends the envelope.
#[derive(Debug, serde::Serialize)]
pub struct ChatRoomListReply {
    /// Every room the daemon knows of, `"main"` first, then the rest by most
    /// recent activity.
    pub rooms: Vec<yagura_protocol::ChatRoomInfo>,
}

/// The reply of [`list_plans`]: every plan the daemon lets this GUI see.
///
/// Wrapped in `plans` rather than returned bare, for the same reason as
/// [`ChatRoomListReply`]: it mirrors the daemon's own `PlanList` shape.
#[derive(Debug, serde::Serialize)]
pub struct PlanListReply {
    /// Every plan, in the order the daemon listed them.
    pub plans: Vec<yagura_protocol::PlanSummary>,
}

/// The reply of [`list_ticket_workspaces`]: every workspace that has
/// tickets, wrapped in `workspaces` like the daemon's own
/// `TicketWorkspaceList`.
#[derive(Debug, serde::Serialize)]
pub struct TicketWorkspaceListReply {
    /// Every workspace, in the order the daemon listed them.
    pub workspaces: Vec<yagura_protocol::TicketWorkspaceInfo>,
}

/// The reply of [`list_tickets`]: one workspace's ticket settings,
/// milestones and the matching ticket summaries, as the daemon's `TicketList`
/// carries them.
#[derive(Debug, serde::Serialize)]
pub struct TicketListReply {
    /// The workspace read.
    pub workspace_id: Option<i64>,
    /// The user's ticket settings, `None` when they could not be read.
    pub config: Option<yagura_protocol::TicketConfigInfo>,
    /// Why the settings could not be read, `None` when they were.
    pub config_error: Option<String>,
    /// The workspace's milestones, in position order.
    pub milestones: Vec<yagura_protocol::MilestoneInfo>,
    /// The matching tickets, in board order.
    pub tickets: Vec<yagura_protocol::TicketSummary>,
    /// Whether more tickets matched than were returned.
    pub truncated: bool,
}

/// The reply of [`list_roles`] and [`save_roles`]: the role table as the
/// daemon stores it, wrapped in `roles` like the daemon's own `RoleList`.
#[derive(Debug, serde::Serialize)]
pub struct RoleListReply {
    /// Every row of the role table.
    pub roles: Vec<yagura_protocol::RoleEntry>,
    /// The row a task falls back on when its own role gives none, `None`
    /// when the table names no default.
    pub default_role: Option<String>,
}

/// The reply of [`list_model_choices`]: the daemon accepted the request, and
/// the list itself follows as a `session://model-choices` push.
#[derive(Debug, serde::Serialize)]
pub struct ModelChoicesRequestedReply {
    /// The id the WebView chose for the request, echoed back by the daemon.
    pub request_id: u64,
    /// The profile whose models are being listed.
    pub profile: String,
}

/// The reply of [`list_mcp_registrations`]: the profiles that describe how
/// to register the MCP bridge with their CLI. Each row's state follows as a
/// `session://mcp-registration-status` push.
#[derive(Debug, serde::Serialize)]
pub struct McpRegistrationsListedReply {
    /// The id the WebView chose for the request, echoed back by the daemon.
    pub request_id: u64,
    /// The bridge the rows register, `None` when the daemon found none.
    pub bridge: Option<String>,
    /// One row per profile, in the daemon's order.
    pub rows: Vec<yagura_protocol::McpRegistrationRow>,
}

/// The reply of [`run_mcp_registration`]: the daemon accepted the operation,
/// and its outcome follows as a `session://mcp-registration-result` push.
#[derive(Debug, serde::Serialize)]
pub struct McpRegistrationRequestedReply {
    /// The id the WebView chose for the request, echoed back by the daemon.
    pub request_id: u64,
    /// The profile whose registration is being changed.
    pub profile: String,
    /// The action, as its wire string.
    pub action: String,
}

/// The reply of [`save_model_aliases`]: a profile's model aliases as the
/// daemon stored them.
#[derive(Debug, serde::Serialize)]
pub struct ModelAliasesSavedReply {
    /// The profile whose aliases were replaced.
    pub profile: String,
    /// The aliases as stored.
    pub aliases: Vec<yagura_protocol::LaunchOptionAlias>,
}

/// The reply of [`set_profile_enabled`]: a profile's launch setting as the
/// daemon stored it.
#[derive(Debug, serde::Serialize)]
pub struct ProfileEnabledSetReply {
    /// The profile whose setting was stored.
    pub profile: String,
    /// `true` when the profile may be launched.
    pub enabled: bool,
}

/// The reply of [`get_profile_detail`]: every value a profile uses, with
/// the layer each comes from.
#[derive(Debug, serde::Serialize)]
pub struct ProfileDetailReply {
    /// The profile read.
    pub profile: String,
    /// Whether the profile is laid over a shipped profile.
    pub shipped_base: bool,
    /// The argv in use on the daemon's host.
    pub command: yagura_protocol::ProfileCommandDetail,
    /// Every value in use, in the profile's own key order.
    pub entries: Vec<yagura_protocol::ProfileDetailEntry>,
}

/// The reply of [`set_profile_command`]: the argv in use after the change.
#[derive(Debug, serde::Serialize)]
pub struct ProfileCommandSetReply {
    /// The profile changed.
    pub profile: String,
    /// The argv now in use.
    pub command: yagura_protocol::ProfileCommandDetail,
}

/// One destructive tool's automatic-approval setting, as carried by
/// [`ApprovalSettingsReply::tools`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ToolAutoApproveReply {
    /// The tool's name. An opaque string, not an enum: the closed set of
    /// recognized names belongs to the daemon, not this layer.
    pub tool: String,
    /// `true` when the tool proceeds without approval.
    pub auto_approve: bool,
}

/// The reply of [`get_approval_settings`], [`set_auto_approve`] and
/// [`set_plan_approval_required`]: the daemon's approval settings as it holds
/// them right now.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct ApprovalSettingsReply {
    /// The playbook state, as a plain string (`"loaded"` / `"not_configured"`
    /// / `"failed"` / `"unknown"`), so an older WebView build need not know a
    /// Rust enum.
    pub playbook: String,
    /// One entry per destructive tool the daemon knows about.
    pub tools: Vec<ToolAutoApproveReply>,
    /// `true` when a newly created plan requires review before work begins.
    pub plans_require_approval: bool,
}

/// A daemon's answer to opening a window for one plan task: the window it
/// opened and the assignment it launched that window with.
///
/// This client sends no such request, so the value only exists to complete
/// the reply classification the same way every other reply shape is.
#[derive(Debug, serde::Serialize)]
pub struct TaskWindowOpenedReply {
    /// The plan holding the task.
    pub plan_id: String,
    /// The task the window was opened for.
    pub task_id: String,
    /// The newly opened window.
    pub window_id: String,
    /// The assignment the window was launched with, and where it came from.
    pub assignment: yagura_protocol::ResolvedAssignment,
}

/// The reply of [`update_window_layout`]: the window whose layout was stored.
#[derive(Debug, serde::Serialize)]
pub struct UpdateWindowLayoutReply {
    /// The window ID whose layout was stored.
    pub window_id: String,
}

/// Lists the previous boot's window configuration for the restore UI.
/// Read-only: never rotates or writes the store.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// the response is invalid.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_restore_candidates(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<RestoreCandidateListReply, CommandFailure> {
    pool.list_restore_candidates(&app)
        .await
        .map_err(CommandFailure::from)
}

/// Asks the daemon what keeps it from opening windows.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// the response is invalid.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_daemon_health(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<DaemonHealthReply, CommandFailure> {
    pool.get_daemon_health(&app)
        .await
        .map_err(CommandFailure::from)
}

/// Reports a window's presentation layout to the daemon, verbatim. The
/// daemon stores it unparsed and never interprets it.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached, the
/// window does not exist, or `layout_json` exceeds the size the daemon
/// accepts.
#[tauri::command(rename_all = "snake_case")]
pub async fn update_window_layout(
    app: AppHandle,
    window_id: String,
    layout_json: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<UpdateWindowLayoutReply, CommandFailure> {
    validate_window_id(&window_id)?;
    validate_layout_json_len(&layout_json)?;
    pool.update_window_layout(&app, &window_id, layout_json)
        .await
        .map_err(CommandFailure::from)
}

/// Lists the detection definition names the daemon can offer for
/// `open_window`'s `detection` argument: the union of its bundled definitions
/// and its detection directory, where a directory definition hides a bundled
/// one of the same name.
///
/// A detection directory that cannot be read is reported through the reply's
/// `warning` field rather than as an error: only the bundled names remain
/// (none at all when that layer is off), and the WebView keeps the list it
/// already had.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_detections(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<DetectionListReply, CommandFailure> {
    pool.list_detections(&app)
        .await
        .map_err(CommandFailure::from)
}

/// Lists the launch profiles the daemon can offer for `open_window`'s
/// `profile` argument.
///
/// A profiles directory that cannot be enumerated is reported through the
/// reply's `warnings` rather than as an error, the same way
/// [`list_detections`] reports its own: the list is simply empty.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_profiles(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ProfileListReply, CommandFailure> {
    pool.list_profiles(&app).await.map_err(CommandFailure::from)
}

/// Opens a new window, either from an explicit `command` or from a launch
/// `profile` (multi-window v2).
///
/// `name`, when present, is validated here (command arguments are
/// untrusted input). `detection`, `vendor` and `profile` are passed through
/// unvalidated: the daemon is the authority on those vocabularies. A
/// well-formed name with no definition behind it is not a rejection -- the
/// window opens and the reply carries a `detection_warning`.
///
/// `profile` is mutually exclusive with `command`, `detection` and `vendor`,
/// which the profile itself supplies; the combination is refused here rather
/// than resolved silently, matching the MCP tool of the same name.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached, the
/// operation fails, `name` is invalid, or the arguments are mutually
/// exclusive.
// A Tauri command takes one parameter per argument name on the wire, so the
// optional arguments cannot be grouped into a struct without renaming them.
#[allow(clippy::too_many_arguments)]
#[tauri::command(rename_all = "snake_case")]
pub async fn open_window(
    app: AppHandle,
    command: Vec<String>,
    cwd: Option<String>,
    name: Option<String>,
    detection: Option<String>,
    profile: Option<String>,
    vendor: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    on_name_taken: Option<OnNameTaken>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<OpenWindowReply, CommandFailure> {
    let on_name_taken = on_name_taken.unwrap_or_default();
    check_open_window_arguments(
        &command,
        name.as_deref(),
        detection.as_deref(),
        profile.as_deref(),
        vendor.as_deref(),
        model.as_deref(),
        effort.as_deref(),
    )?;
    match profile {
        Some(profile) => pool
            .open_window_from_profile(&app, profile, cwd, name, on_name_taken, model, effort)
            .await
            .map_err(CommandFailure::from),
        None => pool
            .open_window(&app, command, cwd, name, on_name_taken, detection, vendor)
            .await
            .map_err(CommandFailure::from),
    }
}

/// The argument checks [`open_window`] runs before it reaches the pool,
/// kept apart from the command (which needs `State`) so they are testable.
fn check_open_window_arguments(
    command: &[String],
    name: Option<&str>,
    detection: Option<&str>,
    profile: Option<&str>,
    vendor: Option<&str>,
    model: Option<&str>,
    effort: Option<&str>,
) -> Result<(), CommandFailure> {
    validate_open_window_exclusivity(
        !command.is_empty(),
        profile,
        detection,
        vendor,
        model.is_some() || effort.is_some(),
    )
    .map_err(CommandFailure::from_reason)?;
    if let Some(name) = name {
        validate_window_name(name).map_err(CommandFailure::from_reason)?;
    }
    Ok(())
}

/// Loads the user's keyboard shortcut overrides from `keymap.toml`.
///
/// Frozen contract on the WebView side: no argument crosses the IPC
/// boundary, since Tauri's `State` argument is invisible to the caller.
/// This never returns `Err` -- every failure to load the file is reported
/// through the returned `failure` field instead (fail-safe).
#[tauri::command(rename_all = "snake_case")]
pub fn load_keymap(startup: State<'_, StartupContext>) -> crate::keymap::KeymapLoadResult {
    load_keymap_with_root(startup.config_root().as_deref())
}

/// Inner dispatch logic for `load_keymap`, separated for testability.
///
/// No configuration root to resolve is treated the same as the file not
/// existing there.
fn load_keymap_with_root(root: Option<&std::path::Path>) -> crate::keymap::KeymapLoadResult {
    match root {
        Some(r) => crate::keymap::load_keymap(r),
        None => crate::keymap::KeymapLoadResult::empty(crate::keymap::KeymapFailure::NotFound),
    }
}

/// Reports the path of the `keymap.toml` this instance reads and writes, for
/// the Keyboard tab to show. `None` when the instance has no config root.
#[tauri::command(rename_all = "snake_case")]
pub fn keymap_file_path(startup: State<'_, StartupContext>) -> Option<String> {
    keymap_file_path_with_root(startup.config_root().as_deref())
}

/// Inner logic for `keymap_file_path`, separated for testability.
fn keymap_file_path_with_root(root: Option<&std::path::Path>) -> Option<String> {
    root.map(|r| {
        crate::keymap::keymap_file_path(r)
            .to_string_lossy()
            .into_owned()
    })
}

/// Saves the user's keyboard shortcut overrides as the `[keys]` table of
/// `keymap.toml`, replacing the file.
///
/// # Errors
///
/// Returns the [`crate::keymap::KeymapSaveFailure`] reported by
/// [`crate::keymap::save_keymap_in`]: `no_config_root` when this instance has
/// no config location, `too_large` past the entry or byte limit, and
/// `write_failed` when the file could not be replaced.
#[tauri::command(rename_all = "snake_case")]
pub fn save_keymap(
    startup: State<'_, StartupContext>,
    keys: std::collections::BTreeMap<String, String>,
) -> Result<(), crate::keymap::KeymapSaveFailure> {
    let location = startup.config_location();
    let location = location
        .as_ref()
        .map(|(base, root)| (base.as_path(), root.as_path()));
    crate::keymap::save_keymap_in(location, &keys)
}

/// Loads the user's theme palettes and vendor/default assignments from
/// `themes.toml`.
///
/// Frozen contract on the WebView side, matching `load_keymap`: no argument
/// crosses the IPC boundary. This never returns `Err` -- every failure to
/// load the file is reported through the returned `failure` field instead
/// (fail-safe).
#[tauri::command(rename_all = "snake_case")]
pub fn load_themes(startup: State<'_, StartupContext>) -> crate::themes::ThemesLoadResult {
    load_themes_with_root(startup.config_root().as_deref())
}

/// Inner dispatch logic for `load_themes`, separated for testability
/// (mirrors `load_keymap_with_root`).
///
/// No configuration root to resolve is treated the same as the file not
/// existing there.
fn load_themes_with_root(root: Option<&std::path::Path>) -> crate::themes::ThemesLoadResult {
    match root {
        Some(r) => crate::themes::load_themes(r),
        None => crate::themes::ThemesLoadResult::empty(crate::themes::ThemesFailure::NotFound),
    }
}

/// Starts the daemon again at the user's request, after automatic recovery
/// has given up.
///
/// Goes through `daemon_supervisor` (`daemon_supervisor.rs`) rather than
/// acting here: the attempt budget and the entry point that starts the
/// daemon belong to the supervisor, and a command that reached either of
/// them directly would be a second place deciding when the application
/// starts a daemon on its own.
///
/// # Errors
/// Never returns `Err`: whether the daemon came back is reported by the
/// recovery status, not by this call.
#[tauri::command(rename_all = "snake_case")]
pub async fn reconnect_daemon(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
    recovery: State<'_, RecoverySupervisorState>,
) -> Result<(), CommandFailure> {
    crate::daemon_supervisor::trigger_manual_reconnect(&app, &pool, &recovery).await;
    Ok(())
}

/// Reads the current daemon-recovery status, so a WebView that has just
/// started listening does not have to wait for the next change.
///
/// Frozen contract: no arguments, and this never returns `Err` -- the status
/// is held in memory and the reason the client settings could not be loaded,
/// if any, is reported through the returned `config_failure` field
/// (fail-safe).
#[tauri::command(rename_all = "snake_case")]
pub fn get_recovery_status(recovery: State<'_, RecoverySupervisorState>) -> RecoveryStatusReport {
    crate::daemon_supervisor::recovery_snapshot(&recovery)
}

/// The last daemon start the control connection announced, `None` when it
/// has announced none yet: the GUI may reach a daemon before the WebView
/// starts listening, and that start's spawn warning must not be lost.
///
/// Never returns `Err`: the value is held in memory.
#[tauri::command(rename_all = "snake_case")]
pub fn get_daemon_connection(
    pool: State<'_, Arc<ConnectionPool>>,
) -> Option<DaemonConnectedPayload> {
    pool.daemon_connection()
}

/// The last change of whether the control connection is open to a daemon of
/// another wire revision, so a WebView that has just started listening does
/// not have to wait for the next one.
///
/// Never returns `Err`: the value is held in memory.
#[tauri::command(rename_all = "snake_case")]
pub fn get_daemon_compatibility(
    pool: State<'_, Arc<ConnectionPool>>,
) -> DaemonCompatibilityPayload {
    pool.daemon_compatibility()
}

/// Largest side, in logical pixels, a minimum size the WebView asks for may have.
const MAX_MIN_WINDOW_SIDE: f64 = 16_384.0;

/// Gives the main window the smallest size, in logical pixels, its shell lays
/// out in, so shrinking it never pushes the shell off screen. Only two numbers
/// cross the boundary.
///
/// Does nothing when there is no main window.
///
/// # Errors
/// Returns a human-readable message when a side is not a finite number in
/// `1..=16384`, or when the main window refuses the size.
#[tauri::command(rename_all = "snake_case")]
pub fn set_main_window_min_size(app: AppHandle, width: f64, height: f64) -> Result<(), String> {
    check_min_window_side(width)?;
    check_min_window_side(height)?;
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return Ok(());
    };
    window
        .set_min_size(Some(LogicalSize::new(width, height)))
        .map_err(|error| error.to_string())
}

/// Accepts a minimum window side the WebView asked for.
///
/// # Errors
/// Returns a human-readable message when `side` is not a finite number in
/// `1..=16384` (NaN and the infinities included).
pub(crate) fn check_min_window_side(side: f64) -> Result<(), String> {
    if (1.0..=MAX_MIN_WINDOW_SIDE).contains(&side) {
        Ok(())
    } else {
        Err(format!(
            "a minimum window side must be between 1 and {MAX_MIN_WINDOW_SIDE}, got {side}"
        ))
    }
}

/// Shows how many things wait for the user outside the WebView: the count in
/// the main window's title, a request for the user's attention when something
/// `arrived` while the main window lacks focus, and on macOS the Dock badge.
///
/// Only counts and a flag cross the boundary, so no daemon-provided text
/// reaches the title.
///
/// # Errors
/// Returns a human-readable message when the title, the attention request or
/// the badge cannot be applied to the main window.
#[tauri::command(rename_all = "snake_case")]
pub fn set_attention_summary(
    app: AppHandle,
    attention: State<'_, AttentionState>,
    needs_you: u32,
    pending_permissions: u32,
    arrived: bool,
) -> Result<(), String> {
    let Some(window) = app.get_webview_window(MAIN_WINDOW_LABEL) else {
        return Ok(());
    };
    // An unreadable focus counts as unfocused: a needless request is
    // ignored by a focused window, while a missed one leaves the user unaware.
    let focused = window.is_focused().unwrap_or(false);
    let update = attention.observe(needs_you, pending_permissions, arrived, focused);
    window
        .set_title(&update.title)
        .map_err(|error| error.to_string())?;
    if update.request_attention {
        window
            .request_user_attention(Some(UserAttentionType::Informational))
            .map_err(|error| error.to_string())?;
    }
    // Only macOS shows the count on the Dock; Windows has no badge count and
    // its overlay icon would need an image per count.
    #[cfg(target_os = "macos")]
    window
        .set_badge_count((update.total > 0).then(|| i64::from(update.total)))
        .map_err(|error| error.to_string())?;
    Ok(())
}

/// Closes a window, terminating its process tree.
///
/// An unreachable daemon and a window the daemon does not hold are reported
/// as outcomes rather than errors (see [`CloseOutcome`]), so the view can
/// retire the pane in either case.
///
/// # Errors
/// Returns a [`CommandFailure`] when the window id is invalid, the
/// daemon rejects the request for a reason other than the window not
/// existing, or this connection is not a registered approver.
#[tauri::command(rename_all = "snake_case")]
pub async fn close_window(
    app: AppHandle,
    window_id: String,
    force: Option<bool>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<CloseOutcome, CommandFailure> {
    validate_window_id(&window_id)?;
    pool.close_window(&app, &window_id, force)
        .await
        .map_err(CommandFailure::from)
}

/// Sends an interrupt signal to a window's child process.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// the window does not exist.
#[tauri::command(rename_all = "snake_case")]
pub async fn interrupt_window(
    app: AppHandle,
    window_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    validate_window_id(&window_id)?;
    pool.interrupt_window(&app, &window_id)
        .await
        .map_err(CommandFailure::from)
}

/// Settles one pending approval request raised through the daemon's human
/// approval gate.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the request for a reason other than it no longer being pending
/// (see [`DecideApprovalOutcome::NotPending`]).
#[tauri::command(rename_all = "snake_case")]
pub async fn decide_approval(
    app: AppHandle,
    request_id: String,
    approved: bool,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<DecideApprovalOutcome, CommandFailure> {
    pool.decide_approval(&app, &request_id, approved)
        .await
        .map_err(CommandFailure::from)
}

/// Pulls the approval requests pending for this GUI, and whether it is a
/// registered approver at all.
///
/// The WebView calls this once its event subscription is open, so a request
/// (or a refusal) that the daemon announced before then is not lost.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the registration for a reason other than refusing this approver.
#[tauri::command(rename_all = "snake_case")]
pub async fn sync_approvals(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ApprovalSyncOutcome, CommandFailure> {
    pool.sync_approvals(&app)
        .await
        .map_err(CommandFailure::from)
}

/// Posts a message to the chat room, addressed to the windows its `@`
/// mentions resolve to.
///
/// `room_id` is passed through unvalidated: the daemon is the authority on
/// which rooms exist. `body` is validated here, because command arguments are
/// untrusted input.
///
/// # Errors
/// Returns a [`CommandFailure`] when the body is empty, too long or holds
/// a forbidden character, when the daemon cannot be reached, or when it
/// refuses the post (including a denied approval).
#[tauri::command(rename_all = "snake_case")]
pub async fn send_chat(
    app: AppHandle,
    body: String,
    room_id: Option<String>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ChatPostedReply, CommandFailure> {
    validate_chat_body(&body).map_err(CommandFailure::from_reason)?;
    pool.send_chat(&app, room_id, body)
        .await
        .map_err(CommandFailure::from)
}

/// Posts a message to every participant in the chat room but the poster.
///
/// Same argument handling as [`send_chat`]; the daemon decides the addressees.
///
/// # Errors
/// Returns a [`CommandFailure`] when the body is invalid, the daemon
/// cannot be reached, or it refuses the post (including a denied approval).
#[tauri::command(rename_all = "snake_case")]
pub async fn broadcast_chat(
    app: AppHandle,
    body: String,
    room_id: Option<String>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ChatPostedReply, CommandFailure> {
    validate_chat_body(&body).map_err(CommandFailure::from_reason)?;
    pool.broadcast_chat(&app, room_id, body)
        .await
        .map_err(CommandFailure::from)
}

/// Reads a page of the chat room's history, plus the room state the panel
/// renders around it.
///
/// An omitted `after_seq` asks the daemon for the most recent page: the GUI
/// reads through its approver connection, for which that is the defined
/// default.
///
/// # Errors
/// Returns a [`CommandFailure`] when `limit` is outside the range the
/// daemon accepts, the daemon cannot be reached, or it rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn read_chat(
    app: AppHandle,
    room_id: Option<String>,
    after_seq: Option<u64>,
    limit: Option<u32>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ChatMessagesReply, CommandFailure> {
    validate_chat_read_limit(limit)?;
    pool.read_chat(&app, room_id, after_seq, limit)
        .await
        .map_err(CommandFailure::from)
}

/// Lists every chat room the daemon knows of. Approver-only on the daemon
/// side; the GUI is registered as one before this command can succeed.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_chat_rooms(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ChatRoomListReply, CommandFailure> {
    pool.list_chat_rooms(&app)
        .await
        .map_err(CommandFailure::from)
}

/// Clears one chat room's history and returns the room the daemon cleared.
/// `room_id` is passed through unvalidated: the daemon is the authority on
/// which rooms exist.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn clear_chat_room(
    app: AppHandle,
    room_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<String, CommandFailure> {
    pool.clear_chat_room(&app, room_id)
        .await
        .map_err(CommandFailure::from)
}

/// Deletes one chat room and returns the room the daemon deleted. The
/// daemon refuses `main` and a room a window is open in (`room_in_use`).
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_chat_room(
    app: AppHandle,
    room_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<String, CommandFailure> {
    pool.delete_chat_room(&app, room_id)
        .await
        .map_err(CommandFailure::from)
}

/// Lists the plans the daemon lets this GUI see. Every plan-ledger command
/// goes through the control connection, which the daemon knows as the
/// approver; the daemon, not this layer, decides what an approver may do.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached,
/// speaks another wire revision, or rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_plans(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<PlanListReply, CommandFailure> {
    pool.list_plans(&app)
        .await
        .map(|plans| PlanListReply { plans })
        .map_err(CommandFailure::from)
}

/// Reads one plan in full.
///
/// # Errors
/// Returns a [`CommandFailure`] when `plan_id` is empty, the daemon
/// cannot be reached, or it rejects the request (an unknown plan included).
#[tauri::command(rename_all = "snake_case")]
pub async fn get_plan(
    app: AppHandle,
    plan_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<yagura_protocol::PlanInfo, CommandFailure> {
    validate_plan_id(&plan_id)?;
    pool.get_plan(&app, plan_id)
        .await
        .map_err(CommandFailure::from)
}

/// Deletes one plan and returns the id the daemon deleted.
///
/// # Errors
/// Returns a [`CommandFailure`] when `plan_id` is empty, the daemon
/// cannot be reached, or it rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn delete_plan(
    app: AppHandle,
    plan_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<String, CommandFailure> {
    validate_plan_id(&plan_id)?;
    pool.delete_plan(&app, plan_id)
        .await
        .map_err(CommandFailure::from)
}

/// Sets the user's assignment override for one task and returns the plan as
/// stored afterwards.
///
/// The assignment's profile and launch options are passed through
/// unvalidated: the daemon validates them with the same rules it applies to
/// every other source of an assignment.
///
/// # Errors
/// Returns a [`CommandFailure`] when an id is empty, the daemon cannot
/// be reached, or it rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_task_assignment(
    app: AppHandle,
    plan_id: String,
    task_id: String,
    assignment: yagura_protocol::TaskAssignment,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<yagura_protocol::PlanInfo, CommandFailure> {
    validate_plan_id(&plan_id)?;
    validate_task_id(&task_id)?;
    pool.set_task_assignment(&app, plan_id, task_id, assignment)
        .await
        .map_err(CommandFailure::from)
}

/// Removes the user's assignment override from one task and returns the plan
/// as stored afterwards.
///
/// # Errors
/// Returns a [`CommandFailure`] when an id is empty, the daemon cannot
/// be reached, or it rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn clear_task_assignment(
    app: AppHandle,
    plan_id: String,
    task_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<yagura_protocol::PlanInfo, CommandFailure> {
    validate_plan_id(&plan_id)?;
    validate_task_id(&task_id)?;
    pool.clear_task_assignment(&app, plan_id, task_id)
        .await
        .map_err(CommandFailure::from)
}

/// Names a window as a plan's owner and returns the plan as stored
/// afterwards. Whether that window is alive is the daemon's decision.
///
/// # Errors
/// Returns a [`CommandFailure`] when an id is empty, the daemon cannot
/// be reached, or it rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_plan_owner(
    app: AppHandle,
    plan_id: String,
    window_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<yagura_protocol::PlanInfo, CommandFailure> {
    validate_plan_id(&plan_id)?;
    validate_window_id(&window_id)?;
    pool.set_plan_owner(&app, plan_id, window_id)
        .await
        .map_err(CommandFailure::from)
}

/// Approves a plan awaiting review and returns it as stored afterwards.
/// `version` is the one the caller last read, so tasks changed since then
/// are never approved unseen.
///
/// # Errors
/// Returns a [`CommandFailure`] when `plan_id` is empty, the daemon
/// cannot be reached, or it rejects the request (a stale `version` included).
#[tauri::command(rename_all = "snake_case")]
pub async fn approve_plan(
    app: AppHandle,
    plan_id: String,
    version: u64,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<yagura_protocol::PlanInfo, CommandFailure> {
    validate_plan_id(&plan_id)?;
    pool.approve_plan(&app, plan_id, version)
        .await
        .map_err(CommandFailure::from)
}

/// Clears the review marks of a plan the user has looked at and returns it
/// as stored afterwards. `version` is the one the caller last read, so marks
/// that arrived after it stay.
///
/// # Errors
/// Returns a [`CommandFailure`] when `plan_id` is empty, the daemon
/// cannot be reached, or it rejects the request (a stale `version` included).
#[tauri::command(rename_all = "snake_case")]
pub async fn mark_plan_seen(
    app: AppHandle,
    plan_id: String,
    version: u64,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<yagura_protocol::PlanInfo, CommandFailure> {
    validate_plan_id(&plan_id)?;
    pool.mark_plan_seen(&app, plan_id, version)
        .await
        .map_err(CommandFailure::from)
}

/// Lists every workspace that has tickets. The ticket commands are read-only
/// and go through the control connection, like the plan-ledger commands.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached,
/// speaks another wire revision, or rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_ticket_workspaces(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<TicketWorkspaceListReply, CommandFailure> {
    pool.list_ticket_workspaces(&app)
        .await
        .map(|workspaces| TicketWorkspaceListReply { workspaces })
        .map_err(CommandFailure::from)
}

/// Reads one workspace's ticket settings, milestones and the summaries of
/// the tickets in `state` and `milestone` (`None` applies no filter), at most
/// `limit` of them (`None` leaves the cap to the daemon).
///
/// # Errors
/// Returns a [`CommandFailure`] when `state` or `milestone` is not a
/// name the daemon could hold, the daemon cannot be reached, or it rejects
/// the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_tickets(
    app: AppHandle,
    workspace_id: i64,
    state: Option<String>,
    milestone: Option<String>,
    limit: Option<u32>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<TicketListReply, CommandFailure> {
    validate_optional_token(state.as_deref(), "state must be a lowercase token")?;
    validate_optional_token(milestone.as_deref(), "milestone must be a lowercase token")?;
    pool.list_tickets(&app, workspace_id, state, milestone, limit)
        .await
        .map_err(CommandFailure::from)
}

/// Reads one ticket of a workspace in full.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the request (an unknown ticket included).
#[tauri::command(rename_all = "snake_case")]
pub async fn get_ticket(
    app: AppHandle,
    workspace_id: i64,
    ticket: u32,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<yagura_protocol::TicketInfo, CommandFailure> {
    pool.get_ticket(&app, workspace_id, ticket)
        .await
        .map_err(CommandFailure::from)
}

/// Reads the role-to-assignment table.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached,
/// speaks another wire revision, or rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_roles(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<RoleListReply, CommandFailure> {
    pool.list_roles(&app).await.map_err(CommandFailure::from)
}

/// Replaces the whole role-to-assignment table and returns it as stored.
///
/// The rows and the default role are passed through unvalidated: the daemon
/// validates the table as a whole (names, duplicates, launch options, a
/// default that names one of the rows) before it stores any of it.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached or
/// rejects the table.
#[tauri::command(rename_all = "snake_case")]
pub async fn save_roles(
    app: AppHandle,
    roles: Vec<yagura_protocol::RoleEntry>,
    default_role: Option<String>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<RoleListReply, CommandFailure> {
    pool.save_roles(&app, roles, default_role)
        .await
        .map_err(CommandFailure::from)
}

/// Asks the daemon for the models a profile's `model` launch option can take.
///
/// Answers at once with the accepted request; the list follows as a
/// `session://model-choices` push carrying the same `request_id`, which the
/// WebView chooses so it can record it before the push can arrive.
///
/// # Errors
/// Returns a [`CommandFailure`] when `profile` is not a valid profile
/// id, the daemon cannot be reached, speaks another wire revision, or rejects the
/// request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_model_choices(
    app: AppHandle,
    profile: String,
    request_id: u64,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ModelChoicesRequestedReply, CommandFailure> {
    validate_profile_id(&profile)?;
    pool.list_model_choices(&app, profile, request_id)
        .await
        .map_err(CommandFailure::from)
}

/// Replaces the values of a profile's model aliases and returns them as
/// stored.
///
/// The aliases are passed through unvalidated: the daemon checks the alias
/// set and every value against the profile file before it writes any of it.
///
/// # Errors
/// Returns a [`CommandFailure`] when `profile` is not a valid profile
/// id, the daemon cannot be reached, speaks another wire revision, or rejects the
/// aliases.
#[tauri::command(rename_all = "snake_case")]
pub async fn save_model_aliases(
    app: AppHandle,
    profile: String,
    aliases: Vec<yagura_protocol::LaunchOptionAlias>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ModelAliasesSavedReply, CommandFailure> {
    validate_profile_id(&profile)?;
    pool.save_model_aliases(&app, profile, aliases)
        .await
        .map_err(CommandFailure::from)
}

/// Turns a profile's launching on or off and returns the setting as the
/// daemon stored it.
///
/// # Errors
/// Returns a [`CommandFailure`] when `profile` is not a valid profile
/// id, the daemon cannot be reached, speaks another wire revision, or rejects the
/// request.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_profile_enabled(
    app: AppHandle,
    profile: String,
    enabled: bool,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ProfileEnabledSetReply, CommandFailure> {
    validate_profile_id(&profile)?;
    pool.set_profile_enabled(&app, profile, enabled)
        .await
        .map_err(CommandFailure::from)
}

/// Reads every value a profile uses, with the layer each comes from.
///
/// # Errors
/// Returns a [`CommandFailure`] when `profile` is not a valid profile
/// id, the daemon cannot be reached, speaks another wire revision, or rejects the
/// request.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_profile_detail(
    app: AppHandle,
    profile: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ProfileDetailReply, CommandFailure> {
    validate_profile_id(&profile)?;
    pool.get_profile_detail(&app, profile)
        .await
        .map_err(CommandFailure::from)
}

/// Sets the argv a profile launches on this host, or with `argv` absent puts
/// the shipped one back, and returns the argv in use afterwards.
///
/// # Errors
/// Returns a [`CommandFailure`] when `profile` is not a valid profile
/// id, the daemon cannot be reached, speaks another wire revision, or rejects the
/// request.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_profile_command(
    app: AppHandle,
    profile: String,
    argv: Option<Vec<String>>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ProfileCommandSetReply, CommandFailure> {
    validate_profile_id(&profile)?;
    pool.set_profile_command(&app, profile, argv)
        .await
        .map_err(CommandFailure::from)
}

/// Reads the daemon's current approval settings.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached,
/// speaks another wire revision, or this connection is not a registered
/// approver.
#[tauri::command(rename_all = "snake_case")]
pub async fn get_approval_settings(
    app: AppHandle,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ApprovalSettingsReply, CommandFailure> {
    pool.get_approval_settings(&app)
        .await
        .map_err(CommandFailure::from)
}

/// Turns automatic approval of one destructive tool on or off and returns the
/// settings after the change.
///
/// `tool` is passed through unvalidated beyond its shape: the daemon owns the
/// closed set of recognized tool names, and an unrecognized-but-well-formed
/// name is refused by its reply, not caught here.
///
/// # Errors
/// Returns a [`CommandFailure`] when `tool` is not a valid tool name, the
/// daemon cannot be reached, speaks another wire revision, or rejects the
/// request.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_auto_approve(
    app: AppHandle,
    tool: String,
    auto_approve: bool,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ApprovalSettingsReply, CommandFailure> {
    validate_tool_name(&tool)?;
    pool.set_auto_approve(&app, tool, auto_approve)
        .await
        .map_err(CommandFailure::from)
}

/// Turns the plan review gate on or off and returns the settings after the
/// change.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached,
/// speaks another wire revision, or rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_plan_approval_required(
    app: AppHandle,
    required: bool,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<ApprovalSettingsReply, CommandFailure> {
    pool.set_plan_approval_required(&app, required)
        .await
        .map_err(CommandFailure::from)
}

/// Asks the daemon which profiles describe how to register the MCP bridge
/// with their CLI.
///
/// The reply carries the rows; each row's state follows as a
/// `session://mcp-registration-status` push carrying the same `request_id`.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached,
/// speaks another wire revision, or rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn list_mcp_registrations(
    app: AppHandle,
    request_id: u64,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<McpRegistrationsListedReply, CommandFailure> {
    pool.list_mcp_registrations(&app, request_id)
        .await
        .map_err(CommandFailure::from)
}

/// Asks the daemon to register, update or remove the MCP bridge with one
/// profile's CLI.
///
/// Answers at once with the accepted request; the outcome follows as a
/// `session://mcp-registration-result` push carrying the same `request_id`.
///
/// # Errors
/// Returns a [`CommandFailure`] when `profile` is not a valid profile
/// id, `action` is not one of `register`, `update` or `remove`, the daemon
/// cannot be reached, speaks another wire revision, or rejects the request.
#[tauri::command(rename_all = "snake_case")]
pub async fn run_mcp_registration(
    app: AppHandle,
    profile: String,
    action: String,
    request_id: u64,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<McpRegistrationRequestedReply, CommandFailure> {
    validate_profile_id(&profile)?;
    let action = parse_mcp_registration_action(&action)?;
    pool.run_mcp_registration(&app, profile, action, request_id)
        .await
        .map_err(CommandFailure::from)
}

/// Renames a window.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon cannot be reached, the
/// window does not exist, or the name is invalid (empty, too long, or contains
/// forbidden characters).
#[tauri::command(rename_all = "snake_case")]
pub async fn rename_window(
    app: AppHandle,
    window_id: String,
    name: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<String, CommandFailure> {
    validate_window_id(&window_id)?;
    // The protocol's error text is forwarded verbatim so the caller can tell
    // "empty" from "too long" from "forbidden character".
    validate_window_name(&name).map_err(CommandFailure::from_reason)?;
    pool.rename_window(&app, &window_id, &name)
        .await
        .map_err(CommandFailure::from)
}

/// Forwards one input event to a window: a `key`, `text`, `paste`,
/// `pointer`, `wheel` or `focus` message, which the daemon turns into bytes
/// with the window's terminal modes. Input typed while the window reconnects
/// is held and delivered in order once it is attached again.
///
/// # Errors
/// Returns a [`CommandFailure`] when `input` is another message type or
/// is past its bounds, the window is not one the GUI knows, or the input held
/// while it reconnects is past its bound.
#[tauri::command(rename_all = "snake_case")]
pub async fn send_window_input(
    window_id: String,
    input: ClientMessage,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    validate_window_id(&window_id)?;
    validate_window_input(&input)?;
    pool.send_window_input(&window_id, &input)
        .await
        .map_err(CommandFailure::from)
}

/// One acknowledgement the WebView sends with its next `next_frames` request:
/// it applied the last part of frame `seq` of a window.
#[derive(Debug, Clone, PartialEq, Eq, serde::Deserialize)]
pub struct FrameAckEntry {
    /// The window the acknowledged frame belongs to.
    pub window_id: String,
    /// The acknowledged frame number.
    pub seq: u64,
}

/// The messages that acknowledge `acks`, each paired with the window whose
/// connection it goes to. An entry with an unusable window id is dropped.
fn frame_ack_messages(acks: Vec<FrameAckEntry>) -> Vec<(String, ClientMessage)> {
    acks.into_iter()
        .filter(|ack| validate_window_id(&ack.window_id).is_ok())
        .map(|ack| (ack.window_id, ClientMessage::FrameAck { seq: ack.seq }))
        .collect()
}

/// Acknowledges the frames the WebView applied, then waits for and returns
/// the next frame parts of any window, as `[window_id, frame]` pairs. The
/// WebView keeps exactly one of these requests pending.
///
/// Never fails: an acknowledgement that cannot be sent (no connection, or one
/// that is being re-established) is dropped, because the WebView's receiving
/// loop must not stop and a re-attach brings a frame of the whole screen.
#[tauri::command(rename_all = "snake_case")]
pub async fn next_frames(
    acks: Vec<FrameAckEntry>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<Vec<(String, Frame)>, CommandFailure> {
    for (window_id, message) in frame_ack_messages(acks) {
        let _ = pool.send_to_window(&window_id, &message).await;
    }
    Ok(pool.frames().take().await)
}

/// Gives the daemon the colors a window is drawn with, which it answers the
/// window's color queries with. A window that is not attached is given them
/// by its next attach.
///
/// # Errors
/// Returns a [`CommandFailure`] when the palette does not hold 16 ANSI
/// colors, the window is not one the GUI knows, or the write fails.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_window_palette(
    window_id: String,
    palette: Palette,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    validate_window_id(&window_id)?;
    validate_palette(&palette)?;
    pool.set_window_palette(&window_id, palette)
        .await
        .map_err(CommandFailure::from)
}

/// Tells the daemon whether a window is on screen, so it builds no frames for
/// one that is not. A window that is not attached is told by its next attach.
///
/// # Errors
/// Returns a [`CommandFailure`] when the window is not one the GUI knows
/// or the write fails.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_window_visible(
    window_id: String,
    visible: bool,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    validate_window_id(&window_id)?;
    pool.set_window_visible(&window_id, visible)
        .await
        .map_err(CommandFailure::from)
}

/// Reads the text selected in a window; `None` when nothing is selected.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon connection is gone or
/// does not answer in time.
#[tauri::command(rename_all = "snake_case")]
pub async fn copy_window_selection(
    window_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<Option<String>, CommandFailure> {
    validate_window_id(&window_id)?;
    pool.copy_selection(&window_id)
        .await
        .map_err(CommandFailure::from)
}

/// Clears the selection in a window, as a copy does once its text is on the
/// clipboard.
///
/// # Errors
/// Returns a [`CommandFailure`] when the daemon connection is gone.
#[tauri::command(rename_all = "snake_case")]
pub async fn clear_window_selection(
    window_id: String,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    validate_window_id(&window_id)?;
    pool.send_to_window(&window_id, &ClientMessage::ClearSelection)
        .await
        .map_err(CommandFailure::from)
}

/// Resizes a window's PTY. A window that is not attached is given the size by
/// its next attach.
///
/// # Errors
/// Returns a [`CommandFailure`] when a dimension is outside the protocol
/// range, the window is not one the GUI knows, or the write fails.
#[tauri::command(rename_all = "snake_case")]
pub async fn resize(
    window_id: String,
    cols: u16,
    rows: u16,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    validate_window_id(&window_id)?;
    validate_dimension(cols, MIN_PTY_COLS, "cols")?;
    validate_dimension(rows, MIN_PTY_ROWS, "rows")?;
    pool.resize(&window_id, cols, rows)
        .await
        .map_err(CommandFailure::from)
}

/// Rejects a window id that no daemon request could ever match.
fn validate_window_id(window_id: &str) -> Result<(), IpcError> {
    if window_id.is_empty() {
        Err(IpcError::InvalidArgument("window_id must not be empty"))
    } else {
        Ok(())
    }
}

/// Rejects a plan id that no daemon request could ever match.
fn validate_plan_id(plan_id: &str) -> Result<(), IpcError> {
    if plan_id.is_empty() {
        Err(IpcError::InvalidArgument("plan_id must not be empty"))
    } else {
        Ok(())
    }
}

/// Rejects a filter name that is given but outside the character set the
/// daemon accepts for state and milestone names.
fn validate_optional_token(value: Option<&str>, reason: &'static str) -> Result<(), IpcError> {
    match value {
        Some(name) if !yagura_protocol::is_valid_token(name) => {
            Err(IpcError::InvalidArgument(reason))
        }
        _ => Ok(()),
    }
}

/// Rejects a profile id outside the character set the daemon accepts, which
/// also keeps it from naming a path outside the profiles directory.
fn validate_profile_id(profile: &str) -> Result<(), IpcError> {
    if yagura_protocol::is_valid_token(profile) {
        Ok(())
    } else {
        Err(IpcError::InvalidArgument(
            "profile must be 1 to 64 characters of a-z, 0-9, '_' or '-'",
        ))
    }
}

/// Rejects a tool name outside the character set the daemon accepts. The
/// closed set of recognized names is the daemon's, not this layer's: this
/// only rules out a shape no daemon request could ever match.
fn validate_tool_name(tool: &str) -> Result<(), IpcError> {
    if yagura_protocol::is_valid_token(tool) {
        Ok(())
    } else {
        Err(IpcError::InvalidArgument(
            "tool must be 1 to 64 characters of a-z, 0-9, '_' or '-'",
        ))
    }
}

const MCP_REGISTRATION_ACTIONS: [yagura_protocol::McpRegistrationAction; 3] = [
    yagura_protocol::McpRegistrationAction::Register,
    yagura_protocol::McpRegistrationAction::Update,
    yagura_protocol::McpRegistrationAction::Remove,
];

/// The wire string of an MCP registration action, as the WebView names it.
pub(crate) fn mcp_registration_action_str(
    action: yagura_protocol::McpRegistrationAction,
) -> &'static str {
    match action {
        yagura_protocol::McpRegistrationAction::Register => "register",
        yagura_protocol::McpRegistrationAction::Update => "update",
        yagura_protocol::McpRegistrationAction::Remove => "remove",
    }
}

/// Accepts only the closed set of actions: an unknown one names an intent
/// the daemon must not have filled in for it.
fn parse_mcp_registration_action(
    action: &str,
) -> Result<yagura_protocol::McpRegistrationAction, IpcError> {
    MCP_REGISTRATION_ACTIONS
        .into_iter()
        .find(|known| mcp_registration_action_str(*known) == action)
        .ok_or(IpcError::InvalidArgument(
            "action must be one of register, update or remove",
        ))
}

/// Rejects a task id that no daemon request could ever match.
fn validate_task_id(task_id: &str) -> Result<(), IpcError> {
    if task_id.is_empty() {
        Err(IpcError::InvalidArgument("task_id must not be empty"))
    } else {
        Ok(())
    }
}

/// Accepts only the messages a WebView sends as input, within their bounds.
///
/// # Errors
///
/// [`IpcError::InvalidArgument`] for a message that is not window input, or
/// input past the bounds `check_input_bounds` names.
pub fn validate_window_input(input: &ClientMessage) -> Result<(), IpcError> {
    check_input_bounds(input).map_err(IpcError::InvalidArgument)?;
    match input {
        ClientMessage::Key(_)
        | ClientMessage::Text { .. }
        | ClientMessage::Paste { .. }
        | ClientMessage::Pointer(_)
        | ClientMessage::Wheel(_)
        | ClientMessage::Focus { .. } => Ok(()),
        _ => Err(IpcError::InvalidArgument(
            "input must be a key, text, paste, pointer, wheel or focus message",
        )),
    }
}

/// Gives the daemon the palettes the GUI would draw a new window with: the
/// one of each vendor that has its own, and the application's for the rest.
/// The daemon answers the color queries of a window it opens with them until
/// the window is shown.
///
/// A vendor no window can have (one the window vendor rule refuses) is
/// dropped rather than sent: it could never be looked up.
///
/// # Errors
/// Returns a [`CommandFailure`] when a palette does not hold 16 ANSI
/// colors, there are more vendors than the daemon takes, or a connected
/// daemon could not be written to.
#[tauri::command(rename_all = "snake_case")]
pub async fn set_default_palettes(
    app: AppHandle,
    app_palette: Palette,
    vendor_palettes: std::collections::BTreeMap<String, Palette>,
    pool: State<'_, Arc<ConnectionPool>>,
) -> Result<(), CommandFailure> {
    let palettes = default_palettes_to_send(app_palette, vendor_palettes)?;
    pool.set_default_palettes(&app, palettes)
        .await
        .map_err(CommandFailure::from)
}

fn default_palettes_to_send(
    app: Palette,
    mut vendors: std::collections::BTreeMap<String, Palette>,
) -> Result<DefaultPalettes, IpcError> {
    vendors.retain(|vendor, _| validate_window_vendor(vendor).is_ok());
    if vendors.len() > DEFAULT_PALETTES_MAX_VENDORS {
        return Err(IpcError::InvalidArgument(
            "too many vendors are assigned a palette",
        ));
    }
    validate_palette(&app)?;
    for palette in vendors.values() {
        validate_palette(palette)?;
    }
    Ok(DefaultPalettes { app, vendors })
}

/// The daemon reads the 16 ANSI colors by index.
fn validate_palette(palette: &Palette) -> Result<(), IpcError> {
    if palette.ansi.len() == ANSI_COLOR_COUNT {
        Ok(())
    } else {
        Err(IpcError::InvalidArgument(
            "palette must hold 16 ANSI colors",
        ))
    }
}

const ANSI_COLOR_COUNT: usize = 16;

/// Rejects a `layout_json` payload the daemon would refuse outright, so the
/// oversized string never crosses the IPC boundary.
fn validate_layout_json_len(layout_json: &str) -> Result<(), IpcError> {
    if layout_json.len() > MAX_LAYOUT_JSON_BYTES {
        Err(IpcError::InvalidArgument("layout_json is too large"))
    } else {
        Ok(())
    }
}

/// Rejects a `limit` the daemon would refuse, so the round trip is not spent
/// on an argument that cannot succeed. An omitted limit is the daemon's own
/// default and is always accepted.
fn validate_chat_read_limit(limit: Option<u32>) -> Result<(), IpcError> {
    match limit {
        Some(value) if value == 0 || value > CHAT_READ_LIMIT_MAX => Err(IpcError::InvalidArgument(
            "limit is outside the accepted range",
        )),
        _ => Ok(()),
    }
}

fn validate_dimension(value: u16, min: u16, name: &'static str) -> Result<(), IpcError> {
    if (min..=MAX_PTY_DIMENSION).contains(&value) {
        Ok(())
    } else {
        Err(IpcError::InvalidArgument(name))
    }
}

/// Builds the failure a command returns from an [`IpcError`], whose `text` is
/// that error's [`Display`](std::fmt::Display).
impl From<IpcError> for CommandFailure {
    fn from(error: IpcError) -> Self {
        let text = error.to_string();
        let kind = match error {
            IpcError::Rejected {
                code,
                message,
                rejected_argument,
            } => FailureKind::Rejected {
                code,
                message,
                rejected_argument,
            },
            IpcError::Unsupported { request_type } => FailureKind::Unsupported { request_type },
            // Refusals this Rust layer raised on its own: no `error.code` stands
            // behind them. Named one by one so a new variant has to be classified.
            IpcError::EndpointName(_)
            | IpcError::NotConnected
            | IpcError::ConnectionLost
            | IpcError::ConnectTimeout
            | IpcError::UnexpectedReply
            | IpcError::Write
            | IpcError::DaemonNotFound
            | IpcError::DaemonSpawn
            | IpcError::InvalidArgument(_)
            | IpcError::WindowNotAttached { .. }
            | IpcError::ApproverNotRegistered => FailureKind::Local,
        };
        Self { kind, text }
    }
}

impl CommandFailure {
    /// Builds the failure a command returns when it refuses an argument with
    /// a check whose error never becomes an [`IpcError`]; the `text` is that
    /// error's [`Display`](std::fmt::Display).
    pub fn from_reason(reason: impl std::fmt::Display) -> Self {
        Self {
            kind: FailureKind::Local,
            text: reason.to_string(),
        }
    }
}

/// Contract tests for the argument validators and dispatch logic in this module.
///
/// The command functions themselves cannot be called here: every command takes
/// `State<'_, Arc<ConnectionPool>>`, which only a running Tauri app can hand out.
#[cfg(test)]
mod tests {
    use super::*;
    use yagura_test_support::paths::TempDir;

    const TEMP_DIR_PREFIX: &str = "yagura-test";

    #[test]
    fn validate_window_id_rejects_the_empty_string() {
        assert!(validate_window_id("").is_err());
    }

    #[test]
    fn a_minimum_window_side_is_taken_only_as_a_finite_number_in_range() {
        for side in [1.0, 929.0, MAX_MIN_WINDOW_SIDE] {
            assert!(
                check_min_window_side(side).is_ok(),
                "{side} is a usable side"
            );
        }
        for side in [
            f64::NAN,
            f64::INFINITY,
            f64::NEG_INFINITY,
            0.0,
            -1.0,
            MAX_MIN_WINDOW_SIDE + 1.0,
        ] {
            assert!(check_min_window_side(side).is_err(), "{side} is refused");
        }
    }

    #[test]
    fn frame_acks_become_one_frame_ack_message_per_window() {
        let acks = vec![
            FrameAckEntry {
                window_id: "a".to_owned(),
                seq: 3,
            },
            FrameAckEntry {
                window_id: "b".to_owned(),
                seq: 9,
            },
        ];
        assert_eq!(
            frame_ack_messages(acks),
            vec![
                ("a".to_owned(), ClientMessage::FrameAck { seq: 3 }),
                ("b".to_owned(), ClientMessage::FrameAck { seq: 9 }),
            ]
        );
    }

    #[test]
    fn a_frame_ack_for_an_unusable_window_id_is_dropped() {
        let acks = vec![
            FrameAckEntry {
                window_id: String::new(),
                seq: 1,
            },
            FrameAckEntry {
                window_id: "a".to_owned(),
                seq: 2,
            },
        ];
        assert_eq!(
            frame_ack_messages(acks),
            vec![("a".to_owned(), ClientMessage::FrameAck { seq: 2 })]
        );
    }

    #[test]
    fn validate_window_id_accepts_a_non_empty_id() {
        assert!(validate_window_id("w1-3f9a").is_ok());
    }

    #[test]
    fn validate_layout_json_len_accepts_exactly_the_bound() {
        let at_limit = "x".repeat(MAX_LAYOUT_JSON_BYTES);
        assert!(validate_layout_json_len(&at_limit).is_ok());
    }

    #[test]
    fn validate_layout_json_len_rejects_one_byte_over_the_bound() {
        let oversized = "x".repeat(MAX_LAYOUT_JSON_BYTES + 1);
        assert!(validate_layout_json_len(&oversized).is_err());
    }

    #[test]
    fn window_name_rule_accepts_an_ordinary_name() {
        assert!(validate_window_name("valid-name").is_ok());
    }

    #[test]
    fn window_name_rule_rejects_the_empty_string() {
        assert!(validate_window_name("").is_err());
    }

    #[test]
    fn chat_read_limit_rule_accepts_an_omitted_limit() {
        assert!(validate_chat_read_limit(None).is_ok());
    }

    #[test]
    fn chat_read_limit_rule_accepts_exactly_the_bound() {
        assert!(validate_chat_read_limit(Some(CHAT_READ_LIMIT_MAX)).is_ok());
    }

    #[test]
    fn chat_read_limit_rule_rejects_one_past_the_bound() {
        assert!(validate_chat_read_limit(Some(CHAT_READ_LIMIT_MAX + 1)).is_err());
    }

    #[test]
    fn chat_read_limit_rule_rejects_zero() {
        assert!(validate_chat_read_limit(Some(0)).is_err());
    }

    #[test]
    fn chat_body_rule_accepts_an_ordinary_message() {
        assert!(validate_chat_body("@reviewer please take a look").is_ok());
    }

    #[test]
    fn chat_body_rule_rejects_the_empty_string() {
        assert!(validate_chat_body("").is_err());
    }

    #[test]
    fn keymap_file_path_with_root_names_the_file_load_keymap_reads() {
        let temp_dir = TempDir::new(TEMP_DIR_PREFIX, "keymap-file-path");

        let reported = keymap_file_path_with_root(Some(temp_dir.path()));

        assert_eq!(
            reported,
            Some(
                temp_dir
                    .path()
                    .join("keymap.toml")
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    #[test]
    fn keymap_file_path_with_root_reports_nothing_without_a_root() {
        assert_eq!(keymap_file_path_with_root(None), None);
    }

    #[test]
    fn load_keymap_with_root_returns_not_found_when_root_is_none() {
        let result = load_keymap_with_root(None);

        assert_eq!(result.failure, Some(crate::keymap::KeymapFailure::NotFound));
        assert!(result.keys.is_empty());
    }

    #[test]
    fn load_keymap_with_root_loads_file_when_root_is_some() {
        let temp_dir = TempDir::new(TEMP_DIR_PREFIX, "load-keymap");
        temp_dir.write_file(
            "keymap.toml",
            "[keys]\nopen-window-form = \"Ctrl+Shift+n\"\n",
        );

        let result = load_keymap_with_root(Some(temp_dir.path()));

        assert_eq!(result.failure, None);
        assert_eq!(
            result.keys.get("open-window-form"),
            Some(&"Ctrl+Shift+n".to_owned())
        );
    }

    #[test]
    fn load_themes_with_root_returns_not_found_when_root_is_none() {
        let result = load_themes_with_root(None);

        assert_eq!(result.failure, Some(crate::themes::ThemesFailure::NotFound));
        assert!(result.themes.is_empty());
    }

    #[test]
    fn load_themes_with_root_loads_file_when_root_is_some() {
        let temp_dir = TempDir::new(TEMP_DIR_PREFIX, "load-themes");
        temp_dir.write_file(
            "themes.toml",
            "[themes.custom]\ncolor-bg-primary = \"#101010\"\n",
        );

        let result = load_themes_with_root(Some(temp_dir.path()));

        assert_eq!(result.failure, None);
        assert_eq!(
            result
                .themes
                .get("custom")
                .and_then(|theme| theme.get("color-bg-primary")),
            Some(&"#101010".to_owned())
        );
    }

    fn command_line() -> Vec<String> {
        vec!["bash".to_owned()]
    }

    /// The kind and the sentence the WebView reads of a failure, off the wire.
    fn kind_and_text(failure: CommandFailure) -> (serde_json::Value, serde_json::Value) {
        let wire = serde_json::to_value(failure).expect("a failure serializes");
        (wire["kind"].clone(), wire["text"].clone())
    }

    /// What a refusal of `reason` reaches the WebView as.
    fn refused_here(reason: impl std::fmt::Display) -> (serde_json::Value, serde_json::Value) {
        (
            serde_json::json!("local"),
            serde_json::json!(reason.to_string()),
        )
    }

    #[test]
    fn open_window_arguments_accept_a_bare_command() {
        assert_eq!(
            check_open_window_arguments(&command_line(), None, None, None, None, None, None),
            Ok(())
        );
    }

    #[test]
    fn open_window_arguments_reject_neither_a_command_nor_a_profile() {
        assert!(check_open_window_arguments(&[], None, None, None, None, None, None).is_err());
    }

    #[test]
    fn open_window_arguments_reject_a_model_without_a_profile() {
        let checked =
            check_open_window_arguments(&command_line(), None, None, None, None, Some("m"), None);
        let reason = validate_open_window_exclusivity(true, None, None, None, true)
            .expect_err("launch options without a profile are refused");
        assert_eq!(
            kind_and_text(checked.expect_err("refused")),
            refused_here(reason)
        );
    }

    #[test]
    fn open_window_arguments_reject_an_effort_without_a_profile() {
        let checked =
            check_open_window_arguments(&command_line(), None, None, None, None, None, Some("e"));
        let reason = validate_open_window_exclusivity(true, None, None, None, true)
            .expect_err("launch options without a profile are refused");
        assert_eq!(
            kind_and_text(checked.expect_err("refused")),
            refused_here(reason)
        );
    }

    #[test]
    fn open_window_arguments_accept_launch_options_with_a_profile() {
        assert_eq!(
            check_open_window_arguments(&[], None, None, Some("p"), None, Some("m"), Some("e")),
            Ok(())
        );
    }

    #[test]
    fn open_window_arguments_reject_an_empty_window_name() {
        let checked =
            check_open_window_arguments(&command_line(), Some(""), None, None, None, None, None);
        let reason = validate_window_name("").expect_err("an empty name is refused");
        assert_eq!(
            kind_and_text(checked.expect_err("refused")),
            refused_here(reason)
        );
    }

    #[test]
    fn validate_plan_id_rejects_the_empty_string_and_accepts_an_id() {
        assert!(validate_plan_id("").is_err());
        assert!(validate_plan_id("plan-1").is_ok());
    }

    #[test]
    fn validate_task_id_rejects_the_empty_string_and_accepts_an_id() {
        assert!(validate_task_id("").is_err());
        assert!(validate_task_id("task-1").is_ok());
    }

    #[test]
    fn validate_profile_id_rejects_a_path_and_accepts_a_token() {
        assert!(validate_profile_id("../etc").is_err());
        assert!(validate_profile_id("claude-code").is_ok());
    }

    #[test]
    fn validate_tool_name_rejects_an_uppercase_name_and_accepts_a_token() {
        assert!(validate_tool_name("Close_Window").is_err());
        assert!(validate_tool_name("close_window").is_ok());
    }

    #[test]
    fn mcp_registration_action_parses_each_known_name_to_its_own_action() {
        for action in MCP_REGISTRATION_ACTIONS {
            let parsed = parse_mcp_registration_action(mcp_registration_action_str(action))
                .expect("a known action name parses");
            assert_eq!(parsed, action);
        }
    }

    #[test]
    fn mcp_registration_action_rejects_an_unknown_name() {
        assert!(parse_mcp_registration_action("purge").is_err());
    }

    fn palette_with(ansi_count: usize) -> Palette {
        let color: yagura_protocol::Rgb = [0, 0, 0];
        Palette {
            foreground: color,
            background: color,
            cursor: color,
            ansi: vec![color; ansi_count],
        }
    }

    #[test]
    fn validate_palette_accepts_exactly_sixteen_ansi_colors() {
        assert!(validate_palette(&palette_with(ANSI_COLOR_COUNT)).is_ok());
    }

    #[test]
    fn validate_palette_rejects_a_palette_missing_an_ansi_color() {
        assert!(validate_palette(&palette_with(ANSI_COLOR_COUNT - 1)).is_err());
    }

    #[test]
    fn validate_dimension_accepts_the_bounds_and_rejects_past_them() {
        assert!(validate_dimension(MIN_PTY_COLS, MIN_PTY_COLS, "cols").is_ok());
        assert!(validate_dimension(MAX_PTY_DIMENSION, MIN_PTY_COLS, "cols").is_ok());
        assert!(validate_dimension(MIN_PTY_COLS - 1, MIN_PTY_COLS, "cols").is_err());
        assert!(validate_dimension(MAX_PTY_DIMENSION + 1, MIN_PTY_COLS, "cols").is_err());
    }
}
