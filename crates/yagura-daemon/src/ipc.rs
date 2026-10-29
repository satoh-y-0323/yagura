//! IPC server: the local transport implementing the length-prefixed JSON
//! framing (LPJF) protocol on top of the daemon's [`WindowRegistry`].
//!
//! Every request that names a target carries a `window_id`, while a
//! connection is attached to at most one window at a time, so the streaming
//! messages (`frame`) stay connection-scoped. Per-window
//! resources -- PTY session, detection engine, status fan-out and the attach
//! slot -- live in [`WindowEntry`]; this module owns none of them.
//!
//! The transport, the endpoint protection and the peer check are reached
//! through the [`yagura_os`] boundary, so this module names no platform type
//! of its own.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt, WriteHalf};
use tokio::sync::{MutexGuard as AsyncMutexGuard, Semaphore, broadcast, mpsc, oneshot, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;
use yagura_os::{IpcTransport, OsError, PeerAuth, PlatformIpc};

use yagura_protocol::{
    ApprovalRequester, ApprovalRequesterKind, ClientMessage, ControlKey, DaemonMessage, FrameError,
    HANDOFF_NOTE_MAX_BYTES, HANDOFF_REFERENCE_MAX_BYTES, HandoffDelivery, InstructionNoticeState,
    MessageError, OnNameTaken, PROTOCOL_REVISION, PointerButton, PointerInput, PointerKind,
    ProfileInfo, RestoreCandidate, TerminalSize, WindowInfo, WindowStatus, check_input_bounds,
    decode_client_message, encode_daemon_message, frame_decode, frame_encode, is_valid_source,
    validate_control_keys, validate_handoff_target, validate_handoff_text,
    validate_launch_option_value, validate_send_text_body,
};

mod approval_settings;
pub mod chat;
mod hook;
mod inbox;
mod mcp_registration;
mod model;
mod notice_queue;
mod permit;
pub mod plan;
mod profile_detail;
mod profile_enabled;
mod profile_job;
pub mod report;
mod store_gate;
pub mod ticket;

use crate::approval::{
    ApprovalBroker, ApprovalLimits, ApproverSink, DecideError, Ticket, WithdrawReason,
};
use crate::config::DaemonConfig;
use crate::control::{
    self, ControlError, DenyReason, DestructiveTool, GateVerdict, RateBucket, RequestOrigin,
};
use crate::detector::VtScreen;
use crate::detector::layers::DetectionLayers;
use crate::detector::screen_view::{self, View};
use crate::handoff::{
    DELIVERY_REACTION_STATUSES, EnvelopeFrom, READY_STATUSES, StartupEvidence, check_envelope_len,
    compose_envelope, compose_task_notice, is_ready,
};
use crate::health;
use crate::inbox::{Parcel, ParcelEnd, PushBudget, Withdrawal};
use crate::input::{SubmitOutcome, SubmitPlan, plan_raw_write};
use crate::input_encode::{self, PointerOutcome, ViewPage, WheelOutcome};
use crate::live_policy::{self, PolicyCell};
use crate::palette_store::{self, PaletteStore};
use crate::plan_permit::{self, ChangeKind, PermitError};
use crate::profile_delivery::{
    ProfileDelivery, WakeMode, launch_context_injection, launch_prompt_injection,
};
use crate::pty::{InputState, PtyError};
use crate::store::{self, SnapshotWindow, StoreError};
use crate::ticket_store::{TicketChange, TicketQuery};
use crate::token::StatusToken;
use crate::waiter::{self, ReactionOutcome, StatusAt, WaitOutcome};
use crate::window::{
    CliInjection, ComposerResidue, EnvelopeWait, HooksRoute, InputOrigin, LaunchIdentity,
    McpConnectionClaim, NameRefusal, NotPlaced, NotPlacedReason, OpenWindowRequest, OpenedWindow,
    RejectedArgument, RequestedCwd, SendStage, SubmitJudgment, SubmitPlacement, TaskOrigin,
    WakeCommand, WindowEntry, WindowLiveness, WindowOpenError, WindowProvenance, WindowRegistry,
    WindowRenameError, resolve_existing_dir,
};

use permit::{
    ApprovedOutcome, GateOutcome, OpenDescription, OpenPurpose, OpenVerdict, OpenWindow, Permit,
    PermittedAction, Verdict, await_verdict, gate_approved, gate_destructive, gate_open,
};

/// A connection accepted from the platform transport, named through the OS
/// boundary.
type Connection = <PlatformIpc as PeerAuth>::Connection;

/// A bound endpoint owning its name, named through the OS boundary.
type Listener = <PlatformIpc as IpcTransport>::Listener;

/// Upper bound on a single `send_text` payload. Independent of the frame size
/// limit: a payload that fits in a frame can still be more text than any
/// window is meant to receive in one go.
const SEND_TEXT_MAX_BYTES: usize = 1024 * 1024;

/// How many failed `identify` authentications one connection may accumulate
/// before it is closed. Bounds the number of guesses a caller that does not
/// hold a window's token can make on a single connection.
const IDENTIFY_FAILURE_LIMIT: u8 = 3;

/// The single client-facing text for an `identify` that did not authenticate.
/// Fixed and generic: it says nothing about which part of the credential was
/// wrong.
const IDENTIFY_AUTH_FAILED_MESSAGE: &str = "identify token is malformed or does not match";

/// Number of characters of the window's text `get_window_status` returns as
/// its `last_output_summary` field.
const LAST_OUTPUT_SUMMARY_MAX_CHARS: usize = 500;

/// Enough of the window's text to hold [`LAST_OUTPUT_SUMMARY_MAX_CHARS`]
/// characters of any width.
const LAST_OUTPUT_SUMMARY_MAX_BYTES: usize = LAST_OUTPUT_SUMMARY_MAX_CHARS * 4;

/// The last `max_chars` characters of `text`.
fn last_chars(text: &str, max_chars: usize) -> String {
    let skip = text.chars().count().saturating_sub(max_chars);
    text.chars().skip(skip).collect()
}

/// Number of trailing lines `read_output` returns when the request omits
/// `lines`.
const READ_OUTPUT_DEFAULT_LINES: usize = 100;

/// Upper bound on the candidate payload one `list_restore_candidates`
/// response carries, measured as the summed JSON byte length of the candidates
/// themselves.
///
/// The store bounds a read to 256 rows, but not the byte size of a row's
/// text columns, so a tampered store could otherwise turn one request into a
/// multi-hundred-megabyte frame. Candidates are dropped from the row that
/// crosses this bound onward (in `position` order), which keeps the answer a
/// usable prefix instead of an error. 2 MiB sits far below the 16 MiB frame
/// limit, leaving room for the rest of the message.
pub const RESTORE_REPLY_BUDGET_BYTES: usize = 2_097_152;

/// The single client-facing text for a restore-candidate read that failed.
/// Fixed and generic: the underlying error names a path and an
/// SQLite failure, neither of which may cross the protocol boundary.
const RESTORE_READ_FAILED_MESSAGE: &str = "failed to read the previous session's windows";

/// How many daemon -> client frames may sit in a connection's outbound queue
/// before back pressure applies. The queue is what decouples this connection's
/// reader from a client that has stopped draining the connection; the bound
/// is what keeps that decoupling from turning into unbounded memory growth.
const OUTBOUND_QUEUE_CAPACITY: usize = 64;

/// Errors returned by [`IpcServer::run`].
#[derive(Debug, Error)]
pub enum IpcError {
    /// The endpoint could not be claimed, or no further connection could be
    /// accepted on it.
    #[error("failed to serve the ipc endpoint: {0}")]
    EndpointBindFailed(#[source] OsError),
    /// Failed to build the protection applied to the endpoint.
    #[error("failed to build the ipc endpoint security: {0}")]
    SecurityInitFailed(#[source] OsError),
}

/// Outcome of processing a single decoded client frame.
enum FrameOutcome {
    /// Keep the connection open and keep reading further frames.
    Continue,
    /// Close this connection (malformed frame).
    Disconnect,
    /// A `shutdown` message was received; the whole server must stop.
    Shutdown,
}

/// The window a connection is currently attached to, together with the
/// generation it holds in that window's attach slot.
struct Attachment {
    entry: Arc<WindowEntry>,
    generation: u64,
}

/// Everything one connection owns for the duration of its session: the
/// attachment (at most one window), its view, and the subscriptions that feed
/// the connection's `select!` branches. None of them is live until the
/// connection attaches, which is what keeps frames and status changes away
/// from unattached connections.
struct ConnectionState {
    /// Names this connection in the approval ledger, which is what lets a
    /// pending request be counted against the connection that asked for it
    /// and decided only by a connection that registered as an approver.
    conn_id: u64,
    attachment: Option<Attachment>,
    /// What this connection was shown of the attached window. Made anew by
    /// every attach, because a `frame` names no window.
    view: View,
    /// Changes once output reached the attached window's model.
    arrivals: Option<watch::Receiver<u64>>,
    /// Changes once the attached window's child exited.
    exit_rx: Option<watch::Receiver<bool>>,
    /// Changes once something was written to the attached window's child.
    writes_rx: Option<watch::Receiver<u64>>,
    status_rx: Option<broadcast::Receiver<StatusAt>>,
    kick_rx: Option<oneshot::Receiver<()>>,
    /// Handles of this connection's outstanding `wait_for_status` tasks. Owned
    /// here so that the concurrency limit is per connection and every task is
    /// aborted when the connection ends -- a waiter that outlived its
    /// connection would write into a dead queue.
    waiters: Vec<JoinHandle<()>>,
    /// Who this connection acts as, which is what the destructive gate judges
    /// a request against. Starts out unproven and only ever widens.
    origin: RequestOrigin,
    /// Held from the moment this connection identifies as a window until it
    /// ends, however it ends, so that window's `mcp_connected` stays exact.
    mcp_claim: Option<McpConnectionClaim>,
    /// How many `identify` attempts this connection has failed to
    /// authenticate. Only credential failures count, so that a caller naming a
    /// window that does not exist cannot be disconnected by its own typo.
    identify_failures: u8,
    /// The peer's executable, canonicalized, as captured once when the
    /// connection was accepted. `None` when it could not be determined,
    /// which `register_approver` treats as a mismatch (fail-closed).
    peer_executable: Option<PathBuf>,
    /// Whether this connection's first refused `register_approver` has
    /// already been logged, so a client retrying in a loop cannot flood the
    /// log with its own failures.
    approver_refusal_logged: bool,
    /// Flipped to `true` once, when this connection ends. The detached
    /// approval, handoff and `send_message` tasks this connection spawned have
    /// no other way to learn that their requester is gone, since they are
    /// deliberately not tracked in `waiters` and never aborted.
    closed: watch::Sender<bool>,
    /// How many windows this connection has opened for a handoff that has
    /// neither been delivered nor torn down yet. Reference-counted because a
    /// slot outlives the connection: the detached task holding it still has
    /// to give it back once it has closed the window it opened.
    open_handoffs: Arc<AtomicUsize>,
    /// How many `send_message` writes this connection started and that have
    /// not answered yet. They run detached and are never aborted, so a write
    /// whose requester left can still take its own body back; they count
    /// against the same per-connection budget as `waiters`.
    open_sends: Arc<AtomicUsize>,
}

impl ConnectionState {
    fn new(conn_id: u64, peer_executable: Option<PathBuf>) -> Self {
        Self {
            conn_id,
            attachment: None,
            view: View::new(),
            arrivals: None,
            exit_rx: None,
            writes_rx: None,
            status_rx: None,
            kick_rx: None,
            waiters: Vec::new(),
            origin: RequestOrigin::External,
            mcp_claim: None,
            identify_failures: 0,
            peer_executable,
            approver_refusal_logged: false,
            closed: watch::channel(false).0,
            open_handoffs: Arc::new(AtomicUsize::new(0)),
            open_sends: Arc::new(AtomicUsize::new(0)),
        }
    }

    /// The window this connection is attached to, if any.
    fn attached_entry(&self) -> Option<Arc<WindowEntry>> {
        self.attachment
            .as_ref()
            .map(|attachment| attachment.entry.clone())
    }

    /// Number of waiters still running, after forgetting the finished ones.
    /// Completed tasks must not count against the limit, or a long-lived
    /// connection would exhaust its budget permanently.
    fn live_waiter_count(&mut self) -> usize {
        self.waiters.retain(|handle| !handle.is_finished());
        self.waiters.len() + self.open_sends.load(Ordering::Acquire)
    }

    /// Takes one unit of the per-connection wait budget for a detached
    /// `send_message`, or `None` when the budget is spent.
    fn try_take_send_slot(&mut self, max: usize) -> Option<CounterGuard> {
        self.waiters.retain(|handle| !handle.is_finished());
        let room = max.saturating_sub(self.waiters.len());
        CounterGuard::try_take(&self.open_sends, room)
    }

    fn abort_waiters(&mut self) {
        for handle in self.waiters.drain(..) {
            handle.abort();
        }
    }
}

/// Result of queueing a droppable streaming message (`status_changed`) on the
/// outbound queue.
enum StreamSend {
    /// The frame is queued and will be written in order.
    Queued,
    /// The queue was full, so the frame was dropped (back pressure).
    Dropped,
    /// The writer is gone: nothing can be sent on this connection any more.
    Closed,
}

/// What a request queued for approval runs once it is approved, carrying the
/// arguments its handler already validated.
enum PendingAction {
    SendText(String),
    SendKeys(Vec<ControlKey>),
    Interrupt,
    Close(bool),
    /// A handoff envelope, plus the requesting window's id for the audit
    /// line (the envelope's own bytes are never logged) and the budget slot
    /// of this handoff, given back when the action is dropped.
    SendHandoff {
        text: String,
        requester: Option<String>,
        slot: Option<HandoffSlot>,
    },
}

impl PendingAction {
    /// The gated tool this action performs.
    fn tool(&self) -> DestructiveTool {
        match self {
            Self::SendText(_) => DestructiveTool::SendMessage,
            Self::SendKeys(_) => DestructiveTool::SendKeys,
            Self::Interrupt => DestructiveTool::InterruptWindow,
            Self::Close(_) => DestructiveTool::CloseWindow,
            Self::SendHandoff { .. } => DestructiveTool::SendHandoff,
        }
    }
}

/// How a pending approval request ended, once the ledger entry has been
/// settled.
// Built once per request and consumed at once; boxing the answer would put an
// indirection in every caller's pattern and buy nothing.
#[allow(clippy::large_enum_variant)]
enum ApprovalEnd {
    /// An approver approved it.
    Approved,
    /// It was denied, or nobody decided before the deadline (or the ledger
    /// entry vanished without an answer, which leaves the requester the same
    /// fail-closed outcome). Carries the answer the requester must get.
    Refused(DaemonMessage),
    /// The requesting connection closed first, so nobody is left to answer.
    RequesterGone,
}

/// One `send_handoff` request's caller-supplied arguments, kept together so
/// the handler reads them as the single message they arrived in.
struct HandoffRequest {
    window_id: Option<String>,
    profile: Option<String>,
    cwd: Option<String>,
    name: Option<String>,
    model: Option<String>,
    effort: Option<String>,
    reference: String,
    note: String,
}

/// A launch profile resolved into everything the registry needs, with
/// nothing spawned yet: deciding what to launch and launching it are separate
/// steps, so a request that will be refused never starts a child.
struct PlannedOpen {
    request: OpenWindowRequest,
    extra_env: Vec<(String, String)>,
    /// The profile's `[mcp]` and `[hooks]` tables prepared for this launch,
    /// in the order they are appended to the argv, rendered right before
    /// spawn once the paths they name are as the CLI sees them. A table that
    /// is absent, or cannot be prepared, contributes nothing.
    profile_injections: Vec<crate::profile::UnrenderedInjection>,
    /// How the paths `profile_injections` name are converted for the CLI;
    /// `None` when the profile hands them over unchanged.
    host_paths: Option<mcp_registration::PathConversion>,
    /// What tells, right before spawn, whether the CLI's own settings already
    /// name the `[mcp]` injection's server; `None` when the profile has no
    /// `absent` guard or no `[mcp]` injection was planned.
    mcp_absent: Option<mcp_registration::AbsentRead>,
    /// The injections that follow the profile's own (a launch prompt), in
    /// argv order.
    injections: Vec<CliInjection>,
    /// What the `[hooks]` injection depends on, checked right before spawn:
    /// `None` when it depends on nothing.
    hooks_gate: Option<HooksGate>,
    /// How the profile arranges hooks for its CLI, before the gate is read
    /// and the argv is spawned.
    hooks: HooksRoute,
    /// How the window's credentials reach its CLI.
    credentials: crate::profile::CredentialsMode,
    /// The profile's `[credentials] launch_args`, for the spawn's argv only.
    launch_args: Vec<String>,
    /// Recorded on the window, so a restore can launch it through the same
    /// profile again.
    launch: LaunchIdentity,
    /// The profile's `[delivery]` table: how the window's doors answer.
    delivery: Option<ProfileDelivery>,
    /// What a wake by command runs, when the profile wakes that way.
    wake_command: Option<WakeCommand>,
    /// How the launch turns the other instances' bridge registrations off,
    /// read right before spawn; `None` when the profile cannot say.
    bridge_scan: Option<mcp_registration::BridgeScan>,
    /// What the profile cannot do on this host
    /// ([`crate::profile::shim_limits`]).
    limits: Vec<yagura_protocol::ProfileLimit>,
    /// Decides, right before spawn, whether the window is handed the room's
    /// introduction.
    chat: Arc<chat::ChatRuntime>,
}

impl PlannedOpen {
    /// The limit on `capability`, when the profile has one on this host.
    fn limit(
        &self,
        capability: yagura_protocol::ProfileCapability,
    ) -> Option<&yagura_protocol::ProfileLimit> {
        self.limits
            .iter()
            .find(|limit| limit.capability == capability)
    }
}

/// A registration whose settings-file entries must hold for a launch to keep
/// its `[hooks]` injection, with the profile environment its file tools run
/// under.
struct HooksGate {
    /// The profile id, named when the gate takes the hooks away.
    profile: String,
    registration: crate::mcp_registration::McpRegistrationSpec,
    env: Vec<(std::ffi::OsString, std::ffi::OsString)>,
    /// The slot an out-of-date registration is rewritten under.
    writes: Arc<Semaphore>,
}

/// Why a launch's `[hooks]` are left out by its [`HooksGate`].
enum HooksLeftOut {
    /// No entry of this instance is in the CLI's settings.
    NotRegistered,
    /// The entries are there but out of date, and could not be rewritten
    /// (or read): the fixed reason.
    NotRewritten(&'static str),
}

/// What [`IpcServer::handle_open_task_window`] learns while it still holds
/// the plan's lock: the change permit itself (so the lock stays held across
/// the async spawn that follows), the plan's cwd, and the task's three
/// unresolved assignment sources.
struct OpenTaskPrelude {
    permit: plan_permit::ChangePermit,
    cwd: String,
    title: String,
    parent: Option<crate::plan::Assignment>,
    user: Option<crate::plan::Assignment>,
    role: Option<String>,
    /// The task's request. When present, a `[yagura task]` notice is queued
    /// on the window this handler opens; `None` queues none.
    instruction: Option<String>,
}

/// Why [`IpcServer::handle_open_task_window`]'s locked prelude refused,
/// beyond a permit refusal.
enum OpenTaskOutcome {
    Permit(PermitError),
    Store(StoreError),
    /// Unreachable in practice: the store's own mutex is held for the whole
    /// closure this is returned from, so nothing else could have deleted the
    /// plan in between the existence check and this read.
    Vanished,
    /// `task_id` names no task of this plan.
    TaskNotFound,
    /// The task's currently bound window can still take a request; opening a
    /// second one before that window is closed or its child exits is refused
    /// rather than binding a second window to the same task, so two writers
    /// never work on one task's files at once.
    AlreadyBound,
}

/// The daemon-wide collaborators one destructive request is judged against.
///
/// Borrowed rather than owned so that a connection's own task and a detached
/// one can each build it out of what they already hold, and so the gate stays
/// a single function both of them call.
struct GateContext<'a> {
    config: &'a DaemonConfig,
    broker: &'a ApprovalBroker,
    registry: &'a WindowRegistry,
    origin: &'a RequestOrigin,
    conn_id: u64,
    policy: &'a PolicyCell,
}

/// Everything a profile-addressed handoff needs to start its window once
/// both gates allow it.
///
/// Owned throughout: while an approver is asked, the handoff waits on a task
/// of its own, holding the slot it reserved. Nothing has started yet.
struct ProfileHandoffOpen {
    planned: PlannedOpen,
    provenance: WindowProvenance,
    outbound: Arc<Outbound>,
    registry: Arc<WindowRegistry>,
    broker: Arc<ApprovalBroker>,
    config: DaemonConfig,
    origin: RequestOrigin,
    conn_id: u64,
    /// Raised when the requesting connection closes.
    closed: watch::Receiver<bool>,
    envelope: String,
    /// What the approver of the envelope reads: the lines naming what would
    /// be started, then the envelope.
    approval_text: String,
    slot: HandoffSlot,
    policy: Arc<PolicyCell>,
}

impl ProfileHandoffOpen {
    /// Ends the handoff before any window exists: the slot is given back,
    /// then `message` answers the requester.
    fn abandon(self, message: &DaemonMessage) {
        abandon_handoff(&self.outbound, self.slot, message);
    }
}

/// Everything [`deliver_handoff`] needs to put an envelope in a window's
/// inbox and answer once that window's CLI took it, or never will: no gate is
/// consulted here, so this carries none of the daemon-wide collaborators a
/// gate call needs.
struct DeliveryJob {
    outbound: Arc<Outbound>,
    registry: Arc<WindowRegistry>,
    config: DaemonConfig,
    entry: Arc<WindowEntry>,
    envelope: String,
    requester: Option<String>,
    slot: Option<HandoffSlot>,
    /// Raised when the requester left: its connection closed, or its CLI
    /// ended the turn the request was made in.
    closed: watch::Receiver<bool>,
    /// The licence this delivery runs under, taken by value so a job cannot
    /// be assembled for a handoff no gate allowed.
    permit: Permit<permit::SendHandoff>,
}

/// What a typed delivery's submit loop could observe of its envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TypedOutcome {
    /// After the submit key, the composer emptied or the window started
    /// working.
    Confirmed,
    /// The submit key was pressed, but no such evidence followed.
    Unconfirmed,
    /// The submit key was pressed, and no reaction was looked for or could be.
    NotObserved,
    /// The submit key was never pressed for it.
    NotWritten,
}

/// What one typed delivery's submit loop ended with.
///
/// Assembled once, where the loop leaves, so every reader reads the same
/// values rather than each recomputing part of them.
struct DeliveryVerdict {
    delivery: TypedOutcome,
    attempts: u8,
    /// The clock the submit sequence itself stamped, so an elapsed time
    /// computed from it measures the same span the writer measured.
    submitted_at: Instant,
    status_before_submit: WindowStatus,
    ready_hold_wait_ms: u128,
}

/// What [`run_delivery`] ended with.
enum DeliveryEnd<'a> {
    /// The submit-and-confirm loop reached a verdict.
    Finished {
        verdict: DeliveryVerdict,
        /// What the last attempt left in the composer, for a caller that
        /// retries the same delivery later.
        residue: Option<CarriedResidue>,
        /// Whether it ended because someone else's text is in the composer.
        foreign_draft: bool,
    },
    /// The watched connection closed before the first byte was written.
    /// Never produced when [`run_delivery`] is called with `closed: None`.
    Aborted,
    /// Writing the envelope itself failed (e.g. the child exited mid-write).
    ///
    /// Carries the delivery lock onward rather than releasing it here: the
    /// caller may still have to tear this window down, and no other delivery
    /// may write into a window while that teardown runs.
    WriteFailed(PtyError, AsyncMutexGuard<'a, ()>),
    /// The text declined under the delivery lock, so nothing was written.
    Skipped,
    /// Nothing was written: the delivery was turned away before its first
    /// byte, either because too many deliveries already wait for this
    /// window's turn or because the shared budget it needs is spent.
    Exhausted(Scarce),
}

/// Which bounded resource turned a delivery away.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Scarce {
    /// The window's queue for its delivery turn is full.
    TurnWaiters,
    /// The daemon-wide budget the delivery draws on is spent.
    SharedBudget,
}

/// The daemon-wide budget a delivery draws one unit from once it holds its
/// window's turn -- never while it only waits for it, so a window that never
/// frees its turn holds at most one unit of it.
enum SharedBudget {
    /// The delivery already holds what it needs (or needs nothing).
    NotNeeded,
    Counter {
        counter: Arc<AtomicUsize>,
        max: usize,
    },
}

/// An envelope one [`run_delivery`] call left in a composer, owned so a later
/// call can pick it up as its own residue.
#[derive(Clone)]
struct CarriedResidue {
    body: String,
    write_seq: u64,
}

/// One outstanding-handoff slot: a handoff this connection has accepted and
/// not yet answered -- the approval wait, the readiness wait, the write and
/// the delivery observation included. A window opened from a profile always
/// belongs to such a handoff, so the older invariant (at most N
/// profile-opened windows awaiting delivery) still holds.
///
/// Held from acquisition until the answer is sent -- the approval wait
/// included, so a handoff awaiting a decision still counts against both
/// budgets. Both budgets are given back when this is dropped.
struct HandoffSlot {
    _per_conn: CounterGuard,
    /// Held from acceptance for a handoff that opens a window: the budget
    /// also bounds the windows (child processes) waiting for a delivery.
    _daemon: Option<CounterGuard>,
    /// For a handoff to an existing window: the daemon-wide budget and its
    /// limit, drawn on only once the delivery holds that window's turn.
    daemon_when_turn_held: Option<(Arc<AtomicUsize>, usize)>,
}

/// Which of the two budgets refused a slot.
enum SlotExhausted {
    PerConn,
    Daemon,
}

/// One unit taken from a bounded counter, given back when dropped.
struct CounterGuard {
    counter: Arc<AtomicUsize>,
}

impl HandoffSlot {
    /// Takes one slot from the per-connection budget and then one from the
    /// daemon-wide budget.
    ///
    /// # Errors
    ///
    /// Returns the [`SlotExhausted`] naming the budget that was already at
    /// its limit. Each step is a compare-and-swap that increments only
    /// while the counter is below its limit, so two connections racing for
    /// the last slot can never both take it. The first step is held as a
    /// guard, so a refusal from the second gives it back by dropping that
    /// guard: a slot exists only when both budgets granted one.
    fn try_acquire(
        per_conn: &Arc<AtomicUsize>,
        per_conn_max: usize,
        daemon: &Arc<AtomicUsize>,
        daemon_max: usize,
    ) -> Result<Self, SlotExhausted> {
        let per_conn =
            CounterGuard::try_take(per_conn, per_conn_max).ok_or(SlotExhausted::PerConn)?;
        let daemon = CounterGuard::try_take(daemon, daemon_max).ok_or(SlotExhausted::Daemon)?;
        Ok(Self {
            _per_conn: per_conn,
            _daemon: Some(daemon),
            daemon_when_turn_held: None,
        })
    }

    /// Takes the per-connection slot only; the daemon-wide budget is left to
    /// [`Self::shared_budget`], drawn once the delivery holds its turn.
    ///
    /// # Errors
    ///
    /// [`SlotExhausted::PerConn`] when this connection is at its limit.
    fn try_acquire_per_conn(
        per_conn: &Arc<AtomicUsize>,
        per_conn_max: usize,
        daemon: &Arc<AtomicUsize>,
        daemon_max: usize,
    ) -> Result<Self, SlotExhausted> {
        let per_conn =
            CounterGuard::try_take(per_conn, per_conn_max).ok_or(SlotExhausted::PerConn)?;
        Ok(Self {
            _per_conn: per_conn,
            _daemon: None,
            daemon_when_turn_held: Some((Arc::clone(daemon), daemon_max)),
        })
    }

    /// What the delivery still has to draw from the daemon-wide budget.
    fn shared_budget(&self) -> SharedBudget {
        match &self.daemon_when_turn_held {
            Some((counter, max)) => SharedBudget::Counter {
                counter: Arc::clone(counter),
                max: *max,
            },
            None => SharedBudget::NotNeeded,
        }
    }
}

impl SlotExhausted {
    /// The refusal the caller is answered with, naming the budget that ran
    /// out so an operator can tell one busy connection apart from a busy
    /// daemon.
    fn to_message(&self) -> DaemonMessage {
        let limit = match self {
            Self::PerConn => "per-connection",
            Self::Daemon => "daemon-wide",
        };
        error_msg(
            "resource_exhausted",
            &format!("too many handoffs are in flight ({limit} limit)"),
        )
    }
}

impl CounterGuard {
    /// Takes one unit if the counter is below `max`, or `None` if it is not.
    fn try_take(counter: &Arc<AtomicUsize>, max: usize) -> Option<Self> {
        crate::window::take_one_below(counter, max).then(|| Self {
            counter: Arc::clone(counter),
        })
    }
}

impl Drop for CounterGuard {
    fn drop(&mut self) {
        self.counter.fetch_sub(1, Ordering::AcqRel);
    }
}

/// The refusal for a delivery turned away before its first byte.
fn delivery_exhausted_msg(scarce: Scarce) -> DaemonMessage {
    match scarce {
        Scarce::TurnWaiters => error_msg(
            "resource_exhausted",
            "too many deliveries are already waiting on this window",
        ),
        Scarce::SharedBudget => SlotExhausted::Daemon.to_message(),
    }
}

/// The refusal for a request past this connection's `max_waiters_per_conn`.
fn pending_waits_exhausted_msg() -> DaemonMessage {
    error_msg(
        "resource_exhausted",
        "this connection already holds the maximum number of pending waits",
    )
}

/// The refusal for a size outside the window limits.
fn size_out_of_range_msg() -> DaemonMessage {
    error_msg(
        "invalid_argument",
        &format!(
            "cols must be within {}..={max} and rows within {}..={max}",
            yagura_protocol::MIN_WINDOW_COLS,
            yagura_protocol::MIN_WINDOW_ROWS,
            max = yagura_protocol::MAX_WINDOW_DIMENSION
        ),
    )
}

/// The write side of one connection: daemon -> client frames are queued here
/// and written to the connection by a dedicated task ([`write_loop`]).
///
/// # Why a queue
///
/// Reading and writing the same connection from a single `select!` loop
/// deadlocks against a client that stops draining its end: the daemon's
/// `write_all` blocks on the full connection buffer, which stops the loop
/// from polling its read branch, which stops the client's own writes from
/// ever being consumed.
/// Queueing makes every send non-blocking, so the reader keeps draining the
/// client's frames no matter how far behind the client's reads are.
///
/// # Back pressure
///
/// The queue holds at most [`OUTBOUND_QUEUE_CAPACITY`] frames, and the two
/// classes of frames handle exhaustion differently:
///
/// * `status_changed` pushes are *droppable*: the next change republishes the
///   current status.
/// * Replies to client requests, and the parts of a view's `frame`, are *not*
///   droppable: silently discarding one would leave the client waiting for an
///   answer -- or a frame to acknowledge -- that never comes. A view sends at
///   most one frame before it is acknowledged, so frames never pile up here.
///   A message that cannot be queued therefore marks the connection broken
///   and it is closed at the next checkpoint (fail-closed) -- a client that
///   stops reading while it keeps issuing requests cannot be served
///   correctly.
///
/// A closed queue (the writer task exited after a write error) is likewise
/// terminal and closes the connection.
struct Outbound {
    tx: mpsc::Sender<Vec<u8>>,
    /// Shared by every handle onto the same connection, so a handle made for
    /// one request still marks the connection itself broken.
    broken: Arc<AtomicBool>,
    /// `Some` on a handle made for one request ([`Outbound::for_one_answer`]):
    /// set by its first reply, after which its replies are dropped.
    answered: Option<AtomicBool>,
}

impl Outbound {
    fn new(tx: mpsc::Sender<Vec<u8>>) -> Self {
        Self {
            tx,
            broken: Arc::new(AtomicBool::new(false)),
            answered: None,
        }
    }

    /// A handle onto the same connection for one request that two parties may
    /// race to answer: whichever replies first is the request's answer.
    fn for_one_answer(&self) -> Arc<Self> {
        Arc::new(Self {
            tx: self.tx.clone(),
            broken: Arc::clone(&self.broken),
            answered: Some(AtomicBool::new(false)),
        })
    }

    /// Queues a reply to a client request. Failure to queue is terminal for
    /// the connection (see the back-pressure rules above).
    fn reply(&self, msg: &DaemonMessage) {
        if let Some(answered) = &self.answered
            && answered.swap(true, Ordering::AcqRel)
        {
            return;
        }
        if self.tx.try_send(encode_frame(msg)).is_err() {
            self.broken.store(true, Ordering::Relaxed);
        }
    }

    /// Queues a streaming frame, reporting back-pressure to the caller
    /// instead of closing the connection.
    fn stream(&self, msg: &DaemonMessage) -> StreamSend {
        match self.tx.try_send(encode_frame(msg)) {
            Ok(()) => StreamSend::Queued,
            Err(mpsc::error::TrySendError::Full(_)) => StreamSend::Dropped,
            Err(mpsc::error::TrySendError::Closed(_)) => {
                self.broken.store(true, Ordering::Relaxed);
                StreamSend::Closed
            }
        }
    }

    /// Queues an already encoded wire frame, as [`Outbound::reply`] does.
    fn reply_encoded(&self, frame: Vec<u8>) {
        if self.tx.try_send(frame).is_err() {
            self.broken.store(true, Ordering::Relaxed);
        }
    }

    fn is_broken(&self) -> bool {
        self.broken.load(Ordering::Relaxed)
    }
}

impl ApproverSink for Outbound {
    /// An unsolicited approval frame is queued through the same non-droppable
    /// path as a reply: a human who never learns that a request exists is the
    /// worst failure this feature has. A queue that cannot take it therefore
    /// marks the connection broken, and reporting that back evicts this
    /// approver from the ledger.
    fn push(&self, msg: &DaemonMessage) -> bool {
        self.reply(msg);
        !self.is_broken()
    }
}

/// The IPC server: serves the protocol against a [`WindowRegistry`] over the
/// platform's local transport, governed by the [`DaemonConfig`] the registry
/// was built with.
/// Constructed once and driven to completion by [`IpcServer::run`].
pub struct IpcServer {
    registry: Arc<WindowRegistry>,
    endpoint: String,
    config: DaemonConfig,
    /// Daemon-wide `StatusEvent` rate bucket. Shared by every
    /// connection: a per-connection bucket would be reset by
    /// simply reconnecting.
    rate_bucket: Arc<Mutex<RateBucket>>,
    /// Mints one identifier per accepted connection, so that the approval
    /// ledger can name a connection without holding a reference to it.
    next_conn_id: AtomicU64,
    /// The pending-approval ledger every connection shares: a request
    /// submitted on one connection is decided on another.
    ///
    /// Reference-counted because the detached task that finishes a
    /// profile-addressed handoff reaches the same ledger from outside any
    /// connection's own task.
    broker: Arc<ApprovalBroker>,
    /// The live approval policy every destructive decision reads, seeded
    /// from `config`'s startup fields and mutated only through the approval
    /// settings IPC handlers.
    policy: Arc<PolicyCell>,
    /// How many windows this daemon has opened for a handoff that has
    /// neither been delivered nor torn down yet, across every connection.
    open_handoffs: Arc<AtomicUsize>,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
    /// The chat room's runtime state. [`IpcServer::new`] builds this with no
    /// chat at all (`ChatRuntime::unwired`, no store touched); the
    /// chat-serving entry point is [`IpcServer::new_with_chat`].
    chat: Arc<chat::ChatRuntime>,
    /// The plan ledger's runtime state. [`IpcServer::new`] and
    /// [`IpcServer::new_with_chat`] build this disabled the same way `chat`
    /// is; the ledger-serving entry point is [`IpcServer::new_with_ledger`].
    ledger: Arc<plan::PlanRuntime>,
    /// The ticket board's runtime state, disabled until
    /// [`IpcServer::with_tickets`] wires one in.
    tickets: Arc<ticket::TicketRuntime>,
    /// Admits `plan::load_role_and_profile_state_blocking`'s blocking reads
    /// so they never run on a request's own async task, sized the same way
    /// `WindowRegistry`'s own `workspace_walks` is.
    role_and_profile_reads: Semaphore,
    /// Admits reads of a window's terminal model (`read_output` and the
    /// status summary), each on a blocking thread, sized like
    /// `role_and_profile_reads`.
    window_text_reads: Semaphore,
    /// Admits `list_model_choices`' listing commands, sized by
    /// [`DaemonConfig::model_choices_max_concurrent`]. `Arc`-wrapped so a
    /// listing's detached task can hold an owned permit
    /// (`try_acquire_owned`) for as long as its own run takes, independent
    /// of the connection that asked for it.
    model_choices_listing: Arc<Semaphore>,
    /// Admits at most one `list_mcp_registrations` listing across the whole
    /// daemon at a time, held for as long as that listing's own row reads
    /// take (see [`mcp_registration::handle_list_mcp_registrations`]).
    mcp_registration_listing: Arc<Semaphore>,
    /// Admits a `list_mcp_registrations` listing's own row reads, sized by
    /// [`DaemonConfig::mcp_registration_status_max_concurrent`].
    mcp_registration_row_reads: Arc<Semaphore>,
    /// Admits the window launches reading their CLI's registrations, sized by
    /// [`DaemonConfig::mcp_registration_status_max_concurrent`]; apart from
    /// `mcp_registration_row_reads` so a slow status read never holds a launch.
    mcp_launch_listings: Arc<Semaphore>,
    /// Admits at most one `run_mcp_registration` operation (register /
    /// update / remove) across the whole daemon at a time, held for as long
    /// as that operation's own steps and status re-read take (see
    /// [`mcp_registration::handle_run_mcp_registration`]).
    mcp_registration_changes: Arc<Semaphore>,
    /// Serializes every write of the registrations' settings files: a user's
    /// operation waits for it, a rewrite at launch or listing gives up after
    /// [`DaemonConfig::mcp_listing_timeout`].
    mcp_registration_writes: Arc<Semaphore>,
    /// Admits the profile reads of `run_hook`, each on a blocking thread, sized
    /// like `role_and_profile_reads`.
    hook_profile_reads: Semaphore,
    /// Every window's inbox of reports from the windows it opened or owns
    /// tasks for. Memory only, and always enabled.
    reports: Arc<report::ReportInboxes>,
    /// Admits one `get_daemon_health` pseudoconsole check at a time: the check
    /// reads and hashes files on a blocking thread, and any connection may ask.
    health_checks: Semaphore,
    /// Where `set_default_palettes` is saved, so a daemon started again
    /// answers with it. Disabled until [`IpcServer::with_palette_store`]:
    /// the palettes then live in the registry's memory only.
    palette_store: store_gate::StoreGate<PaletteStore>,
    /// Held only so the registry's notice launcher, which refers to it
    /// weakly, stays live for as long as this server does.
    _notice_agents: Arc<notice_queue::NoticeAgents>,
}

impl IpcServer {
    /// Builds a server around an already-constructed [`WindowRegistry`].
    ///
    /// Window lifecycle is the registry's responsibility: in production every
    /// window is opened through this server once the endpoint is secured
    /// (`main.rs`), while tests open `echo_child`-backed windows directly.
    pub fn new(
        registry: Arc<WindowRegistry>,
        endpoint: impl Into<String>,
        config: DaemonConfig,
    ) -> Self {
        let chat = Arc::new(chat::ChatRuntime::unwired(&config));
        Self::new_with_chat(registry, endpoint, config, chat)
    }

    /// Builds a server the same way [`IpcServer::new`] does, but serving
    /// `send_chat` / `broadcast_chat` / `read_chat` through `chat` instead of
    /// `unsupported`. `chat` is built by the caller
    /// (`main.rs` / `Harness::start_with_chat`) so opening its store -- a
    /// blocking operation -- never happens on this constructor's own,
    /// synchronous path. The plan ledger starts out disabled the same way
    /// `chat` does in [`IpcServer::new`]; [`IpcServer::with_ledger`] wires
    /// one in.
    pub fn new_with_chat(
        registry: Arc<WindowRegistry>,
        endpoint: impl Into<String>,
        config: DaemonConfig,
        chat: Arc<chat::ChatRuntime>,
    ) -> Self {
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        let rate_bucket = Arc::new(Mutex::new(RateBucket::new(
            config.status_event_burst,
            config.status_event_per_sec,
        )));
        // The registry's nonce is the only per-boot suffix, so a window id
        // and a request id from one daemon instance cannot disagree on it.
        let broker = Arc::new(ApprovalBroker::new(registry.boot_nonce()));
        let policy = Arc::new(PolicyCell::from_config(&config));
        let role_and_profile_reads = Semaphore::new(config.max_windows);
        let window_text_reads = Semaphore::new(config.max_windows);
        let hook_profile_reads = Semaphore::new(config.max_windows);
        let model_choices_listing = Arc::new(Semaphore::new(config.model_choices_max_concurrent));
        let mcp_registration_listing = Arc::new(Semaphore::new(1));
        let mcp_registration_row_reads = Arc::new(Semaphore::new(
            config.mcp_registration_status_max_concurrent,
        ));
        let mcp_launch_listings = Arc::new(Semaphore::new(
            config.mcp_registration_status_max_concurrent,
        ));
        let mcp_registration_changes = Arc::new(Semaphore::new(1));
        let mcp_registration_writes = Arc::new(Semaphore::new(1));
        let reports = Arc::new(report::ReportInboxes::new(Arc::clone(&registry)));
        let ledger = Arc::new(plan::PlanRuntime::disabled());
        let notice_agents = notice_queue::NoticeAgents::install(&registry, Arc::clone(&reports));

        Self {
            registry,
            endpoint: endpoint.into(),
            config,
            rate_bucket,
            next_conn_id: AtomicU64::new(1),
            broker,
            policy,
            open_handoffs: Arc::new(AtomicUsize::new(0)),
            shutdown_tx,
            shutdown_rx,
            chat,
            ledger,
            tickets: Arc::new(ticket::TicketRuntime::disabled()),
            role_and_profile_reads,
            window_text_reads,
            model_choices_listing,
            mcp_registration_listing,
            mcp_registration_row_reads,
            mcp_launch_listings,
            mcp_registration_changes,
            mcp_registration_writes,
            hook_profile_reads,
            reports,
            health_checks: Semaphore::new(1),
            palette_store: store_gate::StoreGate::disabled(),
            _notice_agents: notice_agents,
        }
    }

    /// [`IpcServer::new`]'s counterpart for the plan ledger: serves the plan
    /// tools through `ledger` instead of `unsupported`, with chat left
    /// disabled the way `new` leaves it. `Harness::start_with_ledger` is
    /// this entry point's test-side counterpart.
    pub fn new_with_ledger(
        registry: Arc<WindowRegistry>,
        endpoint: impl Into<String>,
        config: DaemonConfig,
        ledger: Arc<plan::PlanRuntime>,
    ) -> Self {
        Self::new(registry, endpoint, config).with_ledger(ledger)
    }

    /// Swaps in an already-built plan ledger runtime, replacing the disabled
    /// one every constructor above starts with. `ledger` is built by the
    /// caller (`main.rs`) the same way `chat` is built by
    /// [`IpcServer::new_with_chat`]'s caller, so opening its store -- a
    /// blocking operation -- never happens on this method's own path. The
    /// production entry point chains this onto [`IpcServer::new_with_chat`]
    /// so a daemon boot serves both chat and the plan ledger together.
    #[must_use]
    pub fn with_ledger(mut self, ledger: Arc<plan::PlanRuntime>) -> Self {
        self.ledger = ledger;
        self
    }

    /// Swaps in an already-built ticket board runtime. `tickets` is built by
    /// the caller (`main.rs`) on a blocking thread, as the plan ledger is.
    #[must_use]
    pub fn with_tickets(mut self, tickets: Arc<ticket::TicketRuntime>) -> Self {
        self.tickets = tickets;
        self
    }

    /// Saves every `set_default_palettes` to `store` from now on. `store` is
    /// opened by the caller (`main.rs`) on a blocking thread, which also
    /// hands what it held to [`WindowRegistry::set_default_palettes`].
    #[must_use]
    pub fn with_palette_store(mut self, store: PaletteStore) -> Self {
        self.palette_store = store_gate::StoreGate::opened(store);
        self
    }

    /// Claims the endpoint and runs the accept loop on it. Each accepted
    /// connection is handled on its own spawned task. Returns `Ok(())` once a
    /// `shutdown` message has been processed on any connection.
    ///
    /// The endpoint is protected for the current user, so the current user
    /// is also the owner every peer must match.
    ///
    /// The production entry point is [`IpcServer::run_with_listener`]: it
    /// serves an endpoint the caller already claimed, which is what keeps a
    /// losing multi-instance race from orphaning a child process.
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::SecurityInitFailed`] when the current user's
    /// identifier or the endpoint protection cannot be resolved (the daemon
    /// never serves an unprotected endpoint) and
    /// [`IpcError::EndpointBindFailed`] when the endpoint name is already taken.
    pub async fn run(self: Arc<Self>) -> Result<(), IpcError> {
        let ipc = PlatformIpc::default();
        let owner = ipc
            .current_user_id()
            .map_err(IpcError::SecurityInitFailed)?;
        // The protection is built before the endpoint exists and consumed by
        // `bind`, so the endpoint is never reachable unprotected.
        let security = ipc
            .endpoint_security()
            .map_err(IpcError::SecurityInitFailed)?;
        let listener = ipc
            .bind(&self.endpoint, security)
            .await
            .map_err(IpcError::EndpointBindFailed)?;
        self.run_with_listener(listener, owner).await
    }

    /// Runs the accept loop on an endpoint the caller already claimed.
    ///
    /// This is the main entry point for the daemon: the endpoint is claimed
    /// before any window can be opened, ensuring that a losing
    /// multi-instance race never orphans a child process.
    ///
    /// # Arguments
    ///
    /// * `listener` - The bound endpoint, obtained from `IpcTransport::bind`
    /// * `owner` - The user identifier the endpoint belongs to; a peer whose
    ///   identifier differs, or cannot be determined, is disconnected before
    ///   any frame is read
    ///
    /// # Errors
    ///
    /// Returns [`IpcError::EndpointBindFailed`] when the listener can no longer
    /// produce connections.
    pub async fn run_with_listener(
        self: Arc<Self>,
        mut listener: Listener,
        owner: String,
    ) -> Result<(), IpcError> {
        let ipc = PlatformIpc::default();
        let mut shutdown_rx = self.shutdown_rx.clone();
        let owner: Arc<str> = owner.into();
        // Started here, where a runtime exists and no approver can have
        // connected yet.
        chat::spawn_participation_watch(
            Arc::clone(&self.chat),
            Arc::clone(&self.registry),
            Arc::clone(&self.broker),
            self.shutdown_rx.clone(),
        );

        loop {
            if *shutdown_rx.borrow() {
                return Ok(());
            }

            tokio::select! {
                // Cancel-safe: losing this branch to the shutdown one leaves
                // the pending instance on the listener.
                accepted = ipc.accept(&mut listener) => {
                    match accepted {
                        Ok(connection) => {
                            let server = self.clone();
                            let owner = Arc::clone(&owner);
                            tokio::spawn(async move {
                                server.handle_connection(connection, &owner).await;
                            });
                        }
                        // `IpcTransport::accept` only returns `Err` for a
                        // fatal instance-*creation* failure; a listener
                        // that cannot serve must fail loud, not spin.
                        Err(err) => return Err(IpcError::EndpointBindFailed(err)),
                    }
                }
                _ = shutdown_rx.changed() => {
                    return Ok(());
                }
            }
        }
    }

    /// Peer verification: the peer's user identifier must match `owner`.
    /// Fails closed, including for a peer whose identifier cannot be
    /// determined.
    fn verify_peer(connection: &Connection, owner: &str) -> bool {
        let peer = PlatformIpc::default().peer_id(connection);
        yagura_os::is_authorized_peer(peer.as_deref(), owner)
    }

    /// Drives a single accepted connection until it disconnects, is
    /// superseded, sends `shutdown`, or sends a frame that mandates
    /// disconnection per the malformed-frame rules.
    async fn handle_connection(self: Arc<Self>, connection: Connection, owner: &str) {
        if !Self::verify_peer(&connection, owner) {
            tracing::warn!("rejecting ipc connection: peer verification failed");
            return;
        }

        // Captured before the split consumes the `Connection`, and
        // canonicalized here so no later frame has to: one local filesystem
        // lookup per accepted connection, on the task that already ran
        // `verify_peer`'s synchronous OS calls, is too short to be worth a
        // `spawn_blocking` hop.
        let peer_executable = PlatformIpc::default()
            .peer_executable(&connection)
            .and_then(|path| path.canonicalize().ok());

        // Splitting the connection lets the write side block independently of
        // the read side: everything sent goes through the queue `write_loop`
        // drains, so a client that stops reading cannot stall the reader.
        let (mut reader, writer) = tokio::io::split(connection);
        let (tx, rx) = mpsc::channel::<Vec<u8>>(OUTBOUND_QUEUE_CAPACITY);
        // Detached: dropping `outbound` at the end of this function closes the
        // queue, and the writer task then flushes what is still queued (e.g.
        // the `superseded` error) before releasing the connection.
        tokio::spawn(write_loop(writer, rx));
        // Shared with this connection's waiter tasks, which is the
        // only reason it is reference-counted.
        let outbound = Arc::new(Outbound::new(tx));

        let mut read_buf: Vec<u8> = Vec::new();
        let mut recv_chunk = [0u8; 8192];
        let mut state = ConnectionState::new(
            self.next_conn_id.fetch_add(1, Ordering::Relaxed),
            peer_executable,
        );

        'conn: loop {
            if outbound.is_broken() {
                break 'conn;
            }

            // The one place a frame is sent after a change: the change, or the
            // acknowledgement of the last frame, brought the loop round.
            self.push_frames(&outbound, &mut state, false).await;
            if outbound.is_broken() {
                break 'conn;
            }

            tokio::select! {
                changed = next_change(&mut state.arrivals) => {
                    if !changed {
                        state.arrivals = None;
                    }
                    state.view.note_change();
                }

                changed = next_change(&mut state.exit_rx) => {
                    if !changed {
                        state.exit_rx = None;
                    }
                    state.view.note_change();
                }

                // A write releases a synchronized update; nothing else here
                // needs to hear of one.
                changed = next_change(&mut state.writes_rx), if state.view.is_held() => {
                    if !changed {
                        state.writes_rx = None;
                    }
                    state.view.note_change();
                }

                status = recv_status(&mut state.status_rx) => {
                    match status {
                        Ok(at) => {
                            // The attachment defines which window a status change belongs to; a
                            // change delivered after it moved on is dropped, not mislabelled.
                            let Some(entry) = state.attached_entry() else {
                                continue;
                            };
                            // Built from the delivered value alone: reading the
                            // entry back here would stamp this push with a
                            // generation newer than the change it reports.
                            let msg = DaemonMessage::StatusChanged {
                                window_id: entry.window_id().to_string(),
                                status: at.status.status(),
                                unknown_reason: at.status.unknown_reason(),
                                status_rev: Some(at.generation),
                            };
                            // A status change dropped by back pressure is
                            // handled like a skipped one below: the next change
                            // republishes the current status.
                            if let StreamSend::Closed = outbound.stream(&msg) {
                                break 'conn;
                            }
                        }
                        // A skipped intermediate status is not worth a
                        // recovery protocol: the next change (or the attach of
                        // a fresh connection) republishes the current one.
                        Err(broadcast::error::RecvError::Lagged(_)) => {}
                        Err(broadcast::error::RecvError::Closed) => {
                            state.status_rx = None;
                        }
                    }
                }

                () = recv_kick(&mut state.kick_rx) => {
                    outbound.reply(
                        &error_msg("superseded", "another connection has attached to this window"),
                    );
                    break 'conn;
                }

                read_result = reader.read(&mut recv_chunk) => {
                    match read_result {
                        Ok(0) | Err(_) => break 'conn,
                        Ok(n) => {
                            read_buf.extend_from_slice(&recv_chunk[..n]);
                            loop {
                                match frame_decode(&read_buf) {
                                    Ok(Some((body, consumed))) => {
                                        let body_owned = body.to_vec();
                                        read_buf.drain(..consumed);
                                        let outcome = self
                                            .handle_frame(&outbound, &body_owned, &mut state)
                                            .await;
                                        match outcome {
                                            FrameOutcome::Continue => {}
                                            FrameOutcome::Disconnect => break 'conn,
                                            FrameOutcome::Shutdown => {
                                                // Every window's child process
                                                // tree must be terminated
                                                // before exiting.
                                                self.registry.terminate_all().await;
                                                let _ = self.shutdown_tx.send(true);
                                                break 'conn;
                                            }
                                        }
                                    }
                                    Ok(None) => break,
                                    Err(FrameError::TooLarge) => {
                                        outbound.reply(&error_msg(
                                            "frame_too_large",
                                            "frame body exceeds maximum size",
                                        ));
                                        break 'conn;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // A waiter whose connection is gone has nowhere to deliver its answer,
        // so no pending wait may outlive this point. An approval task is the
        // opposite: it is never aborted, because the ledger entry it owns has
        // to be withdrawn (which it does itself, once told the connection is
        // gone) rather than leaked into the approvers' queues forever.
        let _ = state.closed.send(true);
        state.abort_waiters();
        self.broker.unregister_approver(state.conn_id);

        // Clear the slot only while this connection still holds it: a superseded
        // connection sees a *newer* generation, which `clear_attach_if` leaves
        // untouched.
        if let Some(attachment) = state.attachment.take() {
            attachment
                .entry
                .clear_attach_if(attachment.generation)
                .await;
        }
    }

    /// Decodes and dispatches one complete frame body.
    async fn handle_frame(
        &self,
        outbound: &Arc<Outbound>,
        body: &[u8],
        state: &mut ConnectionState,
    ) -> FrameOutcome {
        match decode_client_message(body) {
            Err(MessageError::InvalidJson) => {
                outbound.reply(&error_msg(
                    "invalid_json",
                    "invalid json or missing required field",
                ));
                FrameOutcome::Disconnect
            }
            // Unknown type and direction-violating type are indistinguishable
            // once decoded (both fail `ClientMessage` deserialization) and
            // share the same continue-the-connection handling.
            Err(MessageError::UnknownType) => {
                outbound.reply(&error_msg("unknown_type", "unknown message type"));
                FrameOutcome::Continue
            }
            Ok(ClientMessage::AttachView { window_id, size }) => {
                self.handle_attach_view(outbound, state, &window_id, size)
                    .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::FrameAck { seq }) => {
                state.view.acknowledge(seq);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetPalette(palette)) => {
                if let Some(entry) = attached_or_refuse(outbound, state) {
                    let _ = entry
                        .session()
                        .with_model(move |screen| screen.set_palette(palette))
                        .await;
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetDefaultPalettes(palettes)) => {
                self.handle_set_default_palettes(outbound, palettes).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetVisible { visible }) => {
                state.view.set_visible(visible);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::CopySelection) => {
                if let Some(entry) = attached_or_refuse(outbound, state) {
                    let text = entry
                        .session()
                        .with_model(screen_view::selected_text)
                        .await
                        .flatten();
                    outbound.reply(&DaemonMessage::SelectionText { text });
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ClearSelection) => {
                if let Some(entry) = attached_or_refuse(outbound, state) {
                    let _ = entry
                        .session()
                        .with_model(screen_view::clear_selection)
                        .await;
                    state.view.note_change();
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ScrollView { lines }) => {
                if let Some(entry) = attached_or_refuse(outbound, state) {
                    let _ = entry
                        .session()
                        .with_model(move |screen| screen_view::scroll_view(screen, lines))
                        .await;
                    state.view.note_change();
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::Ping) => {
                outbound.reply(&DaemonMessage::Pong);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::Shutdown) => FrameOutcome::Shutdown,
            Ok(
                msg @ (ClientMessage::Key(_)
                | ClientMessage::Text { .. }
                | ClientMessage::Paste { .. }
                | ClientMessage::Pointer(_)
                | ClientMessage::Wheel(_)
                | ClientMessage::Focus { .. }),
            ) => {
                self.handle_structured_input(outbound, state, &msg).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::Resize { cols, rows }) => {
                self.handle_resize(outbound, state, cols, rows).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::StatusEvent {
                source,
                status,
                window_id,
                token,
            }) => {
                self.handle_status_event(outbound, &token, &source, status, window_id.as_deref())
                    .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::OpenWindow {
                command,
                cwd,
                name,
                vendor,
                detection,
                on_name_taken,
            }) => {
                let request = OpenWindowRequest {
                    command,
                    cwd: cwd.map(|cwd| RequestedCwd::from_caller(PathBuf::from(cwd))),
                    name,
                    on_name_taken,
                    vendor,
                    detection,
                };
                self.handle_open_window(outbound, state, request).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListWindows) => {
                outbound.reply(&DaemonMessage::WindowList {
                    windows: self.registry.list(),
                });
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListDetections) => {
                let listing = DetectionLayers::from_config(&self.config).list();
                outbound.reply(&DaemonMessage::DetectionList {
                    names: listing.names,
                    bundled_names: listing.bundled_names,
                    directory_names: listing.directory_names,
                    warning: listing.warning,
                });
                FrameOutcome::Continue
            }
            Ok(ClientMessage::GetWindowStatus { window_id }) => {
                self.handle_get_window_status(outbound, &window_id).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SendText { window_id, text }) => {
                self.handle_send_text(outbound, state, &window_id, &text);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SendKeys { window_id, keys }) => {
                self.handle_send_keys(outbound, state, &window_id, &keys)
                    .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::InterruptWindow { window_id }) => {
                self.handle_interrupt_window(outbound, state, &window_id)
                    .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::CloseWindow { window_id, force }) => {
                self.handle_close_window(outbound, state, &window_id, force)
                    .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::WaitForStatus {
                window_id,
                target_status,
                timeout_sec,
            }) => {
                self.handle_wait_for_status(
                    outbound,
                    state,
                    &window_id,
                    target_status,
                    timeout_sec,
                );
                FrameOutcome::Continue
            }
            Ok(ClientMessage::RenameWindow { window_id, name }) => {
                self.handle_rename_window(outbound, &window_id, &name).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ReadOutput { window_id, lines }) => {
                self.handle_read_output(outbound, &window_id, lines).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListRestoreCandidates) => {
                self.handle_list_restore_candidates(outbound).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::UpdateWindowLayout {
                window_id,
                layout_json,
            }) => {
                self.handle_update_window_layout(outbound, &window_id, layout_json);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListProfiles) => {
                self.handle_list_profiles(outbound).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::OpenWindowFromProfile {
                profile,
                cwd,
                name,
                model,
                effort,
                on_name_taken,
            }) => {
                self.handle_open_window_from_profile(
                    outbound,
                    state,
                    profile,
                    cwd,
                    name,
                    on_name_taken,
                    crate::profile::LaunchOptionValues { model, effort },
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::Identify { window_id, token }) => {
                self.handle_identify(outbound, state, &window_id, &token)
                    .await
            }
            Ok(ClientMessage::RegisterApprover) => {
                self.handle_register_approver(outbound, state);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SendHandoff {
                window_id,
                profile,
                cwd,
                name,
                model,
                effort,
                reference,
                note,
            }) => {
                self.handle_send_handoff(
                    outbound,
                    state,
                    HandoffRequest {
                        window_id,
                        profile,
                        cwd,
                        name,
                        model,
                        effort,
                        reference,
                        note,
                    },
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ApprovalDecision {
                request_id,
                approved,
            }) => {
                self.handle_approval_decision(outbound, state, &request_id, approved);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SendChat { room_id, body }) => {
                chat::handle_send_chat(
                    &self.chat,
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.policy,
                    outbound,
                    state,
                    room_id,
                    body,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::BroadcastChat { room_id, body }) => {
                chat::handle_broadcast_chat(
                    &self.chat,
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.policy,
                    outbound,
                    state,
                    room_id,
                    body,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ReadChat {
                room_id,
                after_seq,
                limit,
            }) => {
                chat::handle_read_chat(
                    &self.chat,
                    &self.registry,
                    outbound,
                    state,
                    room_id,
                    after_seq,
                    limit,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListChatRooms) => {
                chat::handle_list_chat_rooms(&self.chat, &self.registry, outbound, state).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ClearChatRoom { room_id }) => {
                chat::handle_clear_chat_room(
                    &self.chat,
                    &self.registry,
                    &self.broker,
                    outbound,
                    state,
                    room_id,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::DeleteChatRoom { room_id }) => {
                chat::handle_delete_chat_room(
                    &self.chat,
                    &self.registry,
                    &self.broker,
                    outbound,
                    state,
                    room_id,
                )
                .await;
                FrameOutcome::Continue
            }
            // Answered on every connection without a permission check: a
            // caller has to learn which wire this daemon speaks before it can
            // know which requests are worth making, so gating the answer
            // would deny it exactly where it is needed most. The revision is
            // a property of the build, not of the caller's rights.
            Ok(ClientMessage::GetProtocolRevision) => {
                outbound.reply(&DaemonMessage::ProtocolRevision {
                    revision: PROTOCOL_REVISION,
                });
                FrameOutcome::Continue
            }
            // Answered on every connection without a permission check: the
            // answer names no window, path or user data, and a client has to
            // learn why the daemon cannot open windows before it has any.
            Ok(ClientMessage::GetDaemonHealth) => {
                let conpty_fault = match self.health_checks.acquire().await {
                    Ok(_permit) => tokio::task::spawn_blocking(health::conpty_fault_now)
                        .await
                        .unwrap_or_else(|err| {
                            Some(format!("the pseudoconsole check did not finish: {err}"))
                        }),
                    Err(_) => None,
                };
                outbound.reply(&DaemonMessage::DaemonHealth {
                    problems: health::daemon_problems(conpty_fault.as_deref()),
                });
                FrameOutcome::Continue
            }
            Ok(ClientMessage::DeliverContext { window_id, token }) => {
                if let Some(entry) =
                    self.authenticate_window(outbound, "deliver_context", &token, Some(&window_id))
                {
                    inbox::handle_context(&entry, outbound, self.chat.intro_for(&entry));
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::DeliverTake { window_id, token }) => {
                if let Some(entry) =
                    self.authenticate_window(outbound, "deliver_take", &token, Some(&window_id))
                {
                    inbox::handle_take(&entry, outbound);
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::DeliverWait { window_id, token }) => {
                if let Some(entry) =
                    self.authenticate_window(outbound, "deliver_wait", &token, Some(&window_id))
                {
                    inbox::handle_wait(&entry, outbound, state, self.config.max_waiters_per_conn);
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ReportSession {
                window_id,
                token,
                session,
            }) => {
                if let Some(entry) =
                    self.authenticate_window(outbound, "report_session", &token, Some(&window_id))
                {
                    inbox::handle_report_session(&entry, outbound, &session);
                }
                FrameOutcome::Continue
            }
            Ok(ClientMessage::RunHook {
                window_id,
                token,
                profile,
                name,
                input,
            }) => {
                self.handle_run_hook(outbound, &window_id, &token, &profile, &name, input)
                    .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::CreatePlan {
                title,
                tasks,
                ticket,
            }) => {
                plan::handle_create_plan(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.policy,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    title,
                    tasks,
                    ticket,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::UpdatePlan {
                plan_id,
                title,
                tasks,
            }) => {
                plan::handle_update_plan(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    plan_id,
                    title,
                    tasks,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::GetPlan { plan_id }) => {
                plan::handle_get_plan(
                    &self.ledger,
                    &self.registry,
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    plan_id,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListPlans) => {
                plan::handle_list_plans(&self.ledger, outbound, state).await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::DeletePlan { plan_id }) => {
                plan::handle_delete_plan(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    outbound,
                    state,
                    plan_id,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListRoles) => {
                plan::handle_list_roles(
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SaveRoles {
                roles,
                default_role,
            }) => {
                plan::handle_save_roles(
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    roles,
                    default_role,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetTaskAssignment {
                plan_id,
                task_id,
                assignment,
            }) => {
                plan::handle_set_task_assignment(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    plan_id,
                    task_id,
                    assignment,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ClearTaskAssignment { plan_id, task_id }) => {
                plan::handle_clear_task_assignment(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    plan_id,
                    task_id,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetPlanOwner { plan_id, window_id }) => {
                plan::handle_set_plan_owner(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    plan_id,
                    window_id,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::OpenTaskWindow {
                plan_id,
                task_id,
                name,
            }) => {
                self.handle_open_task_window(outbound, state, plan_id, task_id, name)
                    .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ApprovePlan { plan_id, version }) => {
                plan::handle_approve_plan(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    plan_id,
                    version,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::MarkPlanSeen { plan_id, version }) => {
                plan::handle_mark_plan_seen(
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    plan_id,
                    version,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SendReport {
                outcome,
                summary,
                path,
            }) => {
                report::handle_send_report(
                    &self.reports,
                    &self.ledger,
                    &self.registry,
                    &self.broker,
                    outbound,
                    state,
                    report::ReportBody {
                        outcome,
                        summary,
                        path,
                    },
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::WaitForReport { timeout_sec }) => {
                report::handle_wait_for_report(
                    &self.reports,
                    &self.ledger,
                    &self.config,
                    outbound,
                    state,
                    timeout_sec,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListModelChoices {
                profile,
                request_id,
            }) => {
                model::handle_list_model_choices(
                    &self.config,
                    &self.model_choices_listing,
                    outbound,
                    state,
                    profile,
                    request_id,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SaveModelAliases { profile, aliases }) => {
                model::handle_save_model_aliases(
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    profile,
                    aliases,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetProfileEnabled { profile, enabled }) => {
                profile_enabled::handle_set_profile_enabled(
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    profile,
                    enabled,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::GetProfileDetail { profile }) => {
                profile_detail::handle_get_profile_detail(
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    profile,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetProfileCommand { profile, argv }) => {
                profile_detail::handle_set_profile_command(
                    &self.config,
                    &self.role_and_profile_reads,
                    outbound,
                    state,
                    profile,
                    argv,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::GetApprovalSettings {}) => {
                approval_settings::handle_get_approval_settings(&self.policy, outbound, state);
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetAutoApprove { tool, auto_approve }) => {
                approval_settings::handle_set_auto_approve(
                    &self.config,
                    &self.policy,
                    outbound,
                    state,
                    tool,
                    auto_approve,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::SetPlanApprovalRequired { required }) => {
                approval_settings::handle_set_plan_approval_required(
                    &self.config,
                    &self.policy,
                    outbound,
                    state,
                    required,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListMcpRegistrations { request_id }) => {
                mcp_registration::handle_list_mcp_registrations(
                    &self.config,
                    &self.role_and_profile_reads,
                    &self.mcp_registration_listing,
                    &self.mcp_registration_row_reads,
                    &self.mcp_registration_writes,
                    outbound,
                    state,
                    request_id,
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::RunMcpRegistration {
                profile,
                action,
                request_id,
            }) => {
                mcp_registration::handle_run_mcp_registration(
                    &self.config,
                    &self.role_and_profile_reads,
                    &self.mcp_registration_changes,
                    &self.mcp_registration_writes,
                    outbound,
                    state,
                    mcp_registration::RunRequest {
                        profile,
                        action,
                        request_id,
                    },
                )
                .await;
                FrameOutcome::Continue
            }
            Ok(ClientMessage::ListTicketWorkspaces) => {
                self.handle_ticket(outbound, state, ticket::TicketRequest::ListWorkspaces)
                    .await
            }
            Ok(ClientMessage::ListTickets {
                workspace_id,
                state: ticket_state,
                milestone,
                limit,
            }) => {
                let request = ticket::TicketRequest::List {
                    workspace_id,
                    query: TicketQuery {
                        state: ticket_state,
                        milestone,
                        limit,
                    },
                };
                self.handle_ticket(outbound, state, request).await
            }
            Ok(ClientMessage::GetTicket {
                workspace_id,
                ticket,
            }) => {
                let request = ticket::TicketRequest::Get {
                    workspace_id,
                    number: ticket,
                };
                self.handle_ticket(outbound, state, request).await
            }
            Ok(ClientMessage::CreateTicket {
                title,
                body,
                milestone,
                state: ticket_state,
                fields,
            }) => {
                let request = ticket::TicketRequest::Create(ticket::new_ticket(
                    title,
                    body,
                    milestone,
                    ticket_state,
                    fields,
                ));
                self.handle_ticket(outbound, state, request).await
            }
            Ok(ClientMessage::UpdateTicket {
                ticket,
                title,
                body,
                milestone,
                state: ticket_state,
                fields,
            }) => {
                let request = ticket::TicketRequest::Update {
                    number: ticket,
                    change: TicketChange {
                        title,
                        body,
                        milestone,
                        state: ticket_state,
                        fields: fields.unwrap_or_default(),
                    },
                };
                self.handle_ticket(outbound, state, request).await
            }
            Ok(ClientMessage::AddTicketResult { ticket, text }) => {
                let request = ticket::TicketRequest::AddResult {
                    number: ticket,
                    text,
                };
                self.handle_ticket(outbound, state, request).await
            }
            Ok(ClientMessage::SetMilestone {
                name,
                title,
                description,
                position,
            }) => {
                let request = ticket::TicketRequest::SetMilestone(ticket::milestone(
                    name,
                    title,
                    description,
                    position,
                ));
                self.handle_ticket(outbound, state, request).await
            }
        }
    }

    /// Serves one ticket request through [`ticket::handle_ticket_request`].
    async fn handle_ticket(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        request: ticket::TicketRequest,
    ) -> FrameOutcome {
        ticket::handle_ticket_request(
            &self.tickets,
            &self.registry,
            &self.broker,
            outbound,
            &state.origin,
            request,
        )
        .await;
        FrameOutcome::Continue
    }

    /// Answers `list_restore_candidates` from the previous boot's stored
    /// configuration.
    ///
    /// Read-only in every sense: the store is opened read-only, the `prev`
    /// slot is never rotated or written, and the daemon's live windows are
    /// not consulted at all -- the candidates describe the *previous* boot,
    /// so a window that happens to be open now must not appear in, or be
    /// removed from, this answer.
    ///
    /// Persistence being disabled (no resolved path) is a normal, empty
    /// answer rather than an error: a first run has nothing to restore, and
    /// there is nothing for the client to do about it.
    async fn handle_list_restore_candidates(&self, outbound: &Outbound) {
        let Some(db_path) = self.config.store_db_path_resolved.clone() else {
            outbound.reply(&DaemonMessage::RestoreCandidateList {
                windows: Vec::new(),
                taken_at_epoch_s: None,
            });
            return;
        };

        // SQLite is blocking, and a large store makes this read long enough
        // to matter, so it runs off the async worker; otherwise one restore
        // read would stall every other connection this runtime serves.
        let read = tokio::task::spawn_blocking(move || read_prev_configuration(&db_path)).await;

        let message = match read {
            Ok(Ok((rows, taken_at_epoch_s))) => DaemonMessage::RestoreCandidateList {
                windows: within_reply_budget(rows.into_iter().map(to_restore_candidate).collect()),
                taken_at_epoch_s,
            },
            Ok(Err(err)) => {
                tracing::warn!(error = %err, "failed to read the restore candidates");
                error_msg("internal", RESTORE_READ_FAILED_MESSAGE)
            }
            // The blocking task panicked or was cancelled. Nothing was read,
            // so the client is told the same thing as for a read failure.
            Err(err) => {
                tracing::warn!(error = %err, "the restore-candidate read task did not complete");
                error_msg("internal", RESTORE_READ_FAILED_MESSAGE)
            }
        };
        outbound.reply(&message);
    }

    /// Stores a window's presentation layout.
    ///
    /// Decision order matches the other window-scoped handlers: existence
    /// first, then the argument check. The layout is opaque here -- only its
    /// size is checked, since a value the daemon never interprets has no
    /// other property this layer could judge, and its schema belongs to the
    /// client that wrote it.
    ///
    /// No approval or attach gate applies: this changes nothing about the
    /// child process, and requiring an attachment would make the layout of
    /// every non-focused window unsavable.
    fn handle_update_window_layout(
        &self,
        outbound: &Outbound,
        window_id: &str,
        layout_json: String,
    ) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        // Rejected rather than truncated: a half-stored layout would restore
        // the window to a position nobody put it in.
        if layout_json.len() > store::MAX_LAYOUT_JSON_BYTES {
            outbound.reply(&error_msg(
                "invalid_argument",
                "layout_json exceeds the maximum allowed size",
            ));
            return;
        }
        self.registry.set_window_layout(&entry, layout_json);
        outbound.reply(&DaemonMessage::WindowLayoutUpdated {
            window_id: entry.window_id().to_string(),
        });
    }

    /// Takes the palettes windows opened from now on answer color queries
    /// with, then saves whatever the registry holds by the time the store is
    /// free: the save that runs last writes the latest palettes, whichever
    /// connection sent them.
    async fn handle_set_default_palettes(
        &self,
        outbound: &Arc<Outbound>,
        palettes: yagura_protocol::DefaultPalettes,
    ) {
        let palettes = match palette_store::accept_default_palettes(palettes) {
            Ok(palettes) => palettes,
            Err(err) => {
                outbound.reply(&error_msg("invalid_argument", &err.to_string()));
                return;
            }
        };
        if !self.registry.set_default_palettes(palettes) || !self.palette_store.is_enabled() {
            return;
        }
        let registry = Arc::clone(&self.registry);
        let saved = self
            .palette_store
            .with_store(move |store| store.save(&registry.default_palettes()))
            .await;
        match saved {
            Ok(Ok(())) => {}
            Ok(Err(err)) => tracing::warn!("failed to save the default palettes: {err}"),
            Err(_) => {
                tracing::warn!("failed to save the default palettes: the store task did not finish")
            }
        }
    }

    /// Registers this connection as the (new) attach holder of `window_id`,
    /// superseding any previous holder, resizes the window to `size` when
    /// one is given and differs, and replies with `Attached`, a frame of every
    /// row (whatever was unacknowledged or held before) and one
    /// `StatusChanged` carrying its current status.
    ///
    /// Everything that can fail (the size check, the resize) happens before
    /// the slot is taken, so a failed attach leaves
    /// this connection and the window's holder as they were. A window whose
    /// child has exited is not resized.
    ///
    /// A connection is attached to at most one window: attaching
    /// to a second window releases the first window's slot *for this
    /// connection only* and re-points both subscriptions, without the
    /// connection ever superseding itself.
    async fn handle_attach_view(
        &self,
        outbound: &Outbound,
        state: &mut ConnectionState,
        window_id: &str,
        size: Option<TerminalSize>,
    ) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        if let Some(size) = size
            && !crate::pty::size_within_limit(size.cols, size.rows)
        {
            outbound.reply(&size_out_of_range_msg());
            return;
        }

        // The window takes the size before the screen is read, so the screen
        // is laid out for the caller. The same size changes nothing.
        if let Some(size) = size
            && !is_exited_for_pty_write(&entry)
            && entry.session().size().await != Some((size.cols, size.rows))
        {
            // Armed before the PTY resize, as in `handle_resize`.
            entry.notify_resize_started();
            match entry.session().resize(size.cols, size.rows).await {
                Ok(()) => entry.propagate_resize(),
                // The child left in the meantime; its screen is still shown.
                Err(crate::pty::PtyError::ChildExited) => {}
                Err(_) => {
                    outbound.reply(&error_msg("internal", "failed to resize the PTY"));
                    return;
                }
            }
        }

        let (generation, kick_rx) = entry.register_attach().await;
        // Order matters: registering first means a re-attach to the *same* window
        // already replaced the slot, so this release is a no-op there instead of
        // dropping the attachment just made and swallows the self-directed kick.
        if let Some(previous) = state.attachment.take() {
            previous.entry.clear_attach_if(previous.generation).await;
        }
        state.kick_rx = Some(kick_rx);
        state.attachment = Some(Attachment {
            entry: entry.clone(),
            generation,
        });
        // Subscribed before the frame is read, so a change after it is heard.
        let session = entry.session();
        state.view = View::new();
        state.arrivals = Some(seen(session.output_arrivals()));
        state.exit_rx = Some(session.exit_notified());
        state.writes_rx = Some(seen(session.input_writes()));

        let exit = session.exit_status();
        let msg = DaemonMessage::Attached {
            child_exited: exit.is_some(),
            exit_code: exit,
        };
        outbound.reply(&msg);
        self.push_frames(outbound, state, true).await;

        // The current status follows as a separate `StatusChanged`, since
        // `Attached` carries none. Subscribing before reading the current value
        // re-delivers a racing change rather than losing it.
        state.status_rx = Some(entry.subscribe_status());
        // One read of the published pair, so this snapshot's status, reason and
        // generation all describe the same transition.
        let at = entry.status_snapshot();
        outbound.reply(&DaemonMessage::StatusChanged {
            window_id: entry.window_id().to_string(),
            status: at.status.status(),
            unknown_reason: at.status.unknown_reason(),
            status_rev: Some(at.generation),
        });
    }

    /// Opens a new window through the registry, once the approval gate lets
    /// the request through.
    ///
    /// Validation (command, `cwd`, detection name) and the resource limits
    /// live in the registry; this maps its error enum 1:1 onto the IPC error
    /// codes.
    async fn handle_open_window(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        request: OpenWindowRequest,
    ) {
        let provenance = WindowProvenance {
            opener: opener_of(&state.origin),
            ..WindowProvenance::default()
        };
        let cwd = match self.registry.check_open_request(&request) {
            Ok(cwd) => cwd,
            Err(err) => {
                outbound.reply(&window_open_error_msg(&err));
                return;
            }
        };
        let description = OpenDescription::for_command(&request.command, Some(&cwd));
        let registry = Arc::clone(&self.registry);
        let answer = Arc::clone(outbound);
        self.open_when_permitted(outbound, state, &description, move |verdict| async move {
            match verdict {
                OpenVerdict::Approved(permit) => {
                    let message =
                        match open_command_window(&registry, request, provenance, permit).await {
                            Ok(opened) => opened_message(opened),
                            Err(err) => window_open_error_msg(&err),
                        };
                    answer.reply(&message);
                }
                OpenVerdict::Refused(message) => answer.reply(&message),
            }
        })
        .await;
    }

    /// Runs a request to open a window past the open gate, then hands the
    /// verdict to `then`: straight away when the gate decides at once,
    /// otherwise from a task of its own once an approver has decided, so the
    /// connection keeps serving frames while a human is asked.
    ///
    /// The one place a request to open a window waits for an approver, so
    /// every path that does ends the wait the same way. If the requester
    /// leaves first -- its connection closes, or its CLI drops the call at
    /// the end of a turn -- the approval is withdrawn and `then` is dropped
    /// unrun, releasing whatever it holds (a handoff's slot, a plan's lock).
    /// `then` owns the answer to the requester.
    async fn open_when_permitted<F, Fut>(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        description: &OpenDescription,
        then: F,
    ) where
        F: FnOnce(OpenVerdict) -> Fut + Send + 'static,
        Fut: std::future::Future<Output = ()> + Send,
    {
        let ticket = match gate_open(&self.gate_context(state), description) {
            GateOutcome::Proceed(permit) => return then(OpenVerdict::Approved(permit)).await,
            GateOutcome::Refused(message) => return then(OpenVerdict::Refused(message)).await,
            GateOutcome::Pending(ticket) => ticket,
        };
        let RequestWatch {
            left: mut closed, ..
        } = RequestWatch::new(&self.registry, outbound, state, open_withdrawn_msg());
        let config = self.config.clone();
        tokio::spawn(async move {
            let verdict = await_verdict(ticket, &config, &mut closed).await;
            // A requester that left as the decision came in is still gone.
            let requester_left = *closed.borrow();
            // Dropped before the open runs: a watch that outlives the wait
            // could still answer the call as withdrawn while the window opens.
            drop(closed);
            match verdict {
                Some(verdict) if !requester_left => then(verdict).await,
                _ => drop(then),
            }
        });
    }

    /// Answers `list_profiles` with every launch profile the resolved
    /// profiles directory currently holds.
    /// Routed through the same bounded, blocking-thread-dispatching read
    /// [`plan::load_role_and_profile_state_blocking`] serves the plan
    /// ledger with: `list_profiles` reads the filesystem too, so
    /// this request path gets the same admission bound `workspace_walks`
    /// gives `resolve_key_blocking`. The role table it reads alongside is
    /// unused here.
    async fn handle_list_profiles(&self, outbound: &Outbound) {
        let (_roles, listing) =
            plan::load_role_and_profile_state_blocking(&self.config, &self.role_and_profile_reads)
                .await;
        outbound.reply(&DaemonMessage::ProfileList {
            profiles: listing.profiles.into_iter().map(to_profile_info).collect(),
            warnings: listing.warnings,
        });
    }

    /// Decides everything a launch profile would open, without opening it.
    ///
    /// Decision order: the supplied launch option values are held to the
    /// launch option value rule first (pure, cheapest, and a caller mistake
    /// whatever the profile is), then `resolve_launch` rejects an unknown
    /// identifier, a path-traversal attempt, a profile with no argv for this
    /// operating system and a value for an axis the profile does not map. It
    /// does not look at the working directory it hands on, so the resolved
    /// `cwd` is held to the same rule the registry applies -- through the
    /// registry's own [`resolve_existing_dir`], never a second copy of it --
    /// before anything is spawned.
    ///
    /// # Errors
    ///
    /// Returns the answer the caller must send: `profile_disabled` when the
    /// named profile's `enabled` key switches it off, `unsupported` when a
    /// supplied launch option cannot pass this host's program
    /// ([`crate::profile::shim_limits`]), `invalid_argument` for every other
    /// rejection here, none of which depends on who is asking. A
    /// refused launch option value is reported as `"{axis}: {reason}"` so a
    /// caller supplying two values knows which one.
    // The error is the wire message itself, which every other refusal in this
    // module also builds and hands straight to `reply`; boxing it here would
    // buy an allocation on the refusal path and nothing else.
    #[allow(clippy::result_large_err)]
    fn plan_from_profile(
        &self,
        profile: &str,
        cwd: Option<String>,
        name: Option<String>,
        on_name_taken: OnNameTaken,
        options: crate::profile::LaunchOptionValues,
    ) -> Result<PlannedOpen, DaemonMessage> {
        // The profile's display name is a default, never a name the caller
        // chose, so it is numbered rather than refused when taken.
        let on_name_taken = if name.is_some() {
            on_name_taken
        } else {
            OnNameTaken::Number
        };
        for axis in crate::profile::LaunchOptionAxis::ALL {
            if let Some(value) = options.get(axis) {
                validate_launch_option_value(value).map_err(|err| {
                    error_msg("invalid_argument", &format!("{}: {err}", axis.name()))
                })?;
            }
        }
        let launch = crate::profile::resolve_launch(
            &self.config,
            profile,
            cwd.map(PathBuf::from),
            name,
            &options,
        )
        .map_err(|err| profile_launch_error_msg(&err))?;
        for axis in crate::profile::LaunchOptionAxis::ALL {
            let limited = launch
                .limits
                .iter()
                .find(|limit| limit.capability == axis.capability());
            if let (Some(_), Some(limit)) = (options.get(axis), limited) {
                return Err(limit_refusal_msg(limit));
            }
        }
        if let Some(cwd) = launch.cwd.as_ref() {
            resolve_existing_dir(cwd.path(), cwd.origin())
                .map_err(|err| window_open_error_msg(&err))?;
        }
        // Only rendered here; any settings file it names is written by the
        // registry right before spawn, so a refused request never writes one.
        let config_dir = self.config.mcp_config_dir.as_deref();
        let mut injections = Vec::new();
        // The registered name, so the window's server and the instance's
        // registration are one entry rather than two side by side.
        let target = crate::mcp_registration::RegistrationTarget::for_instance(
            self.config.instance_id.as_deref(),
        );
        let mut mcp_absent = None;
        if let (Some(spec), Some(bridge)) = (launch.mcp.as_ref(), self.config.mcp_bridge.as_deref())
        {
            injections.extend(crate::profile::plan_mcp_injection(
                spec,
                target.server(),
                bridge,
                config_dir,
            ));
            mcp_absent = spec
                .absent
                .as_ref()
                .map(|guard| mcp_registration::AbsentRead {
                    file: guard.file().to_string(),
                    pointer: guard.pointer_for(target.server()),
                    tools: launch
                        .host_paths
                        .as_ref()
                        .and_then(crate::host_paths::HostPaths::file_tools)
                        .cloned(),
                    env: launch
                        .env
                        .iter()
                        .map(|(key, value)| (key.into(), value.into()))
                        .collect(),
                    permits: Arc::clone(&self.mcp_launch_listings),
                });
        }
        // Independent of the bridge: the hooks carry their own credentials
        // (a file or the window's environment, by the profile's mode).
        if let Some(spec) = launch.hooks.as_ref() {
            injections.extend(crate::profile::plan_hooks_injection(spec, config_dir));
        }
        let host_paths =
            launch
                .host_paths
                .clone()
                .map(|host_paths| mcp_registration::PathConversion {
                    host_paths,
                    env: launch
                        .env
                        .iter()
                        .map(|(key, value)| (key.into(), value.into()))
                        .collect(),
                    permits: Arc::clone(&self.mcp_launch_listings),
                });
        let hooks = if launch.hooks.is_some() {
            HooksRoute::Injection
        } else {
            HooksRoute::Unarranged
        };
        let credentials = credentials_mode_for(&self.config, &launch);
        // The program alone, without this launch's options: a wake command
        // runs the profile's own program for a session that already exists.
        let wakes_by_command = matches!(
            launch.delivery.as_ref().map(|delivery| &delivery.wake),
            Some(WakeMode::Command(_))
        );
        // A table that spells its server name out injects one this launch
        // cannot tell from the registrations it would turn off.
        let scanning = match (
            launch.bridge_listing.as_ref(),
            self.config
                .mcp_bridge
                .as_deref()
                .and_then(Path::file_name)
                .and_then(std::ffi::OsStr::to_str),
        ) {
            (Some(list), Some(file_name))
                if launch
                    .mcp
                    .as_ref()
                    .is_none_or(crate::profile::InjectionSpec::names_server) =>
            {
                Some((list.clone(), file_name.to_string()))
            }
            _ => None,
        };
        let base_argv = if wakes_by_command || scanning.is_some() {
            Some(
                crate::profile::resolve_launch(
                    &self.config,
                    profile,
                    None,
                    None,
                    &crate::profile::LaunchOptionValues::default(),
                )
                .map_err(|err| profile_launch_error_msg(&err))?
                .argv,
            )
        } else {
            None
        };
        let wake_command = base_argv
            .clone()
            .filter(|_| wakes_by_command)
            .map(|argv| WakeCommand {
                argv,
                env: launch.env.clone().into_iter().collect(),
            });
        let bridge_scan = scanning
            .zip(base_argv)
            .map(
                |((list, bridge_file_name), launch_argv)| mcp_registration::BridgeScan {
                    list,
                    launch_argv,
                    env: launch
                        .env
                        .iter()
                        .map(|(key, value)| (key.into(), value.into()))
                        .collect(),
                    bridge_file_name,
                    keep: target.server().to_string(),
                    permits: Arc::clone(&self.mcp_launch_listings),
                },
            );
        let hooks_gate = launch
            .hooks_registration
            .filter(crate::mcp_registration::McpRegistrationSpec::owns_entries)
            .map(|registration| HooksGate {
                profile: profile.to_string(),
                registration,
                env: launch
                    .env
                    .iter()
                    .map(|(key, value)| (key.into(), value.into()))
                    .collect(),
                writes: Arc::clone(&self.mcp_registration_writes),
            });

        Ok(PlannedOpen {
            launch: LaunchIdentity {
                profile: launch.vendor.clone(),
                model: options.model,
                effort: options.effort,
            },
            request: OpenWindowRequest {
                command: launch.argv,
                cwd: launch.cwd,
                name: Some(launch.name),
                on_name_taken,
                vendor: Some(launch.vendor),
                detection: launch.detection,
            },
            extra_env: launch.env.into_iter().collect(),
            profile_injections: injections,
            host_paths,
            mcp_absent,
            injections: Vec::new(),
            hooks_gate,
            hooks,
            credentials,
            launch_args: launch.credentials.launch_args.clone(),
            delivery: launch.delivery,
            wake_command,
            bridge_scan,
            limits: launch.limits,
            chat: Arc::clone(&self.chat),
        })
    }

    /// Opens a new window from a launch profile.
    ///
    /// Decision order: the launch content is decided first
    /// ([`IpcServer::plan_from_profile`]), then the approval gate, and only
    /// then is the window actually opened ([`open_planned`]), so a refusal
    /// never leaves a child behind.
    // One argument per `OpenWindowFromProfile` field, plus the connection that asked.
    #[allow(clippy::too_many_arguments)]
    async fn handle_open_window_from_profile(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        profile: String,
        cwd: Option<String>,
        name: Option<String>,
        on_name_taken: OnNameTaken,
        options: crate::profile::LaunchOptionValues,
    ) {
        let planned = match self.plan_from_profile(&profile, cwd, name, on_name_taken, options) {
            Ok(planned) => planned,
            Err(message) => {
                outbound.reply(&message);
                return;
            }
        };
        let cwd = match self.registry.check_open_request(&planned.request) {
            Ok(cwd) => cwd,
            Err(err) => {
                outbound.reply(&window_open_error_msg(&err));
                return;
            }
        };
        let description = OpenDescription::for_profile(&profile, Some(&cwd), None);
        let provenance = WindowProvenance {
            opener: opener_of(&state.origin),
            ..WindowProvenance::default()
        };
        let registry = Arc::clone(&self.registry);
        let config = self.config.clone();
        let answer = Arc::clone(outbound);
        self.open_when_permitted(outbound, state, &description, move |verdict| async move {
            match verdict {
                OpenVerdict::Approved(permit) => {
                    let message =
                        match open_planned(&registry, &config, planned, provenance, permit).await {
                            Ok(opened) => opened_message(opened),
                            Err(err) => window_open_error_msg(&err),
                        };
                    answer.reply(&message);
                }
                OpenVerdict::Refused(message) => answer.reply(&message),
            }
        })
        .await;
    }

    /// Opens a window for one task of a plan, launched with the task's
    /// resolved assignment, and binds the new window to the task.
    ///
    /// Decision order, all under the plan's lock so no check is judged on
    /// state a concurrent change has already replaced: the change permit
    /// (owner liveness, range) -> whether the task's
    /// currently bound window (if any) is still alive -> the resolved
    /// assignment (unassigned, or naming a profile that has since vanished,
    /// is refused before anything is spawned) -> the open gate, which may
    /// wait for an approver while the lock stays held -> the window itself,
    /// opened through the same [`IpcServer::plan_from_profile`] /
    /// [`open_planned`] pair [`IpcServer::handle_open_window_from_profile`]
    /// uses -> the binding record ([`open_task_window`]).
    async fn handle_open_task_window(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        plan_id: String,
        task_id: String,
        name: Option<String>,
    ) {
        if !self.ledger.is_enabled() {
            outbound.reply(&plan::unsupported_msg());
            return;
        }
        if matches!(state.origin, RequestOrigin::External) {
            outbound.reply(&plan::auth_failed_msg());
            return;
        }

        match plan::plan_exists(&self.ledger, &plan_id).await {
            Ok(true) => {}
            Ok(false) => {
                outbound.reply(&plan::plan_not_found_msg());
                return;
            }
            Err(message) => {
                outbound.reply(&message);
                return;
            }
        }

        let (roles_result, profile_listing) =
            plan::load_role_and_profile_state_blocking(&self.config, &self.role_and_profile_reads)
                .await;
        let roles = roles_result.ok();
        let profile_ids: std::collections::HashSet<String> = profile_listing
            .profiles
            .into_iter()
            .map(|entry| entry.id)
            .collect();

        let guard = self.ledger.locks().lock(&plan_id).await;
        let origin = state.origin.clone();
        let registry = Arc::clone(&self.registry);
        let task_id_for_read = task_id.clone();
        let prelude_result = self
            .ledger
            .with_store(move |store| -> Result<OpenTaskPrelude, OpenTaskOutcome> {
                let permit = plan_permit::issue_change(
                    &origin,
                    ChangeKind::OpenTaskWindow,
                    guard,
                    store,
                    &registry,
                )
                .map_err(OpenTaskOutcome::Permit)?;
                let detail = store
                    .get_plan(permit.plan_id())
                    .map_err(OpenTaskOutcome::Store)?
                    .ok_or(OpenTaskOutcome::Vanished)?;
                let Some(task) = detail
                    .tasks
                    .into_iter()
                    .find(|task| task.task_id == task_id_for_read)
                else {
                    return Err(OpenTaskOutcome::TaskNotFound);
                };
                if let Some(bound_window) = task.window_id.as_deref()
                    && matches!(registry.liveness(bound_window), WindowLiveness::Alive(_))
                {
                    return Err(OpenTaskOutcome::AlreadyBound);
                }
                Ok(OpenTaskPrelude {
                    permit,
                    cwd: detail.cwd,
                    title: task.title,
                    parent: task.parent,
                    user: task.user,
                    role: task.role,
                    instruction: task.instruction,
                })
            })
            .await;

        let prelude = match prelude_result {
            Ok(Ok(prelude)) => prelude,
            Ok(Err(OpenTaskOutcome::Permit(PermitError::PlanNotFound))) => {
                self.ledger.locks().remove(&plan_id);
                outbound.reply(&plan::plan_not_found_msg());
                return;
            }
            Ok(Err(OpenTaskOutcome::Permit(err))) => {
                outbound.reply(&plan::permit_error_msg(err));
                return;
            }
            Ok(Err(OpenTaskOutcome::Store(store_err))) => {
                tracing::warn!(error = %store_err, "task read before open failed");
                outbound.reply(&plan::internal_msg());
                return;
            }
            Ok(Err(OpenTaskOutcome::Vanished | OpenTaskOutcome::TaskNotFound)) => {
                outbound.reply(&error_msg("invalid_argument", "the task does not exist"));
                return;
            }
            Ok(Err(OpenTaskOutcome::AlreadyBound)) => {
                outbound.reply(&plan::task_window_alive_msg());
                return;
            }
            Err(access_err) => {
                outbound.reply(&plan::map_access_error(access_err));
                return;
            }
        };

        let (role_assignment, default_assignment) = roles
            .as_ref()
            .map(|table| table.role_and_default_for(prelude.role.as_deref()))
            .unwrap_or_default();
        let resolved = crate::plan::resolve_assignment(
            prelude.user.as_ref(),
            prelude.parent.as_ref(),
            role_assignment.as_ref(),
            default_assignment.as_ref(),
        );
        let Some(assignment) = resolved.assignment.clone() else {
            outbound.reply(&plan::task_unassigned_msg());
            return;
        };
        if !profile_ids.contains(&assignment.profile) {
            outbound.reply(&error_msg(
                "invalid_argument",
                "the assigned profile does not exist",
            ));
            return;
        }

        let options = crate::profile::LaunchOptionValues {
            model: assignment.model.clone(),
            effort: assignment.effort.clone(),
        };
        // Task titles pass the same window-name validation when the plan is stored.
        // A name the caller gave is refused when taken; the title is a default.
        let (name, on_name_taken) = match name {
            Some(name) => (name, OnNameTaken::Refuse),
            None => (prelude.title, OnNameTaken::Number),
        };
        let has_instruction = prelude.instruction.is_some();

        let planned = match self.plan_from_profile(
            &assignment.profile,
            Some(prelude.cwd),
            Some(name),
            on_name_taken,
            options,
        ) {
            Ok(planned) => planned,
            Err(message) => {
                outbound.reply(&message);
                return;
            }
        };
        // Only a task with something to tell the window needs one that can
        // be handed it at launch: a window that could not be is refused before
        // it starts, the same way a profile-addressed handoff is.
        if has_instruction {
            if planned
                .delivery
                .as_ref()
                .is_none_or(|delivery| delivery.launch_prompt.is_none())
            {
                outbound.reply(&no_launch_prompt_msg());
                return;
            }
            if let Some(limit) = planned.limit(yagura_protocol::ProfileCapability::LaunchPrompt) {
                outbound.reply(&limit_refusal_msg(limit));
                return;
            }
        }
        // The plan's lock stays held while an approver is asked, so what is
        // approved is what the plan still says when the window opens.
        let cwd = match self.registry.check_open_request(&planned.request) {
            Ok(cwd) => cwd,
            Err(err) => {
                outbound.reply(&window_open_error_msg(&err));
                return;
            }
        };
        let description = OpenDescription::for_profile(
            &assignment.profile,
            Some(&cwd),
            Some(OpenPurpose::PlanTask(&task_id)),
        );
        let job = TaskWindowOpen {
            ledger: Arc::clone(&self.ledger),
            registry: Arc::clone(&self.registry),
            broker: Arc::clone(&self.broker),
            config: self.config.clone(),
            outbound: Arc::clone(outbound),
            planned,
            change: prelude.permit,
            plan_id,
            task_id,
            has_instruction,
            resolved,
        };
        self.open_when_permitted(outbound, state, &description, move |verdict| async move {
            match verdict {
                OpenVerdict::Approved(permit) => open_task_window(job, permit).await,
                OpenVerdict::Refused(message) => job.outbound.reply(&message),
            }
        })
        .await;
    }

    /// Changes a window's display name.
    ///
    /// Decision order matches the other window-scoped handlers: existence
    /// first, then the argument check. The `child_exited` gate `send_text`
    /// applies does not appear here -- a label change never touches the child,
    /// and gating it would leave an exited window stuck with a name nobody can
    /// correct.
    async fn handle_rename_window(&self, outbound: &Outbound, window_id: &str, name: &str) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        // A malformed name is `invalid_argument` whatever is wrong with it;
        // clients that need to tell the reasons apart run the same
        // `validate_window_name` before sending. A taken or reserved name is
        // `name_taken`.
        let message = match self.registry.rename_window(&entry, name).await {
            Ok(stored) => {
                // A successful rename's new name must reach the store before
                // the reply.
                self.registry.request_snapshot();
                DaemonMessage::WindowRenamed {
                    window_id: entry.window_id().to_string(),
                    name: stored,
                }
            }
            Err(WindowRenameError::Invalid(err)) => error_msg("invalid_argument", &err.to_string()),
            Err(WindowRenameError::NameTaken(refusal)) => name_taken_msg(&refusal),
        };
        outbound.reply(&message);
    }

    /// Answers `get_window_status` for one window.
    ///
    /// `since_ms` is always present (window creation counts as the first
    /// status change); `last_output_summary` is `None` exactly when the
    /// the window shows no text.
    async fn handle_get_window_status(&self, outbound: &Outbound, window_id: &str) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };

        let text = control::read_output(
            &entry,
            usize::MAX,
            LAST_OUTPUT_SUMMARY_MAX_BYTES,
            &self.window_text_reads,
        )
        .await
        .map(|(text, _)| text)
        .unwrap_or_default();
        let last_output_summary =
            (!text.is_empty()).then(|| last_chars(&text, LAST_OUTPUT_SUMMARY_MAX_CHARS));

        // Read as one published pair, so the status answered with and the age
        // answered with are never taken from either side of a change.
        let snapshot = entry.status_snapshot();
        outbound.reply(&DaemonMessage::WindowStatus {
            window_id: entry.window_id().to_string(),
            status: snapshot.status.status(),
            since_ms: snapshot.since_ms(Instant::now()),
            last_output_summary,
            exit_code: entry.session().exit_status(),
            unknown_reason: snapshot.status.unknown_reason(),
            detection_warning: entry.detection_warning().map(str::to_string),
            mcp_connected: Some(entry.mcp_connected()),
        });
    }

    /// Writes text to a window's PTY without requiring an attachment: the
    /// caller is a peer-verified local client acting on behalf of an agent, not
    /// the interactive terminal.
    ///
    /// Decision order: window resolution -> pre-validation (an exited window,
    /// then the payload size, then its characters) -> the status, composer
    /// and approval gates -> the write. Pre-validation comes first so that its
    /// refusals do not depend on who is asking. The character
    /// check runs over the whole payload before it is judged single-line or
    /// raw, so a forbidden character is rejected on both of `execute_send_text`'s
    /// write paths alike.
    ///
    /// The write itself runs on its own task: it waits for the receiver to
    /// show what was written, with no time limit, and this connection must
    /// keep reading meanwhile to see its requester leave.
    fn handle_send_text(
        &self,
        outbound: &Arc<Outbound>,
        state: &mut ConnectionState,
        window_id: &str,
        text: &str,
    ) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        if is_confirmed_exited(&entry) {
            outbound.reply(&child_exited_msg());
            return;
        }
        if text.len() > SEND_TEXT_MAX_BYTES {
            outbound.reply(&error_msg(
                "invalid_argument",
                "text exceeds the maximum allowed size",
            ));
            return;
        }
        if let Err(err) = validate_send_text_body(text) {
            outbound.reply(&error_msg("invalid_argument", &err.to_string()));
            return;
        }
        let Some(slot) = state.try_take_send_slot(self.config.max_waiters_per_conn) else {
            outbound.reply(&pending_waits_exhausted_msg());
            return;
        };
        let permit = match gate_destructive(
            &self.gate_context(state),
            &entry,
            permit::SendMessage,
            Some(text),
        ) {
            GateOutcome::Proceed(permit) => permit,
            GateOutcome::Pending(ticket) => {
                // An approval wait is bounded by the approval budgets instead.
                drop(slot);
                spawn_pending(
                    RequestWatch::new(&self.registry, outbound, state, send_text_withdrawn_msg()),
                    Arc::clone(&self.registry),
                    self.config.clone(),
                    entry,
                    PendingAction::SendText(text.to_string()),
                    ticket,
                );
                return;
            }
            GateOutcome::Refused(message) => return reply_refused(outbound, &message),
        };

        let RequestWatch { outbound, left } =
            RequestWatch::new(&self.registry, outbound, state, send_text_withdrawn_msg());
        let text = text.to_string();
        tokio::spawn(async move {
            execute_send_text(&outbound, &entry, &text, permit, Some(left)).await;
            drop(slot);
        });
    }

    /// Writes a validated named-key sequence to a window's PTY in one raw
    /// operation. Unlike `send_message`, this deliberately bypasses composer
    /// inspection because control keys are the requested keys themselves.
    /// The keys travel through an approval wait as keys: what bytes they
    /// become depends on the window's modes when they are written.
    async fn handle_send_keys(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        window_id: &str,
        keys: &[ControlKey],
    ) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        if is_confirmed_exited(&entry) {
            outbound.reply(&child_exited_msg());
            return;
        }
        if let Err(err) = validate_control_keys(keys) {
            outbound.reply(&error_msg("invalid_argument", &err.to_string()));
            return;
        }
        let approval_text = keys
            .iter()
            .map(|key| key.wire_name())
            .collect::<Vec<_>>()
            .join(", ");
        let permit = match gate_destructive(
            &self.gate_context(state),
            &entry,
            permit::SendKeys,
            Some(&approval_text),
        ) {
            GateOutcome::Proceed(permit) => permit,
            GateOutcome::Pending(ticket) => {
                spawn_pending(
                    RequestWatch::connection_only(outbound, state),
                    Arc::clone(&self.registry),
                    self.config.clone(),
                    entry,
                    PendingAction::SendKeys(keys.to_vec()),
                    ticket,
                );
                return;
            }
            GateOutcome::Refused(message) => return reply_refused(outbound, &message),
        };

        execute_send_keys(outbound, &entry, keys, permit).await;
    }

    /// Interrupts a window the way the configured interrupt method prescribes.
    ///
    /// Decision order: window resolution -> pre-validation (an exited window)
    /// -> the status and approval gates -> the operation itself. An exited
    /// window is reported to every caller alike, since its exit is already
    /// readable through `get_window_status`.
    async fn handle_interrupt_window(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        window_id: &str,
    ) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        if is_confirmed_exited(&entry) {
            outbound.reply(&child_exited_msg());
            return;
        }
        let permit = match gate_destructive(
            &self.gate_context(state),
            &entry,
            permit::InterruptWindow,
            None,
        ) {
            GateOutcome::Proceed(permit) => permit,
            GateOutcome::Pending(ticket) => {
                spawn_pending(
                    RequestWatch::connection_only(outbound, state),
                    Arc::clone(&self.registry),
                    self.config.clone(),
                    entry,
                    PendingAction::Interrupt,
                    ticket,
                );
                return;
            }
            GateOutcome::Refused(message) => return reply_refused(outbound, &message),
        };

        execute_interrupt(outbound, &entry, &self.config, permit).await;
    }

    /// Closes a window and terminates its whole process tree.
    ///
    /// Decision order: window resolution -> the status and approval gates ->
    /// the operation itself. There is no pre-validation: closing an already
    /// exited window is idempotent, and the `Unknown` status gate does not
    /// apply either, because a window whose state cannot be detected must
    /// still be closable (see [`control::check_destructive`]).
    async fn handle_close_window(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        window_id: &str,
        force: Option<bool>,
    ) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        let force = force.unwrap_or(false);
        let permit =
            match gate_destructive(&self.gate_context(state), &entry, permit::CloseWindow, None) {
                GateOutcome::Proceed(permit) => permit,
                GateOutcome::Pending(ticket) => {
                    spawn_pending(
                        RequestWatch::connection_only(outbound, state),
                        Arc::clone(&self.registry),
                        self.config.clone(),
                        entry,
                        PendingAction::Close(force),
                        ticket,
                    );
                    return;
                }
                GateOutcome::Refused(message) => return reply_refused(outbound, &message),
            };

        execute_close(
            outbound,
            &self.registry,
            &self.config,
            &entry,
            force,
            permit,
        )
        .await;
    }

    /// Hands work to another window: an existing one (`window_id`) or a new
    /// one opened from a launch profile (`profile`).
    ///
    /// Decision order: the arguments, then the
    /// envelope and its overall size, then the profile's launch content (no
    /// side effect), then -- for a profile-addressed request -- whether the
    /// profile can be handed the envelope at launch, then the policy
    /// pre-judgement, then the unanswered-handoff slot, then the target
    /// itself. Everything that can be refused before a child exists is
    /// refused before one is spawned, and none of those refusals depends on
    /// who is asking.
    ///
    /// The envelope is composed before the target is resolved because it
    /// describes only the requester: refusing an oversized one must not
    /// depend on whether the target could be reached.
    ///
    /// An existing window is handed the envelope through its inbox, and the
    /// request is answered on a task of its own ([`deliver_handoff`]) once the
    /// window's CLI took it or never will. A profile-addressed request is
    /// approved before anything starts, and its window is started with the
    /// envelope as its launch prompt.
    async fn handle_send_handoff(
        &self,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        request: HandoffRequest,
    ) {
        let HandoffRequest {
            window_id,
            profile,
            cwd,
            name,
            model,
            effort,
            reference,
            note,
        } = request;
        let options = crate::profile::LaunchOptionValues { model, effort };

        if let Err(err) = validate_handoff_target(
            window_id.as_deref(),
            profile.as_deref(),
            cwd.as_deref(),
            name.as_deref(),
            !options.is_empty(),
        ) {
            outbound.reply(&error_msg("invalid_argument", &err.to_string()));
            return;
        }
        if let Err(err) = validate_handoff_text(&reference, HANDOFF_REFERENCE_MAX_BYTES) {
            outbound.reply(&error_msg("invalid_argument", &format!("reference: {err}")));
            return;
        }
        if let Err(err) = validate_handoff_text(&note, HANDOFF_NOTE_MAX_BYTES) {
            outbound.reply(&error_msg("invalid_argument", &format!("note: {err}")));
            return;
        }

        let requester = requester_window(&self.registry, &state.origin);
        let envelope = compose_envelope(&envelope_from(requester.as_ref()), &reference, &note);
        if let Err(err) = check_envelope_len(&envelope) {
            outbound.reply(&error_msg("invalid_argument", &err.to_string()));
            return;
        }
        let requester_id = requester.as_ref().map(|info| info.window_id.clone());

        match (window_id, profile) {
            (Some(window_id), None) => {
                // The slot is taken for a window_id-addressed handoff too: it
                // is a detached task that stays outstanding until answered.
                // The daemon-wide budget is drawn only while the envelope is
                // being typed, not while it waits.
                let slot = match HandoffSlot::try_acquire_per_conn(
                    &state.open_handoffs,
                    self.config.max_open_handoffs_per_conn,
                    &self.open_handoffs,
                    self.config.max_open_handoffs,
                ) {
                    Ok(slot) => slot,
                    Err(exhausted) => {
                        outbound.reply(&exhausted.to_message());
                        return;
                    }
                };

                let Some(entry) = self.registry.get(&window_id) else {
                    abandon_handoff(outbound, slot, &window_not_found_msg());
                    return;
                };
                if is_confirmed_exited(&entry) {
                    abandon_handoff(outbound, slot, &child_exited_msg());
                    return;
                }
                // Answered before any gate: nothing could ever reach this
                // window's CLI, so there is nothing to approve.
                if !entry.delivers() {
                    abandon_handoff(
                        outbound,
                        slot,
                        &DaemonMessage::HandoffSent {
                            window_id: entry.window_id().to_string(),
                            opened: false,
                            delivery: HandoffDelivery::UndeliveredNoDelivery,
                            name: Some(entry.name()),
                        },
                    );
                    return;
                }
                let permit =
                    match permit::gate_handoff(&self.gate_context(state), &entry, &envelope.text) {
                        GateOutcome::Proceed(permit) => permit,
                        GateOutcome::Pending(ticket) => {
                            spawn_pending(
                                RequestWatch::quiet(&self.registry, outbound, state),
                                Arc::clone(&self.registry),
                                self.config.clone(),
                                entry,
                                PendingAction::SendHandoff {
                                    text: envelope.text,
                                    requester: requester_id,
                                    slot: Some(slot),
                                },
                                ticket,
                            );
                            return;
                        }
                        GateOutcome::Refused(message) => {
                            abandon_handoff(outbound, slot, &message);
                            return;
                        }
                    };

                let RequestWatch { outbound, left } =
                    RequestWatch::quiet(&self.registry, outbound, state);
                // Detached so this connection's frame loop keeps serving
                // other requests while the envelope waits for the window's CLI.
                tokio::spawn(deliver_handoff(DeliveryJob {
                    outbound,
                    registry: Arc::clone(&self.registry),
                    config: self.config.clone(),
                    entry,
                    envelope: envelope.text,
                    requester: requester_id,
                    slot: Some(slot),
                    closed: left,
                    permit,
                }));
            }
            (None, Some(profile)) => {
                // Work handed over starts where it came from, so the
                // requester's own window is the default working directory.
                let cwd = cwd.or_else(|| requester.map(|info| info.cwd));
                let planned =
                    match self.plan_from_profile(&profile, cwd, name, OnNameTaken::Refuse, options)
                    {
                        Ok(planned) => planned,
                        Err(message) => {
                            outbound.reply(&message);
                            return;
                        }
                    };
                // The envelope is handed over as the launch prompt, so a
                // profile that declares none, or whose program would pass it
                // through the command interpreter, is refused before anything
                // starts.
                if planned
                    .delivery
                    .as_ref()
                    .is_none_or(|delivery| delivery.launch_prompt.is_none())
                {
                    outbound.reply(&no_launch_prompt_msg());
                    return;
                }
                if let Some(limit) = planned.limit(yagura_protocol::ProfileCapability::LaunchPrompt)
                {
                    outbound.reply(&limit_refusal_msg(limit));
                    return;
                }
                let cwd = match self.registry.check_open_request(&planned.request) {
                    Ok(cwd) => cwd,
                    Err(err) => {
                        outbound.reply(&window_open_error_msg(&err));
                        return;
                    }
                };

                // The same policy half of the gate the envelope faces below.
                // Asking it now means a request nobody could ever approve is
                // refused before a slot is reserved for it.
                let policy_snapshot = self.policy.snapshot();
                match control::policy_gate(
                    live_policy::policy_source(&policy_snapshot),
                    DestructiveTool::SendHandoff,
                    &state.origin,
                    None,
                ) {
                    Ok(GateVerdict::Allow) => {}
                    Ok(GateVerdict::NeedsApproval) if !self.broker.has_approvers() => {
                        outbound.reply(&deny_no_approver(DestructiveTool::SendHandoff));
                        return;
                    }
                    Ok(GateVerdict::NeedsApproval) => {}
                    Err(reason) => {
                        outbound.reply(&deny_msg(DestructiveTool::SendHandoff, reason));
                        return;
                    }
                }

                let slot = match HandoffSlot::try_acquire(
                    &state.open_handoffs,
                    self.config.max_open_handoffs_per_conn,
                    &self.open_handoffs,
                    self.config.max_open_handoffs,
                ) {
                    Ok(slot) => slot,
                    Err(exhausted) => {
                        outbound.reply(&exhausted.to_message());
                        return;
                    }
                };

                // One decision, before anything starts, both starts the
                // process and hands it the envelope.
                let description =
                    OpenDescription::for_profile(&profile, Some(&cwd), Some(OpenPurpose::Handoff));
                let job = ProfileHandoffOpen {
                    planned,
                    provenance: WindowProvenance {
                        opener: opener_of(&state.origin),
                        ..WindowProvenance::default()
                    },
                    outbound: Arc::clone(outbound),
                    registry: Arc::clone(&self.registry),
                    broker: Arc::clone(&self.broker),
                    config: self.config.clone(),
                    origin: state.origin.clone(),
                    conn_id: state.conn_id,
                    closed: state.closed.subscribe(),
                    approval_text: format!("{}\n{}", description.text(), envelope.text),
                    envelope: envelope.text,
                    slot,
                    policy: Arc::clone(&self.policy),
                };
                approve_profile_handoff(job).await;
            }
            // `validate_handoff_target` already refused a request naming
            // neither target or both of them; refusing again is the
            // fail-closed answer if that rule ever stops holding.
            _ => outbound.reply(&error_msg(
                "invalid_argument",
                "exactly one of window_id and profile must be given",
            )),
        }
    }

    /// Establishes this connection's origin as the window whose status token
    /// it presents.
    ///
    /// Checked in the same order as `status_event`: token shape -> window
    /// resolution -> that window's own token. A naming mistake
    /// (`window_not_found`) is kept apart from a credential failure, so only
    /// the latter counts toward [`IDENTIFY_FAILURE_LIMIT`] and every refusal
    /// leaves the origin untouched.
    async fn handle_identify(
        &self,
        outbound: &Outbound,
        state: &mut ConnectionState,
        window_id: &str,
        token: &str,
    ) -> FrameOutcome {
        let well_formed = StatusToken::is_well_formed(token);
        if !well_formed {
            return reject_identify(outbound, state);
        }
        // A child may identify as soon as it is spawned, before its open has
        // registered it.
        let Some(entry) = self.registry.get_after_open(window_id).await else {
            outbound.reply(&window_not_found_msg());
            return FrameOutcome::Continue;
        };
        let authenticated = entry.verify_status_token(token);
        if !authenticated {
            return reject_identify(outbound, state);
        }

        match &state.origin {
            // Re-claiming the same window is the same statement twice, not a
            // conflict: a client that reconnects its own claim must not have
            // to track whether it already made it.
            RequestOrigin::Window(claimed) if claimed == entry.window_id() => {}
            RequestOrigin::External => {
                state.origin = RequestOrigin::Window(entry.window_id().to_string());
                state.mcp_claim = Some(entry.claim_mcp_connection());
            }
            _ => {
                outbound.reply(&error_msg(
                    "invalid_state",
                    "this connection has already claimed a different origin",
                ));
                return FrameOutcome::Continue;
            }
        }
        outbound.reply(&DaemonMessage::Identified {
            window_id: entry.window_id().to_string(),
        });
        FrameOutcome::Continue
    }

    /// Claims this connection as an approver: a party that decides every
    /// pending request and reaches every window without the gate.
    ///
    /// Only a peer running the executable named by `--approver-exe` may
    /// claim it; anyone else is answered `auth_failed` and stays whatever
    /// origin it already had. A refusal is deliberately not counted the way
    /// `identify`'s are: the GUI's control connection retries registration
    /// on a timer, and disconnecting it would turn a version mismatch into a
    /// reconnect loop.
    ///
    /// Idempotent by construction: re-registering replaces this connection's
    /// own sink and answers with the same pending set, so a client that
    /// cannot tell whether it already registered may simply register again.
    fn handle_register_approver(&self, outbound: &Arc<Outbound>, state: &mut ConnectionState) {
        if !approver_allowed(state, &self.config) {
            if !state.approver_refusal_logged {
                state.approver_refusal_logged = true;
                tracing::warn!(
                    peer_executable = ?state.peer_executable,
                    "refusing register_approver: the peer is not the configured approver executable"
                );
            }
            outbound.reply(&error_msg(
                "auth_failed",
                "this connection may not register as an approver",
            ));
            return;
        }
        if let RequestOrigin::Window(_) = state.origin {
            outbound.reply(&error_msg(
                "invalid_state",
                "this connection has already claimed a different origin",
            ));
            return;
        }
        state.origin = RequestOrigin::Approver;
        let sink: Arc<dyn ApproverSink> = Arc::<Outbound>::clone(outbound);
        let pending = self.broker.register_approver(state.conn_id, sink);
        outbound.reply(&DaemonMessage::ApproverRegistered { pending });
    }

    /// Settles one pending request on behalf of this connection.
    ///
    /// A connection that never registered as an approver is answered exactly
    /// like one naming a request that is no longer pending: whether a request
    /// exists is not something an unregistered caller may learn.
    fn handle_approval_decision(
        &self,
        outbound: &Outbound,
        state: &ConnectionState,
        request_id: &str,
        approved: bool,
    ) {
        match self.broker.decide(state.conn_id, request_id, approved) {
            Ok(()) => {
                tracing::info!(request_id, "settled a pending approval request");
                outbound.reply(&DaemonMessage::ApprovalResolved {
                    request_id: request_id.to_string(),
                });
            }
            Err(DecideError::NotPending) => outbound.reply(&error_msg(
                "approval_not_pending",
                "no such request is awaiting a decision from this connection",
            )),
        }
    }

    /// The gate's inputs, as this server and one of its connections hold
    /// them.
    fn gate_context<'a>(&'a self, state: &'a ConnectionState) -> GateContext<'a> {
        GateContext {
            config: &self.config,
            broker: &self.broker,
            registry: &self.registry,
            origin: &state.origin,
            conn_id: state.conn_id,
            policy: &self.policy,
        }
    }

    /// Registers a `wait_for_status` waiter for this connection.
    ///
    /// Nothing is awaited here: the wait runs in its own task so that the
    /// connection keeps serving other messages while it is outstanding. The
    /// answer is delivered by [`send_wait_result`].
    fn handle_wait_for_status(
        &self,
        outbound: &Arc<Outbound>,
        state: &mut ConnectionState,
        window_id: &str,
        target_status: WindowStatus,
        timeout_sec: u64,
    ) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        // Waiting for the fail-safe status would turn "we cannot tell" into a
        // completion condition.
        if target_status == WindowStatus::Unknown {
            outbound.reply(&error_msg(
                "invalid_argument",
                "target_status must not be unknown",
            ));
            return;
        }
        if timeout_sec == 0 || timeout_sec > self.config.wait_timeout_max_secs {
            outbound.reply(&error_msg(
                "invalid_argument",
                "timeout_sec must be within 1..=the configured maximum",
            ));
            return;
        }
        if state.live_waiter_count() >= self.config.max_waiters_per_conn {
            outbound.reply(&pending_waits_exhausted_msg());
            return;
        }

        let RequestWatch { outbound, mut left } =
            RequestWatch::quiet(&self.registry, outbound, state);
        let window_id = entry.window_id().to_string();
        let timeout = Duration::from_secs(timeout_sec);
        let wait_rx = entry.watch_status();
        let now = entry.watch_status();
        state.waiters.push(tokio::spawn(async move {
            // A wait its requester's CLI has dropped ends as if its time ran out.
            let outcome = tokio::select! {
                outcome = waiter::wait_for_status(wait_rx, target_status, timeout) => outcome,
                () = request_left(&mut left) => WaitOutcome::Timeout {
                    current: now.borrow().status,
                },
            };
            send_wait_result(&outbound, &window_id, outcome, target_status);
        }));
    }

    /// Answers `read_output` with the tail of the window's text as it shows it.
    ///
    /// `read_output` is non-destructive and therefore outside the approval
    /// gate, exactly like `get_window_status`.
    async fn handle_read_output(&self, outbound: &Outbound, window_id: &str, lines: Option<u32>) {
        let Some(entry) = self.registry.get(window_id) else {
            outbound.reply(&window_not_found_msg());
            return;
        };
        let lines = match lines {
            Some(lines) => usize::try_from(lines).unwrap_or(usize::MAX),
            None => READ_OUTPUT_DEFAULT_LINES,
        };
        // Rejected rather than clamped, so a caller's mistake is not hidden.
        if lines == 0 || lines > self.config.read_output_max_lines {
            outbound.reply(&error_msg(
                "invalid_argument",
                "lines must be within 1..=the configured maximum",
            ));
            return;
        }

        let Some((text, truncated)) = control::read_output(
            &entry,
            lines,
            self.config.read_output_max_bytes,
            &self.window_text_reads,
        )
        .await
        else {
            outbound.reply(&error_msg("internal", "failed to read the window's text"));
            return;
        };
        outbound.reply(&DaemonMessage::WindowOutput {
            window_id: entry.window_id().to_string(),
            text,
            truncated,
        });
    }

    /// Records a structured-source status event.
    ///
    /// Validation is fail-loud but non-fatal: a rejected event answers an
    /// error and leaves the connection open. No attach is required, since
    /// events arrive from hook processes rather than the GUI. An event that
    /// omits `window_id` is refused: no other window may stand in for the one
    /// it does not name.
    ///
    /// Order: the token's *shape* is checked first, then the target window is
    /// resolved, then the candidate is compared against that window's own
    /// token -- authentication is per-window, so which window is being spoken
    /// for has to be known before the comparison can mean anything. The
    /// daemon-wide rate bucket is charged only once that comparison has
    /// succeeded, so an unauthenticated flood cannot starve the legitimate
    /// hooks' allowance.
    ///
    /// Resolving the window ahead of the comparison lets an unauthenticated
    /// caller tell a live `window_id` from an unknown one. That is acceptable:
    /// a `window_id` is only a matching key, not a secret, and authentication
    /// is carried solely by that window's own token (documented in
    /// `docs/mcp-setup.md`).
    async fn handle_status_event(
        &self,
        outbound: &Outbound,
        token: &str,
        source: &str,
        status: WindowStatus,
        window_id: Option<&str>,
    ) {
        let Some(entry) = self.authenticate_window(outbound, "status_event", token, window_id)
        else {
            return;
        };
        if let Some(refusal) = self.apply_status_event(&entry, Some(source), status) {
            outbound.reply(&refusal);
        }
    }

    /// Charges the event budget, checks `source` (a request that carries none
    /// passes `None`) and records `status` on the authenticated `entry`.
    /// Returns the refusal to answer with, or `None` when the status was
    /// recorded.
    fn apply_status_event(
        &self,
        entry: &WindowEntry,
        source: Option<&str>,
        status: WindowStatus,
    ) -> Option<DaemonMessage> {
        if !self.charge_status_event_budget() {
            return Some(error_msg(
                "rate_limited",
                "status_event rate limit exceeded; the event was discarded",
            ));
        }
        if source.is_some_and(|source| !is_valid_source(source)) {
            return Some(error_msg(
                "invalid_argument",
                "source must match [a-z0-9_-]{1,64}",
            ));
        }
        if is_confirmed_exited(entry) {
            return Some(child_exited_msg());
        }
        // The detector is torn down on `Exited`, so losing it is the same
        // condition as the state check above, observed one step later.
        (!entry.record_status_event(status)).then(child_exited_msg)
    }

    /// Resolves the window a hook's request (`door` names it) speaks for,
    /// answering the refusal itself when it cannot: the token's shape is
    /// checked first, then the window is resolved, then the candidate is
    /// compared against that window's own token. See
    /// [`IpcServer::handle_status_event`] for why this order.
    fn authenticate_window(
        &self,
        outbound: &Outbound,
        door: &str,
        token: &str,
        window_id: Option<&str>,
    ) -> Option<Arc<WindowEntry>> {
        let auth_failed = || {
            error_msg(
                "auth_failed",
                &format!("{door} token is missing or does not match"),
            )
        };
        if !StatusToken::is_well_formed(token) {
            tracing::warn!("rejected a {door} carrying a malformed auth token");
            outbound.reply(&auth_failed());
            return None;
        }
        let Some(target) = window_id else {
            outbound.reply(&error_msg(
                "invalid_argument",
                &format!("{door} requires window_id"),
            ));
            return None;
        };
        let Some(entry) = self.registry.get(target) else {
            outbound.reply(&window_not_found_msg());
            return None;
        };
        if !entry.verify_status_token(token) {
            tracing::warn!(
                window_id = %entry.window_id(),
                "rejected a {door} carrying a token that is not this window's"
            );
            outbound.reply(&auth_failed());
            return None;
        }
        Some(entry)
    }

    /// Takes one token from the daemon-wide `StatusEvent` bucket,
    /// reporting whether the event may proceed.
    ///
    /// The guard is confined to this call so it is never held across an
    /// `await`; a poisoned lock is recovered rather than propagated, because a
    /// bucket left mid-update is still a usable throttle and losing the limiter
    /// entirely would be the worse failure.
    fn charge_status_event_budget(&self) -> bool {
        let mut bucket = self
            .rate_bucket
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        bucket.try_acquire(std::time::Instant::now())
    }

    /// Writes structured input (`key`, `text`, `paste`, `pointer`, `wheel`,
    /// `focus`) to the attached window's PTY as the bytes its child's modes
    /// call for, or applies it to this connection's view when it is not the
    /// child's to see: a selecting pointer, a wheel that moves the view,
    /// Shift+PageUp / Shift+PageDown. The attach gate is the authorization
    /// step for the interactive path and is never relaxed: an unattached
    /// connection cannot reach any PTY. A view action also applies on a
    /// window whose child exited. Answers only on failure.
    async fn handle_structured_input(
        &self,
        outbound: &Outbound,
        state: &mut ConnectionState,
        msg: &ClientMessage,
    ) {
        let Some(entry) = attached_or_refuse(outbound, state) else {
            return;
        };
        let input = entry.session().input_state();
        if let Some(action) = view_action(msg, input) {
            if let Err(message) = check_input_bounds(msg) {
                outbound.reply(&error_msg("invalid_argument", message));
                return;
            }
            let _ = entry
                .session()
                .with_model(move |screen| action.apply(screen))
                .await;
            state.view.note_change();
            return;
        }
        if is_exited_for_pty_write(&entry) {
            outbound.reply(&child_exited_msg());
            return;
        }
        if let Err(message) = check_input_bounds(msg) {
            outbound.reply(&error_msg("invalid_argument", message));
            return;
        }
        // The view's focus is not the child's to see as it stands: the daemon
        // tells the child the focus it should hold (see
        // `PtySession::set_view_focus`).
        if let ClientMessage::Focus { focused } = msg {
            entry.session().set_view_focus(*focused);
            return;
        }
        let bytes = encode_structured_input(msg, input);
        if bytes.is_empty() {
            return;
        }
        // A move with no button held carries no key or button, so it cannot
        // change the composer and does not count as another writer to a
        // delivery waiting on the window.
        if is_report_without_input(msg) {
            entry.session().send_report(bytes);
            return;
        }
        // As with xterm.js `scrollOnUserInput`, typing or pasting brings the view
        // back to the live screen; reports (focus, pointer, wheel) do not.
        if matches!(
            msg,
            ClientMessage::Key(_) | ClientMessage::Text { .. } | ClientMessage::Paste { .. }
        ) && entry
            .session()
            .with_model(screen_view::scroll_to_bottom)
            .await
            .unwrap_or(false)
        {
            state.view.note_change();
        }
        if entry
            .write_pty_input(&bytes, InputOrigin::Interactive)
            .await
            .is_err()
        {
            outbound.reply(&error_msg("internal", "failed to write input to the PTY"));
        }
    }

    /// Resizes the attached window's model and PTY, and tells its detector.
    /// Attach-gated exactly like structured input.
    async fn handle_resize(
        &self,
        outbound: &Outbound,
        state: &mut ConnectionState,
        cols: u16,
        rows: u16,
    ) {
        let Some(entry) = state.attached_entry() else {
            outbound.reply(&error_msg(
                "not_attached",
                "client is not attached to a window",
            ));
            return;
        };
        if is_exited_for_pty_write(&entry) {
            outbound.reply(&child_exited_msg());
            return;
        }
        // Out-of-range is invalid_argument, not a disconnect, and is never
        // clamped.
        if !crate::pty::size_within_limit(cols, rows) {
            outbound.reply(&size_out_of_range_msg());
            return;
        }
        // Armed after argument validation (so an invalid-argument train never
        // holds the attribution gate open) and before the PTY resize call (so
        // the redraw it produces cannot race ahead of the arm).
        entry.notify_resize_started();
        if entry.session().resize(cols, rows).await.is_err() {
            outbound.reply(&error_msg("internal", "failed to resize the PTY"));
            return;
        }
        // Detection reads the resized model itself; it only learns whether
        // the resize had to rebuild it.
        entry.propagate_resize();
        state.view.note_change();
    }

    /// Sends the attached window's next frame, if one is due (see
    /// [`View::capture`]); `full` sends every row whatever is awaited or
    /// held. The model's lock is held only to read the rows that changed;
    /// laying them out and encoding them happens away from it.
    async fn push_frames(&self, outbound: &Outbound, state: &mut ConnectionState, full: bool) {
        if !full && !state.view.wants_frame() {
            return;
        }
        let Some(entry) = state.attached_entry() else {
            return;
        };
        let session = entry.session();
        let child_exited = session.exit_status().is_some();
        let mut view = std::mem::take(&mut state.view);
        let read = session
            .with_model(move |screen| {
                let capture = view.capture(screen, child_exited, full);
                (view, capture)
            })
            .await;
        let Some((view, capture)) = read else {
            // The runtime is shutting down; a fresh view would start over.
            state.view = View::new();
            return;
        };
        state.view = view;
        let Some(capture) = capture else {
            return;
        };
        let encoded = tokio::task::spawn_blocking(move || {
            capture
                .into_parts()
                .into_iter()
                .map(|part| encode_frame(&DaemonMessage::Frame(part)))
                .collect::<Vec<_>>()
        })
        .await;
        match encoded {
            Ok(parts) => {
                for part in parts {
                    outbound.reply_encoded(part);
                }
            }
            // Nothing was sent, so nothing awaits an acknowledgement.
            Err(_) => state.view = View::new(),
        }
    }
}

/// Reads the previous boot's rows and the time they were recorded, through
/// the store's non-destructive read port (`open_readonly` + the `read_*_conn`
/// functions).
///
/// Blocking: SQLite work only, run inside `spawn_blocking`.
fn read_prev_configuration(
    db_path: &Path,
) -> Result<(Vec<SnapshotWindow>, Option<i64>), StoreError> {
    let conn = store::open_readonly(db_path)?;
    let rows = store::read_prev_snapshot_conn(&conn)?;
    let taken_at_epoch_s = store::read_prev_taken_at_conn(&conn)?;
    Ok((rows, taken_at_epoch_s))
}

/// Converts one stored row into its wire form. A pure field mapping: the
/// store's row type stays free of the protocol crate, and the protocol's
/// candidate stays free of the store's own bounds.
fn to_restore_candidate(row: SnapshotWindow) -> RestoreCandidate {
    RestoreCandidate {
        position: row.position,
        command: row.command,
        cwd: row.cwd,
        name: row.name,
        vendor: row.vendor,
        detection: row.detection,
        layout_json: row.layout_json,
        profile: row.profile,
        model: row.model,
        effort: row.effort,
    }
}

/// Converts one enumerated profile into its wire form. A pure field mapping,
/// mirroring [`to_restore_candidate`]: `cwd` becomes a `String` since the
/// daemon's own `PathBuf` has no portable wire representation.
fn to_profile_info(entry: crate::profile::ProfileEntry) -> ProfileInfo {
    ProfileInfo {
        id: entry.id,
        display_name: entry.display_name,
        detection: entry.detection,
        detection_warning: entry.detection_warning,
        cwd: entry.cwd.map(|path| path.to_string_lossy().into_owned()),
        command: entry.command,
        available: entry.available,
        unavailable_reason: entry.unavailable_reason,
        model: entry.model,
        effort: entry.effort,
        enabled: entry.enabled,
        unavailable_kind: entry.unavailable_kind,
        unavailable_guidance: entry.unavailable_guidance,
        limits: entry.limits,
    }
}

/// Keeps the leading candidates whose summed JSON byte length stays within
/// [`RESTORE_REPLY_BUDGET_BYTES`].
///
/// `candidates` arrives in ascending `position` order, so the kept part is
/// always a contiguous low-position prefix: the restore UI shows the windows
/// that were opened first rather than an arbitrary subset. A candidate that
/// cannot be serialized at all (not reachable for the `String`-only shape,
/// kept so a future field type cannot turn into a panic) is dropped the same
/// way. Either kind of loss warns once for the whole call.
fn within_reply_budget(candidates: Vec<RestoreCandidate>) -> Vec<RestoreCandidate> {
    let mut kept = Vec::with_capacity(candidates.len());
    let mut used = 0usize;
    let mut dropped = false;
    for candidate in candidates {
        let Ok(encoded) = serde_json::to_string(&candidate) else {
            dropped = true;
            continue;
        };
        used = used.saturating_add(encoded.len());
        if used > RESTORE_REPLY_BUDGET_BYTES {
            dropped = true;
            break;
        }
        kept.push(candidate);
    }
    if dropped {
        tracing::warn!(
            kept = kept.len(),
            "truncated the restore candidates to stay within the reply budget"
        );
    }
    kept
}

/// The `child_exited` gate: the window's confirmed status is the single
/// source of truth.
fn is_confirmed_exited(entry: &WindowEntry) -> bool {
    entry.current_status() == WindowStatus::Exited
}

/// The bytes structured input becomes for a window in `input`; empty when it
/// writes nothing. A pointer or wheel event off the screen is dropped, since
/// no report could name its cell. Selection and moving the view are not the
/// child's to see, so they write nothing here.
fn encode_structured_input(msg: &ClientMessage, input: InputState) -> Vec<u8> {
    let modes = input.modes;
    match msg {
        ClientMessage::Key(key) => input_encode::encode_key(key, modes),
        ClientMessage::Text { text } => input_encode::encode_text(text),
        ClientMessage::Paste { text } => input_encode::encode_paste(text, modes),
        ClientMessage::Pointer(pointer)
            if input.contains_cell(pointer.cell.col, pointer.cell.row) =>
        {
            match input_encode::encode_pointer(pointer, modes) {
                PointerOutcome::Report(bytes) => bytes,
                PointerOutcome::Select | PointerOutcome::Ignore => Vec::new(),
            }
        }
        ClientMessage::Wheel(wheel) if input.contains_cell(wheel.cell.col, wheel.cell.row) => {
            match input_encode::encode_wheel(wheel, modes) {
                WheelOutcome::Write(bytes) => bytes,
                WheelOutcome::ScrollView { .. } => Vec::new(),
            }
        }
        _ => Vec::new(),
    }
}

fn is_report_without_input(msg: &ClientMessage) -> bool {
    match msg {
        ClientMessage::Pointer(pointer) => {
            pointer.kind == PointerKind::Move && pointer.button == PointerButton::None
        }
        _ => false,
    }
}

/// The `child_exited` gate for the interactive PTY paths (structured input
/// and `resize`), which additionally honour the PTY's own exit observation as a
/// secondary fail-safe: it becomes true immediately, while the confirmed
/// status only follows after debounce.
fn is_exited_for_pty_write(entry: &WindowEntry) -> bool {
    is_confirmed_exited(entry) || entry.session().exit_status().is_some()
}

/// Awaits the next confirmed status change if `rx` is `Some`; otherwise never
/// resolves (which is how an unattached connection is excluded).
async fn recv_status(
    rx: &mut Option<broadcast::Receiver<StatusAt>>,
) -> Result<StatusAt, broadcast::error::RecvError> {
    match rx {
        Some(receiver) => receiver.recv().await,
        None => std::future::pending().await,
    }
}

/// Awaits a supersede notification if `rx` is `Some`; otherwise never
/// resolves.
async fn recv_kick(rx: &mut Option<oneshot::Receiver<()>>) {
    match rx {
        Some(receiver) => {
            let _ = receiver.await;
        }
        None => std::future::pending().await,
    }
}

/// Awaits the next change of `rx` if it is `Some`: `true` on a change,
/// `false` once its sender is gone. Never resolves without a receiver.
async fn next_change<T>(rx: &mut Option<watch::Receiver<T>>) -> bool {
    match rx {
        Some(receiver) => receiver.changed().await.is_ok(),
        None => std::future::pending().await,
    }
}

/// `rx` with its current value marked as seen.
fn seen<T>(mut rx: watch::Receiver<T>) -> watch::Receiver<T> {
    rx.mark_unchanged();
    rx
}

/// The window this connection is attached to, or `None` after refusing the
/// request with `not_attached`.
fn attached_or_refuse(outbound: &Outbound, state: &ConnectionState) -> Option<Arc<WindowEntry>> {
    let entry = state.attached_entry();
    if entry.is_none() {
        outbound.reply(&error_msg(
            "not_attached",
            "client is not attached to a window",
        ));
    }
    entry
}

/// What structured input does to the view instead of reaching the child.
enum ViewAction {
    Page(ViewPage),
    Select(PointerInput),
    Scroll(i32),
}

impl ViewAction {
    fn apply(self, screen: &mut VtScreen) {
        match self {
            Self::Page(page) => screen_view::page_view(screen, page == ViewPage::Up),
            Self::Select(pointer) => screen_view::select(screen, &pointer),
            Self::Scroll(lines) => screen_view::scroll_view(screen, lines),
        }
    }
}

/// The view action `msg` is for a window in `input`, if it is one: a key that
/// pages the view, a pointer event that selects, a wheel that moves the
/// view. A pointer or wheel event off the screen is none.
fn view_action(msg: &ClientMessage, input: InputState) -> Option<ViewAction> {
    let modes = input.modes;
    match msg {
        ClientMessage::Key(key) => input_encode::view_page(key).map(ViewAction::Page),
        ClientMessage::Pointer(pointer)
            if input.contains_cell(pointer.cell.col, pointer.cell.row) =>
        {
            matches!(
                input_encode::encode_pointer(pointer, modes),
                PointerOutcome::Select
            )
            .then_some(ViewAction::Select(*pointer))
        }
        ClientMessage::Wheel(wheel) if input.contains_cell(wheel.cell.col, wheel.cell.row) => {
            match input_encode::encode_wheel(wheel, modes) {
                // The wheel's negative is up; the view's positive is back
                // into the history.
                WheelOutcome::ScrollView { lines } => {
                    Some(ViewAction::Scroll(lines.saturating_neg()))
                }
                WheelOutcome::Write(_) => None,
            }
        }
        _ => None,
    }
}

/// Encodes one [`DaemonMessage`] into its wire frame (LPJF).
fn encode_frame(msg: &DaemonMessage) -> Vec<u8> {
    frame_encode(&encode_daemon_message(msg))
}

/// Writes one connection's queued frames out, in order.
///
/// This is the only place that blocks on the client's read pace. It owns the
/// write half exclusively, so the connection task can keep reading while a
/// write is stalled -- the property that breaks the read/write deadlock (see
/// [`Outbound`]).
///
/// The loop ends when the queue is closed *and* drained (the connection task
/// finished, so its last frames -- e.g. `superseded` -- are still flushed), or
/// at the first write error. A write error also closes the queue, so the
/// connection task observes the dead peer instead of queueing frames nobody
/// will read.
async fn write_loop(mut writer: WriteHalf<Connection>, mut rx: mpsc::Receiver<Vec<u8>>) {
    while let Some(frame) = rx.recv().await {
        if writer.write_all(&frame).await.is_err() {
            rx.close();
            return;
        }
    }
}

/// Milliseconds since the Unix epoch, saturating to `u64::MAX` on overflow and
/// `0` on a clock set before the epoch: it only orders and displays records.
fn now_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| u64::try_from(elapsed.as_millis()).unwrap_or(u64::MAX))
        .unwrap_or(0)
}

fn error_msg(code: &str, message: &str) -> DaemonMessage {
    DaemonMessage::Error {
        code: code.to_string(),
        message: message.to_string(),
        rejected_argument: None,
    }
}

/// [`crate::profile::ProfileLaunchError`] as the wire code the three
/// profile-addressed launch paths (`open_window_from_profile`,
/// `open_task_window`, `send_handoff`) send through
/// [`IpcServer::plan_from_profile`]: `Disabled` alone is `profile_disabled`,
/// every other variant a caller mistake reported as `invalid_argument`.
fn profile_launch_error_msg(err: &crate::profile::ProfileLaunchError) -> DaemonMessage {
    match err {
        crate::profile::ProfileLaunchError::Disabled => {
            error_msg("profile_disabled", &err.to_string())
        }
        _ => error_msg("invalid_argument", &err.to_string()),
    }
}

/// The single answer given for a `window_id` the daemon does not know,
/// including one that has already been evicted.
fn window_not_found_msg() -> DaemonMessage {
    error_msg("window_not_found", "no window with the requested window_id")
}

/// The single answer given for an operation aimed at a window whose child has
/// already exited.
fn child_exited_msg() -> DaemonMessage {
    error_msg("child_exited", "child process has exited")
}

/// The one wording of "this window takes no daemon-composed text", shared by
/// every layer that refuses for that reason so the guidance cannot drift.
/// It names only the alternatives that fit such a window, so it cannot be
/// read as an invitation to reopen the window under another definition.
const NO_COMPOSER_MESSAGE: &str = "this window declares no composer, or declares one without \
     an input probe, so the daemon writes no line into it (naming a probe in the definition's \
     [input] table lets it): to run a command, open a window with it (open_window's command); \
     to press keys in it, use send_keys";

/// Why a window that would be handed a request at launch is refused when its
/// profile declares no way to hand one over. It keeps the alternatives
/// [`NO_COMPOSER_MESSAGE`] names.
const NO_LAUNCH_PROMPT_MESSAGE: &str = "this profile declares no launch_prompt in its [delivery] \
     table, so a request cannot be handed to its CLI at launch: to run a command, open a window \
     with it (open_window's command); to press keys in a window, use send_keys";

/// The `no_composer` refusal, answered wherever a request would type
/// daemon-composed text into a window that declares no composer.
fn no_composer_msg() -> DaemonMessage {
    error_msg("no_composer", NO_COMPOSER_MESSAGE)
}

/// The `not_written` message text for a declined [`SubmitPlacement`], worded
/// the same way as the reason list in `docs/mcp-setup.md`'s `not_written`
/// entry: each reason gets its own words rather than one
/// text asserted for all of them, and none of them claims more than the
/// daemon actually observed (in particular, [`NotPlacedReason::Unobservable`]
/// never says content was present).
fn not_placed_message(not_placed: &NotPlaced) -> &'static str {
    match (not_placed.reason, not_placed.envelope_remains) {
        (NotPlacedReason::Foreign, _) => {
            "another actor's text was already in the window's composer, so nothing was sent"
        }
        (NotPlacedReason::Unobservable, _) => {
            "the window's composer could not be observed, so nothing was sent"
        }
        (NotPlacedReason::NotReflected, true) => {
            "the text was written but the window did not take it (it discarded the text, or \
             the request was cancelled before the text showed), and it could not be confirmed \
             withdrawn; the text, a partly erased copy of it, or the characters written to \
             withdraw it may still be in the composer"
        }
        (NotPlacedReason::NotReflected, false) => {
            "the text was written but the window did not take it (it discarded the text, or \
             the request was cancelled before the text showed), so it was withdrawn and \
             nothing remains"
        }
        (NotPlacedReason::Interrupted, true) => {
            "another write reached the window before the submit key, so the submit key was \
             not written; some of the daemon's own text may still be in the composer alongside \
             that write, because it could not be confirmed gone"
        }
        (NotPlacedReason::Interrupted, false) => {
            "another write reached the window before the submit key, so the submit key was \
             not written; no part of the daemon's own text is known to remain in the composer"
        }
        (NotPlacedReason::NotAccepting, true) => {
            "the window's screen was not accepting input (an approval prompt, most likely), so \
             the submit key was not sent; text already written to the window may still be in \
             the composer"
        }
        (NotPlacedReason::NotAccepting, false) => {
            "the window's screen was not accepting input (an approval prompt, most likely), so \
             nothing was sent"
        }
        (NotPlacedReason::NoComposer, _) => NO_COMPOSER_MESSAGE,
        // Only a send that pressed the submit key on an earlier attempt ends
        // with these, and `send_text` makes a single attempt.
        (NotPlacedReason::Taken, _) => {
            "the window had already taken the text after an earlier submit key, so nothing \
             more was sent"
        }
        (NotPlacedReason::Vanished, _) => {
            "the submit key was written on an earlier attempt, but whether the window took the \
             text could not be confirmed (another write may have reached the window since, or an \
             earlier write was left unconfirmed), so nothing more was sent"
        }
    }
}

/// Translates a control primitive's failure into its client-facing answer.
///
/// The error's own payload is a write-failure summary meant for a log line;
/// the client is told only that the operation failed internally, so no OS
/// error text ever crosses the protocol boundary.
fn control_error_msg(err: &ControlError) -> DaemonMessage {
    // Matched exhaustively (no `_` arm) so a new failure mode must be given a
    // client-facing kind rather than silently inheriting one.
    match err {
        ControlError::ChildExited => child_exited_msg(),
        ControlError::WriteFailed(summary) => {
            tracing::warn!(error = %summary, "control operation failed");
            error_msg("internal", "the control operation failed")
        }
    }
}

/// Whether this connection may become an approver: the daemon was told which
/// executable an approver runs as, the peer's own executable was resolved
/// when the connection was accepted, and the two are the same path.
///
/// Both sides were canonicalized once already -- `--approver-exe` at startup
/// and the peer's path at capture time -- so this is a plain comparison with
/// no filesystem access. An unconfigured `--approver-exe` or an unresolvable
/// peer therefore denies (fail-closed).
fn approver_allowed(state: &ConnectionState, config: &DaemonConfig) -> bool {
    match (
        config.approver_exe.as_deref(),
        state.peer_executable.as_deref(),
    ) {
        (Some(configured), Some(peer)) => configured == peer,
        _ => false,
    }
}

/// Answers an `identify` that did not authenticate, and reports whether the
/// connection has now failed often enough to be closed.
///
/// The reply is the same whatever failed, so a caller cannot use the answer
/// to tell a malformed token apart from a wrong one.
fn reject_identify(outbound: &Outbound, state: &mut ConnectionState) -> FrameOutcome {
    state.identify_failures = state.identify_failures.saturating_add(1);
    if state.identify_failures == 1 {
        // One line per connection: a client retrying in a loop must not be
        // able to flood the log with its own failures.
        tracing::warn!("rejecting an identify whose credential does not match the target window");
    }
    outbound.reply(&error_msg("auth_failed", IDENTIFY_AUTH_FAILED_MESSAGE));
    if state.identify_failures >= IDENTIFY_FAILURE_LIMIT {
        FrameOutcome::Disconnect
    } else {
        FrameOutcome::Continue
    }
}

/// The body of a payload that is exactly one line: `Some(body)` when `text`
/// ends in a single `\r`, `\n` or `\r\n` terminator, holds no other CR or LF,
/// and leaves a non-empty body once that terminator is removed.
///
/// The subject of the judgment is the payload the daemon received. A bridge
/// may normalize line endings before sending, but a client speaking the wire
/// protocol directly does not, so all three terminators are read alike and
/// both kinds of caller reach the same outcome.
///
/// # Examples
///
/// ```
/// use yagura_daemon::ipc::single_line_body;
///
/// assert_eq!(single_line_body("run the tests\r"), Some("run the tests"));
/// assert_eq!(single_line_body("run the tests\n"), Some("run the tests"));
/// assert_eq!(single_line_body("run the tests\r\n"), Some("run the tests"));
/// assert_eq!(single_line_body("first\rsecond\r"), None, "two lines, not one");
/// assert_eq!(single_line_body("no terminator"), None);
/// assert_eq!(single_line_body("\r"), None, "the body would be empty");
/// ```
#[must_use]
pub fn single_line_body(text: &str) -> Option<&str> {
    let body = text
        .strip_suffix("\r\n")
        .or_else(|| text.strip_suffix('\n'))
        .or_else(|| text.strip_suffix('\r'))?;
    if body.is_empty() || body.contains(['\r', '\n']) {
        return None;
    }
    Some(body)
}

/// Writes an approved `send_text` payload to the window's PTY and answers the
/// caller. Shared by every path that may reach the write, so that the audit
/// line and the failure mapping exist once.
///
/// The permit is taken by value: only a gate can issue one, so no caller can
/// reach this write without having passed one. `requester` turns `true` once
/// the connection that asked has gone: nothing new is written after that,
/// beyond taking this write's own body back.
async fn execute_send_text(
    outbound: &Outbound,
    entry: &WindowEntry,
    text: &str,
    _permit: Permit<permit::SendMessage>,
    requester: Option<watch::Receiver<bool>>,
) {
    // Audit trail: the window and the payload size only. The text itself
    // is never logged, matching the rule that no raw PTY data is logged.
    tracing::info!(
        window_id = %entry.window_id(),
        text_len = text.len(),
        "accepted send_text"
    );

    // One line ending in one terminator is this window's "submit this line",
    // so it is judged against the composer before being written; a
    // plain `send_text` carries no residue of its own, since it has no
    // notion of a previous attempt at the same body. Anything else is a
    // sequence of writes the caller composed, written verbatim -- outside
    // the judgment's scope.
    let request = match single_line_body(text) {
        Some(body) => SubmitRequest::Judged {
            body,
            residue: None,
            stage: SendStage::BeforeSubmitKey,
        },
        None => SubmitRequest::RawMessage(plan_raw_write(text)),
    };

    match submit_line_to_pty(entry, request, requester)
        .await
        .map(|judgment| judgment.placement)
    {
        Ok(SubmitPlacement::Placed(_outcome)) => {
            outbound.reply(&DaemonMessage::TextSent {
                window_id: entry.window_id().to_string(),
            });
        }
        Ok(SubmitPlacement::NotPlaced(not_placed)) => {
            outbound.reply(&error_msg("not_written", not_placed_message(&not_placed)));
        }
        // Secondary fail-safe for the same condition the confirmed status is
        // the primary source of truth for: the child can exit between the
        // pre-validation and this write.
        Err(PtyError::ChildExited) => outbound.reply(&child_exited_msg()),
        Err(_) => {
            outbound.reply(&error_msg("internal", "failed to write text to the PTY"));
        }
    }
}

/// Writes an approved control-key sequence without composer inspection,
/// encoded with the window's modes at this moment.
///
/// The input lock is not taken: these keys are often what dismisses a dialog
/// that keeps a delivery holding that lock. The keys are written at once,
/// behind the take-back of such a delivery's unsent text (none in front of
/// the window's own interrupt keys), and the write moves
/// the ledger, so the delivery writes nothing more of its own plan; it answers
/// once it has checked the composer on a screen that accepts a write (see
/// [`WindowEntry::submit_message`]).
async fn execute_send_keys(
    outbound: &Outbound,
    entry: &WindowEntry,
    keys: &[ControlKey],
    _permit: Permit<permit::SendKeys>,
) {
    let modes = entry.session().input_state().modes;
    // Validated before the gate, so no key is unknown here.
    let bytes: Vec<u8> = keys
        .iter()
        .filter_map(|key| input_encode::encode_control_key(*key, modes))
        .flatten()
        .collect();
    tracing::info!(
        window_id = %entry.window_id(),
        key_bytes = bytes.len(),
        "accepted send_keys"
    );
    match entry.write_pty_input(&bytes, InputOrigin::Message).await {
        Ok(()) => outbound.reply(&DaemonMessage::KeysSent {
            window_id: entry.window_id().to_string(),
        }),
        Err(PtyError::ChildExited) => outbound.reply(&child_exited_msg()),
        Err(_) => outbound.reply(&error_msg("internal", "failed to write keys to the PTY")),
    }
}

/// What one call to [`submit_line_to_pty`] asks to have written: the caller's
/// own byte sequence behind the write gate alone, or a single line whose
/// placement [`WindowEntry::submit_message`] judges against the window's
/// composer.
enum SubmitRequest<'a> {
    /// A caller-composed byte sequence outside the judgment's declared scope,
    /// but still behind the write gate: a `send_text` body with no line
    /// terminator, or a bare submit-key terminator.
    RawMessage(SubmitPlan),
    /// A single line, judged before being written. `residue` is this send's
    /// own envelope from a previous attempt, if it has one; `stage` is whether
    /// a previous attempt pressed the submit key.
    Judged {
        body: &'a str,
        residue: Option<ComposerResidue<'a>>,
        stage: SendStage,
    },
}

/// Delivers an approved interrupt the way the configured method prescribes
/// and answers the caller.
///
/// The permit is taken by value: only a gate can issue one, so no caller can
/// reach the interrupt without having passed one.
async fn execute_interrupt(
    outbound: &Outbound,
    entry: &WindowEntry,
    config: &DaemonConfig,
    _permit: Permit<permit::InterruptWindow>,
) {
    match control::interrupt_window(entry, config).await {
        Ok(()) => outbound.reply(&DaemonMessage::WindowInterrupted {
            window_id: entry.window_id().to_string(),
        }),
        Err(err) => outbound.reply(&control_error_msg(&err)),
    }
}

/// Terminates an approved window's whole process tree and answers the caller.
///
/// The GUI drops this window's pane as soon as this replies, without
/// re-querying `list_windows`, while its periodic poll reopens a pane for any
/// window it still sees as something other than `Exited`. Once the process
/// tree is torn down, this therefore waits (bounded by
/// [`DaemonConfig::close_settle_ms`]) for the *confirmed* status to settle to
/// `Exited` before replying. It is a best-effort mitigation: reaching the
/// bound never turns the reply into an error.
///
/// The wait runs inline rather than in a spawned task, because it is bounded
/// by a small configured budget and blocks only the caller that asked for it.
/// Spawning it would either send `WindowClosed` before the wait finished --
/// reopening the very race the wait closes -- or need the same
/// abort-on-disconnect bookkeeping `wait_for_status` carries.
///
/// The permit is taken by value: only a gate can issue one, so no caller can
/// reach the teardown without having passed one.
async fn execute_close(
    outbound: &Outbound,
    registry: &WindowRegistry,
    config: &DaemonConfig,
    entry: &WindowEntry,
    force: bool,
    _permit: Permit<permit::CloseWindow>,
) {
    if let Err(err) = close_for_cleanup(registry, config, entry, force).await {
        outbound.reply(&control_error_msg(&err));
        return;
    }

    let settle_timeout = Duration::from_millis(config.close_settle_ms);
    let outcome =
        waiter::wait_for_status(entry.watch_status(), WindowStatus::Exited, settle_timeout).await;
    match outcome {
        WaitOutcome::Reached => {}
        WaitOutcome::Timeout { current } | WaitOutcome::ChildExited { current } => {
            tracing::warn!(
                window_id = entry.window_id(),
                status = ?current,
                "close_settle_ms elapsed before the window's confirmed status \
                 settled to exited; replying window_closed anyway (fail-safe)"
            );
        }
        // Nothing was observed, so no status is named: the window left the
        // registry while this wait was outstanding.
        WaitOutcome::Detached => {
            tracing::warn!(
                window_id = entry.window_id(),
                "the window left the registry before its confirmed status settled to \
                 exited; replying window_closed anyway (fail-safe)"
            );
        }
    }
    outbound.reply(&DaemonMessage::WindowClosed {
        window_id: entry.window_id().to_string(),
    });
}

/// Terminates a window's process tree and takes it out of the restore
/// candidates, with nobody to answer.
///
/// Shared by the explicit close and by the teardown of a handoff that was
/// never delivered, so a window is retired the same way whoever retires it.
/// Only an explicit retirement excludes a window from the restore candidates,
/// which is why the marking lives here rather than at the reply.
///
/// # Errors
///
/// Returns whatever [`control::close_window`] refused with, in which case
/// nothing was marked closed.
async fn close_for_cleanup(
    registry: &WindowRegistry,
    config: &DaemonConfig,
    entry: &WindowEntry,
    force: bool,
) -> Result<(), ControlError> {
    let grace = Duration::from_millis(config.close_grace_ms);
    control::close_window(entry, force, grace).await?;
    entry.mark_closed();
    registry.request_snapshot();
    Ok(())
}

/// Executes one submit request on an agent's behalf: the only point that
/// writes into a window's PTY for `send_text` and `send_handoff` alike.
/// `requester` is handed to [`WindowEntry::submit_raw_gated`] and
/// [`WindowEntry::submit_message`] as is.
///
/// # Errors
///
/// Returns the [`PtyError`] a write failed with; the caller decides how a
/// failure is reported. A placement [`WindowEntry::submit_message`] declined
/// is never an error (see [`SubmitPlacement::NotPlaced`]).
async fn submit_line_to_pty(
    entry: &WindowEntry,
    request: SubmitRequest<'_>,
    requester: Option<watch::Receiver<bool>>,
) -> Result<SubmitJudgment, PtyError> {
    match request {
        SubmitRequest::RawMessage(plan) => entry.submit_raw_gated(&plan, requester).await,
        SubmitRequest::Judged {
            body,
            residue,
            stage,
        } => entry.submit_message(body, residue, stage, requester).await,
    }
}

/// Submits a composed handoff envelope into `entry`'s input, confirms
/// delivery, resends it while the target stays ready and the definition
/// allows it, and answers the requester.
///
/// The one place a handoff is delivered from, so the audit line, the failure
/// mapping and the order in which teardown, release and answer happen exist
/// once. Whether the write failed or a `delivery` verdict was reached, the
/// slot is released strictly before the answer goes out, so a caller holding
/// its answer also has its budget back. A write failure closes the window
/// this handoff opened (`job.opened`). Every other ending's teardown turns on
/// whether the envelope remains in the receiver's composer, never on the
/// `delivery` value or on whether any byte was written: a
/// window this handoff opened is closed only when nothing of its own is left
/// there, since a window with nothing left in front of the receiver is one
/// the requester has no way to make use of.
///
/// The algorithm below is `entry`'s window: never a wall-clock deadline. Its
/// upper bound is the resend count, `InputProfile::resend_max`, because a
/// deadline computed before the input lock is acquired would be spent by
/// another writer's queued sequence before this one's first byte is written.
///
/// Two waits happen before the first byte. This window's delivery lock comes
/// first and is held until this delivery's own verdict is final, re-sends
/// included, so no other delivery writes into the window in between;
/// turns come in the order deliveries reached that lock,
/// which is not the order their requests arrived. Only so many deliveries
/// may queue for it (`max_turn_waiters_per_window`); the one past that, and
/// one that finds `budget` spent right after winning the turn, ends in
/// [`DeliveryEnd::Exhausted`] before its first byte. Under that lock, a window
/// still in its startup phase is then given `handoff_ready_timeout_secs` to
/// hold a status that accepts input -- the only one of the two bounded by the
/// clock. Reaching that bound is not a failure of its own -- the envelope is
/// written either way.
/// Waits for `closed` to signal a disconnect. Never resolves when `closed` is
/// `None`: chat delivery (`run_delivery(.., None, ..)`) has no single
/// requester connection to watch, so its call to [`run_delivery`] can never
/// end in [`DeliveryEnd::Aborted`].
async fn wait_for_disconnect(closed: &mut Option<watch::Receiver<bool>>) {
    match closed {
        Some(rx) => {
            let _ = rx.changed().await;
        }
        None => std::future::pending::<()>().await,
    }
}

/// Whether the requester `closed` watches has gone; never for `None`.
fn requester_left(closed: &Option<watch::Receiver<bool>>) -> bool {
    closed
        .as_ref()
        .is_some_and(|rx| *rx.borrow() || rx.has_changed().is_err())
}

/// The core of typing a parcel into a window: takes `entry`'s delivery lock,
/// waits out its startup phase if it is still building one, subscribes to its
/// status, then composes the text (`compose`, under the lock, so what it
/// checks cannot change while the delivery waits its turn; `None` ends in
/// [`DeliveryEnd::Skipped`]), writes it and confirms/resends it under
/// `entry`'s own input profile.
///
/// `closed` is raised when whoever the text is for no longer wants it. Raised
/// before the submit key, it ends the delivery and takes the body back; once
/// the key is pressed it is no longer watched, so the delivery runs to its
/// real outcome and cleans up after itself.
/// `ready_hold_wait_ms` is the caller's own prior readiness wait, carried
/// onward into the verdict. `carried` is what an earlier call for the
/// same delivery left in the composer (`None` for a first call); the residue
/// this call leaves is returned in [`DeliveryEnd::Finished`]. `submitted` is
/// raised when the submit key is pressed, for a caller that answers its
/// requester before the delivery's cleanup is done.
#[allow(clippy::too_many_arguments)] // each is a distinct input of the one delivery
async fn run_delivery<'a>(
    entry: &'a WindowEntry,
    compose: &(dyn Fn() -> Option<String> + Sync),
    config: &DaemonConfig,
    mut closed: Option<watch::Receiver<bool>>,
    mut ready_hold_wait_ms: u128,
    carried: Option<CarriedResidue>,
    budget: SharedBudget,
    submitted: Option<&watch::Sender<bool>>,
) -> DeliveryEnd<'a> {
    // Open for the whole delivery, its waits included, so the child holds the
    // focus from the first wait on a screen it may draw only when focused.
    let _focus = entry.session().open_focus_interval();
    let profile = entry.input_profile();
    let mut resent = 0u8;
    // Times the submit key was actually pressed (`attempts`
    // means submit-key presses, never attempts at the write itself; a run
    // that never places the envelope reports 0).
    let mut placements = 0u8;
    // This delivery's own belief about what its envelope currently looks
    // like in the composer, carried into the next attempt's `residue`:
    // `None` until an attempt leaves something of this delivery's own
    // behind, and this envelope's text paired with the window's write count
    // at that instant once one does. The count is what tells this delivery's
    // own residue from anyone else's identical bytes, and
    // only the attempt that wrote can say what it was.
    let mut residue: Option<ComposerResidue<'_>> =
        carried.as_ref().map(|carried| ComposerResidue {
            body: &carried.body,
            write_seq: carried.write_seq,
        });
    let ready_timeout = Duration::from_secs(config.handoff_ready_timeout_secs);

    // Both waits before the first byte -- this window's turn, then its startup
    // phase -- sit under one disconnect watch, checked first so a requester
    // that left during either is never written on behalf of. The turn is taken
    // before the gate is judged, so no other delivery can land between that
    // judgment and this envelope's first byte.
    let acquired = tokio::select! {
        biased;
        () = wait_for_disconnect(&mut closed) => {
            return DeliveryEnd::Aborted;
        }
        acquired = async {
            let Some(guard) = entry
                .lock_delivery_bounded(config.max_turn_waiters_per_window)
                .await
            else {
                return Err(Scarce::TurnWaiters);
            };
            // Drawn right after the turn is won, so a window that never
            // frees its turn holds one unit of the shared budget, not one per
            // delivery queued behind it.
            let shared = match &budget {
                SharedBudget::NotNeeded => None,
                SharedBudget::Counter { counter, max } => {
                    let Some(unit) = CounterGuard::try_take(counter, *max) else {
                        return Err(Scarce::SharedBudget);
                    };
                    Some(unit)
                }
            };
            let mut waited = 0u128;
            // The startup phase is the one thing worth waiting out before the
            // first byte: a prompt painted while a CLI is still starting up
            // takes writes that never reach the agent behind it.
            match entry.startup_evidence() {
                // Nothing pending: this window has already held a status that
                // accepts input for the whole hold, once, and that never comes back.
                StartupEvidence::Left => {}
                // Nothing to accumulate from either, so waiting could only delay an
                // envelope the target can still queue for itself.
                StartupEvidence::Absent => {}
                StartupEvidence::Building => {
                    // Stamped after the turn is won, so time spent queued behind
                    // another delivery is not reported as this one's readiness wait.
                    let started = Instant::now();
                    let outcome = waiter::wait_for_any_held(
                        entry.watch_status(),
                        READY_STATUSES,
                        profile.ready_hold,
                        ready_timeout,
                    )
                    .await;
                    waited = started.elapsed().as_millis();
                    if outcome == WaitOutcome::Reached {
                        entry.note_left_startup_phase();
                    }
                    // A hold that never completed is not an answer: the envelope is
                    // still written, and the delivery verdict reports what came of it.
                }
            }
            Ok((guard, shared, waited))
        } => acquired,
    };
    let (guard, _shared, gate_wait_ms) = match acquired {
        Ok(acquired) => acquired,
        Err(scarce) => return DeliveryEnd::Exhausted(scarce),
    };
    ready_hold_wait_ms += gate_wait_ms;

    // Subscribed after the gate and before the first byte, so a status change
    // published while the gate was waiting cannot be read as a reaction to
    // this envelope.
    let mut rx = entry.subscribe_status();

    let Some(composed) = compose() else {
        return DeliveryEnd::Skipped;
    };
    let envelope = composed.as_str();
    // Whether this envelope remains in the receiver's composer once the loop
    // stops; the only fact the teardown decision reads. `None` once the
    // submit key has been pressed: the envelope may already be in front of
    // the receiver, so no later attempt's report can make the window safe to
    // close. Before that, it remains if any declined attempt left it behind:
    // a later attempt that finds nothing of its own cannot vouch for an
    // earlier write that has yet to be painted.
    let mut envelope_remains: Option<bool> = Some(false);
    let mut foreign_draft: bool;
    // The last attempt that pressed the submit key, for a verdict a later
    // attempt reaches without pressing it again.
    let mut last_submit: Option<SubmitOutcome> = None;

    let verdict = loop {
        // Decided here rather than from `residue`: a declined attempt can
        // drop the residue while the key it pressed earlier still counts.
        let stage = if placements > 0 {
            SendStage::AfterSubmitKey
        } else {
            SendStage::BeforeSubmitKey
        };
        let request = SubmitRequest::Judged {
            body: envelope,
            residue,
            stage,
        };
        let judgment = match submit_line_to_pty(entry, request, closed.clone()).await {
            Ok(judgment) => judgment,
            Err(err) => {
                return DeliveryEnd::WriteFailed(err, guard);
            }
        };
        residue = judgment.residue_write_seq.map(|write_seq| ComposerResidue {
            body: envelope,
            write_seq,
        });
        let (outcome, after_key) = match judgment.placement {
            SubmitPlacement::Placed(outcome) => {
                placements += 1;
                envelope_remains = None;
                // Submitted: nothing of the envelope can be taken back now, so
                // what follows (a resend's probe included) runs to its end
                // instead of stopping with a probe left in the composer.
                closed = None;
                if let Some(submitted) = submitted {
                    submitted.send_replace(true);
                }
                foreign_draft = false;
                last_submit = Some(outcome);
                (outcome, judgment.residue_write_seq)
            }
            SubmitPlacement::NotPlaced(NotPlaced {
                reason: reason @ (NotPlacedReason::Taken | NotPlacedReason::Vanished),
                ..
            }) => {
                foreign_draft = false;
                let delivery = if reason == NotPlacedReason::Taken {
                    TypedOutcome::Confirmed
                } else {
                    TypedOutcome::Unconfirmed
                };
                break DeliveryVerdict {
                    delivery,
                    attempts: placements,
                    submitted_at: last_submit.map_or_else(Instant::now, |last| last.submitted_at),
                    status_before_submit: last_submit
                        .map_or_else(|| entry.current_status(), |last| last.status_before_submit),
                    ready_hold_wait_ms,
                };
            }
            SubmitPlacement::NotPlaced(NotPlaced {
                envelope_remains: remains,
                reason,
            }) => {
                foreign_draft = judgment.foreign_draft;
                // A declined write still spends this delivery's resend
                // budget: re-observing and retrying is what lets a
                // momentarily unwritable composer (a startup discard window,
                // a screen not yet caught up) resolve on its own, without
                // this loop ever writing on top of something it should not
                // touch. `attempts` (`placements`) is unaffected: the submit
                // key was not pressed.
                resent += 1;
                envelope_remains = envelope_remains.map(|earlier| earlier || remains);
                let out_of_resends = !profile.may_resend() || resent > profile.resend_max;
                if out_of_resends || requester_left(&closed) {
                    // `not_written` means the submit key was never
                    // written for this envelope, which is
                    // what a run that never placed anything is. A decline
                    // that follows an earlier placement is not: that
                    // envelope was submitted and simply went unconfirmed.
                    let delivery = if placements == 0 {
                        // The reason a `send_text` caller reads in its error
                        // message is logged here instead, so an operator has
                        // the same distinction available for a typed delivery.
                        tracing::info!(
                            window_id = %entry.window_id(),
                            reason = ?reason,
                            envelope_remains = ?envelope_remains,
                            "typed delivery was not written"
                        );
                        TypedOutcome::NotWritten
                    } else {
                        TypedOutcome::Unconfirmed
                    };
                    break DeliveryVerdict {
                        delivery,
                        attempts: placements,
                        submitted_at: last_submit
                            .map_or_else(Instant::now, |last| last.submitted_at),
                        status_before_submit: last_submit.map_or_else(
                            || entry.current_status(),
                            |last| last.status_before_submit,
                        ),
                        ready_hold_wait_ms,
                    };
                }
                continue;
            }
        };

        if profile.delivery_confirm.is_zero() {
            break DeliveryVerdict {
                delivery: TypedOutcome::NotObserved,
                attempts: placements,
                submitted_at: outcome.submitted_at,
                status_before_submit: outcome.status_before_submit,
                ready_hold_wait_ms,
            };
        }
        let unanswered = DeliveryVerdict {
            delivery: TypedOutcome::Unconfirmed,
            attempts: placements,
            submitted_at: outcome.submitted_at,
            status_before_submit: outcome.status_before_submit,
            ready_hold_wait_ms,
        };
        let evidence =
            wait_for_submit_evidence(entry, &mut rx, outcome, after_key, profile.delivery_confirm)
                .await;
        match evidence {
            SubmitEvidence::Taken => {
                break DeliveryVerdict {
                    delivery: TypedOutcome::Confirmed,
                    attempts: placements,
                    submitted_at: outcome.submitted_at,
                    status_before_submit: outcome.status_before_submit,
                    ready_hold_wait_ms,
                };
            }
            SubmitEvidence::Unanswered => break unanswered,
            SubmitEvidence::Unobservable => {
                break DeliveryVerdict {
                    delivery: TypedOutcome::NotObserved,
                    attempts: placements,
                    submitted_at: outcome.submitted_at,
                    status_before_submit: outcome.status_before_submit,
                    ready_hold_wait_ms,
                };
            }
            SubmitEvidence::Quiet => {
                resent += 1;
                let out_of_resends = !profile.may_resend() || resent > profile.resend_max;
                // The target left the ready statuses on its own while this
                // task was waiting: it may already have taken the envelope,
                // so it is never written to again on the daemon's own
                // judgement.
                let left_ready = !is_ready(entry.current_status());
                if out_of_resends || left_ready {
                    break unanswered;
                }
                // Otherwise loop again with this placement's own envelope
                // still carried as `residue`: the composer still holds it
                // unconfirmed, so the next attempt resends the submit key
                // alone if the screen or the write record still vouches for
                // it.
            }
        }
    };

    drop(guard);
    let residue = residue.map(|residue| CarriedResidue {
        body: residue.body.to_string(),
        write_seq: residue.write_seq,
    });
    DeliveryEnd::Finished {
        verdict,
        residue,
        foreign_draft,
    }
}

