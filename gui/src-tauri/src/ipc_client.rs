//! IPC client: the GUI side of the length-prefixed JSON framing (LPJF)
//! protocol defined by `yagura-protocol`.
//!
//! One `IpcClient` owns exactly one connection to the daemon. A connection is
//! attached to at most one window, so watching N windows takes N
//! clients plus one attach-less client for the window-scoped requests; the
//! [`crate::connection_pool`] module owns that fan-out.
//!
//! The GUI never owns a PTY or a child process: this module only connects to
//! the daemon's endpoint and forwards validated commands. Turning daemon
//! messages into the canonical Tauri events is [`crate::message_dispatch`]'s
//! job; starting the daemon when none is running is
//! [`crate::daemon_launch`]'s.
//!
//! Every operating-system operation goes through [`yagura_os`], so
//! this module names no platform type and is not conditionally compiled.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use tokio::io::WriteHalf;
use tokio::sync::{Mutex, oneshot};
use yagura_os::{IpcTransport, OsError, PeerAuth, PlatformIpc, default_endpoint};
use yagura_protocol::{
    ApprovalRequest, ClientMessage, LaunchOptionAlias, McpRegistrationAction,
    PROTOCOL_REVISION_BASELINE, PlanInfo, PlanSummary, RoleEntry, TaskAssignment, TerminalSize,
    WindowInfo,
};

use crate::commands::{
    ApprovalSettingsReply, ChatMessagesReply, ChatPostedReply, ChatRoomListReply,
    DaemonHealthReply, DetectionListReply, McpRegistrationRequestedReply,
    McpRegistrationsListedReply, ModelAliasesSavedReply, ModelChoicesRequestedReply,
    OpenWindowReply, ProfileCommandSetReply, ProfileDetailReply, ProfileEnabledSetReply,
    ProfileListReply, RestoreCandidateListReply, RoleListReply, TaskWindowOpenedReply,
    UpdateWindowLayoutReply,
};
use crate::constants::{
    ATTACH_TIMEOUT, CONNECT_RETRY_INTERVAL, CONNECT_RETRY_LIMIT, RECONNECT_ATTEMPT_LIMIT,
};
use crate::daemon_launch::connect_after_spawn;
use crate::events::{
    ApprovalRegistrationRefusedPayload, ApprovalRequestedPayload, ApprovalResetPayload,
    AttachResponse, DisconnectedPayload, EVENT_APPROVAL_REGISTRATION_REFUSED,
    EVENT_APPROVAL_REQUESTED, EVENT_APPROVAL_RESET, EVENT_DISCONNECTED, EVENT_EXIT,
    EVENT_SUPERSEDED, ExitPayload, SupersededPayload,
};
use crate::host::GuiHost;
use crate::instance::InstanceResolution;
use crate::message_dispatch::read_loop;

/// A client-side connection to the daemon's endpoint.
pub(crate) type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

/// `error.code` telling this client that another GUI took the session over.
pub(crate) const ERROR_CODE_SUPERSEDED: &str = "superseded";

/// `error.code` for internal daemon failures.
pub(crate) const ERROR_CODE_INTERNAL: &str = "internal";

/// `error.code` a daemon built before a message type existed answers it
/// with. Settles [`IpcClient::ensure_protocol_revision`]'s probe the same as
/// a genuine answer would: the refusal itself says which wire the peer
/// speaks.
const ERROR_CODE_UNKNOWN_TYPE: &str = "unknown_type";

/// The rendered tail of a rejection: the daemon's reason behind a separator,
/// or nothing at all when the error frame carried no reason.
fn reason_suffix(message: &str) -> String {
    if message.is_empty() {
        String::new()
    } else {
        format!(": {message}")
    }
}

/// Failures surfaced to the WebView through a command result.
#[derive(Debug, thiserror::Error)]
pub enum IpcError {
    /// The current user's identifier (and therefore the endpoint name) is
    /// unavailable.
    #[error("failed to resolve the daemon endpoint name: {0}")]
    EndpointName(#[from] OsError),
    /// No connection to the daemon is currently established.
    #[error("not connected to the yagura daemon")]
    NotConnected,
    /// The daemon did not accept a connection within the retry budget.
    #[error("timed out waiting for the yagura daemon to accept a connection")]
    ConnectTimeout,
    /// No `attached` reply arrived in time.
    #[error("timed out waiting for the attach reply")]
    AttachTimeout,
    /// No reply to a window-scoped request arrived in time.
    #[error("timed out waiting for the daemon reply")]
    RequestTimeout,
    /// The daemon answered the outstanding request with an error frame.
    ///
    /// The rendered form carries the daemon's reason as well as the code:
    /// this text reaches the WebView through every command that reports an
    /// [`IpcError`] as a string, and the code alone tells the user nothing
    /// they can act on.
    #[error("the daemon rejected the request: {code}{}", reason_suffix(.message))]
    Rejected {
        /// The `error.code` reported by the daemon.
        code: String,
        /// The daemon's human-readable English reason, empty when the frame
        /// carried none.
        message: String,
        /// The name of the request argument the rejection points at, when it
        /// points at one the caller itself sent. A name, never a value.
        rejected_argument: Option<String>,
    },
    /// The daemon answered with a message that does not belong to the
    /// outstanding request.
    #[error("the daemon sent an unexpected reply")]
    UnexpectedReply,
    /// Writing to the daemon connection failed.
    #[error("failed to write to the daemon connection")]
    Write,
    /// The daemon executable was not found next to the GUI executable.
    #[error("could not locate the daemon executable next to the GUI executable")]
    DaemonNotFound,
    /// The daemon process could not be started.
    #[error("failed to start the yagura daemon")]
    DaemonSpawn,
    /// A command argument failed Rust-side validation.
    #[error("invalid argument: {0}")]
    InvalidArgument(&'static str),
    /// No connection of the pool is attached to the requested window, so
    /// there is nothing to route input or a resize to.
    #[error("no connection is attached to window {window_id}")]
    WindowNotAttached {
        /// The window that was addressed.
        window_id: String,
    },
    /// This connection has not registered as an approver yet, so a
    /// GUI-originated destructive operation (which bypasses the daemon's
    /// human approval gate once registered) is refused
    /// here rather than sent unchecked.
    #[error("not yet registered as an approver with the daemon")]
    ApproverNotRegistered,
    /// The connected daemon has not settled on a wire revision this request
    /// needs, so [`IpcClient::send`] refused to write it rather than risk a
    /// frame an older daemon cannot parse.
    #[error(
        "this request needs a wire revision the connected daemon does not speak: {request_type}"
    )]
    Unsupported {
        /// The wire tag of the request that was refused, one of
        /// [`yagura_protocol::client_message_wire_tags`].
        request_type: String,
    },
}

/// Which request a [`Pending`] slot is waiting for. The reply is matched to
/// the request by this tag because the protocol carries no correlation id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum PendingKind {
    Attach,
    ListWindows,
    ListDetections,
    ListProfiles,
    OpenWindow,
    RenameWindow,
    CloseWindow,
    InterruptWindow,
    ListRestoreCandidates,
    UpdateWindowLayout,
    RegisterApprover,
    DecideApproval,
    /// Both chat posts wait under one tag: the daemon answers `send_chat` and
    /// `broadcast_chat` with the same `chat_posted` frame.
    SendChat,
    ReadChat,
    ListChatRooms,
    GetProtocolRevision,
    GetDaemonHealth,
    /// Every request answered with a whole plan waits under one tag: the
    /// daemon answers reads, overrides and owner changes with the same
    /// `plan_detail` frame.
    PlanDetail,
    PlanList,
    PlanDeleted,
    /// Both role requests wait under one tag: the daemon answers reading and
    /// saving the table with the same `role_list` frame.
    RoleList,
    TaskWindowOpened,
    ListModelChoices,
    SaveModelAliases,
    SetProfileEnabled,
    GetProfileDetail,
    SetProfileCommand,
    GetApprovalSettings,
    SetAutoApprove,
    SetPlanApprovalRequired,
    ListMcpRegistrations,
    RunMcpRegistration,
    CopySelection,
}

impl PendingKind {
    /// Attach keeps a timeout error of its own: it is the one request the
    /// WebView shows as an attach failure rather than a command failure.
    fn timeout_error(self) -> IpcError {
        match self {
            Self::Attach => IpcError::AttachTimeout,
            _ => IpcError::RequestTimeout,
        }
    }
}

/// How hard a connect attempt tries to reach the daemon.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConnectMode {
    /// A single attempt. The caller retries on its own schedule.
    Once,
    /// Retry a refused endpoint, first attempt before any wait:
    /// the daemon offers one pending endpoint instance at a time, so a connect can be refused simply because another client took
    /// it. Never starts a daemon: a window connection presupposes one.
    Retry,
    /// Retry, and start the daemon when none answers at all.
    Spawn,
}

/// The value a daemon reply carries back to the caller that is waiting for it.
#[derive(Debug)]
pub(crate) enum Reply {
    Attached(AttachResponse),
    WindowList(Vec<WindowInfo>),
    DetectionList(DetectionListReply),
    ProfileList(ProfileListReply),
    WindowOpened(OpenWindowReply),
    WindowRenamed(String),
    RestoreCandidateList(RestoreCandidateListReply),
    WindowLayoutUpdated(UpdateWindowLayoutReply),
    /// Every request pending at the moment this connection's approver
    /// registration completed.
    ApproverRegistered(Vec<ApprovalRequest>),
    ChatPosted(ChatPostedReply),
    ChatMessages(ChatMessagesReply),
    ChatRoomList(ChatRoomListReply),
    /// The wire revision the answering daemon implements, from
    /// [`ClientMessage::GetProtocolRevision`]'s reply.
    ProtocolRevision(u32),
    /// The problems the daemon lists, from [`ClientMessage::GetDaemonHealth`]'s
    /// reply.
    DaemonHealth(DaemonHealthReply),
    PlanDetail(PlanInfo),
    PlanList(Vec<PlanSummary>),
    /// The id of the plan the daemon deleted.
    PlanDeleted(String),
    RoleList(RoleListReply),
    // This client never sends `open_task_window`, so no request method reads
    // the value; the variant keeps the frame classified like every other reply.
    #[allow(dead_code)]
    TaskWindowOpened(TaskWindowOpenedReply),
    ModelChoicesRequested(ModelChoicesRequestedReply),
    ModelAliasesSaved(ModelAliasesSavedReply),
    ProfileEnabledSet(ProfileEnabledSetReply),
    ProfileDetail(ProfileDetailReply),
    ProfileCommandSet(ProfileCommandSetReply),
    /// The daemon's approval settings, from
    /// [`ClientMessage::GetApprovalSettings`]'s reply.
    ApprovalSettings(ApprovalSettingsReply),
    /// The approval settings after [`ClientMessage::SetAutoApprove`]'s
    /// change.
    AutoApproveSet(ApprovalSettingsReply),
    /// The approval settings after
    /// [`ClientMessage::SetPlanApprovalRequired`]'s change.
    PlanApprovalRequiredSet(ApprovalSettingsReply),
    McpRegistrationsListed(McpRegistrationsListedReply),
    McpRegistrationRequested(McpRegistrationRequestedReply),
    /// The selected text of the attached window; `None` when nothing is
    /// selected.
    SelectionText(Option<String>),
    /// The request succeeded and carries no value beyond that.
    Done,
}

/// The request currently awaiting a reply on this connection.
#[derive(Debug)]
struct Pending {
    kind: PendingKind,
    sender: oneshot::Sender<Result<Reply, IpcError>>,
}

/// Whether a connection may register as an approver of destructive
/// operations. `IpcClient::default()` -- and so
/// `IpcClient::new()` -- yields `Control`, which is what the connection pool's
/// single control connection relies on without naming a role explicitly;
/// `IpcClient::new_window()` is the one way to get `Window` instead, so a
/// per-window connection always states its role rather than inheriting one.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Role {
    Control,
    Window,
}

