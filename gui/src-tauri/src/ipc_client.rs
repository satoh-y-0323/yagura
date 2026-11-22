//! IPC client: the GUI side of the length-prefixed JSON framing (LPJF)
//! protocol defined by `yagura-protocol`.
//!
//! One `IpcClient` holds one connection to the daemon at a time, and that
//! connection is attached to at most one window, so watching N windows takes
//! N clients plus one attach-less client for the window-scoped requests; the
//! [`crate::connection_pool`] module owns that fan-out. When the connection
//! opens, is lost and is replaced is decided by the client's owner task
//! ([`lifecycle`]) alone: callers ask it for a connection and use the
//! generation it opened.
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

use yagura_os::{IpcTransport, OsError, PeerAuth, PlatformIpc, default_endpoint};
use yagura_protocol::{
    ApprovalRequest, ClientMessage, DefaultPalettes, LaunchOptionAlias, McpRegistrationAction,
    OnNameTaken, PROTOCOL_REVISION_BASELINE, PlanInfo, PlanSummary, RoleEntry, StatusBoot,
    TaskAssignment, TerminalSize, TicketInfo, TicketWorkspaceInfo, WindowInfo,
};

use crate::commands::{
    ApprovalSettingsReply, ChatMessagesReply, ChatPostedReply, ChatRoomListReply,
    DaemonHealthReply, DetectionListReply, McpRegistrationRequestedReply,
    McpRegistrationsListedReply, ModelAliasesSavedReply, ModelChoicesRequestedReply,
    OpenWindowReply, ProfileCommandSetReply, ProfileDetailReply, ProfileEnabledSetReply,
    ProfileListReply, RestoreCandidateListReply, RoleListReply, TaskWindowOpenedReply,
    TicketListReply, UpdateWindowLayoutReply,
};
use crate::events::{
    ApprovalRegistrationRefusedPayload, ApprovalRequestedPayload, ApprovalResetPayload,
    AttachResponse, DaemonConnectedPayload, EVENT_APPROVAL_REGISTRATION_REFUSED,
    EVENT_APPROVAL_REQUESTED, EVENT_APPROVAL_RESET, EVENT_EXIT, ExitPayload, LinkLostReason,
};
use crate::host::GuiHost;
use crate::instance::InstanceResolution;
use crate::window_record::{LinkState, RecordState, SentSize};

mod lifecycle;

pub(crate) use lifecycle::{
    BoxedRead, EndKind, Generation, Interrupt, ReaderLink, Settled, StepReply,
};
use lifecycle::{ClientKind, Registration, Shared, Take, TakenAnswer, Until};

/// A client-side connection to the daemon's endpoint.
pub(crate) type Connection = <PlatformIpc as IpcTransport>::ClientConnection;

/// `error.code` telling this client that another GUI took the session over.
pub(crate) const ERROR_CODE_SUPERSEDED: &str =
    yagura_protocol::ConnectionErrorCode::Superseded.as_str();

/// `error.code` for internal daemon failures.
pub(crate) const ERROR_CODE_INTERNAL: &str = "internal";

/// `error.code` a daemon built before a message type existed answers it
/// with. Settles a generation's revision probe the same as a genuine answer
/// would: the refusal itself says which wire the peer speaks.
const ERROR_CODE_UNKNOWN_TYPE: &str = "unknown_type";

/// Whether a daemon of `revision` names the request each answer belongs to.
/// `None` -- a generation still probing -- does not: the probe's answer is
/// matched by its kind, whichever daemon sends it.
pub(crate) fn answers_carry_correlation_ids(revision: Option<u32>) -> bool {
    revision.is_some_and(|revision| revision >= yagura_protocol::CORRELATION_ID_MIN_REVISION)
}

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
    /// The request was written, and the connection ended before the daemon
    /// answered it: the daemon may have carried it out.
    #[error("the connection to the yagura daemon ended before it answered")]
    ConnectionLost,
    /// The daemon did not accept a connection within the retry budget.
    #[error("timed out waiting for the yagura daemon to accept a connection")]
    ConnectTimeout,
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
    /// needs, so the frame was refused rather than risk one an older daemon
    /// cannot parse.
    #[error(
        "this request needs a wire revision the connected daemon does not speak: {request_type}"
    )]
    Unsupported {
        /// The wire tag of the request that was refused, one of
        /// [`yagura_protocol::client_message_wire_tags`].
        request_type: String,
    },
}

impl IpcError {
    /// The same failure again, for when one failed connect attempt answers
    /// several callers. An endpoint name failure keeps its text.
    fn duplicate(&self) -> Self {
        match self {
            Self::EndpointName(error) => Self::EndpointName(duplicate_os_error(error)),
            Self::NotConnected => Self::NotConnected,
            Self::ConnectionLost => Self::ConnectionLost,
            Self::ConnectTimeout => Self::ConnectTimeout,
            Self::Rejected {
                code,
                message,
                rejected_argument,
            } => Self::Rejected {
                code: code.clone(),
                message: message.clone(),
                rejected_argument: rejected_argument.clone(),
            },
            Self::UnexpectedReply => Self::UnexpectedReply,
            Self::Write => Self::Write,
            Self::DaemonNotFound => Self::DaemonNotFound,
            Self::DaemonSpawn => Self::DaemonSpawn,
            Self::InvalidArgument(argument) => Self::InvalidArgument(argument),
            Self::WindowNotAttached { window_id } => Self::WindowNotAttached {
                window_id: window_id.clone(),
            },
            Self::ApproverNotRegistered => Self::ApproverNotRegistered,
            Self::Unsupported { request_type } => Self::Unsupported {
                request_type: request_type.clone(),
            },
        }
    }
}

/// An [`OsError`] with the same text, which an [`std::io::Error`] does not
/// let be cloned.
fn duplicate_os_error(error: &OsError) -> OsError {
    match error {
        OsError::Unsupported { feature } => OsError::Unsupported { feature },
        OsError::Io(error) => OsError::Io(std::io::Error::new(error.kind(), error.to_string())),
        OsError::Security(message) => OsError::Security(message.clone()),
        OsError::EndpointHeld(message) => OsError::EndpointHeld(message.clone()),
    }
}