/// What [`wait_for_submit_evidence`] settled on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmitEvidence {
    /// The envelope was taken as one message: the composer read empty with
    /// nothing written since the submit key, or a window that accepted input
    /// right before the key moved to working after it.
    Taken,
    /// Neither showed within the bound.
    Quiet,
    /// Status changes were lost, so a reaction may have been missed.
    Unobservable,
    /// Nothing more can show: the child exited, or status changes were lost
    /// after another writer had already written.
    Unanswered,
}

/// Waits, for at most `bound` after the submit key `outcome` describes, for
/// evidence the receiver took the envelope: the composer reading empty with
/// the ledger still at `after_key` (`None` waits for no such reading), or,
/// when the window accepted input right before the key, its status moving to
/// working. Whichever shows first settles it.
async fn wait_for_submit_evidence(
    entry: &WindowEntry,
    rx: &mut broadcast::Receiver<StatusAt>,
    outcome: SubmitOutcome,
    after_key: Option<u64>,
    bound: Duration,
) -> SubmitEvidence {
    // A status that did not accept input right before the key gives no
    // starting point that a reaction could be told apart from.
    let reacts = is_ready(outcome.status_before_submit);
    let composer = async {
        match after_key {
            Some(after_key) => entry.wait_for_envelope_taken(after_key).await,
            None => std::future::pending().await,
        }
    };
    let reaction = async {
        if reacts {
            waiter::wait_for_reaction(rx, DELIVERY_REACTION_STATUSES, outcome.submitted_at, bound)
                .await
        } else {
            std::future::pending().await
        }
    };
    tokio::pin!(composer, reaction);
    let deadline = tokio::time::sleep(bound);
    tokio::pin!(deadline);
    let mut composer_open = after_key.is_some();
    let mut status_open = reacts;
    let mut lost_status = false;
    let mut other_writer = false;
    loop {
        if !composer_open && !status_open {
            break;
        }
        tokio::select! {
            biased;
            taken = &mut composer, if composer_open => match taken {
                EnvelopeWait::Taken => return SubmitEvidence::Taken,
                EnvelopeWait::Exited => return SubmitEvidence::Unanswered,
                EnvelopeWait::OtherWriter => {
                    composer_open = false;
                    other_writer = true;
                }
            },
            reacted = &mut reaction, if status_open => match reacted {
                ReactionOutcome::Reacted(_) => return SubmitEvidence::Taken,
                ReactionOutcome::Exited => return SubmitEvidence::Unanswered,
                ReactionOutcome::Quiet => break,
                // The composer can still show the envelope taken.
                ReactionOutcome::Unobservable => {
                    status_open = false;
                    lost_status = true;
                }
            },
            () = &mut deadline => break,
        }
    }
    match (lost_status, other_writer) {
        (false, _) => SubmitEvidence::Quiet,
        (true, false) => SubmitEvidence::Unobservable,
        (true, true) => SubmitEvidence::Unanswered,
    }
}