/// One connection to the daemon, attached to at most one window.
#[derive(Debug)]
pub struct IpcClient {
    writer: Mutex<wire::Writer<WriteHalf<Connection>>>,
    /// The single outstanding request. There is never more than one: the
    /// protocol has no correlation id and an `error` frame carries no window
    /// id, so a reply can only be attributed to a request by being the only
    /// one in flight. [`IpcClient::request`] holds `request_lock` across the
    /// whole round trip to keep that true.
    pending: Mutex<Option<Pending>>,
    request_lock: Mutex<()>,
    /// Cancels the reader task of the current connection, which is what makes
    /// [`IpcClient::disconnect`] release the connection: the reader owns the
    /// read half, so dropping the write half alone would keep it open.
    ///
    /// A blocking mutex, never held across an await: `install` has to stay
    /// synchronous, or the reader task it spawns would have to prove its own
    /// future `Send` recursively (it reconnects, which installs again).
    cancel_reader: std::sync::Mutex<Option<oneshot::Sender<()>>>,
    /// Incremented on every connection install. A reader task only reports its
    /// own disconnect if it is still the current connection, which keeps a
    /// replaced (stale) connection from emitting events or triggering an extra
    /// reconnect.
    generation: AtomicU64,
    /// The `generation` whose `register_approver` round trip last completed.
    /// The sentinel `u64::MAX` means none has: no real connection reaches
    /// that many installs, so it never collides with an actual generation.
    registered_generation: AtomicU64,
    /// The most recent `generation` whose `register_approver` attempt was
    /// refused (`auth_failed`). The sentinel `u64::MAX` means none has been.
    /// Latching by generation is what stops a live-lock with the window-list
    /// poll: a refusal is not retried until a reconnect advances the
    /// generation, rather than on every tick.
    refused_generation: AtomicU64,
    /// The wire revision the peer settled on, and the `generation` it was
    /// settled for.
    settled: wire::SettledRevision,
    /// The window this client is attached to, or `None` for a control connection.
    /// Set by `request_attach` and cleared by `on_disconnect`.
    window_id: Mutex<Option<String>>,
    /// The size last sent for this window, by an attach or a resize. A
    /// reconnect attaches at it, so a daemon that came back with the window at
    /// its default size is brought to the size the view is drawn at.
    last_size: Mutex<Option<TerminalSize>>,
    /// The visibility last sent for this window. A reconnect restores it: the
    /// daemon forgets it with the connection, and a window that was hidden
    /// would otherwise be built frames for again.
    last_visible: Mutex<Option<bool>>,
    role: Role,
}

impl Default for IpcClient {
    fn default() -> Self {
        Self {
            writer: Mutex::new(wire::Writer::empty()),
            pending: Mutex::new(None),
            request_lock: Mutex::new(()),
            cancel_reader: std::sync::Mutex::new(None),
            generation: AtomicU64::new(0),
            registered_generation: AtomicU64::new(u64::MAX),
            refused_generation: AtomicU64::new(u64::MAX),
            settled: wire::SettledRevision::new(),
            window_id: Mutex::new(None),
            last_size: Mutex::new(None),
            last_visible: Mutex::new(None),
            role: Role::Control,
        }
    }
}

impl IpcClient {
    /// Creates a client with no connection established yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a per-window client with no connection established yet. Never
    /// `Control`: only the connection pool's single control connection may
    /// register as an approver.
    pub(crate) fn new_window() -> Self {
        Self {
            role: Role::Window,
            ..Self::default()
        }
    }

    /// Connects to the daemon, starting one when none answers
    /// Does nothing when already connected.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon can neither be reached nor
    /// started.
    pub async fn connect(self: &Arc<Self>, app: &impl GuiHost) -> Result<(), IpcError> {
        self.connect_inner(app, ConnectMode::Spawn).await
    }

    /// Makes a single attempt to reach a daemon that is already running. The
    /// window-list poll uses this: it has its own interval to retry on, and a
    /// daemon that fails to start must not be spawned again on every tick.
    ///
    /// # Errors
    /// Returns [`IpcError::NotConnected`] when no daemon answers.
    pub async fn connect_existing(self: &Arc<Self>, app: &impl GuiHost) -> Result<(), IpcError> {
        self.connect_inner(app, ConnectMode::Once).await
    }

    /// Attaches this connection to `window_id`, connecting first if needed.
    ///
    /// The connect retries a refused endpoint but never starts a
    /// daemon: a window can only exist if one is already running.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or the window
    /// does not exist.
    pub async fn attach_window(
        self: &Arc<Self>,
        app: &impl GuiHost,
        window_id: &str,
        size: Option<TerminalSize>,
    ) -> Result<AttachResponse, IpcError> {
        self.connect_inner(app, ConnectMode::Retry).await?;
        self.request_attach(window_id, size).await
    }