/// Which request a generation's waiting slot holds. A daemon that returns
/// correlation ids is matched by the key and checked against this; an older
/// one is matched by this alone.
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
    ClearChatRoom,
    DeleteChatRoom,
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
    TicketWorkspaceList,
    TicketList,
    /// Every request answered with a whole ticket waits under one tag; this
    /// client only reads, so the one it sends is `get_ticket`.
    TicketDetail,
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
    /// The id of the room whose history the daemon cleared.
    ChatRoomCleared(String),
    /// The id of the room the daemon deleted.
    ChatRoomDeleted(String),
    /// The wire revision the answering daemon implements and the boot it runs
    /// in, from [`ClientMessage::GetProtocolRevision`]'s reply.
    ProtocolRevision {
        revision: u32,
        boot: Option<StatusBoot>,
    },
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
    TicketWorkspaceList(Vec<TicketWorkspaceInfo>),
    TicketList(TicketListReply),
    TicketDetail(TicketInfo),
    /// The request succeeded and carries no value beyond that.
    Done,
}

/// One client of the daemon, attached to at most one window.
///
/// `IpcClient::default()` -- and so `IpcClient::new()` -- is the control
/// client, which registers as the approver of destructive operations;
/// `IpcClient::new_window(record)` is the one way to get a window client, so
/// a per-window connection always states its role rather than inheriting one.
#[derive(Debug)]
pub struct IpcClient {
    shared: Arc<Shared>,
}

/// The serial of the next client built. Starts above `0`, which no record
/// names as its owner.
static NEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// What this client knows of the daemon start it reached: the start it made
/// and what it last announced.
#[derive(Debug, Clone, Default)]
struct DaemonStart {
    /// The start this client made last, until a settle shows it reached
    /// another one; the next start it makes replaces it.
    made: Option<MadeStart>,
    /// Kept after it is sent: a WebView that starts listening later reads it.
    last: Option<DaemonConnectedPayload>,
}

/// A daemon start this client made, and the warning it was made with.
#[derive(Debug, Clone)]
struct MadeStart {
    warning: Option<String>,
    reached: MadeStartReached,
}

/// Which settled start a made start is taken to be.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MadeStartReached {
    /// No generation has settled since it was made, whichever connection
    /// that settle comes on.
    NotYet,
    /// The start a settle answered with this boot.
    Boot(Option<StatusBoot>),
}

impl DaemonStart {
    /// The value after a generation settles on `boot`, with the control
    /// connection's announcement unless that boot was already announced.
    fn first_settled(
        &self,
        boot: Option<StatusBoot>,
        kind: ClientKind,
        compatible: bool,
    ) -> (Self, Option<DaemonConnectedPayload>) {
        let mut next = self.clone();
        if kind != ClientKind::Control {
            return (next, None);
        }
        let told = boot.is_some()
            && self
                .last
                .as_ref()
                .is_some_and(|last| last.status_boot == boot);
        let (spawn_warning, made) = match &self.made {
            None => (None, None),
            // A boot told before this start was made: the start reached is
            // not the one made.
            Some(made) if made.reached == MadeStartReached::NotYet && told => (None, None),
            Some(made) if made.reached == MadeStartReached::NotYet => (
                made.warning.clone(),
                Some(MadeStart {
                    warning: made.warning.clone(),
                    reached: MadeStartReached::Boot(boot),
                }),
            ),
            // Two starts that name no boot cannot be told apart, so the
            // warning stays, the safe side for a warning.
            Some(made) if made.reached == MadeStartReached::Boot(boot) => {
                (made.warning.clone(), Some(made.clone()))
            }
            Some(_) => (None, None),
        };
        next.made = made;
        if told {
            return (next, None);
        }
        let payload = DaemonConnectedPayload {
            status_boot: boot,
            spawn_warning,
            compatible,
        };
        next.last = Some(payload.clone());
        (next, Some(payload))
    }
}

/// [`IpcClient::set_default_palettes`]'s memory: the palettes, and the
/// generation they were last sent on.
#[derive(Debug, Default)]
struct SentDefaultPalettes {
    latest: Option<DefaultPalettes>,
    sent_on: Option<u64>,
}

impl Default for IpcClient {
    fn default() -> Self {
        Self::with(ClientKind::Control, None)
    }
}

impl Drop for IpcClient {
    // The owner task holds what it shares with the client, not the client:
    // letting the client go is what closes the owner's inbox.
    fn drop(&mut self) {
        self.shared.release();
    }
}

impl IpcClient {
    fn with(kind: ClientKind, record: Option<Arc<crate::window_record::WindowRecord>>) -> Self {
        let serial = NEXT_SERIAL.fetch_add(1, Ordering::SeqCst);
        if let Some(record) = &record {
            record.set_owner(serial);
        }
        Self {
            shared: Arc::new(Shared::new(kind, record, serial)),
        }
    }

    /// Creates a client with no connection established yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a per-window client with no connection established yet, which
    /// becomes the owner of `record`. Never the control client: only the
    /// connection pool's single control connection may register as an approver.
    pub(crate) fn new_window(record: Arc<crate::window_record::WindowRecord>) -> Self {
        Self::with(ClientKind::Window, Some(record))
    }

    /// A control client open on `generation`, which nothing owns.
    #[cfg(test)]
    pub(crate) fn wired(generation: Arc<Generation>) -> Self {
        Self {
            shared: Arc::new(Shared::wired(
                generation,
                NEXT_SERIAL.fetch_add(1, Ordering::SeqCst),
            )),
        }
    }

    /// Waits until a connection to the daemon is open, starting a daemon
    /// when none answers. Returns at once when one is open.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon can neither be reached nor
    /// started, or reached and not opened.
    pub async fn connect(&self, app: &impl GuiHost) -> Result<(), IpcError> {
        self.shared.want(app, ConnectMode::Spawn, Until::Open).await
    }

    /// Makes a single attempt to reach a daemon that is already running. The
    /// window-list poll uses this: it has its own interval to retry on, and a
    /// daemon that fails to start must not be spawned again on every tick.
    ///
    /// # Errors
    /// Returns [`IpcError::NotConnected`] when no daemon answers.
    pub async fn connect_existing(&self, app: &impl GuiHost) -> Result<(), IpcError> {
        self.shared.want(app, ConnectMode::Once, Until::Open).await
    }

    /// Waits until the daemon accepts a connection, starting one when none
    /// answers; it does not wait for the connection to open. What the
    /// recovery supervisor needs to know is only whether a daemon answers.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon can neither be reached nor started.
    pub(crate) async fn reach_daemon(&self, app: &impl GuiHost) -> Result<(), IpcError> {
        self.shared
            .want(app, ConnectMode::Spawn, Until::Reached)
            .await
    }