/// Names each handoff's parcel, so the one this handoff put in a window's
/// inbox can be withdrawn and no other.
static NEXT_HANDOFF_PARCEL: AtomicU64 = AtomicU64::new(1);

/// Puts a handoff's envelope in its window's inbox and answers once the
/// window's CLI took it, or once it is known it never will: nothing took it
/// within `handoff_delivery_timeout_secs`, or the requester left.
///
/// What runs out of time or is withdrawn is taken out of the inbox under the
/// same lock its doors take a parcel under, before the answer: an answer of
/// `undelivered_*` is never followed by the envelope reaching the CLI.
async fn deliver_handoff(job: DeliveryJob) {
    let DeliveryJob {
        outbound,
        registry,
        config,
        entry,
        envelope,
        requester,
        slot,
        mut closed,
        permit: _permit,
    } = job;

    if *closed.borrow() {
        drop(slot);
        return;
    }

    // Audit trail: who asked, which window received it, and how long the
    // envelope was. The envelope itself is never logged, matching the rule
    // that no raw PTY data is logged.
    tracing::info!(
        requester = ?requester,
        window_id = %entry.window_id(),
        envelope_len = envelope.len(),
        "accepted send_handoff"
    );

    let key = format!(
        "handoff:{}",
        NEXT_HANDOFF_PARCEL.fetch_add(1, Ordering::Relaxed)
    );
    // The daemon-wide budget is drawn only while the envelope is being typed,
    // so a window that never takes it holds none of it.
    let budget = slot.as_ref().and_then(|slot| match slot.shared_budget() {
        SharedBudget::Counter { counter, max } => Some(PushBudget { counter, max }),
        SharedBudget::NotNeeded => None,
    });
    let end = match notice_queue::deposit(
        &registry,
        &entry,
        Parcel::new(key.clone(), envelope),
        budget,
        Some(config.max_turn_waiters_per_window),
    ) {
        Ok(end) => end,
        Err(refused) => {
            drop(slot);
            outbound.reply(&match refused {
                notice_queue::Refused::Full => delivery_exhausted_msg(Scarce::TurnWaiters),
                notice_queue::Refused::Ended => child_exited_msg(),
            });
            return;
        }
    };

    let answer = await_handoff_end(&entry, &key, end, &mut closed, &config).await;
    tracing::info!(
        requester = ?requester,
        window_id = %entry.window_id(),
        "handoff delivery finished"
    );
    // Given back before the answer, so a caller holding the answer also has
    // its budget back.
    drop(slot);
    outbound.reply(&answer);
}