    /// Lists all windows the daemon currently manages.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_windows(&self) -> Result<Vec<WindowInfo>, IpcError> {
        match self
            .request(&ClientMessage::ListWindows, PendingKind::ListWindows)
            .await?
        {
            Reply::WindowList(windows) => Ok(windows),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Lists the detection definition names the daemon can resolve, together
    /// with the reason it could not enumerate them, if any.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request. A directory it could not read is not an error here: it comes
    /// back as the reply's `warning`.
    pub async fn list_detections(&self) -> Result<DetectionListReply, IpcError> {
        match self
            .request(&ClientMessage::ListDetections, PendingKind::ListDetections)
            .await?
        {
            Reply::DetectionList(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Opens a new window with the given command and returns the window id
    /// the daemon assigned to it, plus the detection definition's load
    /// warning when there is one (the window is open either way).
    ///
    /// `vendor` is the badge to record for the window; it is what a restored
    /// window carries over from the configuration it was saved in.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or refuses to
    /// open the window.
    pub async fn open_window(
        &self,
        command: Vec<String>,
        cwd: Option<String>,
        name: Option<String>,
        detection: Option<String>,
        vendor: Option<String>,
    ) -> Result<OpenWindowReply, IpcError> {
        let message = ClientMessage::OpenWindow {
            command,
            cwd,
            name,
            vendor,
            detection,
        };
        match self.request(&message, PendingKind::OpenWindow).await? {
            Reply::WindowOpened(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Lists the launch profiles the daemon's resolved profiles directory
    /// currently holds, together with one warning line per file it had to
    /// exclude.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request. A directory it could not enumerate is not an error here: it
    /// comes back as a `warnings` entry with an empty list.
    pub async fn list_profiles(&self) -> Result<ProfileListReply, IpcError> {
        match self
            .request(&ClientMessage::ListProfiles, PendingKind::ListProfiles)
            .await?
        {
            Reply::ProfileList(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Opens a new window from a launch profile, which supplies the command,
    /// the detection definition and the vendor badge. `cwd` and `name`
    /// override the profile's own defaults when present.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, does not know
    /// the profile, or refuses to open the window.
    pub async fn open_window_from_profile(
        &self,
        profile: String,
        cwd: Option<String>,
        name: Option<String>,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<OpenWindowReply, IpcError> {
        let message = ClientMessage::OpenWindowFromProfile {
            profile,
            cwd,
            name,
            model,
            effort,
        };
        // The daemon answers both open paths with `window_opened`, so this
        // waits on the same pending kind as `open_window`.
        match self.request(&message, PendingKind::OpenWindow).await? {
            Reply::WindowOpened(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Closes a window, terminating its process tree.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, the window
    /// does not exist, or the daemon otherwise rejects the request. This
    /// method always sends: it is the caller's job to gate the send on
    /// approver registration ([`crate::connection_pool::ConnectionPool::close_window`]
    /// is what does that).
    pub async fn close_window(&self, window_id: &str, force: Option<bool>) -> Result<(), IpcError> {
        let message = ClientMessage::CloseWindow {
            window_id: window_id.to_owned(),
            force,
        };
        match self.request(&message, PendingKind::CloseWindow).await? {
            Reply::Done => Ok(()),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Sends an interrupt signal to a window's child process.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, the window
    /// does not exist, or the daemon otherwise rejects the request. This
    /// method always sends: it is the caller's job to gate the send on
    /// approver registration ([`crate::connection_pool::ConnectionPool::interrupt_window`]
    /// is what does that).
    pub async fn interrupt_window(&self, window_id: &str) -> Result<(), IpcError> {
        let message = ClientMessage::InterruptWindow {
            window_id: window_id.to_owned(),
        };
        match self.request(&message, PendingKind::InterruptWindow).await? {
            Reply::Done => Ok(()),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Settles one pending approval request. Only a connection registered as
    /// an approver may call this meaningfully; an unregistered connection
    /// gets `invalid_state` back from the daemon.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or the request
    /// is no longer pending (`approval_not_pending`).
    pub async fn decide_approval(&self, request_id: &str, approved: bool) -> Result<(), IpcError> {
        let message = ClientMessage::ApprovalDecision {
            request_id: request_id.to_owned(),
            approved,
        };
        match self.request(&message, PendingKind::DecideApproval).await? {
            Reply::Done => Ok(()),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Renames a window and returns the name as the daemon stored it, which
    /// is what concurrent renames converge on.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, the window
    /// does not exist, or the name is invalid.
    pub async fn rename_window(&self, window_id: &str, name: &str) -> Result<String, IpcError> {
        let message = ClientMessage::RenameWindow {
            window_id: window_id.to_owned(),
            name: name.to_owned(),
        };
        match self.request(&message, PendingKind::RenameWindow).await? {
            Reply::WindowRenamed(stored) => Ok(stored),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Lists the previous boot's window configuration for the restore UI.
    /// Read-only: never rotates or writes the store.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_restore_candidates(&self) -> Result<RestoreCandidateListReply, IpcError> {
        match self
            .request(
                &ClientMessage::ListRestoreCandidates,
                PendingKind::ListRestoreCandidates,
            )
            .await?
        {
            Reply::RestoreCandidateList(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Asks the daemon what keeps it from opening windows. A daemon that
    /// predates the request has no problem to report, so a refusal as
    /// unsupported or as an unknown type answers with an empty list.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached.
    pub async fn get_daemon_health(&self) -> Result<DaemonHealthReply, IpcError> {
        // Settled first: the gate refuses an unsettled revision as unsupported,
        // which would read as an older daemon with no problem to report.
        self.ensure_protocol_revision().await;
        match self
            .request(
                &ClientMessage::GetDaemonHealth,
                PendingKind::GetDaemonHealth,
            )
            .await
        {
            Ok(Reply::DaemonHealth(reply)) => Ok(reply),
            Ok(_) => Err(IpcError::UnexpectedReply),
            Err(IpcError::Unsupported { .. }) => Ok(DaemonHealthReply::default()),
            Err(IpcError::Rejected { code, .. }) if code == ERROR_CODE_UNKNOWN_TYPE => {
                Ok(DaemonHealthReply::default())
            }
            Err(error) => Err(error),
        }
    }

    /// Reports a window's presentation layout to the daemon, verbatim: the
    /// daemon stores it unparsed and never interprets it.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, the window
    /// does not exist, or the layout exceeds the size the daemon accepts.
    pub async fn update_window_layout(
        &self,
        window_id: &str,
        layout_json: String,
    ) -> Result<UpdateWindowLayoutReply, IpcError> {
        let message = ClientMessage::UpdateWindowLayout {
            window_id: window_id.to_owned(),
            layout_json,
        };
        match self
            .request(&message, PendingKind::UpdateWindowLayout)
            .await?
        {
            Reply::WindowLayoutUpdated(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Posts an already validated body to the chat room, addressed to the
    /// windows its `@` mentions resolve to.
    ///
    /// The daemon may hold the post behind a human approval; the reply then
    /// arrives once that decision is made.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, chat is not
    /// available, or the post is rejected.
    pub async fn send_chat(
        &self,
        room_id: Option<String>,
        body: String,
    ) -> Result<ChatPostedReply, IpcError> {
        let message = ClientMessage::SendChat { room_id, body };
        match self.request(&message, PendingKind::SendChat).await? {
            Reply::ChatPosted(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Posts an already validated body to every participant in the chat room
    /// but the poster.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, chat is not
    /// available, or the post is rejected.
    pub async fn broadcast_chat(
        &self,
        room_id: Option<String>,
        body: String,
    ) -> Result<ChatPostedReply, IpcError> {
        let message = ClientMessage::BroadcastChat { room_id, body };
        match self.request(&message, PendingKind::SendChat).await? {
            Reply::ChatPosted(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Reads a page of the chat room's history, plus the room state that goes
    /// with it.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, chat is not
    /// available, or the arguments are rejected.
    pub async fn read_chat(
        &self,
        room_id: Option<String>,
        after_seq: Option<u64>,
        limit: Option<u32>,
    ) -> Result<ChatMessagesReply, IpcError> {
        let message = ClientMessage::ReadChat {
            room_id,
            after_seq,
            limit,
        };
        match self.request(&message, PendingKind::ReadChat).await? {
            Reply::ChatMessages(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Lists every chat room the daemon knows of. Approver-only on the
    /// daemon side.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn list_chat_rooms(&self) -> Result<ChatRoomListReply, IpcError> {
        match self
            .request(&ClientMessage::ListChatRooms, PendingKind::ListChatRooms)
            .await?
        {
            Reply::ChatRoomList(reply) => Ok(reply),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Lists the plans the daemon lets this connection see.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// plan ledger, or rejects the request.
    pub async fn list_plans(&self) -> Result<Vec<PlanSummary>, IpcError> {
        match self
            .request(&ClientMessage::ListPlans, PendingKind::PlanList)
            .await?
        {
            Reply::PlanList(plans) => Ok(plans),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Reads one plan in full.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn get_plan(&self, plan_id: String) -> Result<PlanInfo, IpcError> {
        self.request_plan_detail(&ClientMessage::GetPlan { plan_id })
            .await
    }

    /// Deletes one plan and returns the id the daemon deleted.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn delete_plan(&self, plan_id: String) -> Result<String, IpcError> {
        match self
            .request(
                &ClientMessage::DeletePlan { plan_id },
                PendingKind::PlanDeleted,
            )
            .await?
        {
            Reply::PlanDeleted(deleted) => Ok(deleted),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Sets the user's assignment override for one task.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_task_assignment(
        &self,
        plan_id: String,
        task_id: String,
        assignment: TaskAssignment,
    ) -> Result<PlanInfo, IpcError> {
        self.request_plan_detail(&ClientMessage::SetTaskAssignment {
            plan_id,
            task_id,
            assignment,
        })
        .await
    }

    /// Removes the user's assignment override from one task.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn clear_task_assignment(
        &self,
        plan_id: String,
        task_id: String,
    ) -> Result<PlanInfo, IpcError> {
        self.request_plan_detail(&ClientMessage::ClearTaskAssignment { plan_id, task_id })
            .await
    }

    /// Names a window as a plan's owner.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or rejects the
    /// request.
    pub async fn set_plan_owner(
        &self,
        plan_id: String,
        window_id: String,
    ) -> Result<PlanInfo, IpcError> {
        self.request_plan_detail(&ClientMessage::SetPlanOwner { plan_id, window_id })
            .await
    }

    /// Approves a plan awaiting review, provided `version` is still the
    /// plan's current version.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// plan review, or rejects the request (a stale `version` included).
    pub async fn approve_plan(&self, plan_id: String, version: u64) -> Result<PlanInfo, IpcError> {
        self.request_plan_detail(&ClientMessage::ApprovePlan { plan_id, version })
            .await
    }

    /// Clears the review marks of a plan, provided `version` is still the
    /// plan's current version.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// plan review, or rejects the request (a stale `version` included).
    pub async fn mark_plan_seen(
        &self,
        plan_id: String,
        version: u64,
    ) -> Result<PlanInfo, IpcError> {
        self.request_plan_detail(&ClientMessage::MarkPlanSeen { plan_id, version })
            .await
    }

    /// Reads the role-to-assignment table.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// plan ledger, or rejects the request.
    pub async fn list_roles(&self) -> Result<RoleListReply, IpcError> {
        self.request_role_list(&ClientMessage::ListRoles).await
    }

    /// Replaces the whole role-to-assignment table, default role included,
    /// and returns it as stored.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// plan review while `default_role` is set, or rejects the table.
    pub async fn save_roles(
        &self,
        roles: Vec<RoleEntry>,
        default_role: Option<String>,
    ) -> Result<RoleListReply, IpcError> {
        self.request_role_list(&ClientMessage::SaveRoles {
            roles,
            default_role,
        })
        .await
    }

    /// Asks for the models `profile`'s `model` launch option can take. The
    /// reply only acknowledges the request; the list arrives later as a push
    /// carrying the same `request_id`.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates model
    /// aliases, or rejects the request.
    pub async fn list_model_choices(
        &self,
        profile: String,
        request_id: u64,
    ) -> Result<ModelChoicesRequestedReply, IpcError> {
        match self
            .request(
                &ClientMessage::ListModelChoices {
                    profile,
                    request_id,
                },
                PendingKind::ListModelChoices,
            )
            .await?
        {
            Reply::ModelChoicesRequested(accepted) => Ok(accepted),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Replaces the values of `profile`'s model aliases and returns them as
    /// stored.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates model
    /// aliases, or rejects the aliases.
    pub async fn save_model_aliases(
        &self,
        profile: String,
        aliases: Vec<LaunchOptionAlias>,
    ) -> Result<ModelAliasesSavedReply, IpcError> {
        match self
            .request(
                &ClientMessage::SaveModelAliases { profile, aliases },
                PendingKind::SaveModelAliases,
            )
            .await?
        {
            Reply::ModelAliasesSaved(saved) => Ok(saved),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Asks which profiles describe how to register the MCP bridge with their
    /// CLI. Each row's state arrives later as a push carrying the same
    /// `request_id`.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates MCP
    /// registration, or rejects the request.
    pub async fn list_mcp_registrations(
        &self,
        request_id: u64,
    ) -> Result<McpRegistrationsListedReply, IpcError> {
        match self
            .request(
                &ClientMessage::ListMcpRegistrations { request_id },
                PendingKind::ListMcpRegistrations,
            )
            .await?
        {
            Reply::McpRegistrationsListed(listed) => Ok(listed),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Asks the daemon to run `action` on `profile`'s MCP registration. The
    /// reply only acknowledges the request; the outcome arrives later as a
    /// push carrying the same `request_id`.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates MCP
    /// registration, or rejects the request.
    pub async fn run_mcp_registration(
        &self,
        profile: String,
        action: McpRegistrationAction,
        request_id: u64,
    ) -> Result<McpRegistrationRequestedReply, IpcError> {
        match self
            .request(
                &ClientMessage::RunMcpRegistration {
                    profile,
                    action,
                    request_id,
                },
                PendingKind::RunMcpRegistration,
            )
            .await?
        {
            Reply::McpRegistrationRequested(accepted) => Ok(accepted),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Turns `profile`'s launching on or off and returns the setting as the
    /// daemon stored it.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// setting, or rejects the request.
    pub async fn set_profile_enabled(
        &self,
        profile: String,
        enabled: bool,
    ) -> Result<ProfileEnabledSetReply, IpcError> {
        match self
            .request(
                &ClientMessage::SetProfileEnabled { profile, enabled },
                PendingKind::SetProfileEnabled,
            )
            .await?
        {
            Reply::ProfileEnabledSet(stored) => Ok(stored),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Reads every value a profile uses, with the layer each comes from.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates
    /// profile details, or rejects the request.
    pub async fn get_profile_detail(
        &self,
        profile: String,
    ) -> Result<ProfileDetailReply, IpcError> {
        match self
            .request(
                &ClientMessage::GetProfileDetail { profile },
                PendingKind::GetProfileDetail,
            )
            .await?
        {
            Reply::ProfileDetail(detail) => Ok(detail),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Sets or puts back a profile's argv on the daemon's host.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates
    /// profile details, or rejects the request.
    pub async fn set_profile_command(
        &self,
        profile: String,
        argv: Option<Vec<String>>,
    ) -> Result<ProfileCommandSetReply, IpcError> {
        match self
            .request(
                &ClientMessage::SetProfileCommand { profile, argv },
                PendingKind::SetProfileCommand,
            )
            .await?
        {
            Reply::ProfileCommandSet(set) => Ok(set),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Reads the daemon's current approval settings.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates
    /// approval settings, or this connection is not a registered approver.
    pub async fn get_approval_settings(&self) -> Result<ApprovalSettingsReply, IpcError> {
        match self
            .request(
                &ClientMessage::GetApprovalSettings {},
                PendingKind::GetApprovalSettings,
            )
            .await?
        {
            Reply::ApprovalSettings(settings) => Ok(settings),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Turns automatic approval of one destructive tool on or off and returns
    /// the settings after the change.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates
    /// approval settings, or rejects the request (an unrecognized tool name
    /// included).
    pub async fn set_auto_approve(
        &self,
        tool: String,
        auto_approve: bool,
    ) -> Result<ApprovalSettingsReply, IpcError> {
        match self
            .request(
                &ClientMessage::SetAutoApprove { tool, auto_approve },
                PendingKind::SetAutoApprove,
            )
            .await?
        {
            Reply::AutoApproveSet(settings) => Ok(settings),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Turns the plan review gate on or off and returns the settings after
    /// the change.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates
    /// approval settings, or rejects the request.
    pub async fn set_plan_approval_required(
        &self,
        required: bool,
    ) -> Result<ApprovalSettingsReply, IpcError> {
        match self
            .request(
                &ClientMessage::SetPlanApprovalRequired { required },
                PendingKind::SetPlanApprovalRequired,
            )
            .await?
        {
            Reply::PlanApprovalRequiredSet(settings) => Ok(settings),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// One round trip for a request the daemon answers with `plan_detail`.
    async fn request_plan_detail(&self, message: &ClientMessage) -> Result<PlanInfo, IpcError> {
        match self.request(message, PendingKind::PlanDetail).await? {
            Reply::PlanDetail(plan) => Ok(plan),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// One round trip for a request the daemon answers with `role_list`.
    async fn request_role_list(&self, message: &ClientMessage) -> Result<RoleListReply, IpcError> {
        match self.request(message, PendingKind::RoleList).await? {
            Reply::RoleList(table) => Ok(table),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Sends an already validated message that needs no reply to the window
    /// this connection is attached to. Routing is the pool's job: the daemon
    /// applies it to the connection's own attachment.
    ///
    /// # Errors
    /// Returns [`IpcError`] when no connection is established or the write
    /// fails.
    pub async fn send_attached(&self, message: &ClientMessage) -> Result<(), IpcError> {
        // Remembered even when the send fails: a reconnect restores what the
        // WebView asked for, and the WebView does not ask again.
        if let Some(visible) = visibility_set_by(message) {
            *self.last_visible.lock().await = Some(visible);
        }
        self.send(message).await
    }

    /// Reads the text selected in the window this connection is attached to.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the round trip fails.
    pub async fn copy_selection(&self) -> Result<Option<String>, IpcError> {
        match self
            .request(&ClientMessage::CopySelection, PendingKind::CopySelection)
            .await?
        {
            Reply::SelectionText(text) => Ok(text),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Sends an already validated resize request for the window this
    /// connection is attached to.
    ///
    /// # Errors
    /// Returns [`IpcError`] when no connection is established or the write
    /// fails.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), IpcError> {
        *self.last_size.lock().await = Some(TerminalSize { cols, rows });
        self.send(&ClientMessage::Resize { cols, rows }).await
    }

    /// Releases the connection on purpose: the reader task is cancelled, and
    /// the bumped generation keeps it from reconnecting or reporting a
    /// disconnect for a window that was retired deliberately.
    pub async fn disconnect(&self) {
        self.generation.fetch_add(1, Ordering::SeqCst);
        {
            let mut slot = self.writer.lock().await;
            slot.clear();
            if let Some(cancel) = lock_cancel(&self.cancel_reader).take() {
                let _ = cancel.send(());
            }
        }
        *self.window_id.lock().await = None;
        self.fail_pending(IpcError::NotConnected).await;
    }

    /// Whether a connection is established right now.
    ///
    /// A predicate, not a step: unlike [`IpcClient::connect`] it neither
    /// establishes a connection nor starts a daemon, so a supervisor can
    /// observe an outage without changing it.
    pub(crate) async fn is_connected(&self) -> bool {
        self.writer.lock().await.is_connected()
    }

    /// The window this client is currently attached to, or `None` for a
    /// control connection. A read-only accessor over the private field, kept
    /// for [`crate::message_dispatch::read_loop`], which needs the value to
    /// route an incoming message but must not otherwise reach into the
    /// connection's internal state.
    pub(crate) async fn window_id(&self) -> Option<String> {
        self.window_id.lock().await.clone()
    }

    /// The generation whose `register_approver` round trip is still owed, or
    /// `None` when there is nothing to send (role `Window`, or the current
    /// generation already registered).
    ///
    /// Takes no host on purpose: this is the pure half of
    /// [`Self::ensure_approver_registered`].
    pub(crate) fn approver_registration_due(&self) -> Option<u64> {
        if self.role != Role::Control {
            return None;
        }
        let generation = self.generation.load(Ordering::SeqCst);
        if self.registered_generation.load(Ordering::SeqCst) == generation
            || self.refused_generation.load(Ordering::SeqCst) == generation
        {
            None
        } else {
            Some(generation)
        }
    }

    /// Records that `generation`'s `register_approver` round trip completed.
    pub(crate) fn mark_approver_registered(&self, generation: u64) {
        self.registered_generation
            .store(generation, Ordering::SeqCst);
    }

    /// Whether the current generation's registration has actually completed
    /// -- distinct from [`Self::approver_registration_due`], which also
    /// returns "nothing to send" while a refusal is latched even though
    /// registration never succeeded. Callers that must not act
    /// without a registered approver (destructive-operation gating) use this,
    /// not the absence of `due`.
    pub(crate) fn is_approver_registered(&self) -> bool {
        self.registered_generation.load(Ordering::SeqCst) == self.generation.load(Ordering::SeqCst)
    }

    /// Latches `generation` as refused. Returns `true` only the first time a
    /// given generation is latched, which is what makes the caller's
    /// `EVENT_APPROVAL_REGISTRATION_REFUSED` emission happen once per
    /// generation rather than once per concurrent caller.
    ///
    /// `swap` rather than `compare_exchange` against the sentinel: the
    /// sentinel is only ever the *initial* value, so a `compare_exchange`
    /// against it could never latch a second (or later) generation's refusal.
    pub(crate) fn mark_approver_refused(&self, generation: u64) -> bool {
        self.refused_generation.swap(generation, Ordering::SeqCst) != generation
    }

    /// One `register_approver` round trip for `generation`, and the only
    /// place that sends it: sending, recording a completed registration,
    /// latching an `auth_failed` refusal and emitting the refusal event are
    /// one unit, so the push and the pull caller cannot drift apart.
    ///
    /// # Errors
    /// Returns [`IpcError::Rejected`] with `auth_failed` when the daemon
    /// refused this connection as an approver (having latched the generation,
    /// and emitted `EVENT_APPROVAL_REGISTRATION_REFUSED` if this was the
    /// first refusal of that generation), [`IpcError::UnexpectedReply`] for a
    /// reply of any other kind, and the transport's own error otherwise. Only
    /// the accepted registration records the generation, so every other
    /// outcome leaves it owed.
    async fn register_round_trip(
        &self,
        app: &impl GuiHost,
        generation: u64,
    ) -> Result<Vec<ApprovalRequest>, IpcError> {
        let result = self
            .request(
                &ClientMessage::RegisterApprover,
                PendingKind::RegisterApprover,
            )
            .await;
        match result {
            Ok(Reply::ApproverRegistered(pending)) => {
                self.mark_approver_registered(generation);
                Ok(pending)
            }
            Ok(_) => Err(IpcError::UnexpectedReply),
            Err(IpcError::Rejected {
                code,
                message,
                rejected_argument,
            }) if code == "auth_failed" => {
                if self.mark_approver_refused(generation) {
                    app.emit_event(
                        EVENT_APPROVAL_REGISTRATION_REFUSED,
                        ApprovalRegistrationRefusedPayload {},
                    );
                }
                Err(IpcError::Rejected {
                    code,
                    message,
                    rejected_argument,
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Registers this connection as an approver when [`Self::approver_registration_due`]
    /// says it is owed, and forwards the requests pending at that moment to
    /// the WebView as a reset followed by one `session://approval-requested`
    /// event per item -- the same shape a fresh registration would produce,
    /// so a reconnect's restored queue looks no different.
    ///
    /// Idempotent per generation: a round trip refused with anything other
    /// than `auth_failed` (a timeout, a dropped connection) leaves the
    /// generation neither registered nor refused, so the next call (from any
    /// of this client's three call sites) retries it. An `auth_failed`
    /// refusal instead latches the generation: retrying it on
    /// every call site's next tick would live-lock with the window-list
    /// poll's own reconnect-on-failure.
    pub(crate) async fn ensure_approver_registered(&self, app: &impl GuiHost) {
        let Some(generation) = self.approver_registration_due() else {
            return;
        };
        let Ok(pending) = self.register_round_trip(app, generation).await else {
            return;
        };
        app.emit_event(
            EVENT_APPROVAL_RESET,
            ApprovalResetPayload {
                requests: pending.clone(),
            },
        );
        for request in pending {
            app.emit_event(
                EVENT_APPROVAL_REQUESTED,
                ApprovalRequestedPayload { request },
            );
        }
    }

    /// Pulls the requests pending for this approver, on the WebView's own
    /// initiative rather than on the connection's: the round trip is the same
    /// registration, so a generation that was never registered is registered
    /// here, and one that already is answers with its current pending list.
    ///
    /// Unlike [`Self::ensure_approver_registered`] this emits nothing and
    /// ignores [`Self::approver_registration_due`]: the caller applies the
    /// returned list itself, so an event here would deliver it twice, and a
    /// registered generation must still answer the pull with the list.
    ///
    /// Only the control connection registers as an approver, so this is never
    /// reached on a `Window`-role client; an empty list means "nothing is
    /// pending", never "wrong role".
    ///
    /// # Errors
    /// Returns whatever [`Self::register_round_trip`] returns, unchanged: the
    /// caller maps the `auth_failed` refusal onto its own outcome type.
    pub(crate) async fn sync_approvals(
        &self,
        app: &impl GuiHost,
    ) -> Result<Vec<ApprovalRequest>, IpcError> {
        debug_assert!(self.role == Role::Control);
        self.register_round_trip(app, self.generation.load(Ordering::SeqCst))
            .await
    }

    /// The generation whose wire-revision probe is still owed, or `None`
    /// when this connection has already settled one for its current
    /// generation.
    ///
    /// Unlike [`Self::approver_registration_due`] this applies to every
    /// role: a window connection's revision is recorded the same way, even
    /// though only [`Self::send`] ever weighs it.
    fn protocol_revision_settle_due(&self) -> Option<u64> {
        let generation = self.generation.load(Ordering::SeqCst);
        let (_, settled_generation) = self.settled.load();
        if settled_generation == generation {
            None
        } else {
            Some(generation)
        }
    }

    /// The wire revision this connection has settled on for its *current*
    /// generation, `None` when it has not. Read by the
    /// window-list poll to push information to the WebView; never a gate --
    /// `wire::Writer::send` is the only body that weighs
    /// [`crate::pool_outcomes::protocol_revision_gate`].
    pub(crate) fn protocol_revision(&self) -> Option<u32> {
        let (revision, settled_generation) = self.settled.load();
        if settled_generation == self.generation.load(Ordering::SeqCst) {
            revision
        } else {
            None
        }
    }

    /// Settles this connection's wire revision when
    /// [`Self::protocol_revision_settle_due`] says it is owed, doing nothing
    /// otherwise.
    ///
    /// Idempotent per generation, the same shape as
    /// [`Self::ensure_approver_registered`]: a round trip that fails
    /// outright (a timeout, a dropped connection) leaves the generation
    /// unsettled for the next caller to retry, while either a revision
    /// answer or an `unknown_type` refusal settles it for good, because both
    /// are themselves an answer to "which wire does this peer speak" (a
    /// daemon built before `get_protocol_revision` existed answers with the
    /// latter, and is credited with [`PROTOCOL_REVISION_BASELINE`]).
    ///
    /// A failure here is never surfaced to the caller that triggered it:
    /// the probe is this connection's own housekeeping, not
    /// the request that happened to prompt it, the same reason
    /// [`Self::ensure_approver_registered`] swallows its own failures.
    ///
    /// Takes no guard of its own and must never be called while one of
    /// `writer`, `request_lock` or `pending` is held: it runs a round trip
    /// of its own through [`Self::request`], which locks all three, and
    /// holding any of them here would deadlock that round trip against
    /// itself.
    pub(crate) async fn ensure_protocol_revision(&self) {
        let Some(generation) = self.protocol_revision_settle_due() else {
            return;
        };
        let result = self
            .request(
                &ClientMessage::GetProtocolRevision,
                PendingKind::GetProtocolRevision,
            )
            .await;
        let revision = match result {
            Ok(Reply::ProtocolRevision(revision)) => revision,
            Err(IpcError::Rejected { code, .. }) if code == ERROR_CODE_UNKNOWN_TYPE => {
                PROTOCOL_REVISION_BASELINE
            }
            _ => return,
        };
        self.settled.store(revision, generation);
    }

    async fn connect_inner(
        self: &Arc<Self>,
        app: &impl GuiHost,
        mode: ConnectMode,
    ) -> Result<(), IpcError> {
        let mut slot = self.writer.lock().await;
        if slot.is_connected() {
            return Ok(());
        }
        let endpoint = app.endpoint()?;
        let reached = match mode {
            // One attempt decides whether a daemon has to be
            // started, so this path does not spend the retry budget first.
            ConnectMode::Once | ConnectMode::Spawn => connect_once(&endpoint).await,
            ConnectMode::Retry => connect_retrying(&endpoint).await,
        };
        let connection = match reached {
            Some(connection) => connection,
            None if mode == ConnectMode::Spawn => {
                app.spawn_daemon()?;
                connect_after_spawn(&endpoint).await?
            }
            None => return Err(IpcError::NotConnected),
        };
        self.install(app, connection, &mut slot);
        // Dropped explicitly before the requests below: `send` also locks
        // `writer`, and holding this guard across either would deadlock.
        drop(slot);
        self.ensure_approver_registered(app).await;
        self.ensure_protocol_revision().await;
        Ok(())
    }

    fn install(
        self: &Arc<Self>,
        app: &impl GuiHost,
        connection: Connection,
        slot: &mut wire::Writer<WriteHalf<Connection>>,
    ) {
        let (reader, writer) = tokio::io::split(connection);
        slot.install(writer);
        let (cancel_tx, cancel_rx) = oneshot::channel();
        *lock_cancel(&self.cancel_reader) = Some(cancel_tx);
        let generation = self.generation.fetch_add(1, Ordering::SeqCst) + 1;
        app.spawn_task(read_loop(
            reader,
            app.clone(),
            Arc::clone(self),
            generation,
            cancel_rx,
        ));
    }

    /// Sends `message` and waits for the reply the daemon answers it with.
    ///
    /// The request lock is held for the whole round trip, which is what makes
    /// the single [`Pending`] slot sufficient (see its documentation).
    async fn request(&self, message: &ClientMessage, kind: PendingKind) -> Result<Reply, IpcError> {
        let _in_flight = self.request_lock.lock().await;
        let (sender, receiver) = oneshot::channel();
        *self.pending.lock().await = Some(Pending { kind, sender });
        if let Err(error) = self.send(message).await {
            *self.pending.lock().await = None;
            return Err(error);
        }
        match tokio::time::timeout(ATTACH_TIMEOUT, receiver).await {
            Ok(Ok(result)) => result,
            // The sender was dropped, which only happens when the connection
            // went away before replying.
            Ok(Err(_)) => Err(IpcError::NotConnected),
            Err(_) => {
                *self.pending.lock().await = None;
                Err(kind.timeout_error())
            }
        }
    }

    /// Attaches at `size` (`None` keeps the window's own size), remembering a
    /// size that was sent for the next reconnect.
    ///
    /// `attach_view` is gated on the daemon's wire revision, so a revision the
    /// connection has not settled yet is settled first; an older daemon is
    /// refused with [`IpcError::Unsupported`] without a frame being sent.
    async fn request_attach(
        &self,
        window_id: &str,
        size: Option<TerminalSize>,
    ) -> Result<AttachResponse, IpcError> {
        self.ensure_protocol_revision().await;
        // Recorded before the request goes out: the daemon may stream output
        // for the window between its `attached` reply and this task waking
        // up, and the reader needs the window id to route it.
        *self.window_id.lock().await = Some(window_id.to_owned());
        let message = ClientMessage::AttachView {
            window_id: window_id.to_owned(),
            size,
        };
        match self.request(&message, PendingKind::Attach).await {
            Ok(Reply::Attached(response)) => {
                if size.is_some() {
                    *self.last_size.lock().await = size;
                }
                Ok(response)
            }
            Ok(_) => {
                *self.window_id.lock().await = None;
                Err(IpcError::UnexpectedReply)
            }
            Err(error) => {
                *self.window_id.lock().await = None;
                Err(error)
            }
        }
    }

    async fn send(&self, message: &ClientMessage) -> Result<(), IpcError> {
        self.writer
            .lock()
            .await
            .send(message, &self.settled, &self.generation)
            .await
    }

    /// Kind of the outstanding request, if any.
    pub(crate) async fn pending_kind(&self) -> Option<PendingKind> {
        self.pending
            .lock()
            .await
            .as_ref()
            .map(|pending| pending.kind)
    }

    /// Answers the outstanding request, but only when it is the one `kind`
    /// belongs to: a message that answers nothing in flight is dropped rather
    /// than mis-attributed.
    pub(crate) async fn complete_pending(&self, kind: PendingKind, reply: Reply) {
        let taken = self
            .pending
            .lock()
            .await
            .take_if(|pending| pending.kind == kind);
        if let Some(pending) = taken {
            let _ = pending.sender.send(Ok(reply));
        }
    }

    /// Fails the outstanding request, whatever it is.
    pub(crate) async fn fail_pending(&self, error: IpcError) {
        if let Some(pending) = self.pending.lock().await.take() {
            let _ = pending.sender.send(Err(error));
        }
    }

    /// Reports the end of the reader task owning `generation`.
    ///
    /// A reader task that is no longer the current connection stays silent:
    /// its slot has already been taken over by a newer connection, so it must
    /// neither clear that connection nor emit events for it. The generation
    /// check and the slot reset share one lock because `install` bumps the
    /// generation while holding it.
    pub(crate) async fn on_disconnect(
        self: &Arc<Self>,
        app: &impl GuiHost,
        superseded: bool,
        generation: u64,
    ) {
        let wid = self.window_id.lock().await.clone();
        {
            let mut slot = self.writer.lock().await;
            if self.generation.load(Ordering::SeqCst) != generation {
                return;
            }
            slot.clear();
        }
        *self.window_id.lock().await = None;

        self.fail_pending(IpcError::NotConnected).await;

        // A superseded session must not fight the new GUI for the
        // daemon, so it never reconnects.
        if superseded {
            if let Some(window_id) = wid {
                app.emit_event(
                    EVENT_SUPERSEDED,
                    SupersededPayload {
                        window_id,
                        reason: ERROR_CODE_SUPERSEDED.to_owned(),
                    },
                );
            }
            return;
        }

        for _ in 0..RECONNECT_ATTEMPT_LIMIT {
            if self.try_reconnect(app, wid.as_deref()).await {
                return;
            }
        }
        if let Some(window_id) = wid {
            app.emit_event(
                EVENT_DISCONNECTED,
                DisconnectedPayload {
                    window_id,
                    reason: "reconnect_failed".to_owned(),
                },
            );
        }
    }

    /// Reconnects to an existing daemon and re-attaches to the window this
    /// connection was serving. The daemon is never spawned here: an automatic
    /// reconnect only recovers from a lost connection, not from a daemon that
    /// is gone.
    async fn try_reconnect(self: &Arc<Self>, app: &impl GuiHost, window_id: Option<&str>) -> bool {
        let Ok(endpoint) = app.endpoint() else {
            return false;
        };
        // Same retry form as the initial window connection: N
        // connections dropped at once would otherwise fight over the single
        // pending endpoint instance.
        let Some(connection) = connect_retrying(&endpoint).await else {
            return false;
        };
        {
            let mut slot = self.writer.lock().await;
            self.install(app, connection, &mut slot);
        }
        self.ensure_approver_registered(app).await;
        self.ensure_protocol_revision().await;

        // A control connection has no attachment to restore.
        let Some(window_id) = window_id else {
            return true;
        };

        let size = *self.last_size.lock().await;
        let restore_visibility = visibility_restore(*self.last_visible.lock().await);
        match self.request_attach(window_id, size).await {
            // The daemon follows the answer with a frame of the whole screen,
            // which replaces whatever the WebView drew before.
            Ok(response) => {
                if let Some(message) = restore_visibility {
                    // A failed write is the next disconnect's business.
                    let _ = self.send(&message).await;
                }
                if response.child_exited {
                    // Re-attaching to an exited child reports the
                    // termination through the same event as a live `exit`.
                    app.emit_event(
                        EVENT_EXIT,
                        ExitPayload {
                            window_id: window_id.to_owned(),
                            exit_code: response.exit_code.unwrap_or(-1),
                        },
                    );
                }
                true
            }
            Err(_) => {
                // Retire the connection so its reader task stays silent and the
                // next explicit attach starts from a clean state.
                let mut slot = self.writer.lock().await;
                self.generation.fetch_add(1, Ordering::SeqCst);
                slot.clear();
                false
            }
        }
    }
}

/// The connection's write half and the wire revision it is weighed against.
/// Bytes reach the connection only through [`Writer::send`], which weighs the
/// frame against the settled revision while the caller holds the writer's
/// guard, so a connection cannot be replaced between the judgement and the
/// write.
mod wire {
    use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};

    use tokio::io::{AsyncWrite, AsyncWriteExt};
    use yagura_protocol::{
        ClientMessage, PROTOCOL_REVISION_BASELINE, encode_client_message, frame_encode,
        required_protocol_revision,
    };

    use super::{IpcError, client_message_wire_tag};
    use crate::pool_outcomes::{ProtocolRevisionGate, protocol_revision_gate};

    /// The wire revision the peer answered `get_protocol_revision` with (the
    /// baseline when it refused the request as unknown), and the generation
    /// it was settled for. Atomics rather than a locked value, so settling
    /// never needs a guard the round trip it takes also needs.
    #[derive(Debug)]
    pub(super) struct SettledRevision {
        revision: AtomicU32,
        /// The sentinel `u64::MAX` means no generation has settled one yet.
        generation: AtomicU64,
    }

    impl SettledRevision {
        pub(super) fn new() -> Self {
            Self {
                revision: AtomicU32::new(PROTOCOL_REVISION_BASELINE),
                generation: AtomicU64::new(u64::MAX),
            }
        }

        /// The settled revision and its generation. The revision is `None`
        /// while no generation has settled one, which keeps a fresh client
        /// from reading as settled at generation `0`.
        pub(super) fn load(&self) -> (Option<u32>, u64) {
            let generation = self.generation.load(Ordering::SeqCst);
            let revision = (generation != u64::MAX).then(|| self.revision.load(Ordering::SeqCst));
            (revision, generation)
        }

        pub(super) fn store(&self, revision: u32, generation: u64) {
            self.revision.store(revision, Ordering::SeqCst);
            self.generation.store(generation, Ordering::SeqCst);
        }
    }

    /// The write half of the current connection, `None` while disconnected.
    #[derive(Debug)]
    pub(super) struct Writer<W>(Option<W>);

    impl<W: AsyncWrite + Unpin> Writer<W> {
        pub(super) fn empty() -> Self {
            Self(None)
        }

        pub(super) fn install(&mut self, writer: W) {
            self.0 = Some(writer);
        }

        pub(super) fn clear(&mut self) {
            self.0 = None;
        }

        pub(super) fn is_connected(&self) -> bool {
            self.0.is_some()
        }

        /// Writes `message` as one frame, unless the revision `settled` holds
        /// for the connection's current `generation` cannot carry it. A failed
        /// write drops the connection.
        pub(super) async fn send(
            &mut self,
            message: &ClientMessage,
            settled: &SettledRevision,
            generation: &AtomicU64,
        ) -> Result<(), IpcError> {
            let writer = self.0.as_mut().ok_or(IpcError::NotConnected)?;
            let body = encode_client_message(message);
            let (settled_revision, settled_generation) = settled.load();
            if protocol_revision_gate(
                settled_revision,
                settled_generation,
                generation.load(Ordering::SeqCst),
                required_protocol_revision(message),
            ) == ProtocolRevisionGate::Refused
            {
                return Err(IpcError::Unsupported {
                    request_type: client_message_wire_tag(&body),
                });
            }
            if writer.write_all(&frame_encode(&body)).await.is_err() {
                self.0 = None;
                return Err(IpcError::Write);
            }
            Ok(())
        }
    }
}

/// The visibility a message sets, if it is one that sets it.
fn visibility_set_by(message: &ClientMessage) -> Option<bool> {
    match message {
        ClientMessage::SetVisible { visible } => Some(*visible),
        _ => None,
    }
}

/// The message that puts a re-attached window back to the visibility it last
/// had, or `None` when none was ever sent and the daemon's default stands.
fn visibility_restore(last_visible: Option<bool>) -> Option<ClientMessage> {
    last_visible.map(|visible| ClientMessage::SetVisible { visible })
}

/// The wire `type` tag of an already-encoded client message body, read back
/// from the JSON [`encode_client_message`] already produced rather than a
/// second `ClientMessage`-to-tag mapping kept in step with it by hand.
///
/// Never panics: a body this crate did not itself produce (which
/// `encode_client_message` never yields) reads back as the empty string
/// rather than a tag invented here.
fn client_message_wire_tag(body: &[u8]) -> String {
    let Ok(value) = serde_json::from_slice::<serde_json::Value>(body) else {
        return String::new();
    };
    value
        .get("type")
        .and_then(serde_json::Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// Locks the cancellation slot, recovering from poisoning: the slot holds one
/// sender and a panicking holder cannot have left it inconsistent.
fn lock_cancel(
    slot: &std::sync::Mutex<Option<oneshot::Sender<()>>>,
) -> std::sync::MutexGuard<'_, Option<oneshot::Sender<()>>> {
    slot.lock().unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Non-owning: the gui is a client, so it never creates the Unix stage-3
/// runtime directory; only the daemon's `main.rs` does.
///
/// `resolution` never falls back to the default instance on failure: an
/// endpoint built from `Err` would connect to whichever daemon holds the
/// default instance's endpoint, silently ignoring the reason start-up
/// failed.
///
/// # Errors
/// Returns [`IpcError::NotConnected`] when `resolution` is `Err`, and
/// [`IpcError::EndpointName`] when the current user's identifier cannot be
/// resolved.
pub fn current_endpoint_with(resolution: &InstanceResolution) -> Result<String, IpcError> {
    let namespace = resolution.as_ref().map_err(|_| IpcError::NotConnected)?;
    Ok(default_endpoint(
        &PlatformIpc::default().current_user_id()?,
        namespace,
    ))
}

async fn connect_once(endpoint: &str) -> Option<Connection> {
    PlatformIpc::default().connect(endpoint).await.ok()
}

/// Connects to a running daemon, retrying a refused endpoint up to `limit`
/// times. The first attempt precedes any wait, so an endpoint that is free
/// costs no delay (waiting first would cost one interval per window).
pub(crate) async fn connect_retrying_with_limit(endpoint: &str, limit: u32) -> Option<Connection> {
    for attempt in 0..limit {
        if attempt > 0 {
            tokio::time::sleep(CONNECT_RETRY_INTERVAL).await;
        }
        if let Some(connection) = connect_once(endpoint).await {
            return Some(connection);
        }
    }
    None
}

/// [`connect_retrying_with_limit`] with the ordinary [`CONNECT_RETRY_LIMIT`]
/// budget; [`crate::daemon_launch::connect_after_spawn`] is the one caller
/// that needs a different budget.
pub(crate) async fn connect_retrying(endpoint: &str) -> Option<Connection> {
    connect_retrying_with_limit(endpoint, CONNECT_RETRY_LIMIT).await
}

#[cfg(test)]
mod host_tests;

#[cfg(test)]
mod tests {
    use super::*;

    /// A message the baseline revision cannot carry.
    fn frame_gated_message() -> ClientMessage {
        ClientMessage::SetVisible { visible: true }
    }

    /// Sends `message` through a writer over an in-memory pipe and returns
    /// the outcome with every byte that reached the other end.
    async fn send_and_collect(
        message: &ClientMessage,
        settled: &wire::SettledRevision,
    ) -> (Result<(), IpcError>, Vec<u8>) {
        use tokio::io::AsyncReadExt;
        let (near, mut far) = tokio::io::duplex(64 * 1024);
        let mut writer = wire::Writer::empty();
        writer.install(near);
        let outcome = writer.send(message, settled, &AtomicU64::new(0)).await;
        drop(writer);
        let mut written = Vec::new();
        far.read_to_end(&mut written)
            .await
            .expect("the far end reads until the writer is gone");
        (outcome, written)
    }

    #[tokio::test]
    async fn a_frame_the_settled_revision_cannot_carry_is_refused_before_any_byte_is_written() {
        let (outcome, written) =
            send_and_collect(&frame_gated_message(), &wire::SettledRevision::new()).await;
        assert!(
            matches!(outcome, Err(IpcError::Unsupported { .. })),
            "got {outcome:?}"
        );
        assert!(written.is_empty(), "wrote {written:?}");
    }

    #[tokio::test]
    async fn a_frame_is_written_once_the_current_generation_settled_a_revision_that_carries_it() {
        let settled = wire::SettledRevision::new();
        settled.store(yagura_protocol::PROTOCOL_REVISION, 0);
        let message = frame_gated_message();
        let (outcome, written) = send_and_collect(&message, &settled).await;
        assert!(outcome.is_ok(), "got {outcome:?}");
        assert_eq!(
            written,
            yagura_protocol::frame_encode(&yagura_protocol::encode_client_message(&message))
        );
    }

    #[tokio::test]
    async fn a_revision_settled_for_another_generation_does_not_carry_a_frame() {
        let settled = wire::SettledRevision::new();
        settled.store(yagura_protocol::PROTOCOL_REVISION, 1);
        let (outcome, written) = send_and_collect(&frame_gated_message(), &settled).await;
        assert!(
            matches!(outcome, Err(IpcError::Unsupported { .. })),
            "got {outcome:?}"
        );
        assert!(written.is_empty(), "wrote {written:?}");
    }

    #[tokio::test]
    async fn a_visibility_and_size_set_while_disconnected_are_restored_on_reconnect() {
        let client = IpcClient::new_window();
        assert!(
            client
                .send_attached(&ClientMessage::SetVisible { visible: false })
                .await
                .is_err()
        );
        assert!(client.resize(100, 30).await.is_err());
        assert_eq!(
            visibility_restore(*client.last_visible.lock().await),
            Some(ClientMessage::SetVisible { visible: false })
        );
        assert_eq!(
            *client.last_size.lock().await,
            Some(TerminalSize {
                cols: 100,
                rows: 30
            })
        );
    }

    #[test]
    fn only_a_set_visible_message_sets_a_visibility() {
        assert_eq!(
            visibility_set_by(&ClientMessage::SetVisible { visible: false }),
            Some(false)
        );
        assert_eq!(
            visibility_set_by(&ClientMessage::SetVisible { visible: true }),
            Some(true)
        );
        assert_eq!(visibility_set_by(&ClientMessage::FrameAck { seq: 1 }), None);
    }

    #[test]
    fn a_reconnect_restores_the_last_visibility_that_was_sent() {
        assert_eq!(
            visibility_restore(Some(false)),
            Some(ClientMessage::SetVisible { visible: false })
        );
        assert_eq!(
            visibility_restore(Some(true)),
            Some(ClientMessage::SetVisible { visible: true })
        );
    }

    #[test]
    fn a_reconnect_sends_no_visibility_when_none_was_ever_sent() {
        assert_eq!(visibility_restore(None), None);
    }

    #[test]
    fn approver_registration_due_is_owed_for_the_initial_generation_of_a_new_client() {
        let client = IpcClient::new();
        assert_eq!(client.approver_registration_due(), Some(0));
    }

    #[test]
    fn approver_registration_due_clears_once_the_current_generation_is_marked_registered() {
        let client = IpcClient::new();
        client.mark_approver_registered(0);
        assert_eq!(client.approver_registration_due(), None);
    }

    #[test]
    fn approver_registration_due_reopens_when_the_generation_advances_past_the_registered_one() {
        let client = IpcClient::new();
        client.mark_approver_registered(0);
        client.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(client.approver_registration_due(), Some(1));
    }

    #[test]
    fn approver_registration_due_is_always_none_for_a_window_role_client() {
        let client = IpcClient::new_window();
        assert_eq!(client.approver_registration_due(), None);
        client.mark_approver_registered(0);
        assert_eq!(client.approver_registration_due(), None);
    }

    // After a refusal is latched, `due` is `None` and
    // `is_approver_registered` is `false` -- distinguishing "nothing to send"
    // from "registered" is the whole point of splitting the two predicates.
    #[test]
    fn a_latched_refusal_leaves_due_none_and_registration_unsatisfied() {
        let client = IpcClient::new();
        assert!(client.mark_approver_refused(0));
        assert_eq!(client.approver_registration_due(), None);
        assert!(!client.is_approver_registered());
    }

    // A second refusal of the same generation is not a
    // fresh latch, which is what keeps the refused-registration event from
    // firing more than once per generation.
    #[test]
    fn mark_approver_refused_returns_false_on_a_second_call_for_the_same_generation() {
        let client = IpcClient::new();
        assert!(client.mark_approver_refused(0));
        assert!(!client.mark_approver_refused(0));
    }

    // A reconnect (generation advances) reopens `due` even
    // though the previous generation is still latched as refused.
    #[test]
    fn due_reopens_for_a_new_generation_after_a_previous_ones_refusal() {
        let client = IpcClient::new();
        assert!(client.mark_approver_refused(0));
        client.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(client.approver_registration_due(), Some(1));
    }

    // A `compare_exchange` against the sentinel could only ever latch the
    // first refused generation; `swap` latches
    // every one, including a second refusal several generations later.
    #[test]
    fn a_second_generations_refusal_latches_and_reopens_due_afterward() {
        let client = IpcClient::new();
        assert!(client.mark_approver_refused(0));
        client.generation.fetch_add(1, Ordering::SeqCst);
        assert!(client.mark_approver_refused(1));
        assert_eq!(client.approver_registration_due(), None);
        client.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(client.approver_registration_due(), Some(2));
    }

    // Guards against an `is_approver_registered` that
    // always returns `false`, which `approver_registration_due` clearing
    // alone would not catch.
    #[test]
    fn is_approver_registered_is_true_once_the_current_generation_is_marked_registered() {
        let client = IpcClient::new();
        client.mark_approver_registered(0);
        assert!(client.is_approver_registered());
    }

    #[tokio::test]
    async fn is_connected_is_false_for_a_freshly_created_control_client() {
        let client = IpcClient::new();
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn is_connected_is_false_for_a_freshly_created_window_client() {
        let client = IpcClient::new_window();
        assert!(!client.is_connected().await);
    }

    const ENDPOINT_PREFIX: &str = "yagura-test-gui-ipc-client";

    /// How long the stand-in daemon waits for a request frame before failing.
    const FRAME_LIMIT: std::time::Duration = std::time::Duration::from_secs(5);

    type PeerConnection = <PlatformIpc as PeerAuth>::Connection;

    /// A client whose write half reaches `PeerConnection` over a real
    /// endpoint. The read half stands in for the reader task; replies are
    /// delivered the way that task delivers them.
    async fn wired_unsettled(
        tag: &str,
    ) -> (IpcClient, PeerConnection, tokio::io::ReadHalf<Connection>) {
        let ipc = PlatformIpc::default();
        let endpoint = yagura_test_support::ipc::unique_endpoint_name(ENDPOINT_PREFIX, tag);
        let security = ipc.endpoint_security().expect("endpoint security");
        let mut listener = ipc.bind(&endpoint, security).await.expect("bind");
        let (accepted, connected) =
            tokio::join!(ipc.accept(&mut listener), connect_once(&endpoint));
        let (read, write) = tokio::io::split(connected.expect("connect"));
        let client = IpcClient::new();
        client.writer.lock().await.install(write);
        (client, accepted.expect("accept"), read)
    }

    /// [`wired_unsettled`] with the current wire revision settled, so every
    /// request can be carried.
    async fn wired(tag: &str) -> (IpcClient, PeerConnection, tokio::io::ReadHalf<Connection>) {
        let connected = wired_unsettled(tag).await;
        connected.0.settled.store(
            yagura_protocol::PROTOCOL_REVISION,
            connected.0.generation.load(Ordering::SeqCst),
        );
        connected
    }

    async fn next_request(peer: &mut PeerConnection) -> ClientMessage {
        use tokio::io::AsyncReadExt;
        let mut length = [0u8; 4];
        peer.read_exact(&mut length).await.expect("frame length");
        let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
        peer.read_exact(&mut body).await.expect("frame body");
        yagura_protocol::decode_client_message(&body).expect("a client frame")
    }

    /// Plays the daemon for one round trip: reads the request, then settles it
    /// under `kind` as the reader task does. Returns the request that arrived.
    async fn answer(
        client: &IpcClient,
        peer: &mut PeerConnection,
        kind: PendingKind,
        outcome: Result<Reply, IpcError>,
    ) -> ClientMessage {
        let request =
            yagura_test_support::ipc::reply_within(FRAME_LIMIT, "request", next_request(peer))
                .await;
        match outcome {
            Ok(reply) => client.complete_pending(kind, reply).await,
            Err(error) => client.fail_pending(error).await,
        }
        request
    }

    fn rejected(code: &str) -> IpcError {
        IpcError::Rejected {
            code: code.to_owned(),
            message: String::new(),
            rejected_argument: None,
        }
    }

    fn refused(outcome: &Result<(), IpcError>, wanted: &str) -> bool {
        matches!(outcome, Err(IpcError::Rejected { code, .. }) if code == wanted)
    }

    fn window_info(window_id: &str) -> WindowInfo {
        WindowInfo {
            window_id: window_id.to_owned(),
            name: "build".to_owned(),
            vendor: None,
            cwd: "/work".to_owned(),
            status: yagura_protocol::WindowStatus::Idle,
            exit_code: None,
            layout_json: None,
            room: None,
            unknown_reason: None,
            detection_warning: None,
            status_rev: None,
            mcp_connected: None,
            status_reports_missing: None,
        }
    }

    fn profile_command() -> yagura_protocol::ProfileCommandDetail {
        yagura_protocol::ProfileCommandDetail {
            argv: vec!["sh".to_owned()],
            shipped: None,
            changed: true,
        }
    }

    fn approval_settings(plans_require_approval: bool) -> ApprovalSettingsReply {
        ApprovalSettingsReply {
            playbook: "loaded".to_owned(),
            tools: Vec::new(),
            plans_require_approval,
        }
    }

    #[test]
    fn only_an_attach_times_out_as_an_attach_failure() {
        assert!(matches!(
            PendingKind::Attach.timeout_error(),
            IpcError::AttachTimeout
        ));
        assert!(matches!(
            PendingKind::ListWindows.timeout_error(),
            IpcError::RequestTimeout
        ));
    }

    #[tokio::test]
    async fn a_reply_answers_only_the_request_of_its_own_kind() {
        let client = IpcClient::new();
        let (sender, mut receiver) = oneshot::channel();
        *client.pending.lock().await = Some(Pending {
            kind: PendingKind::ListWindows,
            sender,
        });
        client
            .complete_pending(PendingKind::PlanList, Reply::PlanList(Vec::new()))
            .await;
        assert_eq!(client.pending_kind().await, Some(PendingKind::ListWindows));
        assert!(receiver.try_recv().is_err());

        client
            .complete_pending(PendingKind::ListWindows, Reply::WindowList(Vec::new()))
            .await;
        assert_eq!(client.pending_kind().await, None);
        let delivered = receiver.try_recv();
        assert!(
            matches!(&delivered, Ok(Ok(Reply::WindowList(windows))) if windows.is_empty()),
            "got {delivered:?}"
        );
    }

    #[tokio::test]
    async fn window_requests_hand_back_what_the_daemon_answered() {
        let (client, mut peer, _read) = wired("window-requests").await;

        let (windows, sent) = tokio::join!(
            client.list_windows(),
            answer(
                &client,
                &mut peer,
                PendingKind::ListWindows,
                Ok(Reply::WindowList(vec![window_info("w1")]))
            )
        );
        assert_eq!(sent, ClientMessage::ListWindows);
        assert_eq!(windows.expect("list_windows"), vec![window_info("w1")]);

        let (detections, sent) = tokio::join!(
            client.list_detections(),
            answer(
                &client,
                &mut peer,
                PendingKind::ListDetections,
                Ok(Reply::DetectionList(DetectionListReply {
                    names: vec!["shell".to_owned()],
                    warning: None,
                }))
            )
        );
        assert_eq!(sent, ClientMessage::ListDetections);
        assert_eq!(detections.expect("list_detections").names, ["shell"]);

        let (profiles, sent) = tokio::join!(
            client.list_profiles(),
            answer(
                &client,
                &mut peer,
                PendingKind::ListProfiles,
                Ok(Reply::ProfileList(ProfileListReply {
                    profiles: Vec::new(),
                    warnings: vec!["skipped bad.toml".to_owned()],
                }))
            )
        );
        assert_eq!(sent, ClientMessage::ListProfiles);
        assert_eq!(
            profiles.expect("list_profiles").warnings,
            ["skipped bad.toml"]
        );

        let (opened, sent) = tokio::join!(
            client.open_window(
                vec!["sh".to_owned()],
                Some("/work".to_owned()),
                Some("build".to_owned()),
                None,
                Some("vendor-a".to_owned()),
            ),
            answer(
                &client,
                &mut peer,
                PendingKind::OpenWindow,
                Ok(Reply::WindowOpened(OpenWindowReply {
                    window_id: "w2".to_owned(),
                    detection_warning: None,
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::OpenWindow {
                command: vec!["sh".to_owned()],
                cwd: Some("/work".to_owned()),
                name: Some("build".to_owned()),
                vendor: Some("vendor-a".to_owned()),
                detection: None,
            }
        );
        assert_eq!(opened.expect("open_window").window_id, "w2");

        let (opened, sent) = tokio::join!(
            client.open_window_from_profile(
                "shell".to_owned(),
                None,
                None,
                Some("fast".to_owned()),
                None,
            ),
            answer(
                &client,
                &mut peer,
                PendingKind::OpenWindow,
                Ok(Reply::WindowOpened(OpenWindowReply {
                    window_id: "w3".to_owned(),
                    detection_warning: None,
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::OpenWindowFromProfile {
                profile: "shell".to_owned(),
                cwd: None,
                name: None,
                model: Some("fast".to_owned()),
                effort: None,
            }
        );
        assert_eq!(opened.expect("open_window_from_profile").window_id, "w3");

        // The daemon's stored name, not the requested one, is what comes back.
        let (renamed, sent) = tokio::join!(
            client.rename_window("w1", "build"),
            answer(
                &client,
                &mut peer,
                PendingKind::RenameWindow,
                Ok(Reply::WindowRenamed("build (2)".to_owned()))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::RenameWindow {
                window_id: "w1".to_owned(),
                name: "build".to_owned(),
            }
        );
        assert_eq!(renamed.expect("rename_window"), "build (2)");

        let (candidates, sent) = tokio::join!(
            client.list_restore_candidates(),
            answer(
                &client,
                &mut peer,
                PendingKind::ListRestoreCandidates,
                Ok(Reply::RestoreCandidateList(RestoreCandidateListReply {
                    windows: Vec::new(),
                    taken_at_epoch_s: Some(42),
                }))
            )
        );
        assert_eq!(sent, ClientMessage::ListRestoreCandidates);
        assert_eq!(
            candidates
                .expect("list_restore_candidates")
                .taken_at_epoch_s,
            Some(42)
        );

        let (health, sent) = tokio::join!(
            client.get_daemon_health(),
            answer(
                &client,
                &mut peer,
                PendingKind::GetDaemonHealth,
                Ok(Reply::DaemonHealth(DaemonHealthReply {
                    problems: vec![yagura_protocol::DaemonProblem {
                        kind: yagura_protocol::DaemonProblemKind::WindowsUnavailable,
                        detail: "conpty.dll missing".to_owned(),
                    }],
                }))
            )
        );
        assert_eq!(sent, ClientMessage::GetDaemonHealth);
        assert_eq!(health.expect("get_daemon_health").problems.len(), 1);

        let (older, sent) = tokio::join!(
            client.get_daemon_health(),
            answer(
                &client,
                &mut peer,
                PendingKind::GetDaemonHealth,
                Err(rejected(ERROR_CODE_UNKNOWN_TYPE))
            )
        );
        assert_eq!(sent, ClientMessage::GetDaemonHealth);
        assert!(
            older
                .expect("an older daemon has no problem to report")
                .problems
                .is_empty()
        );

        let (updated, sent) = tokio::join!(
            client.update_window_layout("w1", "{}".to_owned()),
            answer(
                &client,
                &mut peer,
                PendingKind::UpdateWindowLayout,
                Ok(Reply::WindowLayoutUpdated(UpdateWindowLayoutReply {
                    window_id: "w1".to_owned(),
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::UpdateWindowLayout {
                window_id: "w1".to_owned(),
                layout_json: "{}".to_owned(),
            }
        );
        assert_eq!(updated.expect("update_window_layout").window_id, "w1");
    }

    #[tokio::test]
    async fn destructive_requests_succeed_when_the_daemon_is_done() {
        let (client, mut peer, _read) = wired("destructive-done").await;

        let (closed, sent) = tokio::join!(
            client.close_window("w1", Some(true)),
            answer(
                &client,
                &mut peer,
                PendingKind::CloseWindow,
                Ok(Reply::Done)
            )
        );
        assert_eq!(
            sent,
            ClientMessage::CloseWindow {
                window_id: "w1".to_owned(),
                force: Some(true),
            }
        );
        assert!(closed.is_ok(), "got {closed:?}");

        let (interrupted, sent) = tokio::join!(
            client.interrupt_window("w1"),
            answer(
                &client,
                &mut peer,
                PendingKind::InterruptWindow,
                Ok(Reply::Done)
            )
        );
        assert_eq!(
            sent,
            ClientMessage::InterruptWindow {
                window_id: "w1".to_owned(),
            }
        );
        assert!(interrupted.is_ok(), "got {interrupted:?}");

        let (decided, sent) = tokio::join!(
            client.decide_approval("r1", false),
            answer(
                &client,
                &mut peer,
                PendingKind::DecideApproval,
                Ok(Reply::Done)
            )
        );
        assert_eq!(
            sent,
            ClientMessage::ApprovalDecision {
                request_id: "r1".to_owned(),
                approved: false,
            }
        );
        assert!(decided.is_ok(), "got {decided:?}");
    }

    #[tokio::test]
    async fn destructive_requests_report_the_daemons_rejection() {
        let (client, mut peer, _read) = wired("destructive-rejected").await;

        let (closed, _) = tokio::join!(
            client.close_window("w1", None),
            answer(
                &client,
                &mut peer,
                PendingKind::CloseWindow,
                Err(rejected("window_not_found"))
            )
        );
        assert!(refused(&closed, "window_not_found"), "got {closed:?}");

        let (interrupted, _) = tokio::join!(
            client.interrupt_window("w1"),
            answer(
                &client,
                &mut peer,
                PendingKind::InterruptWindow,
                Err(rejected("invalid_state"))
            )
        );
        assert!(
            refused(&interrupted, "invalid_state"),
            "got {interrupted:?}"
        );

        let (decided, _) = tokio::join!(
            client.decide_approval("r1", true),
            answer(
                &client,
                &mut peer,
                PendingKind::DecideApproval,
                Err(rejected("approval_not_pending"))
            )
        );
        assert!(refused(&decided, "approval_not_pending"), "got {decided:?}");
    }

    #[tokio::test]
    async fn chat_requests_hand_back_what_the_daemon_answered() {
        let (client, mut peer, _read) = wired("chat-requests").await;
        let posted = |seq| ChatPostedReply {
            room_id: "r1".to_owned(),
            seq,
            targets: Vec::new(),
            user: false,
            unresolved: Vec::new(),
            halted: false,
        };

        let (sent_chat, sent) = tokio::join!(
            client.send_chat(Some("r1".to_owned()), "hi".to_owned()),
            answer(
                &client,
                &mut peer,
                PendingKind::SendChat,
                Ok(Reply::ChatPosted(posted(3)))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::SendChat {
                room_id: Some("r1".to_owned()),
                body: "hi".to_owned(),
            }
        );
        assert_eq!(sent_chat.expect("send_chat").seq, 3);

        let (broadcast, sent) = tokio::join!(
            client.broadcast_chat(None, "all".to_owned()),
            answer(
                &client,
                &mut peer,
                PendingKind::SendChat,
                Ok(Reply::ChatPosted(posted(4)))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::BroadcastChat {
                room_id: None,
                body: "all".to_owned(),
            }
        );
        assert_eq!(broadcast.expect("broadcast_chat").seq, 4);

        let (page, sent) = tokio::join!(
            client.read_chat(Some("r1".to_owned()), Some(2), Some(10)),
            answer(
                &client,
                &mut peer,
                PendingKind::ReadChat,
                Ok(Reply::ChatMessages(ChatMessagesReply {
                    room_id: "r1".to_owned(),
                    messages: Vec::new(),
                    next_after_seq: 7,
                    halted: None,
                    needs_user: false,
                    roster: Vec::new(),
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::ReadChat {
                room_id: Some("r1".to_owned()),
                after_seq: Some(2),
                limit: Some(10),
            }
        );
        assert_eq!(page.expect("read_chat").next_after_seq, 7);

        let (rooms, sent) = tokio::join!(
            client.list_chat_rooms(),
            answer(
                &client,
                &mut peer,
                PendingKind::ListChatRooms,
                Ok(Reply::ChatRoomList(ChatRoomListReply { rooms: Vec::new() }))
            )
        );
        assert_eq!(sent, ClientMessage::ListChatRooms);
        assert!(rooms.is_ok(), "got {rooms:?}");
    }

    #[tokio::test]
    async fn plan_and_role_requests_hand_back_what_the_daemon_answered() {
        let (client, mut peer, _read) = wired("plan-requests").await;
        let summary = PlanSummary {
            plan_id: "p1".to_owned(),
            title: "release".to_owned(),
            owner_window: None,
            task_count: 2,
            review: None,
            unseen_changes: false,
        };
        let plan = PlanInfo {
            plan_id: "p1".to_owned(),
            title: "release".to_owned(),
            cwd: "/work".to_owned(),
            owner_window: Some("w1".to_owned()),
            waves: Vec::new(),
            tasks: Vec::new(),
            roles_warning: None,
            review: None,
            version: 3,
        };

        let (plans, sent) = tokio::join!(
            client.list_plans(),
            answer(
                &client,
                &mut peer,
                PendingKind::PlanList,
                Ok(Reply::PlanList(vec![summary.clone()]))
            )
        );
        assert_eq!(sent, ClientMessage::ListPlans);
        assert_eq!(plans.expect("list_plans"), vec![summary]);

        let (detail, sent) = tokio::join!(
            client.get_plan("p1".to_owned()),
            answer(
                &client,
                &mut peer,
                PendingKind::PlanDetail,
                Ok(Reply::PlanDetail(plan.clone()))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::GetPlan {
                plan_id: "p1".to_owned(),
            }
        );
        assert_eq!(detail.expect("get_plan"), plan);

        let (deleted, sent) = tokio::join!(
            client.delete_plan("p1".to_owned()),
            answer(
                &client,
                &mut peer,
                PendingKind::PlanDeleted,
                Ok(Reply::PlanDeleted("p1".to_owned()))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::DeletePlan {
                plan_id: "p1".to_owned(),
            }
        );
        assert_eq!(deleted.expect("delete_plan"), "p1");

        let (roles, sent) = tokio::join!(
            client.list_roles(),
            answer(
                &client,
                &mut peer,
                PendingKind::RoleList,
                Ok(Reply::RoleList(RoleListReply {
                    roles: Vec::new(),
                    default_role: Some("dev".to_owned()),
                }))
            )
        );
        assert_eq!(sent, ClientMessage::ListRoles);
        assert_eq!(
            roles.expect("list_roles").default_role.as_deref(),
            Some("dev")
        );
    }

    #[tokio::test]
    async fn profile_requests_hand_back_what_the_daemon_answered() {
        let (client, mut peer, _read) = wired("profile-requests").await;
        let alias = LaunchOptionAlias {
            alias: "fast".to_owned(),
            value: "model-fast".to_owned(),
        };

        let (choices, sent) = tokio::join!(
            client.list_model_choices("shell".to_owned(), 5),
            answer(
                &client,
                &mut peer,
                PendingKind::ListModelChoices,
                Ok(Reply::ModelChoicesRequested(ModelChoicesRequestedReply {
                    request_id: 5,
                    profile: "shell".to_owned(),
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::ListModelChoices {
                profile: "shell".to_owned(),
                request_id: 5,
            }
        );
        assert_eq!(choices.expect("list_model_choices").request_id, 5);

        let (saved, sent) = tokio::join!(
            client.save_model_aliases("shell".to_owned(), vec![alias.clone()]),
            answer(
                &client,
                &mut peer,
                PendingKind::SaveModelAliases,
                Ok(Reply::ModelAliasesSaved(ModelAliasesSavedReply {
                    profile: "shell".to_owned(),
                    aliases: vec![alias.clone()],
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::SaveModelAliases {
                profile: "shell".to_owned(),
                aliases: vec![alias.clone()],
            }
        );
        assert_eq!(saved.expect("save_model_aliases").aliases, vec![alias]);

        let (listed, sent) = tokio::join!(
            client.list_mcp_registrations(6),
            answer(
                &client,
                &mut peer,
                PendingKind::ListMcpRegistrations,
                Ok(Reply::McpRegistrationsListed(McpRegistrationsListedReply {
                    request_id: 6,
                    bridge: None,
                    rows: Vec::new(),
                }))
            )
        );
        assert_eq!(sent, ClientMessage::ListMcpRegistrations { request_id: 6 });
        assert_eq!(listed.expect("list_mcp_registrations").request_id, 6);

        let (requested, sent) = tokio::join!(
            client.run_mcp_registration("shell".to_owned(), McpRegistrationAction::Register, 7),
            answer(
                &client,
                &mut peer,
                PendingKind::RunMcpRegistration,
                Ok(Reply::McpRegistrationRequested(
                    McpRegistrationRequestedReply {
                        request_id: 7,
                        profile: "shell".to_owned(),
                        action: "register".to_owned(),
                    }
                ))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::RunMcpRegistration {
                profile: "shell".to_owned(),
                action: McpRegistrationAction::Register,
                request_id: 7,
            }
        );
        assert_eq!(requested.expect("run_mcp_registration").request_id, 7);

        let (enabled, sent) = tokio::join!(
            client.set_profile_enabled("shell".to_owned(), false),
            answer(
                &client,
                &mut peer,
                PendingKind::SetProfileEnabled,
                Ok(Reply::ProfileEnabledSet(ProfileEnabledSetReply {
                    profile: "shell".to_owned(),
                    enabled: false,
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::SetProfileEnabled {
                profile: "shell".to_owned(),
                enabled: false,
            }
        );
        assert!(!enabled.expect("set_profile_enabled").enabled);

        let (detail, sent) = tokio::join!(
            client.get_profile_detail("shell".to_owned()),
            answer(
                &client,
                &mut peer,
                PendingKind::GetProfileDetail,
                Ok(Reply::ProfileDetail(ProfileDetailReply {
                    profile: "shell".to_owned(),
                    shipped_base: true,
                    command: profile_command(),
                    entries: Vec::new(),
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::GetProfileDetail {
                profile: "shell".to_owned(),
            }
        );
        assert!(detail.expect("get_profile_detail").shipped_base);

        let (command_set, sent) = tokio::join!(
            client.set_profile_command("shell".to_owned(), Some(vec!["sh".to_owned()])),
            answer(
                &client,
                &mut peer,
                PendingKind::SetProfileCommand,
                Ok(Reply::ProfileCommandSet(ProfileCommandSetReply {
                    profile: "shell".to_owned(),
                    command: profile_command(),
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::SetProfileCommand {
                profile: "shell".to_owned(),
                argv: Some(vec!["sh".to_owned()]),
            }
        );
        assert_eq!(
            command_set.expect("set_profile_command").command,
            profile_command()
        );
    }

    #[tokio::test]
    async fn approval_settings_requests_hand_back_what_the_daemon_answered() {
        let (client, mut peer, _read) = wired("approval-settings").await;

        let (settings, sent) = tokio::join!(
            client.get_approval_settings(),
            answer(
                &client,
                &mut peer,
                PendingKind::GetApprovalSettings,
                Ok(Reply::ApprovalSettings(approval_settings(false)))
            )
        );
        assert_eq!(sent, ClientMessage::GetApprovalSettings {});
        assert_eq!(
            settings.expect("get_approval_settings"),
            approval_settings(false)
        );

        let (settings, sent) = tokio::join!(
            client.set_auto_approve("close_window".to_owned(), true),
            answer(
                &client,
                &mut peer,
                PendingKind::SetAutoApprove,
                Ok(Reply::AutoApproveSet(approval_settings(false)))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::SetAutoApprove {
                tool: "close_window".to_owned(),
                auto_approve: true,
            }
        );
        assert_eq!(
            settings.expect("set_auto_approve"),
            approval_settings(false)
        );

        let (settings, sent) = tokio::join!(
            client.set_plan_approval_required(true),
            answer(
                &client,
                &mut peer,
                PendingKind::SetPlanApprovalRequired,
                Ok(Reply::PlanApprovalRequiredSet(approval_settings(true)))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::SetPlanApprovalRequired { required: true }
        );
        assert_eq!(
            settings.expect("set_plan_approval_required"),
            approval_settings(true)
        );
    }

    #[tokio::test]
    async fn copy_selection_hands_back_the_selected_text_or_none() {
        let (client, mut peer, _read) = wired("copy-selection").await;

        let (selected, sent) = tokio::join!(
            client.copy_selection(),
            answer(
                &client,
                &mut peer,
                PendingKind::CopySelection,
                Ok(Reply::SelectionText(Some("picked".to_owned())))
            )
        );
        assert_eq!(sent, ClientMessage::CopySelection);
        assert_eq!(selected.expect("copy_selection").as_deref(), Some("picked"));

        let (selected, _) = tokio::join!(
            client.copy_selection(),
            answer(
                &client,
                &mut peer,
                PendingKind::CopySelection,
                Ok(Reply::SelectionText(None))
            )
        );
        assert_eq!(selected.expect("copy_selection"), None);
    }

    #[tokio::test]
    async fn an_attached_client_reports_the_window_it_attached_to() {
        let (client, mut peer, _read) = wired("attach").await;
        let size = Some(TerminalSize { cols: 80, rows: 24 });

        let (attached, sent) = tokio::join!(
            client.request_attach("w1", size),
            answer(
                &client,
                &mut peer,
                PendingKind::Attach,
                Ok(Reply::Attached(AttachResponse {
                    child_exited: false,
                    exit_code: None,
                }))
            )
        );
        assert_eq!(
            sent,
            ClientMessage::AttachView {
                window_id: "w1".to_owned(),
                size,
            }
        );
        assert!(attached.is_ok(), "got {attached:?}");
        assert_eq!(client.window_id().await.as_deref(), Some("w1"));
    }

    #[tokio::test]
    async fn disconnect_releases_the_connection_and_fails_the_request_in_flight() {
        let (client, mut peer, _read) = wired("disconnect").await;
        assert!(client.is_connected().await);

        let (listed, ()) = tokio::join!(client.list_windows(), async {
            yagura_test_support::ipc::reply_within(FRAME_LIMIT, "request", next_request(&mut peer))
                .await;
            client.disconnect().await;
        });
        assert!(
            matches!(listed, Err(IpcError::NotConnected)),
            "got {listed:?}"
        );
        assert!(!client.is_connected().await);
    }

    /// Runs `ensure_protocol_revision` against a daemon that settles the probe
    /// with `outcome`.
    async fn probe_settled_with(tag: &str, outcome: Result<Reply, IpcError>) -> IpcClient {
        let (client, mut peer, _read) = wired_unsettled(tag).await;
        let ((), sent) = tokio::join!(
            client.ensure_protocol_revision(),
            answer(
                &client,
                &mut peer,
                PendingKind::GetProtocolRevision,
                outcome
            )
        );
        assert_eq!(sent, ClientMessage::GetProtocolRevision);
        client
    }

    #[tokio::test]
    async fn health_settles_an_unsettled_revision_before_asking_instead_of_answering_empty() {
        let (client, mut peer, _read) = wired_unsettled("health-unsettled").await;
        let (health, sent) = tokio::join!(client.get_daemon_health(), async {
            let probe = answer(
                &client,
                &mut peer,
                PendingKind::GetProtocolRevision,
                Ok(Reply::ProtocolRevision(yagura_protocol::PROTOCOL_REVISION)),
            )
            .await;
            assert_eq!(probe, ClientMessage::GetProtocolRevision);
            answer(
                &client,
                &mut peer,
                PendingKind::GetDaemonHealth,
                Ok(Reply::DaemonHealth(DaemonHealthReply {
                    problems: vec![yagura_protocol::DaemonProblem {
                        kind: yagura_protocol::DaemonProblemKind::WindowsUnavailable,
                        detail: "conpty.dll cannot be read".to_owned(),
                    }],
                })),
            )
            .await
        });
        assert_eq!(sent, ClientMessage::GetDaemonHealth);
        assert_eq!(
            health.expect("get_daemon_health").problems.len(),
            1,
            "a connection still settling its revision must ask, not report no problems"
        );
    }

    #[tokio::test]
    async fn the_revision_the_daemon_answers_is_settled_for_the_current_generation() {
        let client = probe_settled_with("probe-answered", Ok(Reply::ProtocolRevision(9))).await;
        assert_eq!(client.protocol_revision(), Some(9));
    }

    #[tokio::test]
    async fn a_daemon_that_does_not_know_the_probe_is_credited_with_the_baseline() {
        let client =
            probe_settled_with("probe-unknown-type", Err(rejected(ERROR_CODE_UNKNOWN_TYPE))).await;
        assert_eq!(client.protocol_revision(), Some(PROTOCOL_REVISION_BASELINE));
    }

    #[tokio::test]
    async fn a_probe_refused_for_another_reason_settles_nothing() {
        let client = probe_settled_with("probe-internal", Err(rejected(ERROR_CODE_INTERNAL))).await;
        assert_eq!(client.protocol_revision(), None);
        assert_eq!(client.protocol_revision_settle_due(), Some(0));
    }

    #[test]
    fn a_revision_probe_is_owed_once_per_generation() {
        let client = IpcClient::new();
        assert_eq!(client.protocol_revision_settle_due(), Some(0));
        client.settled.store(PROTOCOL_REVISION_BASELINE, 0);
        assert_eq!(client.protocol_revision_settle_due(), None);
        client.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(client.protocol_revision_settle_due(), Some(1));
    }

    #[test]
    fn only_the_current_generations_settled_revision_is_reported() {
        let client = IpcClient::new();
        assert_eq!(client.protocol_revision(), None);
        client.settled.store(9, 0);
        assert_eq!(client.protocol_revision(), Some(9));
        client.generation.fetch_add(1, Ordering::SeqCst);
        assert_eq!(client.protocol_revision(), None);
    }

    #[tokio::test]
    async fn a_refused_frame_names_the_request_it_refused() {
        let (outcome, _) =
            send_and_collect(&frame_gated_message(), &wire::SettledRevision::new()).await;
        assert!(
            matches!(&outcome, Err(IpcError::Unsupported { request_type }) if request_type == "set_visible"),
            "got {outcome:?}"
        );
    }

    async fn listening(
        tag: &str,
    ) -> (PlatformIpc, <PlatformIpc as IpcTransport>::Listener, String) {
        let ipc = PlatformIpc::default();
        let endpoint = yagura_test_support::ipc::unique_endpoint_name(ENDPOINT_PREFIX, tag);
        let security = ipc.endpoint_security().expect("endpoint security");
        let listener = ipc.bind(&endpoint, security).await.expect("bind");
        (ipc, listener, endpoint)
    }

    #[tokio::test]
    async fn every_connect_form_reaches_a_listening_endpoint() {
        let (ipc, mut listener, endpoint) = listening("connect-once").await;
        let (accepted, reached) = tokio::join!(ipc.accept(&mut listener), connect_once(&endpoint));
        assert!(accepted.is_ok() && reached.is_some(), "connect_once");

        let (ipc, mut listener, endpoint) = listening("connect-limited").await;
        let (accepted, reached) = tokio::join!(
            ipc.accept(&mut listener),
            connect_retrying_with_limit(&endpoint, 1)
        );
        assert!(
            accepted.is_ok() && reached.is_some(),
            "connect_retrying_with_limit"
        );

        let (ipc, mut listener, endpoint) = listening("connect-retrying").await;
        let (accepted, reached) =
            tokio::join!(ipc.accept(&mut listener), connect_retrying(&endpoint));
        assert!(accepted.is_ok() && reached.is_some(), "connect_retrying");
    }

    // A lower bound only: the sleeps guarantee it, and an upper bound would
    // depend on the machine's load.
    #[tokio::test]
    async fn a_refused_endpoint_is_retried_an_interval_apart() {
        let endpoint = yagura_test_support::ipc::unique_endpoint_name(ENDPOINT_PREFIX, "nobody");
        let started = std::time::Instant::now();
        assert!(connect_retrying_with_limit(&endpoint, 3).await.is_none());
        assert!(
            started.elapsed() >= CONNECT_RETRY_INTERVAL * 2,
            "three attempts took {:?}",
            started.elapsed()
        );
    }
}