    /// Attaches this connection to `window_id`, connecting first if needed,
    /// and restores what the window's record asks for. A failure leaves the
    /// window's link lost.
    ///
    /// The connect retries a refused endpoint but never starts a
    /// daemon: a window can only exist if one is already running.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached or the window
    /// does not exist, and [`IpcError::WindowNotAttached`] on a control
    /// connection.
    pub async fn attach_window(
        &self,
        app: &impl GuiHost,
        window_id: &str,
        size: Option<TerminalSize>,
    ) -> Result<AttachResponse, IpcError> {
        let attached = match self
            .shared
            .want(app, ConnectMode::Retry, Until::Open)
            .await
            .and_then(|()| self.shared.open())
        {
            Ok(generation) => {
                attach_restoring(app, &self.shared, &generation, window_id, size, None)
                    .await
                    .into_result()
            }
            Err(error) => Err(error),
        };
        if attached.is_err() {
            self.lose_link(app, window_id, LinkLostReason::AttachFailed)
                .await;
        }
        attached
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
        on_name_taken: OnNameTaken,
        detection: Option<String>,
        vendor: Option<String>,
    ) -> Result<OpenWindowReply, IpcError> {
        let message = ClientMessage::OpenWindow {
            command,
            cwd,
            name,
            vendor,
            detection,
            on_name_taken,
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
        on_name_taken: OnNameTaken,
        model: Option<String>,
        effort: Option<String>,
    ) -> Result<OpenWindowReply, IpcError> {
        let message = ClientMessage::OpenWindowFromProfile {
            profile,
            cwd,
            name,
            model,
            effort,
            on_name_taken,
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
    /// Returns what the daemon answered, or why it gave no answer. With no
    /// answer the daemon may or may not have applied the decision, so the
    /// caller must not treat it as a failure.
    pub(crate) async fn decide_approval(
        &self,
        request_id: &str,
        approved: bool,
    ) -> StepReply<Result<(), IpcError>> {
        let generation = self.shared.open().ok();
        request_decide_approval(generation.as_deref(), request_id, approved).await
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
    pub async fn get_daemon_health(
        &self,
        _app: &impl GuiHost,
    ) -> Result<DaemonHealthReply, IpcError> {
        // An open connection has settled its revision, so `Unsupported` here
        // is an older daemon's, never one not yet probed.
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

    /// Clears one chat room's history and returns the room the daemon
    /// cleared. Approver-only on the daemon side.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates this
    /// request, or rejects it (an unknown room included).
    pub async fn clear_chat_room(&self, room_id: String) -> Result<String, IpcError> {
        match self
            .request(
                &ClientMessage::ClearChatRoom { room_id },
                PendingKind::ClearChatRoom,
            )
            .await?
        {
            Reply::ChatRoomCleared(cleared) => Ok(cleared),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Deletes one chat room and returns the room the daemon deleted.
    /// Approver-only on the daemon side.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates this
    /// request, or rejects it (`room_in_use` when a window is open in the
    /// room).
    pub async fn delete_chat_room(&self, room_id: String) -> Result<String, IpcError> {
        match self
            .request(
                &ClientMessage::DeleteChatRoom { room_id },
                PendingKind::DeleteChatRoom,
            )
            .await?
        {
            Reply::ChatRoomDeleted(deleted) => Ok(deleted),
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

    /// Lists every workspace that has tickets.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// ticket board, or rejects the request.
    pub async fn list_ticket_workspaces(&self) -> Result<Vec<TicketWorkspaceInfo>, IpcError> {
        match self
            .request(
                &ClientMessage::ListTicketWorkspaces,
                PendingKind::TicketWorkspaceList,
            )
            .await?
        {
            Reply::TicketWorkspaceList(workspaces) => Ok(workspaces),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Reads one workspace's ticket settings, milestones and the summaries of
    /// the tickets that match `state` and `milestone`, at most `limit` of them.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// ticket board, or rejects the request.
    pub async fn list_tickets(
        &self,
        workspace_id: i64,
        state: Option<String>,
        milestone: Option<String>,
        limit: Option<u32>,
    ) -> Result<TicketListReply, IpcError> {
        let message = ClientMessage::ListTickets {
            workspace_id: Some(workspace_id),
            state,
            milestone,
            limit,
        };
        match self.request(&message, PendingKind::TicketList).await? {
            Reply::TicketList(list) => Ok(list),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// Reads one ticket of a workspace in full.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the daemon cannot be reached, predates the
    /// ticket board, or rejects the request (an unknown ticket included).
    pub async fn get_ticket(&self, workspace_id: i64, ticket: u32) -> Result<TicketInfo, IpcError> {
        let message = ClientMessage::GetTicket {
            workspace_id: Some(workspace_id),
            ticket,
        };
        match self.request(&message, PendingKind::TicketDetail).await? {
            Reply::TicketDetail(ticket) => Ok(ticket),
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

    /// Gives the daemon the palettes a window it opens from now on answers
    /// color queries with, unless the open connection already has the same
    /// ones. They are remembered and sent again whenever a connection opens,
    /// so a daemon that is not reachable now, or not yet, still gets them.
    ///
    /// # Errors
    /// Returns [`IpcError`] when the open connection could not be written to.
    /// A daemon too old to take them is not sent them, which is not an error.
    pub async fn set_default_palettes(
        &self,
        _app: &impl GuiHost,
        palettes: DefaultPalettes,
    ) -> Result<(), IpcError> {
        // The owner publishes a generation open under this lock, so the
        // value is sent by the opening or here, never by neither.
        let mut sent = self.shared.palettes().lock().await;
        let open = self.shared.open().ok();
        let open_number = open.as_ref().map(|generation| generation.number());
        if sent.latest.as_ref() == Some(&palettes)
            && open_number.is_some()
            && sent.sent_on == open_number
        {
            return Ok(());
        }
        sent.latest = Some(palettes.clone());
        let Some(generation) = open else {
            return Ok(());
        };
        match generation
            .write(&ClientMessage::SetDefaultPalettes(palettes))
            .await
        {
            Ok(()) => {
                sent.sent_on = Some(generation.number());
                Ok(())
            }
            Err(IpcError::NotConnected | IpcError::Unsupported { .. }) => Ok(()),
            Err(error) => Err(error),
        }
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
    /// connection is attached to. Remembers nothing: the size a window asked
    /// for is its record's.
    ///
    /// # Errors
    /// Returns [`IpcError`] when no connection is open or the write fails.
    pub async fn resize(&self, cols: u16, rows: u16) -> Result<(), IpcError> {
        self.send(&ClientMessage::Resize { cols, rows }).await
    }

    /// Lets the client go for good: once this returns, nothing of it is
    /// open and nothing opens again, the request in flight has ended, and a
    /// window client changes its record no more.
    pub async fn disconnect(&self) {
        self.shared.retire().await;
    }

    /// Whether the daemon is reached right now, as far as the recovery
    /// supervisor is concerned.
    ///
    /// A predicate, not a step: unlike [`IpcClient::connect`] it neither
    /// establishes a connection nor starts a daemon, so a supervisor can
    /// observe an outage without changing it.
    pub(crate) async fn is_connected(&self) -> bool {
        self.shared.published().public().is_connected()
    }

    /// The window the open connection is attached to, `None` when none is
    /// open or it attached none.
    #[cfg(test)]
    pub(crate) async fn window_id(&self) -> Option<String> {
        self.shared
            .open()
            .ok()
            .and_then(|generation| generation.window_id())
    }

    /// This client's current connection: its serial, and the number of the
    /// generation open now, `u64::MAX` -- a number no generation gets -- when
    /// none is.
    pub(crate) fn connection_id(&self) -> (u64, u64) {
        let open = self
            .shared
            .open()
            .map_or(u64::MAX, |generation| generation.number());
        (self.shared.serial(), open)
    }

    /// Moves the window's link to lost, when this client still holds the
    /// record.
    async fn lose_link(&self, app: &impl GuiHost, window_id: &str, reason: LinkLostReason) {
        let Some(record) = self.shared.record() else {
            return;
        };
        let mut state = record.lock().await;
        if self.shared.holds(record) {
            state.move_to(app, window_id, LinkState::Lost(reason));
        }
    }

    /// Whether the open connection registered as the approver. A generation
    /// whose registration was refused opens too, so destructive operations
    /// gate on this rather than on being connected.
    pub(crate) fn is_approver_registered(&self) -> bool {
        self.shared.published().public().is_approver_registered()
    }

    /// Pulls the requests pending for this approver, on the WebView's own
    /// initiative: the same idempotent registration on the open connection,
    /// which answers with the current pending list.
    ///
    /// This emits nothing: the caller applies the returned list itself, so
    /// an event here would deliver it twice. What opening the connection
    /// settled about the registration is not changed by it.
    ///
    /// Only the control connection registers as an approver, so this is never
    /// reached on a window client; an empty list means "nothing is
    /// pending", never "wrong role".
    ///
    /// # Errors
    /// Returns [`IpcError::NotConnected`] when no connection is open, and
    /// the daemon's refusal unchanged: the caller maps the `auth_failed`
    /// refusal onto its own outcome type.
    pub(crate) async fn sync_approvals(
        &self,
        _app: &impl GuiHost,
    ) -> Result<Vec<ApprovalRequest>, IpcError> {
        debug_assert!(self.shared.kind() == ClientKind::Control);
        match self
            .request(
                &ClientMessage::RegisterApprover,
                PendingKind::RegisterApprover,
            )
            .await?
        {
            Reply::ApproverRegistered(pending) => Ok(pending),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// The wire revision the open connection settled on, `None` when none
    /// is open. Never a gate: the open generation's write is the only body
    /// that weighs [`crate::pool_outcomes::protocol_revision_gate`].
    #[cfg(test)]
    pub(crate) fn protocol_revision(&self) -> Option<u32> {
        self.shared.published().public().protocol_revision()
    }

    /// Lists the windows, with the wire revision of the connection that
    /// answered: the two are one fact, where a revision read afterwards may
    /// be another connection's.
    ///
    /// # Errors
    /// As [`Self::list_windows`].
    pub(crate) async fn list_windows_with_revision(
        &self,
    ) -> Result<(Vec<WindowInfo>, Option<u32>), IpcError> {
        let generation = self.shared.open()?;
        match generation
            .request(&ClientMessage::ListWindows, PendingKind::ListWindows)
            .await?
        {
            Reply::WindowList(windows) => Ok((windows, generation.revision())),
            _ => Err(IpcError::UnexpectedReply),
        }
    }

    /// The daemon start this client last announced, `None` before the first.
    pub(crate) fn daemon_connection(&self) -> Option<DaemonConnectedPayload> {
        self.shared.daemon_connection()
    }

    /// Sends `message` on the open connection and waits for its answer. Never
    /// waits for a connection to open: none open is [`IpcError::NotConnected`].
    async fn request(&self, message: &ClientMessage, kind: PendingKind) -> Result<Reply, IpcError> {
        self.shared.open()?.request(message, kind).await
    }

    /// Attaches the open connection at `size` (`None` keeps the window's own size).
    #[cfg(test)]
    async fn request_attach(
        &self,
        _app: &impl GuiHost,
        window_id: &str,
        size: Option<TerminalSize>,
    ) -> Result<AttachResponse, IpcError> {
        let generation = self.shared.open()?;
        request_attach(&generation, window_id, size, None)
            .await
            .into_result()
    }

    /// Writes `message` on the open connection.
    ///
    /// # Errors
    /// Returns [`IpcError`] when no connection is open, the connection's
    /// revision cannot carry the message, or the write fails.
    pub(crate) async fn send(&self, message: &ClientMessage) -> Result<(), IpcError> {
        self.shared.open()?.write(message).await
    }
}

/// Registers a generation that is opening as the approver, and tells the
/// WebView what the registration reported: the pending requests as a reset
/// followed by one event each, or the refusal. Both come before the opening
/// announces the daemon start. An answer read as `None` opens nothing.
async fn register_approver(
    app: &impl GuiHost,
    generation: &Generation,
) -> StepReply<Option<Registration>> {
    let reply = generation
        .request_step(
            &ClientMessage::RegisterApprover,
            PendingKind::RegisterApprover,
            Some(registration_taken),
        )
        .await;
    reply.map(|answer| {
        let registration = registration_answer(&answer);
        match (registration, answer) {
            (Some(Registration::Accepted), Ok(Reply::ApproverRegistered(pending))) => {
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
            (Some(Registration::Refused), _) => app.emit_event(
                EVENT_APPROVAL_REGISTRATION_REFUSED,
                ApprovalRegistrationRefusedPayload {},
            ),
            _ => {}
        }
        registration
    })
}

/// The registration the daemon's answer to `RegisterApprover` settles on;
/// `None` is an answer that opens nothing.
fn registration_answer(answer: &Result<Reply, IpcError>) -> Option<Registration> {
    match answer {
        Ok(Reply::ApproverRegistered(_)) => Some(Registration::Accepted),
        Err(IpcError::Rejected { code, .. }) if code == "auth_failed" => {
            Some(Registration::Refused)
        }
        _ => None,
    }
}

/// The revision and boot a generation's probe settles on: the daemon's
/// answer, or the baseline for a daemon built before the probe existed.
/// `None` is any other outcome, which leaves the generation unopened.
fn revision_probe_outcome(result: &Result<Reply, IpcError>) -> Option<(u32, Option<StatusBoot>)> {
    match result {
        Ok(Reply::ProtocolRevision { revision, boot }) => Some((*revision, *boot)),
        Err(IpcError::Rejected { code, .. }) if code == ERROR_CODE_UNKNOWN_TYPE => {
            Some((PROTOCOL_REVISION_BASELINE, None))
        }
        _ => None,
    }
}

/// The response an answer to `AttachView` attaches with; `None` is an answer
/// that attaches nothing.
fn attached(answer: &Result<Reply, IpcError>) -> Option<AttachResponse> {
    match answer {
        Ok(Reply::Attached(response)) => Some(response.clone()),
        _ => None,
    }
}

// What each opening step's answer counts as once its generation ends: read by
// the same function the step's own outcome is read by.

fn revision_taken(answer: &Result<Reply, IpcError>) -> TakenAnswer {
    revision_probe_outcome(answer).map_or(TakenAnswer::Refused, |(revision, _)| {
        TakenAnswer::Revision(revision)
    })
}

fn registration_taken(answer: &Result<Reply, IpcError>) -> TakenAnswer {
    registration_answer(answer).map_or(TakenAnswer::Refused, TakenAnswer::Registration)
}

fn restore_taken(answer: &Result<Reply, IpcError>) -> TakenAnswer {
    attached(answer).map_or(TakenAnswer::Refused, |_| TakenAnswer::Restore)
}

/// Attaches `generation` at `size`, or at the size the window's record holds
/// when `size` is `None`, and then brings the daemon to what the record asks
/// for: the one place a window's state is restored, whether the attach is
/// the WebView's, the window-list poll's or a reconnect's.
///
/// Restoring happens under the record's lock taken again after the reply,
/// in this order: the visibility, palette and size, then the input held
/// meanwhile in the order it was typed, then the link becomes attached,
/// only if all of it was written on `generation` and it is still the open one.
/// The state goes first because it is what the screen was when the input
/// was typed. A child that has exited is restored nothing: its held input
/// is dropped and the exit is reported.
///
/// `take` makes the attach a step of the generation's opening: its answer,
/// a refusal before the request included, is what the generation took.
async fn attach_restoring(
    app: &impl GuiHost,
    shared: &Shared,
    generation: &Arc<Generation>,
    window_id: &str,
    size: Option<TerminalSize>,
    take: Option<Take>,
) -> StepReply<Result<AttachResponse, IpcError>> {
    // A control connection has no record, and attaches no window.
    let Some(record) = shared.record() else {
        let refusal = IpcError::WindowNotAttached {
            window_id: window_id.to_owned(),
        };
        return generation.refuse(take, refusal).map(Err);
    };
    let size = {
        let mut state = record.lock().await;
        if size.is_some() {
            state.size = size;
        }
        state.size
    };
    let response = match request_attach(generation, window_id, size, take).await {
        StepReply::Answered(Ok(response)) => response,
        other => return other,
    };
    let mut state = record.lock().await;
    if !shared.holds(record) || state.link == LinkState::Superseded {
        return StepReply::Answered(Ok(response));
    }
    if response.child_exited {
        state.move_to(app, window_id, LinkState::Exited);
        app.emit_event(
            EVENT_EXIT,
            ExitPayload {
                window_id: window_id.to_owned(),
                exit_code: response.exit_code,
            },
        );
        return StepReply::Answered(Ok(response));
    }
    if state.link == LinkState::Exited {
        return StepReply::Answered(Ok(response));
    }
    let restored = match restore_state(generation, &state, size).await {
        Ok(()) => send_held(generation, &mut state).await,
        Err(error) => Err(error),
    };
    let still_open = shared
        .open()
        .is_ok_and(|open| open.number() == generation.number());
    if restored.is_err() || !still_open {
        // The answering connection is gone or going; its end or its
        // replacement attaches again. Every attach still reports its outcome.
        state.report(app, window_id);
        return StepReply::Answered(Ok(response));
    }
    state.sent = Some(SentSize {
        size: state.size,
        connection: (shared.serial(), generation.number()),
    });
    state.move_to(app, window_id, LinkState::Attached);
    StepReply::Answered(Ok(response))
}

/// Sends what the record asks for that the daemon forgot with the old
/// connection, on `generation`, stopping at the first write that fails.
async fn restore_state(
    generation: &Generation,
    state: &RecordState,
    attached_at: Option<TerminalSize>,
) -> Result<(), IpcError> {
    let resize = state
        .size
        .filter(|size| Some(*size) != attached_at)
        .map(|size| ClientMessage::Resize {
            cols: size.cols,
            rows: size.rows,
        });
    let restoring = [
        visibility_restore(state.visible),
        state.palette.clone().map(ClientMessage::SetPalette),
        resize,
    ];
    for message in restoring.into_iter().flatten() {
        generation.write(&message).await?;
    }
    Ok(())
}

/// Writes the held input, oldest first, on `generation`. Each input is let
/// go of only once it is written; a write that fails, or a generation that
/// ended, leaves the rest held for the next attach.
async fn send_held(generation: &Generation, state: &mut RecordState) -> Result<(), IpcError> {
    while let Some(input) = state.first_pending() {
        generation.write(input).await?;
        state.sent_first();
    }
    state.release_bound_if_drained();
    Ok(())
}

/// Attaches `generation` at `size` (`None` keeps the window's own size), as
/// a step of its opening when `take` is given.
async fn request_attach(
    generation: &Generation,
    window_id: &str,
    size: Option<TerminalSize>,
    take: Option<Take>,
) -> StepReply<Result<AttachResponse, IpcError>> {
    // Recorded before the request goes out: the daemon may stream output
    // for the window between its `attached` reply and this task waking up,
    // and the reader needs the window id to route it.
    generation.set_window_id(Some(window_id.to_owned()));
    let message = ClientMessage::AttachView {
        window_id: window_id.to_owned(),
        size,
    };
    let reply = generation
        .request_step(&message, PendingKind::Attach, take)
        .await;
    // Only an answer that attached nothing takes the window back: with no
    // answer the generation is ending, and its end reports the window.
    reply.map(|answer| match attached(&answer) {
        Some(response) => Ok(response),
        None => {
            generation.set_window_id(None);
            Err(answer.err().unwrap_or(IpcError::UnexpectedReply))
        }
    })
}

/// Settles one pending approval request on `generation`, the open one if
/// there is one. With none, nothing is written and the answer is
/// [`IpcError::NotConnected`] as no answer, which the caller treats like any
/// other unanswered decision.
async fn request_decide_approval(
    generation: Option<&Generation>,
    request_id: &str,
    approved: bool,
) -> StepReply<Result<(), IpcError>> {
    let Some(generation) = generation else {
        return StepReply::Unanswered(IpcError::NotConnected);
    };
    let message = ClientMessage::ApprovalDecision {
        request_id: request_id.to_owned(),
        approved,
    };
    generation
        .request_step(&message, PendingKind::DecideApproval, None)
        .await
        .map(|answer| match answer {
            Ok(Reply::Done) => Ok(()),
            Ok(_) => Err(IpcError::UnexpectedReply),
            Err(error) => Err(error),
        })
}

/// How a frame reaches a connection: weighed against the connection's
/// settled revision first, so a frame its peer cannot parse is never written.
mod wire {
    use tokio::io::{AsyncWrite, AsyncWriteExt};
    use yagura_protocol::{
        ClientMessage, encode_correlated_client_message, frame_encode, required_protocol_revision,
    };

    use super::{IpcError, client_message_wire_tag};
    use crate::pool_outcomes::{ProtocolRevisionGate, protocol_revision_gate};

    /// Why a frame was not written whole.
    #[derive(Debug)]
    pub(super) enum WriteError {
        /// The settled revision cannot carry the frame: no byte was written.
        Refused { request_type: String },
        /// The write failed. Only the generation reads the error itself.
        Io(std::io::Error),
    }

    impl From<WriteError> for IpcError {
        fn from(error: WriteError) -> Self {
            match error {
                WriteError::Refused { request_type } => Self::Unsupported { request_type },
                WriteError::Io(_) => Self::Write,
            }
        }
    }

    /// Refuses `message` when `revision` -- the one the connection settled,
    /// `None` before it did -- cannot carry it.
    pub(super) fn gate(message: &ClientMessage, revision: Option<u32>) -> Result<(), WriteError> {
        if protocol_revision_gate(revision, required_protocol_revision(message))
            == ProtocolRevisionGate::Refused
        {
            return Err(WriteError::Refused {
                request_type: client_message_wire_tag(&encode_correlated_client_message(
                    message, None,
                )),
            });
        }
        Ok(())
    }

    /// Writes `message` as one frame carrying `correlation_id`, unless
    /// [`gate`] refuses it, in which case no byte is written.
    pub(super) async fn write_frame<W: AsyncWrite + Unpin + ?Sized>(
        writer: &mut W,
        message: &ClientMessage,
        correlation_id: Option<u64>,
        revision: Option<u32>,
    ) -> Result<(), WriteError> {
        gate(message, revision)?;
        let body = encode_correlated_client_message(message, correlation_id);
        writer
            .write_all(&frame_encode(&body))
            .await
            .map_err(WriteError::Io)
    }
}

/// The message that puts a re-attached window back to the visibility it last
/// asked for, or `None` when it never asked and the daemon's default stands.
fn visibility_restore(last_visible: Option<bool>) -> Option<ClientMessage> {
    last_visible.map(|visible| ClientMessage::SetVisible { visible })
}

/// The wire `type` tag of an already-encoded client message body, read back
/// from the JSON the encoder already produced rather than a
/// second `ClientMessage`-to-tag mapping kept in step with it by hand.
///
/// Never panics: a body this crate did not itself produce (which the encoder
/// never yields) reads back as the empty string rather than a tag invented here.
fn client_message_wire_tag(body: &[u8]) -> String {
    yagura_protocol::read_frame_label(body)
        .tag
        .unwrap_or_default()
}

/// Locks a blocking slot, recovering from poisoning: a holder writes a slot in one store of
/// a value computed before anything that can fail, so a panic leaves the old value or the new.
fn lock_blocking<T>(slot: &std::sync::Mutex<T>) -> std::sync::MutexGuard<'_, T> {
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

#[cfg(test)]
mod host_tests;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::host::fake::FakeHost;
    use crate::window_record::WindowRecord;

    /// A message the baseline revision cannot carry.
    fn frame_gated_message() -> ClientMessage {
        ClientMessage::SetVisible { visible: true }
    }

    /// Sends `message` through a writer over an in-memory pipe and returns
    /// the outcome with every byte that reached the other end.
    async fn send_and_collect(
        message: &ClientMessage,
        settled: Option<u32>,
    ) -> (Result<(), IpcError>, Vec<u8>) {
        use tokio::io::AsyncReadExt;
        let (mut near, mut far) = tokio::io::duplex(64 * 1024);
        let outcome = wire::write_frame(&mut near, message, None, settled)
            .await
            .map_err(IpcError::from);
        drop(near);
        let mut written = Vec::new();
        far.read_to_end(&mut written)
            .await
            .expect("the far end reads until the writer is gone");
        (outcome, written)
    }

    #[tokio::test]
    async fn a_frame_the_settled_revision_cannot_carry_is_refused_before_any_byte_is_written() {
        let (outcome, written) = send_and_collect(&frame_gated_message(), None).await;
        assert!(
            matches!(outcome, Err(IpcError::Unsupported { .. })),
            "got {outcome:?}"
        );
        assert!(written.is_empty(), "wrote {written:?}");
    }

    #[tokio::test]
    async fn a_frame_is_written_once_the_current_generation_settled_a_revision_that_carries_it() {
        let settled = Some(yagura_protocol::PROTOCOL_REVISION);
        let message = frame_gated_message();
        let (outcome, written) = send_and_collect(&message, settled).await;
        assert!(outcome.is_ok(), "got {outcome:?}");
        assert_eq!(
            written,
            yagura_protocol::frame_encode(&yagura_protocol::encode_client_message(&message))
        );
    }

    #[tokio::test]
    async fn a_daemon_one_revision_behind_is_not_sent_a_room_removal() {
        let message = ClientMessage::ClearChatRoom {
            room_id: "main".to_owned(),
        };
        let behind = Some(yagura_protocol::CHAT_ROOM_REMOVAL_MIN_REVISION - 1);
        let (refused, written) = send_and_collect(&message, behind).await;
        assert!(
            matches!(refused, Err(IpcError::Unsupported { .. })),
            "got {refused:?}"
        );
        assert!(written.is_empty(), "wrote {written:?}");

        let level = Some(yagura_protocol::CHAT_ROOM_REMOVAL_MIN_REVISION);
        let (sent, _) = send_and_collect(&message, level).await;
        assert!(sent.is_ok(), "got {sent:?}");
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

    #[tokio::test]
    async fn is_connected_is_false_for_a_freshly_created_control_client() {
        let client = IpcClient::new();
        assert!(!client.is_connected().await);
    }

    #[tokio::test]
    async fn is_connected_is_false_for_a_freshly_created_window_client() {
        let client = IpcClient::new_window(Arc::new(WindowRecord::new()));
        assert!(!client.is_connected().await);
    }

    /// A control client open on a generation over an in-memory stream, with
    /// the far end of that stream to play the daemon on. Answers are handed to
    /// the generation the way its reader hands them over, so what a request
    /// returns is measured apart from how frames are read.
    fn wired() -> (
        IpcClient,
        Arc<Generation>,
        tokio::io::DuplexStream,
        impl Sized,
    ) {
        let (near, far) = tokio::io::duplex(1 << 20);
        let (generation, reader) = Generation::wired(near);
        let client = IpcClient::wired(Arc::clone(&generation));
        (client, generation, far, reader)
    }

    /// The next request written to `peer`, with the correlation id it carries.
    async fn next_request(peer: &mut tokio::io::DuplexStream) -> (Option<u64>, ClientMessage) {
        use tokio::io::AsyncReadExt;
        let mut length = [0u8; 4];
        peer.read_exact(&mut length).await.expect("frame length");
        let mut body = vec![0u8; u32::from_le_bytes(length) as usize];
        peer.read_exact(&mut body).await.expect("frame body");
        yagura_protocol::decode_correlated_client_message(&body).expect("a client frame")
    }

    /// Plays the daemon for one round trip: reads the request, then hands
    /// `outcome` to `generation` under the request's key and `kind`. Returns
    /// the request that arrived.
    async fn answer(
        generation: &Generation,
        peer: &mut tokio::io::DuplexStream,
        kind: PendingKind,
        outcome: Result<Reply, IpcError>,
    ) -> ClientMessage {
        let (key, request) =
            yagura_test_support::ipc::reply_within("request", next_request(peer)).await;
        generation.settle(key, kind, outcome);
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
            status_boot: None,
            mcp_connected: None,
            status_reports_missing: None,
            queued_count: None,
            task_report: None,
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

    // A daemon that returns keys answers under the key of the request; an
    // answer under another key, or under none, answers nothing waiting here.
    #[tokio::test]
    async fn an_answer_meets_the_request_only_under_the_key_the_request_carried() {
        let (client, generation, mut peer, _reader) = wired();

        let (listed, ()) = tokio::join!(client.list_windows(), async {
            let (key, request) =
                yagura_test_support::ipc::reply_within("request", next_request(&mut peer)).await;
            assert_eq!(request, ClientMessage::ListWindows);
            let key = key.expect("a request carries a correlation id");
            generation.settle(
                Some(key.wrapping_add(1)),
                PendingKind::ListWindows,
                Ok(Reply::WindowList(vec![window_info("other")])),
            );
            generation.settle(
                None,
                PendingKind::ListWindows,
                Ok(Reply::WindowList(vec![window_info("unkeyed")])),
            );
            generation.settle(
                Some(key),
                PendingKind::ListWindows,
                Ok(Reply::WindowList(vec![window_info("w1")])),
            );
        });

        assert_eq!(listed.expect("list_windows"), vec![window_info("w1")]);
    }

    #[tokio::test]
    async fn window_requests_hand_back_what_the_daemon_answered() {
        let (client, generation, mut peer, _reader) = wired();
        let host = FakeHost::new("window-requests");

        let (windows, sent) = tokio::join!(
            client.list_windows(),
            answer(
                &generation,
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
                &generation,
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
                &generation,
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
                Default::default(),
                None,
                Some("vendor-a".to_owned()),
            ),
            answer(
                &generation,
                &mut peer,
                PendingKind::OpenWindow,
                Ok(Reply::WindowOpened(OpenWindowReply {
                    window_id: "w2".to_owned(),
                    name: None,
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
                on_name_taken: Default::default(),
            }
        );
        assert_eq!(opened.expect("open_window").window_id, "w2");

        let (opened, sent) = tokio::join!(
            client.open_window_from_profile(
                "shell".to_owned(),
                None,
                None,
                Default::default(),
                Some("fast".to_owned()),
                None,
            ),
            answer(
                &generation,
                &mut peer,
                PendingKind::OpenWindow,
                Ok(Reply::WindowOpened(OpenWindowReply {
                    window_id: "w3".to_owned(),
                    name: None,
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
                on_name_taken: Default::default(),
            }
        );
        assert_eq!(opened.expect("open_window_from_profile").window_id, "w3");

        // The daemon's stored name, not the requested one, is what comes back.
        let (renamed, sent) = tokio::join!(
            client.rename_window("w1", "build"),
            answer(
                &generation,
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
                &generation,
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
            client.get_daemon_health(&host),
            answer(
                &generation,
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
            client.get_daemon_health(&host),
            answer(
                &generation,
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

        let (behind, sent) = tokio::join!(
            client.get_daemon_health(&host),
            answer(
                &generation,
                &mut peer,
                PendingKind::GetDaemonHealth,
                Err(IpcError::Unsupported {
                    request_type: "get_daemon_health".to_owned()
                })
            )
        );
        assert_eq!(sent, ClientMessage::GetDaemonHealth);
        assert!(
            behind
                .expect("a daemon whose revision cannot carry the request has no problem to report")
                .problems
                .is_empty()
        );

        let (updated, sent) = tokio::join!(
            client.update_window_layout("w1", "{}".to_owned()),
            answer(
                &generation,
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
        let (client, generation, mut peer, _reader) = wired();

        let (closed, sent) = tokio::join!(
            client.close_window("w1", Some(true)),
            answer(
                &generation,
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
                &generation,
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
                &generation,
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
        let decided = decided.into_result();
        assert!(decided.is_ok(), "got {decided:?}");
    }

    #[tokio::test]
    async fn destructive_requests_report_the_daemons_rejection() {
        let (client, generation, mut peer, _reader) = wired();

        let (closed, _) = tokio::join!(
            client.close_window("w1", None),
            answer(
                &generation,
                &mut peer,
                PendingKind::CloseWindow,
                Err(rejected("window_not_found"))
            )
        );
        assert!(refused(&closed, "window_not_found"), "got {closed:?}");

        let (interrupted, _) = tokio::join!(
            client.interrupt_window("w1"),
            answer(
                &generation,
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
                &generation,
                &mut peer,
                PendingKind::DecideApproval,
                Err(rejected("approval_not_pending"))
            )
        );
        let decided = decided.into_result();
        assert!(refused(&decided, "approval_not_pending"), "got {decided:?}");
    }

    #[tokio::test]
    async fn chat_requests_hand_back_what_the_daemon_answered() {
        let (client, generation, mut peer, _reader) = wired();
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
                &generation,
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
                &generation,
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
                &generation,
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
                &generation,
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
        let (client, generation, mut peer, _reader) = wired();
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
            ticket: None,
        };

        let (plans, sent) = tokio::join!(
            client.list_plans(),
            answer(
                &generation,
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
                &generation,
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
                &generation,
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
                &generation,
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
        let (client, generation, mut peer, _reader) = wired();
        let alias = LaunchOptionAlias {
            alias: "fast".to_owned(),
            value: "model-fast".to_owned(),
        };

        let (choices, sent) = tokio::join!(
            client.list_model_choices("shell".to_owned(), 5),
            answer(
                &generation,
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
                &generation,
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
                &generation,
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
                &generation,
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
                &generation,
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
                &generation,
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
                &generation,
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
        let (client, generation, mut peer, _reader) = wired();

        let (settings, sent) = tokio::join!(
            client.get_approval_settings(),
            answer(
                &generation,
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
                &generation,
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
                &generation,
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
        let (client, generation, mut peer, _reader) = wired();

        let (selected, sent) = tokio::join!(
            client.copy_selection(),
            answer(
                &generation,
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
                &generation,
                &mut peer,
                PendingKind::CopySelection,
                Ok(Reply::SelectionText(None))
            )
        );
        assert_eq!(selected.expect("copy_selection"), None);
    }

    #[tokio::test]
    async fn an_attached_client_reports_the_window_it_attached_to() {
        let (client, generation, mut peer, _reader) = wired();
        let host = FakeHost::new("attach");
        let size = Some(TerminalSize { cols: 80, rows: 24 });

        let (attached, sent) = tokio::join!(
            client.request_attach(&host, "w1", size),
            answer(
                &generation,
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

    // A request written before the disconnect may have been carried out, so
    // it ends as lost rather than as never sent.
    #[tokio::test]
    async fn disconnect_releases_the_connection_and_fails_the_request_in_flight() {
        use crate::host::fake::{Reaction, ScriptedDaemon};
        use tokio::sync::Notify;

        let reached = Arc::new(Notify::new());
        let daemon = ScriptedDaemon::new({
            let reached = Arc::clone(&reached);
            move |request| match request {
                ClientMessage::ListWindows => Reaction::replies_after(
                    Arc::clone(&reached),
                    Arc::new(Notify::new()),
                    Vec::new(),
                ),
                _ => None,
            }
        });
        let host = FakeHost::served_by("disconnect", &daemon).await;
        let client = Arc::new(IpcClient::new());
        client.connect(&host).await.expect("connect");
        assert!(client.is_connected().await);
        let generation = client.shared.open().expect("the open generation");

        let (listed, ()) = tokio::join!(
            yagura_test_support::ipc::reply_within("the held list", client.list_windows()),
            async {
                yagura_test_support::ipc::reply_within("the held list", reached.notified()).await;
                client.disconnect().await;
            }
        );
        assert!(
            matches!(listed, Err(IpcError::ConnectionLost)),
            "got {listed:?}"
        );
        assert!(!client.is_connected().await);

        // A caller still holding the ended generation is refused before
        // anything is written: unlike a request lost after its frame went
        // out, whose decision the daemon may have applied.
        let asked = generation
            .request(&ClientMessage::ListWindows, PendingKind::ListWindows)
            .await;
        let sent = generation.write(&ClientMessage::ListWindows).await;
        assert!(
            matches!(asked, Err(IpcError::NotConnected)),
            "a request on an ended generation: got {asked:?}"
        );
        assert!(
            matches!(sent, Err(IpcError::NotConnected)),
            "a write on an ended generation: got {sent:?}"
        );

        // With no open generation a decision is written nowhere and gets no
        // answer, which leaves it unconfirmed like one lost in flight.
        let decided = client.decide_approval("r1", true).await;
        assert!(
            matches!(decided, StepReply::Unanswered(IpcError::NotConnected)),
            "a decision with no open generation: got {decided:?}"
        );
        let outcome = crate::pool_outcomes::map_decide_approval_result(decided);
        assert!(
            matches!(
                outcome,
                Ok(crate::pool_outcomes::DecideApprovalOutcome::Unconfirmed)
            ),
            "a decision with no open generation maps to: got {outcome:?}"
        );
    }

    #[test]
    fn the_revision_the_daemon_answers_is_settled_for_the_current_generation() {
        let outcome = revision_probe_outcome(&Ok(Reply::ProtocolRevision {
            revision: 9,
            boot: None,
        }));
        assert_eq!(outcome.map(|(revision, _)| revision), Some(9));
    }

    #[test]
    fn a_daemon_that_does_not_know_the_probe_is_credited_with_the_baseline() {
        let outcome = revision_probe_outcome(&Err(rejected(ERROR_CODE_UNKNOWN_TYPE)));
        assert_eq!(
            outcome.map(|(revision, _)| revision),
            Some(PROTOCOL_REVISION_BASELINE)
        );
    }

    #[test]
    fn a_probe_refused_for_another_reason_settles_nothing() {
        let outcome = revision_probe_outcome(&Err(rejected(ERROR_CODE_INTERNAL)));
        assert_eq!(outcome.map(|(revision, _)| revision), None);
    }

    #[tokio::test]
    async fn a_refused_frame_names_the_request_it_refused() {
        let (outcome, _) = send_and_collect(&frame_gated_message(), None).await;
        assert!(
            matches!(&outcome, Err(IpcError::Unsupported { request_type }) if request_type == "set_visible"),
            "got {outcome:?}"
        );
    }
}