/// Waits for the end of the handoff parcel named `key` and returns the answer.
async fn await_handoff_end(
    entry: &WindowEntry,
    key: &str,
    mut end: oneshot::Receiver<ParcelEnd>,
    closed: &mut watch::Receiver<bool>,
    config: &DaemonConfig,
) -> DaemonMessage {
    let sent = |delivery| DaemonMessage::HandoffSent {
        window_id: entry.window_id().to_string(),
        opened: false,
        delivery,
        name: Some(entry.name()),
    };
    let deadline = tokio::time::sleep(Duration::from_secs(config.handoff_delivery_timeout_secs));
    tokio::pin!(deadline);
    let mut timed_out = false;
    // Watched once the bound has run out under a push still typing.
    let mut submitted: Option<watch::Receiver<bool>> = None;
    loop {
        tokio::select! {
            biased;
            ended = &mut end => {
                return match ended {
                    Ok(ParcelEnd::Taken) => sent(HandoffDelivery::Taken),
                    Ok(ParcelEnd::TurnedAway) => SlotExhausted::Daemon.to_message(),
                    Ok(ParcelEnd::Closed | ParcelEnd::Stopped) => child_exited_msg(),
                    // Dropped unsettled: it was withdrawn under way, and its
                    // push failed -- after the submit key it still reached
                    // the CLI.
                    Err(_) if submitted.as_ref().is_some_and(|pressed| *pressed.borrow()) => {
                        sent(HandoffDelivery::Taken)
                    }
                    Err(_) => sent(HandoffDelivery::UndeliveredTimeout),
                };
            }
            _ = closed.changed() => {
                // Withdrawn before the answer: a push under way gives up and
                // takes its text back if it still can.
                let _ = entry.inbox().withdraw(key);
                return handoff_withdrawn_msg(entry);
            }
            () = &mut deadline, if !timed_out => {
                timed_out = true;
                match entry.inbox().withdraw(key) {
                    Withdrawal::Removed => return sent(HandoffDelivery::UndeliveredTimeout),
                    // The submit key reached the CLI: the push cleans up alone,
                    // and a composer that never reads back must not hold the
                    // requester. Short of the key, the push gives up and takes
                    // the body back, which ends it.
                    Withdrawal::UnderWay { submitted: pressed } => {
                        if *pressed.borrow() {
                            return sent(HandoffDelivery::Taken);
                        }
                        submitted = Some(pressed);
                    }
                    // It already ended.
                    Withdrawal::Gone => {}
                }
            }
            pressed = async {
                match &mut submitted {
                    Some(pressed) => pressed.wait_for(|pressed| *pressed).await.is_ok(),
                    None => std::future::pending().await,
                }
            }, if submitted.is_some() => {
                if pressed {
                    return sent(HandoffDelivery::Taken);
                }
                // The push ended without pressing it: `end` says how.
                submitted = None;
            }
        }
    }
}

/// Answers a handoff that reserved a slot but will never be delivered: the
/// slot is released strictly before the answer goes out, so a caller holding
/// that answer also has its budget back.
fn abandon_handoff(outbound: &Outbound, slot: HandoffSlot, message: &DaemonMessage) {
    drop(slot);
    outbound.reply(message);
}

/// The refusal for a window that would be handed a request at launch whose
/// profile declares no way to.
fn no_launch_prompt_msg() -> DaemonMessage {
    error_msg("no_composer", NO_LAUNCH_PROMPT_MESSAGE)
}

/// The refusal for a launch that needs a capability its profile cannot offer
/// on this host, before anyone is asked to approve it. The limit's reason
/// comes last, so its guidance ends the message.
fn limit_refusal_msg(limit: &yagura_protocol::ProfileLimit) -> DaemonMessage {
    error_msg(
        "unsupported",
        &format!("nothing was started: {}", limit.reason),
    )
}

/// Judges a profile-addressed handoff before anything starts, then starts its
/// window with the envelope.
async fn approve_profile_handoff(job: ProfileHandoffOpen) {
    let outcome = {
        let context = GateContext {
            config: &job.config,
            broker: &job.broker,
            registry: &job.registry,
            origin: &job.origin,
            conn_id: job.conn_id,
            policy: &job.policy,
        };
        permit::gate_launched_handoff(&context, &job.approval_text)
    };
    match outcome {
        GateOutcome::Proceed(permit) => start_profile_handoff(job, permit).await,
        GateOutcome::Refused(message) => job.abandon(&message),
        GateOutcome::Pending(ticket) => {
            let RequestWatch {
                outbound,
                left: mut closed,
            } = RequestWatch::detached(
                &job.registry,
                &job.outbound,
                &job.origin,
                job.closed.clone(),
                open_withdrawn_msg(),
            );
            let config = job.config.clone();
            tokio::spawn(async move {
                let verdict = await_verdict(ticket, &config, &mut closed).await;
                let requester_left = *closed.borrow();
                drop(closed);
                let job = ProfileHandoffOpen { outbound, ..job };
                match verdict {
                    Some(Verdict::Approved(permit)) if !requester_left => {
                        start_profile_handoff(job, permit).await;
                    }
                    Some(Verdict::Refused(message)) if !requester_left => job.abandon(&message),
                    // Nobody is left to answer; dropping the job gives its slot back.
                    _ => drop(job),
                }
            });
        }
    }
}

/// Starts the window a profile-addressed handoff is for, with its envelope as
/// the launch prompt, and answers `taken` once the window started: the CLI
/// was handed the envelope by being started with it.
async fn start_profile_handoff(job: ProfileHandoffOpen, permit: Permit<permit::LaunchHandoff>) {
    let (open_permit, _handoff_permit) = permit.split();
    let ProfileHandoffOpen {
        mut planned,
        provenance,
        outbound,
        registry,
        config,
        closed,
        envelope,
        slot,
        ..
    } = job;
    if *closed.borrow() {
        drop(slot);
        return;
    }
    let Some(prompt) = planned
        .delivery
        .as_ref()
        .and_then(|delivery| launch_prompt_injection(delivery, &envelope))
    else {
        abandon_handoff(&outbound, slot, &no_launch_prompt_msg());
        return;
    };
    planned.injections.push(prompt);
    match open_planned(&registry, &config, planned, provenance, open_permit).await {
        Ok(opened) => {
            tracing::info!(
                window_id = %opened.window_id,
                envelope_len = envelope.len(),
                "handoff envelope handed over at launch"
            );
            abandon_handoff(
                &outbound,
                slot,
                &DaemonMessage::HandoffSent {
                    window_id: opened.window_id,
                    opened: true,
                    delivery: HandoffDelivery::Taken,
                    name: Some(opened.name),
                },
            );
        }
        Err(err) => abandon_handoff(&outbound, slot, &window_open_error_msg(&err)),
    }
}

/// What opening the window of one plan task needs once the open gate allows
/// it: everything [`IpcServer::handle_open_task_window`] decided while it held
/// the plan's lock, with that lock carried along in `change`.
///
/// Owned throughout, so that it can wait for an approver on a task of its own.
struct TaskWindowOpen {
    ledger: Arc<plan::PlanRuntime>,
    registry: Arc<WindowRegistry>,
    broker: Arc<ApprovalBroker>,
    config: DaemonConfig,
    outbound: Arc<Outbound>,
    planned: PlannedOpen,
    change: plan_permit::ChangePermit,
    plan_id: String,
    task_id: String,
    has_instruction: bool,
    resolved: crate::plan::ResolvedAssignment,
}

/// Opens the window for a plan task under the permit the open gate issued,
/// binds it to the task and answers the requester.
///
/// A binding write that fails closes the window just opened, the same
/// teardown an explicit `close_window` gives (`close_for_cleanup`), rather
/// than leaving an unreachable child behind.
async fn open_task_window(job: TaskWindowOpen, permit: Permit<OpenWindow>) {
    let TaskWindowOpen {
        ledger,
        registry,
        broker,
        config,
        outbound,
        mut planned,
        change,
        plan_id,
        task_id,
        has_instruction,
        resolved,
    } = job;
    // Handed over as the launch prompt only, so a window started again from
    // what is stored is never handed the task a second time.
    if has_instruction {
        let notice = compose_task_notice(&plan_id, &task_id);
        let Some(prompt) = planned
            .delivery
            .as_ref()
            .and_then(|delivery| launch_prompt_injection(delivery, &notice))
        else {
            outbound.reply(&no_launch_prompt_msg());
            return;
        };
        planned.injections.push(prompt);
    }
    // No opener: a task window's parent comes from its plan alone, and
    // it has none once it is no longer bound to the task.
    let provenance = WindowProvenance {
        task_origin: Some(TaskOrigin {
            plan_id: plan_id.clone(),
            task_id: task_id.clone(),
        }),
        ..WindowProvenance::default()
    };
    let opened = match open_planned(&registry, &config, planned, provenance, permit).await {
        Ok(opened) => opened,
        Err(err) => {
            outbound.reply(&window_open_error_msg(&err));
            return;
        }
    };

    let plan_id_for_bind = plan_id.clone();
    let task_id_for_bind = task_id.clone();
    let window_id_for_bind = opened.window_id.clone();
    let bind_result = ledger
        .with_store(move |store| {
            store.set_task_window(&change, &task_id_for_bind, &window_id_for_bind)
        })
        .await;

    match bind_result {
        Ok(Ok(())) => {
            // The window was started with its notice, so it is delivered.
            if has_instruction {
                plan::record_task_notice(
                    &ledger,
                    &broker,
                    &opened.window_id,
                    &plan_id_for_bind,
                    &task_id,
                    InstructionNoticeState::Delivered,
                )
                .await;
            }
            broker.notify_approvers(&DaemonMessage::PlanChanged {
                plan_id: plan_id_for_bind.clone(),
            });
            outbound.reply(&DaemonMessage::TaskWindowOpened {
                plan_id: plan_id_for_bind,
                task_id,
                window_id: opened.window_id,
                name: Some(opened.name),
                assignment: plan::wire_resolved(resolved),
            });
        }
        Ok(Err(store_err)) => {
            tracing::warn!(
                error = %store_err,
                "task binding write failed; closing the window it was opening for"
            );
            if let Err(close_err) =
                close_for_cleanup(&registry, &config, &opened.entry, false).await
            {
                tracing::warn!(
                    error = %close_err,
                    "cleanup close after a failed binding write also failed"
                );
            }
            outbound.reply(&plan::internal_msg());
        }
        Err(access_err) => {
            tracing::warn!(
                "the plan store became unavailable while binding a task window; closing the \
                 window it was opening for"
            );
            if let Err(close_err) =
                close_for_cleanup(&registry, &config, &opened.entry, false).await
            {
                tracing::warn!(
                    error = %close_err,
                    "cleanup close after a failed binding write also failed"
                );
            }
            outbound.reply(&plan::map_access_error(access_err));
        }
    }
}

/// Opens a window that runs the caller's own command. The permit is the
/// approval gate's licence for starting the process.
///
/// # Errors
///
/// Returns the registry's own [`WindowOpenError`]; callers translate it
/// to the answer they must send via [`window_open_error_msg`].
async fn open_command_window(
    registry: &WindowRegistry,
    request: OpenWindowRequest,
    provenance: WindowProvenance,
    _permit: Permit<OpenWindow>,
) -> Result<OpenedWindow, WindowOpenError> {
    registry
        .open_with_extra_env(request, Vec::new(), Vec::new(), provenance)
        .await
}

/// Opens a window from an already-decided plan, through the registry's
/// additional-env entry point so the profile's own `[env]` table and
/// `[mcp]` / `[hooks]` injections reach the child, recording `provenance` -- with the
/// plan's own launch identity -- on the new window. The permit is the
/// approval gate's licence for starting the process.
///
/// # Errors
///
/// Returns the registry's own [`WindowOpenError`]; callers translate it
/// to the answer they must send via [`window_open_error_msg`].
async fn open_planned(
    registry: &WindowRegistry,
    config: &DaemonConfig,
    planned: PlannedOpen,
    provenance: WindowProvenance,
    _permit: Permit<OpenWindow>,
) -> Result<OpenedWindow, WindowOpenError> {
    let mut planned = planned;
    let mut hooks_left_out = None;
    // Read at each launch so a registration changed outside the daemon
    // is seen by the next window, and rewritten when out of date.
    if let Some(gate) = planned.hooks_gate.take() {
        let reconciled = mcp_registration::read_launch_entries(
            config,
            &gate.writes,
            &gate.profile,
            &gate.registration,
            &gate.env,
        )
        .await;
        let left_out = match (reconciled.read, reconciled.rewrite) {
            (mcp_registration::EntriesRead::Hold, _) => {
                // Without hooks of its own, the profile's CLI runs the
                // registered entries as its hooks.
                if planned.hooks == HooksRoute::Unarranged {
                    planned.hooks = HooksRoute::RegisteredEntries;
                }
                None
            }
            (_, mcp_registration::Rewrite::Failed(reason)) => {
                Some(HooksLeftOut::NotRewritten(reason))
            }
            (mcp_registration::EntriesRead::Unreadable, _) => {
                Some(HooksLeftOut::NotRewritten("the settings could not be read"))
            }
            // The normal state of a CLI the user chose not to register.
            (mcp_registration::EntriesRead::Differ(_), _) => Some(HooksLeftOut::NotRegistered),
        };
        if let Some(left_out) = left_out {
            planned
                .profile_injections
                .retain(|injection| injection.kind() != crate::window::InjectionKind::Hooks);
            planned.hooks = HooksRoute::LeftOut;
            hooks_left_out = Some((gate.profile, left_out));
        }
    }
    if let Some(read) = planned.mcp_absent.take()
        && let Some(reason) = mcp_registration::mcp_injection_conflicts(config, read).await
    {
        planned
            .profile_injections
            .retain(|injection| injection.kind() != crate::window::InjectionKind::Mcp);
        tracing::warn!(
            profile = %planned.launch.profile,
            reason,
            "the MCP bridge was left out of this launch; the window opens without it"
        );
    }
    let credentials_dir = plan_window_credentials(config, &mut planned);
    // A table whose path cannot be converted is left out here; a `[hooks]`
    // table left out this way settles as left out at spawn.
    let mut injections = mcp_registration::render_profile_injections(
        config,
        std::mem::take(&mut planned.profile_injections),
        planned.host_paths.take(),
    )
    .await;
    // Whether or not the injections above survived.
    if !planned.launch_args.is_empty() {
        injections.push(CliInjection {
            kind: crate::window::InjectionKind::LaunchArgs,
            args: std::mem::take(&mut planned.launch_args),
            config_file: None,
            config_dir: None,
        });
    }
    let hooks_in_argv = injections
        .iter()
        .any(|injection| injection.kind == crate::window::InjectionKind::Hooks);
    let launch_context = launch_context_injection_for(config, &planned, hooks_in_argv);
    injections.append(&mut planned.injections);
    planned.injections = injections;
    if let Some(context) = launch_context {
        // Before the launch prompt, which has to stay the CLI's last argument.
        insert_before_launch_prompt(&mut planned.injections, context);
    }
    // A window whose directory cannot be resolved is refused by the open
    // below, so its listing is skipped rather than run somewhere else.
    if let Some(scan) = planned.bridge_scan.take()
        && let Some(window_cwd) = window_cwd_of(config, &planned.request).await
        && let Some(disable) =
            mcp_registration::disable_other_bridges(config, scan, window_cwd).await
    {
        insert_before_launch_prompt(&mut planned.injections, disable);
    }
    let provenance = WindowProvenance {
        launch: Some(planned.launch),
        delivery: planned.delivery,
        hooks: planned.hooks,
        wake_command: planned.wake_command,
        credentials: planned.credentials,
        credentials_dir,
        ..provenance
    };
    let opened = registry
        .open_with_extra_env(
            planned.request,
            planned.extra_env,
            planned.injections,
            provenance,
        )
        .await?;
    match hooks_left_out {
        Some((profile, HooksLeftOut::NotRewritten(reason))) => tracing::warn!(
            window_id = %opened.window_id,
            profile,
            reason,
            "hooks were left out of this launch; register the CLI again to restore them"
        ),
        Some((profile, HooksLeftOut::NotRegistered)) => tracing::info!(
            window_id = %opened.window_id,
            profile,
            reason = "this instance is not registered in the CLI's settings",
            "hooks were left out of this launch"
        ),
        None => {}
    }
    Ok(opened)
}

/// For a `launch` window: plans its credentials folder and points its
/// `[hooks]` and `[mcp]` settings at it, returning the folder. A window that
/// cannot be given a usable place for the credentials file (none configured,
/// or a location whose characters could break out of the settings) keeps
/// neither injection and gets no folder.
fn plan_window_credentials(config: &DaemonConfig, planned: &mut PlannedOpen) -> Option<PathBuf> {
    use crate::profile::CredentialsMode;
    use crate::window::{InjectionKind, credentials_file_argument, plan_credentials_folder};

    if planned.credentials != CredentialsMode::Launch {
        return None;
    }
    let dir = config
        .mcp_config_dir
        .as_deref()
        .and_then(|config_dir| plan_credentials_folder(config_dir).ok());
    let usable = dir.and_then(|dir| credentials_file_argument(&dir).map(|file| (dir, file)));
    let Some((dir, file)) = usable else {
        tracing::warn!(
            profile = %planned.launch.profile,
            "no usable place for the window's credentials file; the window opens without hooks \
             and the MCP bridge"
        );
        planned.profile_injections.retain(|injection| {
            !matches!(injection.kind(), InjectionKind::Hooks | InjectionKind::Mcp)
        });
        return None;
    };
    planned.profile_injections = std::mem::take(&mut planned.profile_injections)
        .into_iter()
        .map(|injection| injection.for_window_folder(&dir, &file))
        .collect();
    Some(dir)
}

/// The directory the window `request` opens will start in, by the same rule
/// the open applies; `None` when the open is going to refuse it.
async fn window_cwd_of(config: &DaemonConfig, request: &OpenWindowRequest) -> Option<PathBuf> {
    let requested = request.cwd.clone();
    let default_cwd = config.default_window_cwd.clone();
    // Canonicalizing probes the filesystem, so it runs off the async runtime.
    tokio::task::spawn_blocking(move || {
        crate::window::resolve_window_cwd(requested.as_ref(), default_cwd.as_deref()).ok()
    })
    .await
    .ok()
    .flatten()
}

/// Puts `injection` ahead of the launch prompt, which has to stay the CLI's
/// last argument.
fn insert_before_launch_prompt(injections: &mut Vec<CliInjection>, injection: CliInjection) {
    let at = injections
        .iter()
        .position(|injection| injection.kind == crate::window::InjectionKind::LaunchPrompt)
        .unwrap_or(injections.len());
    injections.insert(at, injection);
}

/// How `launch`'s window gets its credentials: in a file only when the profile
/// asks for it and every setting the window really uses ([`DaemonConfig::mcp_bridge`]
/// decides whether `[mcp]` is used) hands that file to its CLI. Otherwise the
/// window's hooks or bridge would never learn its credentials and would go
/// silent, so it opens with them in its environment.
fn credentials_mode_for(
    config: &DaemonConfig,
    launch: &crate::profile::ProfileLaunch,
) -> crate::profile::CredentialsMode {
    use crate::profile::CredentialsMode;

    if launch.credentials.mode != CredentialsMode::Launch {
        return CredentialsMode::Env;
    }
    let used: Vec<&crate::profile::InjectionSpec> = launch
        .hooks
        .iter()
        .chain(launch.mcp.iter().filter(|_| config.mcp_bridge.is_some()))
        .collect();
    if !used.is_empty() && used.iter().all(|spec| spec.spells_credentials_file()) {
        return CredentialsMode::Launch;
    }
    tracing::warn!(
        profile = %launch.vendor,
        "the profile asks for launch credentials but a setting it uses does not name the credentials file; the window opens with its credentials in the environment"
    );
    CredentialsMode::Env
}

/// The room's introduction as a launch argument, for a window that will take
/// part in chat. `hooks_in_argv` is whether the launch's `[hooks]` injection
/// survived rendering. A program that is a `.cmd` / `.bat` shim is opened
/// without it ([`crate::profile::shim_limits`]).
///
/// The detection name is the one the window will be asked to load; a
/// definition that then fails to load leaves the window a participant
/// whatever the exclusion list says, so only the name is judged here.
fn launch_context_injection_for(
    config: &DaemonConfig,
    planned: &PlannedOpen,
    hooks_in_argv: bool,
) -> Option<CliInjection> {
    let delivery = planned.delivery.as_ref()?;
    delivery.launch_context.as_ref()?;
    let detection = planned
        .request
        .detection
        .as_deref()
        .unwrap_or(config.detection_name.as_str());
    let intro =
        planned
            .chat
            .launch_intro(true, planned.hooks.settle(hooks_in_argv), Some(detection));
    if intro.is_empty() {
        return None;
    }
    if planned
        .limit(yagura_protocol::ProfileCapability::LaunchContext)
        .is_some()
    {
        tracing::warn!(
            profile = %planned.launch.profile,
            "the room's introduction was left out of this launch: the program is a command interpreter shim"
        );
        return None;
    }
    launch_context_injection(delivery, intro)
}

/// The answer to a request that opened a window.
fn opened_message(opened: OpenedWindow) -> DaemonMessage {
    DaemonMessage::WindowOpened {
        window_id: opened.window_id,
        detection_warning: opened.detection_warning,
        name: Some(opened.name),
    }
}

/// Answers a caller whose destructive request the gate refused.
///
/// [`GateOutcome::Refused`] carries the answer rather than sending it, so
/// every caller that reaches it ends the same way; that ending lives here
/// instead of being written out again at each gate.
fn reply_refused(outbound: &Outbound, message: &DaemonMessage) {
    outbound.reply(message);
}

/// Waits for the decision on `ticket` and settles its ledger entry, so no
/// approver is left holding a dialog nobody can answer.
///
/// Meant for a task of its own: a decision that waits for a human must not
/// consume the connection's `wait_for_status` budget, and it must not be
/// aborted when the connection ends either, since the ledger entry it owns has
/// to be withdrawn. The connection's end reaches it through `closed` instead.
async fn await_approval(
    ticket: &mut Ticket,
    deadline: Duration,
    closed: &mut watch::Receiver<bool>,
) -> ApprovalEnd {
    tokio::select! {
        decided = &mut ticket.rx => match decided {
            Ok(true) => ApprovalEnd::Approved,
            Ok(false) => ApprovalEnd::Refused(error_msg(
                "approval_denied",
                "an approver denied this operation",
            )),
            Err(_) => timed_out(ticket),
        },
        () = tokio::time::sleep(deadline) => timed_out(ticket),
        // An `Err` here means the connection's sender is gone, which
        // is the same news as the value it would have carried.
        _ = closed.changed() => {
            ticket.withdraw(WithdrawReason::RequesterDisconnected);
            ApprovalEnd::RequesterGone
        }
    }
}

fn timed_out(ticket: &mut Ticket) -> ApprovalEnd {
    ticket.withdraw(WithdrawReason::Timeout);
    ApprovalEnd::Refused(error_msg(
        "approval_timeout",
        "no decision was made before the approval deadline elapsed",
    ))
}

fn spawn_pending(
    request: RequestWatch,
    registry: Arc<WindowRegistry>,
    config: DaemonConfig,
    entry: Arc<WindowEntry>,
    action: PendingAction,
    mut ticket: Ticket,
) {
    let RequestWatch {
        outbound,
        left: mut closed,
    } = request;
    let deadline = Duration::from_secs(config.approval_timeout_secs);
    let tool = action.tool();

    tokio::spawn(async move {
        match await_approval(&mut ticket, deadline, &mut closed).await {
            ApprovalEnd::Approved => {}
            ApprovalEnd::Refused(message) => {
                drop(action);
                outbound.reply(&message);
                return;
            }
            ApprovalEnd::RequesterGone => {
                // A handoff's watch answers nothing itself; one whose
                // requester only ended its turn still has a connection to
                // answer on.
                if let PendingAction::SendHandoff { .. } = action {
                    drop(action);
                    outbound.reply(&handoff_withdrawn_msg(&entry));
                }
                return;
            }
        }

        // The window may have become undetectable while the request sat
        // waiting, and an approval decided against the old status is no
        // licence to fire at the new one.
        let permitted = match gate_approved(action, entry.current_detected()) {
            ApprovedOutcome::Permitted(permitted) => permitted,
            ApprovedOutcome::Denied(action, reason) => {
                drop(action);
                outbound.reply(&deny_msg(tool, reason));
                return;
            }
        };

        match permitted {
            PermittedAction::SendText(text, permit) => {
                execute_send_text(&outbound, &entry, &text, permit, Some(closed)).await;
            }
            PermittedAction::SendKeys(keys, permit) => {
                execute_send_keys(&outbound, &entry, &keys, permit).await;
            }
            PermittedAction::Interrupt(permit) => {
                execute_interrupt(&outbound, &entry, &config, permit).await;
            }
            PermittedAction::Close(force, permit) => {
                execute_close(&outbound, &registry, &config, &entry, force, permit).await;
            }
            PermittedAction::SendHandoff {
                text,
                requester,
                slot,
                permit,
            } => {
                deliver_handoff(DeliveryJob {
                    outbound,
                    registry,
                    config,
                    entry,
                    envelope: text,
                    requester,
                    slot,
                    closed,
                    permit,
                })
                .await;
            }
        }
    });
}

/// The caps [`ApprovalBroker::submit`] enforces, read from the daemon's own
/// configuration.
fn approval_limits(config: &DaemonConfig) -> ApprovalLimits {
    ApprovalLimits {
        max_pending: config.max_pending_approvals,
        max_pending_per_conn: config.max_pending_approvals_per_conn,
    }
}

/// Describes `origin` to an approver.
///
/// A window is named with the display name the registry holds now, which is
/// the only place that name lives. One whose entry can no longer be found is
/// reported as an unidentified caller rather than as a window with a missing
/// name.
fn requester_for(registry: &WindowRegistry, origin: &RequestOrigin) -> ApprovalRequester {
    if let RequestOrigin::Window(window_id) = origin
        && let Some(window) = window_info(registry, window_id)
    {
        return ApprovalRequester {
            kind: ApprovalRequesterKind::Window,
            window_id: Some(window_id.clone()),
            name: Some(window.name),
        };
    }
    ApprovalRequester {
        kind: ApprovalRequesterKind::External,
        window_id: None,
        name: None,
    }
}

/// The registry's current record for `window_id`, if it still holds one.
///
/// Reads the one entry asked about rather than every window the registry
/// holds; the record is `list`'s record minus the listing-only `room`.
fn window_info(registry: &WindowRegistry, window_id: &str) -> Option<WindowInfo> {
    registry.get(window_id).map(|entry| entry.window_info())
}

/// The opener a window opened on behalf of `origin` records: only an
/// identified window can be one, so an approver's or an unidentified
/// caller's window has no parent to report to.
fn opener_of(origin: &RequestOrigin) -> Option<String> {
    match origin {
        RequestOrigin::Window(window_id) => Some(window_id.clone()),
        RequestOrigin::External | RequestOrigin::Approver => None,
    }
}

/// The window a connection speaks for, as the registry knows it now.
///
/// `None` for a caller that never identified, for an approver, and for one
/// whose window is gone: all three are callers a handoff cannot be replied
/// to.
fn requester_window(registry: &WindowRegistry, origin: &RequestOrigin) -> Option<WindowInfo> {
    match origin {
        RequestOrigin::Window(window_id) => window_info(registry, window_id),
        RequestOrigin::External | RequestOrigin::Approver => None,
    }
}

/// What a detached `send_message` or handoff answers through, and the signal
/// it hands the window as "the requester left".
struct RequestWatch {
    outbound: Arc<Outbound>,
    left: watch::Receiver<bool>,
}

impl RequestWatch {
    /// Watches the requesting connection and nothing else.
    fn connection_only(outbound: &Arc<Outbound>, state: &ConnectionState) -> Self {
        Self {
            outbound: Arc::clone(outbound),
            left: state.closed.subscribe(),
        }
    }

    /// Watches the requesting connection, and also, when the requester is a
    /// window whose CLI drops its calls at the end of a turn without
    /// cancelling them, that window ending the turn the request was made in.
    ///
    /// In the latter case the connection is still open, so `withdrawn` is
    /// answered on it at once, and whatever the window later returns for the
    /// same request is not answered again.
    fn new(
        registry: &WindowRegistry,
        outbound: &Arc<Outbound>,
        state: &ConnectionState,
        withdrawn: DaemonMessage,
    ) -> Self {
        Self::watching(
            registry,
            outbound,
            &state.origin,
            state.closed.subscribe(),
            Some(withdrawn),
        )
    }

    /// [`RequestWatch::new`] that answers nothing itself when the turn ends:
    /// the request answers once it has let go of what it holds.
    fn quiet(registry: &WindowRegistry, outbound: &Arc<Outbound>, state: &ConnectionState) -> Self {
        Self::watching(
            registry,
            outbound,
            &state.origin,
            state.closed.subscribe(),
            None,
        )
    }

    /// [`RequestWatch::new`] for a task that no longer holds the
    /// connection's state: `origin` and `closed` are what it took from it.
    fn detached(
        registry: &WindowRegistry,
        outbound: &Arc<Outbound>,
        origin: &RequestOrigin,
        closed: watch::Receiver<bool>,
        withdrawn: DaemonMessage,
    ) -> Self {
        Self::watching(registry, outbound, origin, closed, Some(withdrawn))
    }

    /// The watch every constructor above builds; `withdrawn` is answered at
    /// the end of the turn when given.
    fn watching(
        registry: &WindowRegistry,
        outbound: &Arc<Outbound>,
        origin: &RequestOrigin,
        closed: watch::Receiver<bool>,
        withdrawn: Option<DaemonMessage>,
    ) -> Self {
        let requester = match origin {
            RequestOrigin::Window(window_id) => registry
                .get(window_id)
                .filter(|entry| entry.abandons_calls_at_turn_end()),
            RequestOrigin::External | RequestOrigin::Approver => None,
        };
        let Some(requester) = requester else {
            return Self {
                outbound: Arc::clone(outbound),
                left: closed,
            };
        };
        // Subscribed before the current status is read, so no change between
        // the two is missed.
        let changes = requester.subscribe_status();
        let turn = TurnWatch {
            ran: requester.current_status() == WindowStatus::Running,
        };
        let outbound = outbound.for_one_answer();
        let (left_tx, left) = watch::channel(false);
        tokio::spawn(withdraw_at_turn_end(
            closed,
            changes,
            turn,
            left_tx,
            Arc::clone(&outbound),
            withdrawn,
        ));
        Self { outbound, left }
    }
}

/// The requester's turn, as its confirmed statuses tell it.
struct TurnWatch {
    /// Whether the requester has been `Running` since the request was made,
    /// the moment it was made included: a CLI is running when it calls a tool.
    ran: bool,
}

impl TurnWatch {
    /// Whether `now` ends the turn. `Unknown` and `WaitingApproval` say
    /// nothing about it: an undetectable window never ends a request, and an
    /// approval dialog is part of a turn.
    fn ends_at(&mut self, now: WindowStatus) -> bool {
        match now {
            WindowStatus::Running => {
                self.ran = true;
                false
            }
            WindowStatus::WaitingInput
            | WindowStatus::Interrupted
            | WindowStatus::Idle
            | WindowStatus::Exited => self.ran,
            WindowStatus::WaitingApproval | WindowStatus::Unknown => false,
        }
    }
}

/// Raises `left` once the requesting connection closes, or once `turn` ends
/// (answering `withdrawn` first when given, since that connection is still
/// open).
///
/// Holds `left` for as long as anyone watches it: a dropped sender reads as a
/// requester that left. Ends without raising it once nobody does, which is
/// when the request it watches for has ended.
async fn withdraw_at_turn_end(
    mut closed: watch::Receiver<bool>,
    mut changes: broadcast::Receiver<StatusAt>,
    mut turn: TurnWatch,
    left: watch::Sender<bool>,
    outbound: Arc<Outbound>,
    withdrawn: Option<DaemonMessage>,
) {
    let mut window_open = true;
    loop {
        tokio::select! {
            biased;
            () = left.closed() => return,
            changed = closed.changed() => {
                if changed.is_err() || *closed.borrow_and_update() {
                    let _ = left.send(true);
                    return;
                }
            }
            change = changes.recv(), if window_open => {
                let now = match change {
                    Ok(change) => change.status.status(),
                    // Statuses were missed: whether the turn ended among them
                    // cannot be told, so it is left to the ones that follow.
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    // The window is gone.
                    Err(broadcast::error::RecvError::Closed) => {
                        window_open = false;
                        WindowStatus::Exited
                    }
                };
                if turn.ends_at(now) {
                    if let Some(withdrawn) = &withdrawn {
                        outbound.reply(withdrawn);
                    }
                    let _ = left.send(true);
                    return;
                }
            }
        }
    }
}

/// Resolves once a [`RequestWatch`]'s requester has left the request; never
/// when the watch can no longer say so.
async fn request_left(left: &mut watch::Receiver<bool>) {
    if left.wait_for(|left| *left).await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// The answer to a `send_message` whose requester ended its turn: its text
/// is still being taken back, so whether any of it remains cannot be told.
fn send_text_withdrawn_msg() -> DaemonMessage {
    error_msg(
        "not_written",
        not_placed_message(&NotPlaced {
            envelope_remains: true,
            reason: NotPlacedReason::NotReflected,
        }),
    )
}

/// The answer to a request to open a window whose requester ended its turn
/// before an approver decided: nothing was opened.
fn open_withdrawn_msg() -> DaemonMessage {
    error_msg(
        "approval_timeout",
        "the requester ended its turn before an approver decided, so no window was opened",
    )
}

/// The answer to a handoff whose requester left: its envelope was taken out
/// of the window's inbox, and nothing hands it over after this answer.
fn handoff_withdrawn_msg(entry: &WindowEntry) -> DaemonMessage {
    DaemonMessage::HandoffSent {
        window_id: entry.window_id().to_string(),
        opened: false,
        delivery: HandoffDelivery::UndeliveredTimeout,
        name: Some(entry.name()),
    }
}

/// Turns the requester's registry record into the envelope's `from` value.
fn envelope_from(requester: Option<&WindowInfo>) -> EnvelopeFrom {
    match requester {
        Some(window) => EnvelopeFrom::Window {
            id: window.window_id.clone(),
            name: window.name.clone(),
            cwd: window.cwd.clone(),
        },
        None => EnvelopeFrom::External,
    }
}

/// Refuses an operation nobody is connected to approve.
///
/// Shared by the gate, which learns it from a failed submit, and by the
/// pre-judgement that asks before opening a window, so both refusals read the
/// same to a caller.
fn deny_no_approver(tool: DestructiveTool) -> DaemonMessage {
    deny_msg(tool, DenyReason::ApprovalRequired)
}

/// The refusal for a window name that is taken or reserved.
fn name_taken_msg(refusal: &NameRefusal) -> DaemonMessage {
    DaemonMessage::Error {
        code: "name_taken".to_string(),
        message: refusal.to_string(),
        rejected_argument: Some(RejectedArgument::Name.wire_name().to_string()),
    }
}

/// Renders a refused window open, one answer per [`WindowOpenError`] variant.
fn window_open_error_msg(err: &WindowOpenError) -> DaemonMessage {
    match err {
        WindowOpenError::InvalidArgument { reason, argument } => DaemonMessage::Error {
            code: "invalid_argument".to_string(),
            message: reason.clone(),
            rejected_argument: argument.map(|argument| argument.wire_name().to_string()),
        },
        WindowOpenError::NameTaken(refusal) => name_taken_msg(refusal),
        WindowOpenError::SpawnFailed(reason) => error_msg("spawn_failed", reason),
        WindowOpenError::ResourceExhausted => error_msg(
            "resource_exhausted",
            "the daemon already holds the maximum number of windows",
        ),
    }
}

/// Renders a refused destructive operation.
///
/// An undetectable status is reported as such to every caller alike:
/// the window's detected state is already readable through
/// `get_window_status`, so masking it here only cost the caller a diagnosis.
fn deny_msg(tool: DestructiveTool, reason: DenyReason) -> DaemonMessage {
    match reason {
        DenyReason::StatusUnknown(reason) => error_msg(
            "status_unknown",
            &format!(
                "the window's status could not be detected ({}), so this operation is refused",
                reason.phrase()
            ),
        ),
        DenyReason::ApprovalRequired => {
            error_msg("approval_required", &approval_required_message(tool, false))
        }
        DenyReason::PlaybookBroken => {
            error_msg("approval_required", &approval_required_message(tool, true))
        }
        DenyReason::NoComposer => no_composer_msg(),
    }
}

/// Explains *why* the operation needed an approval it did not get, so that an
/// operator can tell a broken policy file apart from the ordinary case where
/// the tool simply needs a human and none is available.
fn approval_required_message(tool: DestructiveTool, playbook_broken: bool) -> String {
    if playbook_broken {
        return format!(
            "the configured playbook failed to load, so {} is denied",
            tool.as_tool_name()
        );
    }
    format!(
        "no approver is connected, so {} is denied",
        tool.as_tool_name()
    )
}

/// The one place a completed wait is answered.
///
/// Every outcome -- including the two that surface as errors -- is enqueued
/// through the fail-closed `reply()` path: a `wait_result` that silently
/// vanished under back pressure would leave the caller waiting forever, so a
/// queue that cannot take it marks the connection broken instead.
///
/// Every answer is built from what `outcome` carries and nothing else: the
/// window is never read back here, so a reported status is always one the
/// wait actually observed. `target_status` is what the caller asked to wait
/// for, which is the status a `Reached` settled on by definition.
fn send_wait_result(
    outbound: &Arc<Outbound>,
    window_id: &str,
    outcome: WaitOutcome,
    target_status: WindowStatus,
) {
    let message = match outcome {
        WaitOutcome::Reached => DaemonMessage::WaitResult {
            window_id: window_id.to_string(),
            outcome: "reached".to_string(),
            status: target_status,
        },
        WaitOutcome::Timeout { current } => DaemonMessage::WaitResult {
            window_id: window_id.to_string(),
            outcome: "timeout".to_string(),
            status: current.status(),
        },
        // The target can never be reached now.
        WaitOutcome::ChildExited { .. } => child_exited_msg(),
        // A dropped status sender means the window is gone from the registry.
        WaitOutcome::Detached => window_not_found_msg(),
    };
    outbound.reply(&message);
}

impl std::fmt::Debug for IpcServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("IpcServer")
            .field("endpoint", &self.endpoint)
            .field("registry", &self.registry)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Well-formed, so the request reaches the window lookup.
    const TOKEN: &str = "0123456789abcdef0123456789abcdef";

    #[tokio::test]
    async fn identify_waits_for_an_open_in_progress_before_answering() {
        let registry = Arc::new(WindowRegistry::new(DaemonConfig::default()));
        let server = IpcServer::new(Arc::clone(&registry), "unused", DaemonConfig::default());
        let (tx, mut rx) = mpsc::channel(8);
        let outbound = Outbound::new(tx);
        let mut state = ConnectionState::new(0, None);
        let opening = registry.begin_opening("w-pending");

        let identify = server.handle_identify(&outbound, &mut state, "w-pending", TOKEN);
        tokio::pin!(identify);
        assert!(
            tokio::time::timeout(Duration::from_millis(50), &mut identify)
                .await
                .is_err(),
            "identify must not answer while the window's open is in progress"
        );
        assert!(rx.try_recv().is_err());

        drop(opening);
        let outcome = identify.await;
        assert!(matches!(outcome, FrameOutcome::Continue));
        assert_eq!(
            rx.try_recv().ok(),
            Some(encode_frame(&window_not_found_msg()))
        );
    }

    #[cfg(windows)]
    fn idle_command() -> Vec<String> {
        vec!["cmd.exe".into(), "/c".into(), "exit".into()]
    }

    #[cfg(not(windows))]
    fn idle_command() -> Vec<String> {
        vec!["sh".into(), "-c".into(), "true".into()]
    }

    /// A handoff withdrawn at its bound while its push was still short of the
    /// submit key, whose push then pressed the key and failed a later write,
    /// reached the CLI: it is answered taken, never undelivered.
    #[tokio::test]
    async fn a_withdrawn_handoff_whose_push_pressed_the_key_before_failing_is_answered_taken() {
        let config = DaemonConfig {
            handoff_delivery_timeout_secs: 0,
            ..DaemonConfig::default()
        };
        let registry = Arc::new(WindowRegistry::new(config.clone()));
        let opened = registry
            .open_with_extra_env(
                OpenWindowRequest {
                    command: idle_command(),
                    cwd: None,
                    name: None,
                    vendor: None,
                    detection: None,
                    on_name_taken: Default::default(),
                },
                Vec::new(),
                Vec::new(),
                WindowProvenance::default(),
            )
            .await
            .expect("open a registered window");
        let entry = registry.get(&opened.window_id).expect("registered");
        let key = "handoff:test".to_string();
        let end = entry
            .inbox()
            .deposit_tracked(Parcel::new(key.clone(), "envelope"), None, None)
            .end
            .expect("taken in");
        let mut push = entry.inbox().begin_push(false).expect("the parcel waits");
        let (_closed_tx, mut closed) = watch::channel(false);

        let answering = tokio::spawn({
            let entry = Arc::clone(&entry);
            async move { await_handoff_end(&entry, &key, end, &mut closed, &config).await }
        });
        push.withdrawn
            .wait_for(|withdrawn| *withdrawn)
            .await
            .expect("the inbox holds the push");
        push.submitted.send_replace(true);
        entry
            .inbox()
            .finish_push(push.id, crate::inbox::PushEnd::Failed);

        let answer = answering.await.expect("the answer");
        assert!(
            matches!(
                answer,
                DaemonMessage::HandoffSent {
                    delivery: HandoffDelivery::Taken,
                    ..
                }
            ),
            "{answer:?}"
        );
    }
}
