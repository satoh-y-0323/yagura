//! Window registry: the daemon's multi-window state.
//!
//! [`WindowRegistry`] owns `window_id -> WindowEntry` and is the SSOT for
//! opening, listing, looking up and tearing down windows. [`WindowEntry`]
//! bundles one window's resources: the [`PtySession`], the detection engine
//! front end, status fan-out and the attach slot.
//!
//! This module is also the SSOT for the background tasks that feed the
//! detection engine (`spawn_output_pump` / `spawn_exit_watcher`) and for
//! resolving which detection definition a window uses
//! (`load_definition_for`).

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use thiserror::Error;
use tokio::sync::{
    Mutex as AsyncMutex, MutexGuard as AsyncMutexGuard, Notify, broadcast, mpsc, oneshot, watch,
};
use yagura_os::program;
use yagura_protocol::{
    DetectedStatus, INSTANCE_ENV, MCP_BRIDGE_ENV, RoomRef, STATUS_TOKEN_ENV, UnknownReason,
    WINDOW_ID_ENV, WindowInfo, WindowNameError, WindowStatus, validate_window_name,
    validate_window_vendor,
};

use crate::config::DaemonConfig;
use crate::control::ComposerAccess;
use crate::detector::definition::{
    ConfirmMsSource, Definition, DefinitionError, INPUT_SUBMIT_DELAY_DEFAULT_MS,
    check_submit_delay_outlasts, is_valid_detection_name,
};
use crate::detector::engine::{
    ComposerObservation, ConfirmedStatus, DetectionEngine, DetectorHandle, InputEcho,
};
use crate::detector::layers::{DefinitionSource, DetectionLayers, LayerError, ResolvedDefinition};
use crate::detector::screen::ComposerReadout;
use crate::handoff::{ReadyHoldLatch, StartupEvidence};
use crate::input::{
    InputProfile, SubmitAction, SubmitOutcome, SubmitPlan, SubmitStep, TakeBack, plan_submit_line,
};
use crate::pty::{
    GuardedWrite, PtyResult, PtySession, TakeBackCall, TakeBackPrefix, TakeBackRegistration,
    lock_recover,
};
use crate::token::StatusToken;
use crate::waiter::StatusAt;
use crate::workspace::{self, WorkspaceKey, resolve_key_blocking};

/// Capacity of the fan-out channel carrying confirmed status changes to
/// subscribers. Status changes are debounced and therefore rare, so this is
/// generous enough that a lagging subscriber means a stalled connection
/// rather than a burst.
pub(crate) const STATUS_BROADCAST_CAPACITY: usize = 64;

/// The current holder of a window's "attached" slot. A later
/// [`WindowEntry::register_attach`] supersedes the previous holder, which is
/// notified via the stored [`oneshot::Sender`].
type AttachSlot = Option<(u64, oneshot::Sender<()>)>;

/// Number of base36 digits in a registry's boot nonce (see [`boot_nonce`]).
const BOOT_NONCE_LEN: usize = 6;

/// Process-wide sequence mixed into every boot nonce so that two registries
/// created inside the same clock resolution (tests, and any future
/// multi-registry daemon) still get distinct nonces.
static REGISTRY_SEQUENCE: AtomicU64 = AtomicU64::new(0);

/// Builds one registry's boot nonce: a short base36 string derived from the
/// wall clock, the process ID and [`REGISTRY_SEQUENCE`].
///
/// This is not a cryptographic value and carries no secret. Its only job is
/// that identifiers minted by different daemon instances do not
/// collide, so a client still holding an identifier from a previous daemon
/// gets `window_not_found` instead of silently matching a different window.
fn boot_nonce() -> String {
    // Fail-safe: a clock that cannot be read (or predates the Unix epoch)
    // must not abort daemon startup, and the process ID plus the sequence
    // still keep nonces distinct within this process, so degrade to 0.
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .ok()
        .and_then(|elapsed| u64::try_from(elapsed.as_nanos()).ok())
        .unwrap_or(0);
    let pid = u64::from(std::process::id());
    let sequence = REGISTRY_SEQUENCE.fetch_add(1, Ordering::Relaxed);

    // Odd multipliers spread each component across the low bits that
    // `to_base36` keeps, so two registries differing only in `sequence` still
    // differ in the emitted digits.
    let mixed = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(pid.wrapping_mul(0xD6E8_FEB8_6659_FD93))
        .wrapping_add(sequence.wrapping_mul(0xA076_1D64_78BD_642F));

    to_base36(mixed, BOOT_NONCE_LEN)
}

/// Renders the low digits of `value` in base36 (`0-9a-z`), least significant
/// digit first, always exactly `digits` characters.
fn to_base36(mut value: u64, digits: usize) -> String {
    const ALPHABET: &[u8; 36] = b"0123456789abcdefghijklmnopqrstuvwxyz";
    let mut out = String::with_capacity(digits);
    for _ in 0..digits {
        let index = usize::try_from(value % 36).unwrap_or(0);
        out.push(char::from(ALPHABET.get(index).copied().unwrap_or(b'0')));
        value /= 36;
    }
    out
}

/// The result of request validation in [`validate_open_request`].
struct ValidatedRequest {
    /// The validated display name.
    name: String,
    /// The validated vendor label.
    vendor: Option<String>,
    /// The canonicalized working directory path.
    resolved_cwd: PathBuf,
    /// The string representation of the resolved cwd (pre-computed for storage).
    cwd_string: String,
}

/// Where the working directory being checked came from.
///
/// Carried so that a rejection can say whether the caller itself named the
/// directory: a value the caller never sent is not an argument of theirs to
/// be told about.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CwdOrigin {
    /// The caller sent it with the request.
    Caller,
    /// A launch profile declared it and the caller sent none.
    ProfileDeclared,
    /// Nobody named one, so the daemon's start-up default
    /// ([`DaemonConfig::default_window_cwd`]) stands in.
    DaemonDefault,
    /// Nobody named one and the daemon has no start-up default, so its own
    /// current directory stands in.
    DaemonCurrentDir,
}

/// A working directory a request carries, together with where it came from.
///
/// The two are one value rather than two fields so a path can never be
/// paired with the wrong origin, and so the daemon's own origins
/// ([`CwdOrigin::DaemonDefault`], [`CwdOrigin::DaemonCurrentDir`]), which no
/// request carries, are unrepresentable here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RequestedCwd {
    path: PathBuf,
    origin: CwdOrigin,
}

impl RequestedCwd {
    /// A working directory the caller sent with the request.
    pub fn from_caller(path: PathBuf) -> Self {
        Self {
            path,
            origin: CwdOrigin::Caller,
        }
    }

    /// A working directory a launch profile declared.
    pub fn from_profile(path: PathBuf) -> Self {
        Self {
            path,
            origin: CwdOrigin::ProfileDeclared,
        }
    }

    /// The directory itself.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Where this directory came from.
    pub fn origin(&self) -> CwdOrigin {
        self.origin
    }
}

/// A request to open a new window.
///
/// Every optional field has a documented default applied by
/// [`WindowRegistry::open`]: `name` defaults to `command[0]`'s file stem,
/// `cwd` defaults to [`DaemonConfig::default_window_cwd`] when set and to the
/// daemon's own resolved current directory otherwise, and
/// `detection` defaults to [`DaemonConfig::detection_name`].
#[derive(Debug, Clone)]
pub struct OpenWindowRequest {
    /// Argument vector to spawn (`command[0]` is the executable). Must be
    /// non-empty.
    pub command: Vec<String>,
    /// Working directory, with where it came from. Must exist and be a
    /// directory if provided; otherwise defaults to the daemon's current
    /// directory.
    pub cwd: Option<RequestedCwd>,
    /// Display name. Defaults to `command[0]`'s file stem.
    pub name: Option<String>,
    /// Vendor identifier: a data value, never a type variant. Free-form
    /// except that it is displayed, so it must satisfy
    /// [`validate_window_vendor`].
    pub vendor: Option<String>,
    /// Detection definition name (`[a-z0-9_-]{1,64}`). `None` selects the
    /// daemon's configured default.
    pub detection: Option<String>,
}

/// The plan task a window was opened for by `open_task_window`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskOrigin {
    /// The plan the task belongs to.
    pub plan_id: String,
    /// The task's id within its plan.
    pub task_id: String,
}

/// Who opened a window, for a task window which task it was opened for, and
/// for a profile launch which profile and option values it was launched
/// with.
///
/// Kept out of [`OpenWindowRequest`] on purpose: the opener and the task
/// come from the requesting connection's proven identity and the daemon's
/// own decision, and the launch identity is what the daemon itself resolved
/// a profile request from -- none of it is a field a raw `OpenWindow`
/// caller can write.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WindowProvenance {
    /// The window whose identified connection asked for this one. `None`
    /// when an approver, an unidentified caller or the daemon itself did, and
    /// for a task window, whose parent is its plan's owner instead.
    pub opener: Option<String>,
    /// The plan task this window was opened for, if any.
    pub task_origin: Option<TaskOrigin>,
    /// The profile launch this window came from. `None` for a window opened
    /// from a raw argv.
    pub launch: Option<LaunchIdentity>,
}

/// The profile and launch option values a profile-addressed window was
/// opened with: enough to launch it again through the same profile, which
/// is why the option values are kept as the caller supplied them rather
/// than as the argv they resolved to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchIdentity {
    /// The profile identifier.
    pub profile: String,
    /// The `model` value supplied with the launch, if any.
    pub model: Option<String>,
    /// The `effort` value supplied with the launch, if any.
    pub effort: Option<String>,
}

/// Which profile table an injection comes from. Only what tells the two
/// apart in a log line and in a file name lives here; the rules each table
/// follows are shared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InjectionKind {
    /// The `[mcp]` table: points the CLI at the daemon's MCP bridge.
    Mcp,
    /// The `[hooks]` table: makes the CLI report its turn boundaries.
    Hooks,
}

impl InjectionKind {
    /// The name log lines use for this kind.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Mcp => "MCP",
            Self::Hooks => "hooks",
        }
    }
}

/// Argv elements a profile's `[mcp]` or `[hooks]` table contributes to one
/// launch, already rendered, plus the settings file they may name.
///
/// Travels beside the request rather than inside it: the elements are
/// appended to the spawn argv only, so neither the window's recorded argv nor
/// a session snapshot ever records them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CliInjection {
    /// The table these elements come from.
    pub kind: InjectionKind,
    /// Elements appended after the launch's own argv.
    pub args: Vec<String>,
    /// The settings file `args` refer to, written before spawn when absent.
    pub config_file: Option<InjectedConfigFile>,
}

/// A settings file a [`CliInjection`] needs on disk before spawn. The name
/// is derived from the content, so a file already present under that name
/// already holds exactly this content and is never rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InjectedConfigFile {
    /// The directory the file lives in.
    pub dir: PathBuf,
    /// The file name inside `dir`.
    pub file_name: String,
    /// The file's content.
    pub content: String,
}

impl InjectedConfigFile {
    /// The file's full path.
    #[must_use]
    pub fn path(&self) -> PathBuf {
        self.dir.join(&self.file_name)
    }
}

/// The successful outcome of [`WindowRegistry::open`].
#[derive(Debug)]
pub struct OpenedWindow {
    /// The assigned window id.
    pub window_id: String,
    /// English reason text when this window has no detection definition to
    /// run: one failed to load, or no definition layer is configured at all.
    /// Either way the window is open and stays `Unknown`. See
    /// [`load_definition_for`].
    pub detection_warning: Option<String>,
    /// The registered entry of the window that was just opened. A caller
    /// that has to act on the window it opened holds it from here instead of
    /// looking it back up: a lookup can miss, and a miss would leave a
    /// spawned child that nothing can reach.
    pub entry: Arc<WindowEntry>,
}

/// Snapshot of one window's participation-relevant facts, returned by
/// [`WindowRegistry::list_participation`]. Deliberately silent on what a
/// "chat participant" is: that judgement is the chat room's own, built from these plain facts.
#[derive(Debug, Clone)]
pub struct WindowParticipation {
    /// The window's id.
    pub window_id: String,
    /// The window's current display name.
    pub name: String,
    /// See [`WindowEntry::detection_definition`].
    pub detection_definition: Option<String>,
    /// The window's current confirmed status.
    pub status: WindowStatus,
    /// This window's resolved workspace key: the chat room
    /// participation key alongside `detection_definition`.
    pub workspace: WorkspaceKey,
    /// See [`WindowEntry::composer_access`].
    pub composer_access: ComposerAccess,
}

/// What [`WindowRegistry::initialize_detection_for_window`] resolved for a
/// new window: the detector handle and its status stream, the definition's
/// load warning, and the input profile read out of the definition before it
/// was moved into the detection engine.
struct DetectionSetup {
    handle: Arc<DetectorHandle>,
    detected_rx: mpsc::UnboundedReceiver<ConfirmedStatus>,
    warning: Option<String>,
    input: InputProfile,
    /// The name resolution actually used to load this window's definition
    /// (the `detection` argument, or [`DaemonConfig::detection_name`] when it
    /// was omitted), `Some` only when a definition was found this way. This is the file stem [`DetectionLayers::resolve`]
    /// looked up, not the definition's own declared `name` field.
    definition_name: Option<String>,
    /// What the daemon may type into the window, read from the loaded
    /// definition before it moves into the engine, which never hands it back.
    composer_access: ComposerAccess,
    /// The loaded definition's `[requests] abandoned_at_turn_end`, read for
    /// the same reason; `false` without a definition.
    abandons_calls_at_turn_end: bool,
}

/// A request argument a rejection points at.
///
/// A name, never a value: what reaches a client is which argument was
/// refused, not what was in it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RejectedArgument {
    /// The request's working directory.
    Cwd,
}

impl RejectedArgument {
    /// The argument name clients read.
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Cwd => "cwd",
        }
    }
}

/// Errors returned by [`WindowRegistry::open`], 1:1 with the IPC error codes
/// `invalid_argument` / `spawn_failed` / `resource_exhausted`. The mapping
/// stays 1:1; which argument was refused rides along on
/// [`WindowOpenError::InvalidArgument`] instead of splitting the code.
#[derive(Debug, Clone, Error)]
pub enum WindowOpenError {
    /// Rejected input: an empty command, an invalid `cwd`, or a
    /// syntactically invalid detection name.
    #[error("invalid argument: {reason}")]
    InvalidArgument {
        /// Human-readable English reason.
        reason: String,
        /// The refused argument, named only when the caller sent the value
        /// at fault: `None` also covers a value that came from elsewhere.
        argument: Option<RejectedArgument>,
    },
    /// Validation passed but the process could not be spawned.
    #[error("failed to spawn the window: {0}")]
    SpawnFailed(String),
    /// A resource limit rejected this open: the living-window cap, the
    /// total-entry cap, or the workspace-walk permit gate.
    #[error("too many windows are open")]
    ResourceExhausted,
}

impl WindowOpenError {
    /// The one place a working directory's origin decides which argument a
    /// refusal names, so the call sites that check a directory cannot each
    /// answer it differently.
    fn invalid_cwd(origin: CwdOrigin, reason: String) -> Self {
        let argument = match origin {
            // Naming an argument tells a caller that the value they sent is
            // at fault, so a directory they never sent is never named.
            CwdOrigin::Caller => Some(RejectedArgument::Cwd),
            CwdOrigin::ProfileDeclared | CwdOrigin::DaemonDefault | CwdOrigin::DaemonCurrentDir => {
                None
            }
        };
        Self::InvalidArgument { reason, argument }
    }
}

/// Who wrote the bytes going to a window's PTY.
///
/// The three variants map 1:1 onto the daemon's three PTY write paths and
/// exist so the detector can tell interactive line editing (whose output is
/// just an echo) from a write that may start work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InputOrigin {
    /// Keystrokes and pastes from an attached client (IPC `input`).
    Interactive,
    /// Text delivered through `send_text` / the MCP `send_message` tool.
    Message,
    /// A control byte sequence delivered by `interrupt_window`.
    Control,
}

/// Classifies a PTY input write for the detector's echo attribution.
///
/// A pure function so this decision is unit-testable without a daemon.
/// The payload is inspected for a line terminator only; it is never stored,
/// logged or forwarded anywhere else.
///
/// Who wrote the bytes does not change what they do: a payload carrying no
/// line terminator submits nothing, so the output that follows it is its own
/// echo, regardless of whether it came from an attached client or a message
/// the daemon wrote on an agent's behalf. A control byte sequence carries no
/// terminator but is never line editing, so it is always classified as
/// possibly starting work.
///
/// The `match` lists every [`InputOrigin`] variant instead of using a
/// wildcard, so adding a variant fails to compile until its classification
/// is decided here.
///
/// # Examples
///
/// ```
/// use yagura_daemon::detector::InputEcho;
/// use yagura_daemon::window::{InputOrigin, classify_input_echo};
///
/// // A body with no line terminator is just an echo, whoever wrote it.
/// assert_eq!(
///     classify_input_echo(b"hello", InputOrigin::Message),
///     InputEcho::EchoOnly
/// );
/// // The submit key that follows it may start work.
/// assert_eq!(
///     classify_input_echo(b"\r", InputOrigin::Message),
///     InputEcho::MayStartWork
/// );
/// ```
#[must_use]
pub fn classify_input_echo(data: &[u8], origin: InputOrigin) -> InputEcho {
    match origin {
        InputOrigin::Interactive | InputOrigin::Message => {
            if data.contains(&b'\r') || data.contains(&b'\n') {
                InputEcho::MayStartWork
            } else {
                InputEcho::EchoOnly
            }
        }
        InputOrigin::Control => InputEcho::MayStartWork,
    }
}

/// What [`WindowEntry::submit_message`] ended with: whether
/// the submit key was reached, or the envelope was never placed at all. Kept
/// as separate variants rather than fields on one struct, so a caller reading
/// a declined placement cannot also reach [`SubmitOutcome`]'s fields, which
/// are only meaningful once the submit key has actually been written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubmitPlacement {
    /// The submit key was written; carries what the write sequence observed.
    Placed(SubmitOutcome),
    /// The envelope was not placed in the composer.
    NotPlaced(NotPlaced),
}

/// [`SubmitPlacement::NotPlaced`]'s payload: two facts a caller needs, kept
/// separate because neither can be derived from the other.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotPlaced {
    /// Whether this call's own envelope remains in the receiver's composer.
    /// The only fact a caller's teardown decision may use: a reason on its
    /// own cannot say, since [`NotPlacedReason::NotReflected`] covers both a
    /// take-back that succeeded and one that could not run at all.
    pub envelope_remains: bool,
    /// Why the envelope was not placed, for a response message or a log line
    /// only -- never for deciding whether anything remains.
    pub reason: NotPlacedReason,
}

/// Why [`WindowEntry::submit_message`] did not place the envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NotPlacedReason {
    /// The composer held content that did not match what this call brought,
    /// or nothing else could vouch for the residue it was asked
    /// to trust.
    Foreign,
    /// The composer could not be observed at all: the marker was not found
    /// before the requester left or another writer wrote, or the window's
    /// degenerate-empty marker is still active.
    Unobservable,
    /// This call's own body did not end up in the composer: the receiver
    /// dropped it (the probe written behind it showed alone), or the
    /// requester left once it was written and it was taken back -- the
    /// take-back left the composer confirmed empty, could not run at all, or
    /// its own effect could not be confirmed.
    NotReflected,
    /// A write from some other writer landed in this window between this
    /// call's own first byte and the point it would have pressed the submit
    /// key or taken its write back.
    Interrupted,
    /// The screen, at the instant this call would have written, matched the
    /// detection definition's approval rule or a rule's explicit `unknown`
    /// declaration -- an approval prompt, most commonly -- so nothing was
    /// written. Never based on the confirmed status, which can trail the
    /// screen by `confirm_ms`: this reason names what the rules alone say
    /// about the exact screen the write was about to land on.
    NotAccepting,
    /// The window's detection definition declares no composer, or declares
    /// one without a probe where the body must be judged, so the daemon
    /// writes no line into it: nothing on screen could tell whether a body
    /// would land in an input line or in a running program's input, or
    /// whether it showed. Nothing was read or written.
    NoComposer,
}

/// What a send plan believes its own previous attempt left in a window's
/// composer: the text it wrote, and the window's write
/// count at the instant it stopped writing.
///
/// A window holds neither value, so two senders writing the same text are
/// told apart by what each brings back rather than by anything the window
/// remembers about who wrote last.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposerResidue<'a> {
    /// The text this send left in the composer.
    pub body: &'a str,
    /// [`WindowEntry::write_seq`] as it stood once this send's own writes
    /// were done. Still equal to the window's current count means not one
    /// write, from any writer, has landed since.
    pub write_seq: u64,
}

/// What one [`WindowEntry::submit_message`] call ended as.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SubmitJudgment {
    /// Whether the submit key was written, and what the sequence observed.
    pub placement: SubmitPlacement,
    /// The [`ComposerResidue::write_seq`] a follow-up attempt at the same
    /// body must bring back, or `None` when nothing of this call's own is
    /// left in the composer. Counts this call's own writes only, so a
    /// foreign write that landed mid-sequence is never folded into the
    /// baseline handed on.
    pub residue_write_seq: Option<u64>,
    /// Set when the envelope was declined because someone else's text is in
    /// the composer. That text stays until somebody empties it, so a retry
    /// has nothing new to see until the composer reads empty.
    pub foreign_draft: bool,
}

/// Builds a declined judgment. `residue_write_seq` is the single source for
/// both the baseline a retry brings back and [`NotPlaced::envelope_remains`]:
/// this call's own bytes are left in the composer exactly when there is a
/// baseline to describe them. The one exception is a take-back whose effect
/// could not be confirmed ([`declined_unvouched`]).
fn declined(reason: NotPlacedReason, residue_write_seq: Option<u64>) -> SubmitJudgment {
    SubmitJudgment {
        placement: SubmitPlacement::NotPlaced(NotPlaced {
            envelope_remains: residue_write_seq.is_some(),
            reason,
        }),
        residue_write_seq,
        foreign_draft: false,
    }
}

/// Builds a declined judgment for a composer that may still hold something
/// once a take-back was written but its effect could not be confirmed. The
/// ledger only proves no other writer touched the window, not that the
/// take-back's own bytes erased anything, so no baseline is handed on for a
/// retry to press the submit key against.
fn declined_unvouched(reason: NotPlacedReason) -> SubmitJudgment {
    SubmitJudgment {
        placement: SubmitPlacement::NotPlaced(NotPlaced {
            envelope_remains: true,
            reason,
        }),
        residue_write_seq: None,
        foreign_draft: false,
    }
}

/// `judgment` (a [`NotPlacedReason::Foreign`] decline) marked as declined for
/// someone else's text.
fn foreign_draft(mut judgment: SubmitJudgment) -> SubmitJudgment {
    judgment.foreign_draft = true;
    judgment
}

/// Builds a placed judgment: the submit key was written, and `write_seq` is
/// what a resend of the same body must bring back.
fn placed(outcome: SubmitOutcome, write_seq: u64) -> SubmitJudgment {
    SubmitJudgment {
        placement: SubmitPlacement::Placed(outcome),
        residue_write_seq: Some(write_seq),
        foreign_draft: false,
    }
}

/// What [`WindowEntry::read_through_probe`] found.
enum ProbeReading {
    /// The probe showed last in the composer, on a screen that accepts a
    /// write; `before` is what precedes it.
    Shown { before: String },
    /// Another writer wrote after the probe.
    Interrupted,
    /// The requester left, before the probe was written or before it showed.
    Abandoned,
}

/// What [`WindowEntry::erase_probe`] found.
enum ProbeErased {
    /// The composer reads empty, on the screen this rule verdict was read on,
    /// with the ledger at this count.
    Emptied(Option<DetectedStatus>, u64),
    /// Another writer wrote; this call's own writes left the ledger here.
    Interrupted(u64),
}

/// What ended [`WindowEntry::wait_for_composer`].
enum ComposerWait<T> {
    /// A reading the caller's `settle` accepted, and what it made of it.
    Done(T),
    /// Another writer moved the ledger past the caller's own writes.
    OtherWriter,
    /// The child exited.
    Exited,
    /// The requester of the write left.
    RequesterGone,
}

/// What the composer shows once a fresh body and the probe behind it were
/// written: the receiver handles input in order, so the probe showing means
/// the body was handled before it.
enum Reflection {
    /// Something shows before the probe: the body landed, reading as
    /// `before`.
    Behind(String),
    /// The probe shows alone: the receiver dropped the body.
    Dropped,
    /// The composer cannot be read back, and nothing but this call's own
    /// writes reached the window since it was read empty: the body landed
    /// past what can be read (a body running off the viewport, most often).
    Unreadable,
}

/// The requester of one write, as far as the window can tell. `None` is a
/// caller with nobody to lose (chat, notices), and a write that must run to
/// its end whatever the requester does.
struct Requester(Option<watch::Receiver<bool>>);

impl Requester {
    fn none() -> Self {
        Self(None)
    }

    /// Whether the requester has left: it said so, or nobody is left to say
    /// anything.
    fn has_left(&self) -> bool {
        self.0
            .as_ref()
            .is_some_and(|left| *left.borrow() || left.has_changed().is_err())
    }

    /// Resolves once the requester has left; never when there is none.
    async fn left(&mut self) {
        let Some(left) = self.0.as_mut() else {
            return std::future::pending().await;
        };
        loop {
            if *left.borrow_and_update() || left.changed().await.is_err() {
                return;
            }
        }
    }
}

/// Resolves once the child has exited, or once nothing is left to report it.
async fn child_exited(exit: &mut watch::Receiver<bool>) {
    loop {
        if *exit.borrow_and_update() || exit.changed().await.is_err() {
            return;
        }
    }
}

/// Resolves once the write ledger reads other than `own_seq`; never for
/// `None`.
async fn ledger_moved(writes: &mut watch::Receiver<u64>, own_seq: Option<u64>) {
    let Some(own_seq) = own_seq else {
        return std::future::pending().await;
    };
    loop {
        if *writes.borrow_and_update() != own_seq {
            return;
        }
        if writes.changed().await.is_err() {
            return std::future::pending().await;
        }
    }
}

/// Resolves on the next change of `cue`; never once its sender is gone.
async fn next_change<T>(cue: &mut watch::Receiver<T>) {
    if cue.changed().await.is_err() {
        std::future::pending::<()>().await;
    }
}

/// Reads what a fresh `body` and the `probe` written behind it left in the
/// composer, or `None` while that is not decided yet. A reading on its way to
/// the body and the probe -- a proper prefix of both together -- is not
/// decided, so a body made of or starting with the probe's character is never
/// taken for the probe alone.
fn reflection_of(readout: &ComposerReadout, body: &str, probe: char) -> Option<Reflection> {
    match readout {
        ComposerReadout::Body(text) => {
            let before = text.strip_suffix(probe)?;
            if is_proper_prefix_ignoring_whitespace(text, body, probe) {
                return None;
            }
            Some(if before.is_empty() {
                Reflection::Dropped
            } else {
                Reflection::Behind(before.to_string())
            })
        }
        ComposerReadout::NotObserved | ComposerReadout::Tail(_) => Some(Reflection::Unreadable),
        ComposerReadout::Empty => None,
    }
}

/// Whether `text` is a proper prefix of `body` followed by `probe`, once
/// whitespace is disregarded (see
/// [`composer_text_matches_ignoring_lost_whitespace`]).
fn is_proper_prefix_ignoring_whitespace(text: &str, body: &str, probe: char) -> bool {
    let shown: Vec<char> = text.chars().filter(|c| !c.is_whitespace()).collect();
    let written: Vec<char> = body
        .chars()
        .chain(std::iter::once(probe))
        .filter(|c| !c.is_whitespace())
        .collect();
    shown.len() < written.len() && written.starts_with(&shown)
}

/// Whether `text` is what this call wrote before a take-back (`written`, or
/// `written` short of its trailing probe), still showing: a reading the
/// take-back has not reached yet.
fn reads_as_before_take_back(text: &str, written: &str, probe: char) -> bool {
    composer_text_matches_ignoring_lost_whitespace(text, written)
        || written
            .strip_suffix(probe)
            .is_some_and(|body| composer_text_matches_ignoring_lost_whitespace(text, body))
}

/// The one write gate every byte the daemon decides to write on its own --
/// a fresh body, the clear sequence, the submit key, a key-only resend, and a
/// `send_message` body written straight through -- passes before its first
/// byte: closed while `screen_rule` alone reads the screen as an approval
/// prompt, or as a rule's explicit `unknown` declaration. `screen_rule` must
/// come from the very [`ComposerObservation`] the decision to write was made
/// from, never a fresh one taken a moment later, so the check and the write
/// see the same screen.
fn screen_accepts_write(screen_rule: Option<DetectedStatus>) -> bool {
    !matches!(
        screen_rule,
        Some(DetectedStatus::WaitingApproval)
            | Some(DetectedStatus::Unknown(UnknownReason::DeclaredUnknown))
    )
}

/// Declines with [`NotPlacedReason::NotAccepting`], carrying forward whatever
/// baseline the caller already had (`residue_write_seq`): this decline
/// performs no write of its own, so nothing about that baseline changes.
fn decline_not_accepting(residue_write_seq: Option<u64>) -> SubmitJudgment {
    declined(NotPlacedReason::NotAccepting, residue_write_seq)
}

/// Whether `observed` (a screen reading) and `expected` (a body this daemon
/// wrote or was asked to write) are the same text once whitespace is
/// disregarded.
///
/// `VtScreen::read_composer` trims each painted line's trailing whitespace
/// before joining them, so a wrapped body's own space at its very last wrap
/// boundary is lost from the reading, not from anything actually written.
/// Ignoring all whitespace (not only that one position) only ever forgives
/// this class of loss: a body differing by more than whitespace still
/// compares unequal.
fn composer_text_matches_ignoring_lost_whitespace(observed: &str, expected: &str) -> bool {
    observed
        .chars()
        .filter(|c| !c.is_whitespace())
        .eq(expected.chars().filter(|c| !c.is_whitespace()))
}

/// Whether `readout` reads back as `body` itself: the single test for "this
/// call's own body, and nothing else, is what the composer shows".
fn composer_reads_back_as(readout: &ComposerReadout, body: &str) -> bool {
    matches!(readout, ComposerReadout::Body(text)
        if composer_text_matches_ignoring_lost_whitespace(text, body))
}

/// How a take-back ended: the composer confirmed empty (with the screen rule
/// verdict of that reading), or a judgment declining the attempt.
/// `Emptied` also carries the ledger count the empty reading was guarded at.
enum TakenBack {
    Emptied(Option<DetectedStatus>, u64),
    Declined(SubmitJudgment),
}

/// What [`WindowEntry::run_judged_steps`] did.
enum JudgedSteps {
    /// Every step was written.
    Written(SubmitOutcome),
    /// Another write was counted first; the steps before the one expecting
    /// `at` were written, and nothing after.
    Moved { at: u64 },
}

/// What one [`WindowEntry::submit_message`] call knows of its own part of the
/// composer, from its own writes: what it checks once another writer's write
/// ends it. Never read from the PTY's registration, which another writer's
/// bytes empty whether or not the receiver acted on the take-back.
#[derive(Default)]
struct OwnPart {
    left: OwnLeft,
    /// Whether the call read its own body back off the screen.
    read_back: bool,
    /// The ledger count the call's own latest write left.
    after_own_write: Option<u64>,
}

#[derive(Default)]
enum OwnLeft {
    /// Nothing the call wrote is known to be in the composer.
    #[default]
    Nothing,
    /// Text the call confirmed as its own part of the composer.
    Text(String),
    /// A probe written behind a composer that read as this, not seen yet.
    UnconfirmedProbe(ComposerReadout),
}

/// What [`WindowEntry::check_own_text`] found of the call's own text.
enum Checked {
    Gone,
    MayRemain,
}

/// How a composer reading stands against a call's own text.
enum OwnShows {
    /// The call's own text and nothing else.
    Alone,
    /// The call's own text, or what may be part of it, beside something else.
    WithOthers,
    /// None of the call's own text.
    Absent,
}

/// How `text` reads against `own`, the call's own text (possibly ending with
/// the probe it wrote). A reading `own` starts with counts as beside
/// something rather than gone, since part of `own` may be what is left.
fn own_text_shows(text: &str, own: &str, probe: char) -> OwnShows {
    if reads_as_before_take_back(text, own, probe) {
        return OwnShows::Alone;
    }
    let shown: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let core: String = own
        .strip_suffix(probe)
        .unwrap_or(own)
        .chars()
        .filter(|c| !c.is_whitespace())
        .collect();
    if !shown.is_empty() && (shown.contains(&core) || core.starts_with(&shown)) {
        OwnShows::WithOthers
    } else {
        OwnShows::Absent
    }
}

/// `judgment`, still reporting the envelope as possibly remaining.
fn remaining(judgment: SubmitJudgment) -> SubmitJudgment {
    if leaves_envelope(&judgment) {
        judgment
    } else {
        declined_unvouched(NotPlacedReason::Interrupted)
    }
}

fn leaves_envelope(judgment: &SubmitJudgment) -> bool {
    match judgment.placement {
        SubmitPlacement::NotPlaced(not_placed) => not_placed.envelope_remains,
        SubmitPlacement::Placed(_) => false,
    }
}

/// Ends a call's take-back generation on drop, however the call ends, so
/// its registration never outlives it.
struct TakeBackScope<'a> {
    entry: &'a WindowEntry,
    call: TakeBackCall,
}

impl Drop for TakeBackScope<'_> {
    fn drop(&mut self) {
        *lock_recover(&self.entry.take_back_call) = None;
        *lock_recover(&self.entry.own_part) = OwnPart::default();
        self.entry.session.end_take_back_call(self.call);
    }
}

/// One place in a window's queue for its delivery turn, given back on drop.
struct TurnWaiterPlace<'a> {
    waiters: &'a AtomicUsize,
}

impl<'a> TurnWaiterPlace<'a> {
    /// Takes a place while fewer than `max` are taken, or `None`.
    fn take(waiters: &'a AtomicUsize, max: usize) -> Option<Self> {
        take_one_below(waiters, max).then(|| Self { waiters })
    }
}

/// Adds one to `counter` if it is below `max`, as one compare-and-swap, so
/// two takers racing for the last unit can never both get it.
pub(crate) fn take_one_below(counter: &AtomicUsize, max: usize) -> bool {
    let mut current = counter.load(Ordering::Acquire);
    while current < max {
        match counter.compare_exchange_weak(
            current,
            current + 1,
            Ordering::AcqRel,
            Ordering::Acquire,
        ) {
            Ok(_) => return true,
            Err(observed) => current = observed,
        }
    }
    false
}

impl Drop for TurnWaiterPlace<'_> {
    fn drop(&mut self) {
        self.waiters.fetch_sub(1, Ordering::AcqRel);
    }
}

/// Per-window resources: the PTY session, the detection engine front end,
/// status fan-out, the attach slot and the window's own `StatusEvent` auth
/// token.
///
/// The detector is never exposed as an `Arc` (a `&Arc` return and its
/// eventual drop on `Exited` cannot coexist safely); every interaction goes
/// through a delegating method that becomes a no-op / `false` once the
/// window has exited.
pub struct WindowEntry {
    window_id: String,
    /// Mutable after creation, so every read goes through the lock. A guard
    /// on it is never held across an `.await` and never taken by `Debug`
    /// (see that impl), because `std::sync::Mutex` is not reentrant.
    name: Mutex<String>,
    vendor: Option<String>,
    cwd: String,
    /// This window's resolved workspace key, computed once at
    /// open time from `cwd` and never re-derived: renaming or moving the
    /// directory after the window is open does not change its room.
    workspace: WorkspaceKey,
    /// Argument vector this window was spawned with, as a session snapshot
    /// records it.
    command: Vec<String>,
    /// The `detection` field this window's [`OpenWindowRequest`] carried,
    /// verbatim (`None` when the caller left it to the daemon's default):
    /// what the caller asked for, not the resolved default name.
    detection: Option<String>,
    /// The name resolution actually used to load this window's detection
    /// definition, `Some` only when a definition was found this way: the chat room's participation key.
    detection_definition: Option<String>,
    /// English reason text when this window has no detection definition to
    /// run (the same text [`OpenedWindow::detection_warning`] carried at
    /// open time). `None` only when a definition loaded.
    detection_warning: Option<String>,
    /// The presentation layout the client last reported for this window,
    /// held verbatim and never interpreted here: the daemon owns
    /// the window, the client owns how it is drawn. `None` until a client
    /// reports one. Mutable after creation, so every access goes through the
    /// lock.
    layout_json: Mutex<Option<String>>,
    /// Set by [`WindowEntry::mark_closed`] once `close_window` has succeeded:
    /// [`WindowRegistry::request_snapshot`] excludes a closed window from the
    /// restore candidates it builds, while a window whose child merely exited
    /// on its own stays included.
    closed: AtomicBool,
    session: Arc<PtySession>,
    /// The one and only long-lived strong holder besides
    /// `spawn_exit_watcher`'s task. Taken (set to `None`) once the window's
    /// confirmed status reaches `Exited`.
    detector: Mutex<Option<Arc<DetectorHandle>>>,
    /// Kept even after `detector` is taken so [`WindowEntry::detector_task_alive`]
    /// can observe the driver task's teardown converging to zero strong
    /// references.
    detector_weak: Weak<DetectorHandle>,
    status_changes: broadcast::Sender<StatusAt>,
    current_status: watch::Sender<StatusAt>,
    /// Whether this window has been shown to be past its startup phase, so a
    /// handoff into it may skip the wait for one.
    ready_hold: ReadyHoldLatch,
    current_attach: Mutex<AttachSlot>,
    attach_generation: AtomicU64,
    /// This window's own `StatusEvent` auth token, scoped to it. Held behind
    /// an [`Arc`] because
    /// [`DaemonConfig::status_token_override`] makes every window share one
    /// value; without the override each window owns a freshly generated token.
    /// No accessor exposes the raw value:
    /// [`WindowEntry::verify_status_token`] and the env injection in
    /// [`WindowRegistry::open`] are the only consumers.
    status_token: Arc<StatusToken>,
    /// How this window's CLI accepts and submits one line, resolved from its
    /// detection definition's `[input]` table at open time (`InputProfile`
    /// defaults when the definition has none, or none loaded).
    input: InputProfile,
    /// What the daemon may type into this window, fixed for the window's
    /// lifetime because the definition is loaded once.
    composer_access: ComposerAccess,
    /// See [`WindowEntry::abandons_calls_at_turn_end`].
    abandons_calls_at_turn_end: bool,
    /// Serializes the daemon's own write sequences
    /// ([`WindowEntry::submit_message`], [`WindowEntry::submit_raw_gated`])
    /// so two of them writing into the same window's input can never
    /// interleave their steps. Interactive keystrokes, `send_keys` and
    /// control bytes do not take this lock: they must reach a window whose
    /// sequence is stuck waiting on it.
    ///
    /// What it does not cover: anything spanning more than one sequence. A
    /// caller that writes, waits for a reaction and writes again holds this
    /// lock only for each write, so [`WindowEntry::delivery_lock`] exists for
    /// that longer unit.
    input_lock: AsyncMutex<()>,
    /// Serializes whole handoff deliveries into this window (startup-phase
    /// judgment, envelope, confirmation wait and re-sends), which
    /// [`WindowEntry::submit_message`]'s per-sequence lock is too small a unit
    /// for: one delivery's clear sequence would otherwise erase an envelope
    /// another delivery wrote while it was waiting.
    delivery_lock: AsyncMutex<()>,
    /// Deliveries queued on `delivery_lock` without holding it.
    turn_waiters: AtomicUsize,
    /// Set to the write sequence number at the instant a write could not be
    /// confirmed and could not be taken back (no clear sequence): while it
    /// still equals the current sequence number, this window's "empty" reads
    /// are not trusted, since the bytes that failed to
    /// reflect may still be sitting unobserved. It self-clears the moment any
    /// further write moves the sequence past this value.
    degenerate_since: Mutex<Option<u64>>,
    /// The write ledger's count right after a probe whose requester left
    /// before it showed (see [`InputProfile::probe`]): if it shows later,
    /// with nothing written since, what the composer holds can still be
    /// settled.
    unanswered_probe: Mutex<Option<u64>>,
    /// What [`crate::control::interrupt_window`] writes, from the same
    /// configuration: a write of exactly these bytes goes out with no
    /// take-back in front, since some CLIs exit on an interrupt to an empty
    /// composer.
    interrupt_bytes: Vec<u8>,
    /// The take-back generation of the [`WindowEntry::submit_message`] call
    /// holding `input_lock`; `None` between calls.
    take_back_call: Mutex<Option<TakeBackCall>>,
    /// What that call has of its own in the composer.
    own_part: Mutex<OwnPart>,
    /// The chat intro's outcome, written at most once. `None`
    /// while the intro is still queued, or when none was queued (no chat, no
    /// intro, or not a participant).
    chat_intro_outcome: std::sync::OnceLock<ChatIntroOutcome>,
    /// The notices waiting for this window to accept input, and whether a
    /// delivery agent is running for them. The agent itself lives in the IPC
    /// layer and is started through [`WindowRegistry::wake_notices`].
    notices: Mutex<NoticeQueue>,
    /// Wakes this window's running delivery agent.
    notice_wake: Notify,
    /// How many deliveries this window's agent has attempted, written or not.
    notice_attempts: AtomicU64,
    /// Fixed at open time and never rewritten, so a later request can never
    /// move who this window reports to.
    provenance: WindowProvenance,
    /// How many live connections have identified as this window. Shared with
    /// each [`McpConnectionClaim`] so a claim never keeps the entry alive.
    mcp_connections: Arc<AtomicUsize>,
}

/// The chat intro's outcome, observed through
/// [`WindowEntry::chat_intro_outcome`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatIntroOutcome {
    /// The intro was written into the window (`run_delivery` reached a
    /// verdict, whether or not the target reacted to it).
    Sent,
    /// Not recorded by this daemon: an intro that finds the chat delivery
    /// budget exhausted stays queued and is tried again.
    Skipped,
    /// Not recorded by this daemon: an intro that could not be written stays
    /// queued and is tried again.
    NotPlaced,
    /// The window ended before the intro was written.
    Abandoned,
}

/// One notice waiting in a window's own queue. A window holds at most one of
/// each kind, except [`Self::PlanApproved`] (one owner can have several plans
/// approved in a row, each naming a different plan); a report notice is
/// never queued, it is derived from the window's report inbox whenever the
/// queue is empty.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum QueuedNotice {
    /// The chat intro. Blocks the queue until the window's room is settled,
    /// so nothing is written before the room row exists.
    Intro { room_settled: bool },
    /// The `[yagura task]` notice for the plan task this window is bound to.
    Task { plan_id: String, task_id: String },
    /// The `[yagura plan]` notice telling this plan's owner that `plan_id`
    /// was approved.
    PlanApproved { plan_id: String },
}

impl QueuedNotice {
    /// Whether `self` and `other` are the same queued item: for every kind
    /// but [`Self::PlanApproved`] this is the discriminant alone, so a window
    /// never holds two of the same kind at once; a `PlanApproved` also
    /// compares `plan_id`, since one owner may have several plans approved
    /// in a row.
    fn same_kind(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::PlanApproved { plan_id: a }, Self::PlanApproved { plan_id: b }) => a == b,
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

#[derive(Default)]
struct NoticeQueue {
    items: VecDeque<QueuedNotice>,
    agent_running: bool,
}

impl WindowEntry {
    /// This window's registry-assigned identifier
    /// (`"w{sequence}-{boot nonce}"`, for example `"w1-k3f9xa"`).
    ///
    /// Non-persistent and opaque: identifiers are only valid for the lifetime
    /// of the daemon process, and the boot nonce makes an
    /// identifier minted by a previous daemon fail to resolve rather than
    /// match a different window.
    pub fn window_id(&self) -> &str {
        &self.window_id
    }

    /// The PTY session backing this window (input / resize / screen /
    /// `send_text` write target).
    pub fn session(&self) -> &Arc<PtySession> {
        &self.session
    }

    /// The working directory this window was opened with.
    pub fn cwd(&self) -> &str {
        &self.cwd
    }

    /// The window whose identified connection opened this one, if any.
    /// `None` for a window opened by an approver, an unidentified
    /// connection, or the daemon itself.
    #[must_use]
    pub fn opener(&self) -> Option<&str> {
        self.provenance.opener.as_deref()
    }

    /// The plan task this window was opened for by `open_task_window`, if
    /// any. Recorded once and never cleared, even after the task is rebound
    /// to another window.
    #[must_use]
    pub fn task_origin(&self) -> Option<&TaskOrigin> {
        self.provenance.task_origin.as_ref()
    }

    /// The `detection` field this window's [`OpenWindowRequest`] carried,
    /// verbatim. `None` when the caller left it to the daemon's configured
    /// default.
    pub fn detection(&self) -> Option<&str> {
        self.detection.as_deref()
    }

    /// The name resolution actually used to load this window's detection
    /// definition, `Some` only when a definition was found this way: the chat room's participation key.
    pub fn detection_definition(&self) -> Option<&str> {
        self.detection_definition.as_deref()
    }

    /// English reason text when this window has no detection definition to
    /// run. `None` only when a definition loaded.
    #[must_use]
    pub fn detection_warning(&self) -> Option<&str> {
        self.detection_warning.as_deref()
    }

    /// What the daemon may type into this window, from the detection
    /// definition it was opened with: [`ComposerAccess::Absent`] without a
    /// `[composer]` table (or without a definition), [`ComposerAccess::RawOnly`]
    /// with a composer but no `[input].probe`, and
    /// [`ComposerAccess::Judged`] with both. The daemon writes a line of its
    /// own composition only into a window that is `Judged`.
    #[must_use]
    pub fn composer_access(&self) -> ComposerAccess {
        self.composer_access
    }

    /// Whether this window's CLI, by its detection definition's `[requests]`
    /// table, drops a tool call still out when it ends a turn, without
    /// cancelling it. `false` without a definition.
    #[must_use]
    pub fn abandons_calls_at_turn_end(&self) -> bool {
        self.abandons_calls_at_turn_end
    }

    /// This window's workspace key, resolved once when it was opened: the chat room a post from this window belongs to.
    pub(crate) fn workspace(&self) -> &WorkspaceKey {
        &self.workspace
    }

    /// This window's chat intro outcome, once settled. `None`
    /// means either no intro was queued for this window, or it is still
    /// queued.
    #[must_use]
    pub fn chat_intro_outcome(&self) -> Option<ChatIntroOutcome> {
        self.chat_intro_outcome.get().copied()
    }

    /// Records this window's chat intro outcome. A no-op if already set: it
    /// is written at most once.
    pub(crate) fn set_chat_intro_outcome(&self, outcome: ChatIntroOutcome) {
        let _ = self.chat_intro_outcome.set(outcome);
    }

    /// How many notice delivery attempts this window's delivery agent has
    /// finished so far, whether or not they were written. Monotonic; an
    /// attempt is counted once what it leaves behind is recorded.
    #[must_use]
    pub fn notice_attempts(&self) -> u64 {
        self.notice_attempts.load(Ordering::SeqCst)
    }

    pub(crate) fn note_notice_attempt(&self) {
        self.notice_attempts.fetch_add(1, Ordering::SeqCst);
    }

    /// Queues `notice` behind whatever is already waiting, unless one of the
    /// same kind already is. Returns whether it was queued.
    ///
    /// A window with no composer is queued to as well: its agent never
    /// delivers the notice, but settles it (a task's as `window_closed`)
    /// once the window ends.
    pub(crate) fn queue_notice(&self, notice: QueuedNotice) -> bool {
        let mut queue = lock_recover(&self.notices);
        if queue.items.iter().any(|queued| queued.same_kind(&notice)) {
            return false;
        }
        queue.items.push_back(notice);
        true
    }

    /// Lets a queued intro through once its room is settled.
    pub(crate) fn settle_intro_room(&self) {
        let mut queue = lock_recover(&self.notices);
        for queued in &mut queue.items {
            if let QueuedNotice::Intro { room_settled } = queued {
                *room_settled = true;
            }
        }
    }

    pub(crate) fn front_notice(&self) -> Option<QueuedNotice> {
        lock_recover(&self.notices).items.front().cloned()
    }

    fn has_queued_notices(&self) -> bool {
        !lock_recover(&self.notices).items.is_empty()
    }

    /// Removes the queued notice of `notice`'s kind, wherever it stands.
    pub(crate) fn remove_notice(&self, notice: &QueuedNotice) {
        lock_recover(&self.notices)
            .items
            .retain(|queued| !queued.same_kind(notice));
    }

    /// Marks a delivery agent as running for this window. `false` when one
    /// already is.
    pub(crate) fn claim_notice_agent(&self) -> bool {
        !std::mem::replace(&mut lock_recover(&self.notices).agent_running, true)
    }

    /// Ends this window's delivery agent, handing back every notice it never
    /// wrote. Under the same lock as [`Self::claim_notice_agent`], so a notice
    /// queued after this starts a new agent rather than being stranded.
    pub(crate) fn release_notice_agent(&self) -> Vec<QueuedNotice> {
        let mut queue = lock_recover(&self.notices);
        queue.agent_running = false;
        queue.items.drain(..).collect()
    }

    pub(crate) fn wake_notice_agent(&self) {
        self.notice_wake.notify_one();
    }

    pub(crate) fn notice_woken(&self) -> tokio::sync::futures::Notified<'_> {
        self.notice_wake.notified()
    }

    /// Marks this window closed. Called only from the `close_window` success
    /// path in `ipc.rs`, immediately after the process tree has been torn
    /// down. A window whose child merely exited on its own is never marked
    /// this way and stays a restore candidate.
    pub(crate) fn mark_closed(&self) {
        self.closed.store(true, Ordering::SeqCst);
    }

    /// Writes `data` to this window's PTY, telling the detector what kind of
    /// output to expect from it.
    ///
    /// The detector is notified *before* the write, so the notification is
    /// queued ahead of the echo it explains. A window whose
    /// detector has already been taken (`Exited`) is written to without a
    /// notification: nothing is left to attribute the output to.
    ///
    /// A [`WindowEntry::submit_message`] call's own unsent text goes in front
    /// of `data`, taken back (see [`PtySession::write_input_with`]), unless
    /// `data` is exactly this window's configured interrupt bytes.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::pty::PtyError`] from the underlying write; in
    /// particular [`crate::pty::PtyError::ChildExited`] once the child is gone.
    pub async fn write_pty_input(
        &self,
        data: &[u8],
        origin: InputOrigin,
    ) -> crate::pty::PtyResult<()> {
        // Scoped so the `std::sync::Mutex` guard is released before the
        // `.await` below (`WindowEntry::detector`).
        {
            let guard = lock_recover(&self.detector);
            if let Some(detector) = guard.as_ref() {
                detector.notify_input(classify_input_echo(data, origin));
            }
        }
        let prefix = if !data.is_empty() && data == self.interrupt_bytes.as_slice() {
            TakeBackPrefix::Withhold
        } else {
            TakeBackPrefix::Apply
        };
        self.session.write_input_with(data, prefix).await
    }

    /// Writes `data` as one of [`WindowEntry::submit_message`]'s own writes:
    /// only while the ledger still reads `expected`, the value the decision
    /// to write was made on. The detector hears of it only once that holds.
    async fn write_judged(
        &self,
        data: &[u8],
        expected: u64,
        registration: TakeBackRegistration,
    ) -> PtyResult<GuardedWrite> {
        let detector = lock_recover(&self.detector).clone();
        let echo = classify_input_echo(data, InputOrigin::Message);
        let left = match &registration {
            TakeBackRegistration::Keep => None,
            TakeBackRegistration::Empty => Some(OwnLeft::Nothing),
            TakeBackRegistration::Set { text, .. } => Some(OwnLeft::Text(text.clone())),
        };
        let written = self
            .session
            .write_input_if_unmoved(data, expected, registration, move || {
                if let Some(detector) = detector {
                    detector.notify_input(echo);
                }
            })
            .await?;
        if written == GuardedWrite::Written {
            let mut own = lock_recover(&self.own_part);
            own.after_own_write = Some(expected.wrapping_add(1));
            if let Some(left) = left {
                own.left = left;
            }
        }
        Ok(written)
    }

    /// Notes that the running call read its own `body` back off the screen,
    /// when `shown` is it.
    fn note_read_back(&self, shown: &str, body: &str) {
        if composer_text_matches_ignoring_lost_whitespace(shown, body) {
            lock_recover(&self.own_part).read_back = true;
        }
    }

    /// [`WindowEntry::write_judged`] for each step of `plan` in turn, the
    /// first expecting `expected` and each later one the count after the step
    /// before it. `registration` gets each step's index.
    async fn run_judged_steps(
        &self,
        plan: &SubmitPlan,
        expected: u64,
        registration: impl Fn(usize) -> TakeBackRegistration,
    ) -> PtyResult<JudgedSteps> {
        let last_index = plan.steps.len().saturating_sub(1);
        let mut status_before_submit = self.current_status();
        let mut submitted_at = tokio::time::Instant::now();
        for (index, step) in plan.steps.iter().enumerate() {
            if index == last_index {
                status_before_submit = self.current_status();
            }
            let at = expected + index as u64;
            match self
                .write_judged(&step.bytes, at, registration(index))
                .await?
            {
                GuardedWrite::Written => {}
                GuardedWrite::Moved => return Ok(JudgedSteps::Moved { at }),
            }
            if index == last_index {
                submitted_at = tokio::time::Instant::now();
            }
            if !step.delay_after.is_zero() {
                tokio::time::sleep(step.delay_after).await;
            }
        }
        Ok(JudgedSteps::Written(SubmitOutcome {
            status_before_submit,
            submitted_at,
        }))
    }

    /// The registration naming `text` as the running call's own part of the
    /// composer; none when this window's profile cannot take text back (or no
    /// call is running).
    fn own_text(&self, text: &str) -> TakeBackRegistration {
        match (*lock_recover(&self.take_back_call), self.input.take_back()) {
            (Some(call), TakeBack::Fixed) => TakeBackRegistration::Set {
                call,
                text: text.to_string(),
                clear: self.input.clear.clone(),
            },
            (None, _) | (Some(_), TakeBack::Unavailable) => TakeBackRegistration::Empty,
        }
    }

    /// Starts the take-back generation for a call holding `input_lock`.
    fn begin_take_back_scope(&self) -> TakeBackScope<'_> {
        let call = self.session.begin_take_back_call();
        *lock_recover(&self.take_back_call) = Some(call);
        *lock_recover(&self.own_part) = OwnPart::default();
        TakeBackScope { entry: self, call }
    }

    /// How many writes this window has taken, from every writer alike. A send plan pairs this with the text it left in the
    /// composer ([`ComposerResidue`]) to tell, on its next attempt, whether
    /// anything has been written since.
    #[must_use]
    pub fn write_seq(&self) -> u64 {
        self.session.writes_recorded()
    }

    /// This window's resolved input profile: how its CLI accepts and submits
    /// one line, from its detection definition's `[input]` table (or this
    /// module's defaults when the definition has none, or none loaded).
    pub fn input_profile(&self) -> &InputProfile {
        &self.input
    }

    /// Writes `plan`'s whole sequence as one unbroken unit under this window's
    /// input lock, once it passes this window's
    /// write gate: `plan` is refused, unwritten, while the screen -- as of the
    /// very reading this call itself takes, immediately before `plan`'s first
    /// byte -- matches the detection definition's approval rule or a rule's
    /// explicit `unknown` declaration. Used for a `send_message` body outside
    /// [`WindowEntry::submit_message`]'s own judgment (a body with no line
    /// terminator, or a bare submit-key terminator), which is always
    /// [`InputOrigin::Message`]: unlike that judgment, this write carries no
    /// residue of a previous attempt, so a decline always reports nothing of
    /// its own left behind. `send_keys` bypasses this gate and the lock
    /// entirely, since its bytes are what a caller explicitly asked to have
    /// written.
    ///
    /// `requester` turns `true` once whoever asked for the write has left (a
    /// closed channel counts too): nothing is written after that. `None`
    /// never leaves.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::pty::PtyError`] from `plan`'s write; in particular
    /// [`crate::pty::PtyError::ChildExited`] if the child exits before or
    /// during the write. A write this gate declines to make is never an
    /// error: it is reported as [`SubmitPlacement::NotPlaced`], with
    /// [`NotPlacedReason::NoComposer`] before anything is read when this
    /// window's definition declares no composer.
    pub async fn submit_raw_gated(
        &self,
        plan: &SubmitPlan,
        requester: Option<watch::Receiver<bool>>,
    ) -> PtyResult<SubmitJudgment> {
        let mut requester = Requester(requester);
        let Some(_guard) = self.lock_input(&mut requester).await else {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        };
        if self.composer_access == ComposerAccess::Absent {
            return Ok(declined(NotPlacedReason::NoComposer, None));
        }
        let Some(observation) = self.observe_composer(&mut requester).await? else {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        };
        if !screen_accepts_write(observation.screen_rule) {
            return Ok(declined(NotPlacedReason::NotAccepting, None));
        }
        let outcome = self.run_plan_steps(plan, InputOrigin::Message).await?;
        Ok(SubmitJudgment {
            placement: SubmitPlacement::Placed(outcome),
            residue_write_seq: None,
            foreign_draft: false,
        })
    }

    /// One write sequence's step loop, run under the input lock its caller
    /// holds, for bytes the caller composed itself
    /// ([`WindowEntry::submit_raw_gated`]); [`WindowEntry::submit_message`]'s
    /// own writes go through [`WindowEntry::run_judged_steps`] instead.
    ///
    /// `status_before_submit` is read immediately before the final step and
    /// `submitted_at` is the instant that write completes, both inside the
    /// lock, so no other daemon sequence's step lands between the body and
    /// the read.
    async fn run_plan_steps(
        &self,
        plan: &SubmitPlan,
        origin: InputOrigin,
    ) -> PtyResult<SubmitOutcome> {
        let last_index = plan.steps.len().saturating_sub(1);
        let mut status_before_submit = self.current_status();
        let mut submitted_at = tokio::time::Instant::now();
        for (index, step) in plan.steps.iter().enumerate() {
            if index == last_index {
                status_before_submit = self.current_status();
            }
            self.write_pty_input(&step.bytes, origin).await?;
            if index == last_index {
                submitted_at = tokio::time::Instant::now();
            }
            if !step.delay_after.is_zero() {
                tokio::time::sleep(step.delay_after).await;
            }
        }
        Ok(SubmitOutcome {
            status_before_submit,
            submitted_at,
        })
    }

    /// The daemon's single judged entry point for putting one line of its own
    /// composition into a window's input: every decision (write, resend the submit key only, or
    /// refuse) is made from what this call observes of the composer *and*
    /// this window's write ledger, immediately before acting, inside one
    /// exclusive region.
    ///
    /// Whether a write showed is decided by the order the receiver handles
    /// its input in, never by a clock: the probe is written behind the body,
    /// and the body counts as handled once the probe shows. Until then the
    /// call waits, and only the child exiting, `requester` leaving or another
    /// writer's write ends that wait without an answer from the screen.
    ///
    /// Another writer's write gets this call's unsent text taken back in
    /// front of it, and this call writes nothing more of its plan. Before it
    /// answers, it reads a screen that accepts a write, with no time limit,
    /// and takes its text back again behind a probe of its own when that
    /// text shows alone (the receiver dropped the take-back). The envelope is
    /// reported as gone only when the screen shows it gone; a further write,
    /// `requester` leaving or the child exiting ends that check with it
    /// reported as possibly remaining.
    ///
    /// `residue` is what the caller's own send plan believes it left in the
    /// composer from a previous attempt at the same envelope (`None` for a
    /// first attempt, or for a caller with no notion of its own residue, such
    /// as a plain `send_text`); [`SubmitJudgment::residue_write_seq`] is
    /// where a caller gets the one this call leaves behind. Neither half of
    /// it is read from this window: two callers writing the same content are
    /// told apart by what they each bring, not by anything this window
    /// remembers about who wrote what.
    ///
    /// `requester` turns `true` once whoever asked for the write has left (a
    /// closed channel counts too); `None` never leaves. Nothing new is
    /// written after it leaves, except what takes this call's own body back:
    /// that runs to its end, so the call answers only once the composer is
    /// confirmed empty, or with the body reported as remaining when it cannot
    /// be taken back.
    ///
    /// Bytes the caller composed itself (a multi-line body, or one with no
    /// terminator) go through [`WindowEntry::submit_raw_gated`] instead;
    /// interrupt control bytes and `send_keys` are written without the lock.
    ///
    /// # Errors
    ///
    /// Propagates [`crate::pty::PtyError`] from any write this call performs;
    /// [`crate::pty::PtyError::ChildExited`] when the child exits before or
    /// while this call waits or writes. A write this judgment declines to
    /// make is never an error: it is reported as
    /// [`SubmitPlacement::NotPlaced`], with [`NotPlacedReason::NoComposer`]
    /// before anything is read when this window's definition declares no
    /// composer or names no probe.
    pub async fn submit_message(
        &self,
        body: &str,
        residue: Option<ComposerResidue<'_>>,
        requester: Option<watch::Receiver<bool>>,
    ) -> PtyResult<SubmitJudgment> {
        let mut requester = Requester(requester);
        let Some(_guard) = self.lock_input(&mut requester).await else {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        };
        let _scope = self.begin_take_back_scope();
        let ComposerAccess::Judged { probe } = self.composer_access else {
            return Ok(declined(NotPlacedReason::NoComposer, None));
        };
        let judgment = self
            .judge_submit(body, probe, residue, &mut requester)
            .await?;
        self.check_after_another_write(judgment, probe, &mut requester)
            .await
    }

    /// [`WindowEntry::submit_message`]'s judgment, before what another
    /// writer's write leaves is checked.
    async fn judge_submit(
        &self,
        body: &str,
        probe: char,
        residue: Option<ComposerResidue<'_>>,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        // The ledger as of the reading every write below is decided on: read
        // before asking for it, so a write landing meanwhile is caught by the
        // first ledger-checked write.
        let mut seen = self.write_seq();
        let Some(mut observation) = self.observe_composer(requester).await? else {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        };
        let vouched_seq = residue
            .filter(|residue| self.record_vouches_for_residue(Some(*residue), body))
            .map(|residue| residue.write_seq);
        if let (ComposerReadout::Empty, Some(vouched_seq)) = (&observation.readout, vouched_seq) {
            if self.degenerate_and_unchanged() {
                return Ok(declined(NotPlacedReason::Unobservable, None));
            }
            return self
                .confirm_unshown_envelope(body, probe, &observation, vouched_seq, requester)
                .await;
        } else if matches!(observation.readout, ComposerReadout::NotObserved)
            && vouched_seq.is_none()
        {
            // A screen caught between erasing and repainting its composer
            // reads as unobservable for a moment; nothing is decided on that.
            let readable = |observation: &ComposerObservation| {
                (!matches!(observation.readout, ComposerReadout::NotObserved))
                    .then(|| observation.clone())
            };
            let guarded = self.write_seq();
            match self
                .wait_for_composer(Some(guarded), requester, readable)
                .await
            {
                ComposerWait::Done(readable) => {
                    observation = readable;
                    seen = guarded;
                }
                ComposerWait::OtherWriter => {
                    return Ok(declined(NotPlacedReason::Interrupted, None));
                }
                ComposerWait::RequesterGone => {
                    return Ok(declined(NotPlacedReason::Unobservable, None));
                }
                ComposerWait::Exited => return Err(crate::pty::PtyError::ChildExited),
            }
        }
        match &observation.readout {
            ComposerReadout::Empty => {
                // An unconfirmed write may still be on its way: anything
                // written now, a probe included, would land on top of it.
                if self.degenerate_and_unchanged() {
                    return Ok(declined(NotPlacedReason::Unobservable, None));
                }
                self.confirm_empty_then_write(body, probe, &observation, seen, requester)
                    .await
            }
            ComposerReadout::Body(text) => {
                match self
                    .settle_unanswered_probe(text, probe, observation.screen_rule)
                    .await?
                {
                    Some(TakenBack::Emptied(settled_rule, seq)) => {
                        return self
                            .write_fresh_and_decide(body, probe, settled_rule, seq, requester)
                            .await;
                    }
                    Some(TakenBack::Declined(judgment)) => return Ok(judgment),
                    None => {}
                }
                // The caller's own envelope is read back off the screen when it
                // can be; otherwise the write ledger is asked instead. A screen
                // that does not restore to `residue` is not by itself someone
                // else's content: a long body folds into a placeholder or runs
                // past the viewport, and only the ledger tells those apart
                // from a foreign body ("cannot be restored"). The
                // comparison against the screen ignores whitespace: a wrapped
                // body's trailing space at its last wrap boundary is lost by `read_composer` itself, not
                // by anything this call wrote.
                let restored_own_envelope = residue.is_some_and(|residue| {
                    composer_text_matches_ignoring_lost_whitespace(text, residue.body)
                        && residue.body == body
                });
                if restored_own_envelope {
                    self.note_read_back(text, body);
                }
                let vouched_seq = residue
                    .filter(|residue| self.record_vouches_for_residue(Some(*residue), body))
                    .map(|residue| residue.write_seq);
                if restored_own_envelope || vouched_seq.is_some() {
                    // A dialog's option row can read exactly like a composer
                    // holding other text (an approval screen's `1. Yes`, most
                    // notably), so the ledger match above cannot by itself
                    // tell that apart from the caller's own envelope. The
                    // screen rule can: it names the dialog, not the row's
                    // shape.
                    if !screen_accepts_write(observation.screen_rule) {
                        return Ok(decline_not_accepting(residue.map(|r| r.write_seq)));
                    }
                    let Some(vouched_seq) = vouched_seq else {
                        // Someone wrote since the caller's baseline, so the
                        // envelope on screen may have unshown input behind it.
                        return self
                            .confirm_envelope_then_resend(
                                body,
                                probe,
                                &observation.readout,
                                seen,
                                requester,
                            )
                            .await;
                    };
                    self.resend_key_only(body, vouched_seq, requester).await
                } else if let Some(residue) =
                    // The ledger alone vouches the composer holds only the
                    // caller's own text, however the CLI draws it (folded or
                    // not); the probe after the take-back confirms it emptied.
                    residue.filter(|residue| residue.write_seq == self.write_seq())
                {
                    self.take_back_own_residue_then_write(
                        body,
                        probe,
                        residue,
                        &observation,
                        requester,
                    )
                    .await
                } else if !self.degenerate_and_unchanged() {
                    // What reads as someone else's text may only be a screen
                    // that has not caught up with input that emptied it.
                    self.confirm_empty_then_write(body, probe, &observation, seen, requester)
                        .await
                } else {
                    Ok(foreign_draft(declined(NotPlacedReason::Foreign, None)))
                }
            }
            ComposerReadout::NotObserved | ComposerReadout::Tail(_) => match vouched_seq {
                Some(vouched_seq) => {
                    if !screen_accepts_write(observation.screen_rule) {
                        return Ok(decline_not_accepting(Some(vouched_seq)));
                    }
                    self.resend_key_only(body, vouched_seq, requester).await
                }
                None => Ok(declined(NotPlacedReason::Unobservable, None)),
            },
        }
    }

    /// Once another writer's write has ended the call, goes back to the
    /// screen before answering: the receiver may have dropped the take-back
    /// put in front of that write (a dialog, or input dropped right after a
    /// paste), and nobody else will clean up what is left. What the call
    /// itself wrote decides the check, never the PTY's registration.
    async fn check_after_another_write(
        &self,
        judgment: SubmitJudgment,
        probe: char,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        let SubmitPlacement::NotPlaced(NotPlaced {
            reason: NotPlacedReason::Interrupted,
            ..
        }) = judgment.placement
        else {
            return Ok(judgment);
        };
        let own = std::mem::take(&mut *lock_recover(&self.own_part));
        match &own.left {
            OwnLeft::Nothing => Ok(judgment),
            // A requester already gone is not waited for.
            OwnLeft::Text(_) if requester.has_left() => Ok(remaining(judgment)),
            OwnLeft::Text(text) => Ok(
                match self.check_own_text(text, &own, probe, requester).await? {
                    Checked::Gone => declined(NotPlacedReason::Interrupted, None),
                    Checked::MayRemain => remaining(judgment),
                },
            ),
            OwnLeft::UnconfirmedProbe(before_probe) => {
                if !requester.has_left() {
                    self.check_unconfirmed_probe(before_probe, probe, requester)
                        .await?;
                }
                Ok(judgment)
            }
        }
    }

    /// The first reading of a screen that accepts a write, with the ledger
    /// count it was guarded at. There is no time limit: `None` once another
    /// writer writes or `requester` leaves first.
    async fn read_writable_screen(
        &self,
        requester: &mut Requester,
    ) -> PtyResult<Option<(ComposerReadout, u64)>> {
        let seen = self.write_seq();
        match self
            .wait_for_composer(Some(seen), requester, |observation| {
                screen_accepts_write(observation.screen_rule).then(|| observation.readout.clone())
            })
            .await
        {
            ComposerWait::Done(readout) => Ok(Some((readout, seen))),
            ComposerWait::OtherWriter | ComposerWait::RequesterGone => Ok(None),
            ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// Checks what is left of `own`, the call's own text, once another
    /// writer's write ended the call. A probe is written only when the
    /// screen shows `own` alone: anything beside it may be the other
    /// writer's, still typing, and a probe would land in the middle of that.
    /// What gone means for a reading without `own` depends on `read_back`: a
    /// body never read back may simply not be drawn yet.
    ///
    /// When the only write since the call's own was the interrupt keys, which
    /// put no text in the composer, whatever the composer holds is the call's
    /// own, even drawn otherwise (a folded paste), so it is taken back.
    async fn check_own_text(
        &self,
        own: &str,
        part: &OwnPart,
        probe: char,
        requester: &mut Requester,
    ) -> PtyResult<Checked> {
        let gone_if_read_back = if part.read_back {
            Checked::Gone
        } else {
            Checked::MayRemain
        };
        let Some((reading, seen)) = self.read_writable_screen(requester).await? else {
            return Ok(Checked::MayRemain);
        };
        let shows = match &reading {
            ComposerReadout::Empty => OwnShows::Absent,
            ComposerReadout::NotObserved | ComposerReadout::Tail(_) => {
                return Ok(Checked::MayRemain);
            }
            ComposerReadout::Body(text) => own_text_shows(text, own, probe),
        };
        let only_interrupt_keys_since = part.after_own_write.map(|after| after.wrapping_add(1))
            == Some(seen)
            && self.session.latest_withheld_write() == Some(seen);
        if only_interrupt_keys_since
            && matches!(reading, ComposerReadout::Body(_))
            && matches!(shows, OwnShows::Absent | OwnShows::WithOthers)
        {
            return self.take_back_own(own, probe, seen, &reading).await;
        }
        match shows {
            OwnShows::Absent => return Ok(gone_if_read_back),
            OwnShows::WithOthers => return Ok(Checked::MayRemain),
            OwnShows::Alone => {}
        }
        let ProbeReading::Shown { before } = self
            .read_through_probe(probe, seen, &reading, requester)
            .await?
        else {
            return Ok(Checked::MayRemain);
        };
        let after_probe = seen + 1;
        if before.chars().all(char::is_whitespace) {
            self.erase_probe_behind(after_probe, probe, &before).await?;
            return Ok(Checked::Gone);
        }
        match own_text_shows(&before, own, probe) {
            OwnShows::Alone => {
                let written = format!("{own}{probe}");
                let shown = ComposerReadout::Body(format!("{before}{probe}"));
                self.take_back_own(&written, probe, after_probe, &shown)
                    .await
            }
            OwnShows::WithOthers => {
                self.erase_probe_behind(after_probe, probe, &before).await?;
                Ok(Checked::MayRemain)
            }
            OwnShows::Absent => {
                self.erase_probe_behind(after_probe, probe, &before).await?;
                Ok(gone_if_read_back)
            }
        }
    }

    /// [`WindowEntry::take_back_and_settle`] of the call's own `written`,
    /// as what the check found of it.
    async fn take_back_own(
        &self,
        written: &str,
        probe: char,
        seq_after_written: u64,
        before_take_back: &ComposerReadout,
    ) -> PtyResult<Checked> {
        Ok(
            match self
                .take_back_and_settle(written, probe, seq_after_written, before_take_back)
                .await?
            {
                TakenBack::Emptied(..) => Checked::Gone,
                TakenBack::Declined(judgment) if leaves_envelope(&judgment) => Checked::MayRemain,
                TakenBack::Declined(_) => Checked::Gone,
            },
        )
    }

    /// Once another writer's write ended the call while its probe, written
    /// behind a composer reading as `before_probe`, had not shown: erases
    /// that probe when the screen shows it alone behind `before_probe`, and
    /// writes nothing otherwise (it may sit among the other writer's text).
    async fn check_unconfirmed_probe(
        &self,
        before_probe: &ComposerReadout,
        probe: char,
        requester: &mut Requester,
    ) -> PtyResult<()> {
        let before = match before_probe {
            ComposerReadout::Empty => String::new(),
            ComposerReadout::Body(text) => text.clone(),
            ComposerReadout::NotObserved | ComposerReadout::Tail(_) => return Ok(()),
        };
        let probed = format!("{before}{probe}");
        let Some((reading, seen)) = self.read_writable_screen(requester).await? else {
            return Ok(());
        };
        if !composer_reads_back_as(&reading, &probed) {
            return Ok(());
        }
        let ProbeReading::Shown { before: shown } = self
            .read_through_probe(probe, seen, &reading, requester)
            .await?
        else {
            return Ok(());
        };
        let after_check = seen + 1;
        if composer_text_matches_ignoring_lost_whitespace(&shown, &probed) {
            if self.erase_probe_behind(after_check, probe, &probed).await? == GuardedWrite::Written
            {
                self.erase_probe_behind(after_check + 1, probe, &before)
                    .await?;
            }
        } else {
            self.erase_probe_behind(after_check, probe, &shown).await?;
        }
        Ok(())
    }

    /// Takes this window's input lock, unless `requester` leaves first.
    async fn lock_input(&self, requester: &mut Requester) -> Option<AsyncMutexGuard<'_, ()>> {
        if requester.has_left() {
            return None;
        }
        tokio::select! {
            biased;
            () = requester.left() => None,
            guard = self.input_lock.lock() => Some(guard),
        }
    }

    /// Asks this window's detector for the composer's content, and the rule
    /// verdict on the same screen, as they stand right now (see
    /// [`DetectorHandle::query_composer`]). The fail-safe, gate-closed answer
    /// once this window has exited and its detector has been taken.
    async fn query_composer(&self) -> ComposerObservation {
        let detector = lock_recover(&self.detector).clone();
        match detector {
            Some(detector) => {
                let mut observation = detector.query_composer().await;
                // No decision here can tell a tail from the caller's own body
                // yet, so a tail is held to what any unreadable composer is.
                if let ComposerReadout::Tail(_) = observation.readout {
                    observation.readout = ComposerReadout::NotObserved;
                }
                observation
            }
            None => ComposerObservation::closed_unobserved(),
        }
    }

    /// One reading of the composer, taken through the same wait every other
    /// reading uses so a detector that does not answer never outlasts the
    /// child or the requester. `None` once the requester has left.
    async fn observe_composer(
        &self,
        requester: &mut Requester,
    ) -> PtyResult<Option<ComposerObservation>> {
        match self
            .wait_for_composer(None, requester, |observation| Some(observation.clone()))
            .await
        {
            ComposerWait::Done(observation) => Ok(Some(observation)),
            // With no ledger count to compare against, no other writer ends
            // this wait.
            ComposerWait::RequesterGone | ComposerWait::OtherWriter => Ok(None),
            ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// Reads the composer until `settle` accepts a reading, or something ends
    /// the wait without one: the child exiting, `requester` leaving, or --
    /// when `own_seq` is given -- another writer moving the ledger past it.
    /// When more than one holds at once they rank in that order, ahead of any
    /// reading. Output reaching the screen and a status change are cues to
    /// read again and nothing more; there is no time limit, and this relies
    /// on the detector answering a query it has taken up (see
    /// [`DetectorHandle::query_composer`]).
    async fn wait_for_composer<T>(
        &self,
        own_seq: Option<u64>,
        requester: &mut Requester,
        mut settle: impl FnMut(&ComposerObservation) -> Option<T>,
    ) -> ComposerWait<T> {
        let mut exit = self.session.exit_notified();
        let mut writes = self.session.subscribe_writes_recorded();
        let mut arrivals = self.session.output_arrivals();
        let mut status = self.watch_status();
        loop {
            // Seen before the query, so a change while it runs is a cue.
            arrivals.borrow_and_update();
            status.borrow_and_update();
            if let Some(end) = self.wait_ended(&exit, requester, own_seq) {
                return end;
            }
            let observation = tokio::select! {
                biased;
                () = child_exited(&mut exit) => return ComposerWait::Exited,
                () = requester.left() => return ComposerWait::RequesterGone,
                () = ledger_moved(&mut writes, own_seq) => return ComposerWait::OtherWriter,
                observation = self.query_composer() => observation,
            };
            if let Some(end) = self.wait_ended(&exit, requester, own_seq) {
                return end;
            }
            if let Some(settled) = settle(&observation) {
                return ComposerWait::Done(settled);
            }
            tokio::select! {
                biased;
                () = child_exited(&mut exit) => return ComposerWait::Exited,
                () = requester.left() => return ComposerWait::RequesterGone,
                () = ledger_moved(&mut writes, own_seq) => return ComposerWait::OtherWriter,
                () = next_change(&mut arrivals) => {}
                () = next_change(&mut status) => {}
            }
        }
    }

    /// Resolves once the composer reads empty on a screen that accepts a
    /// write, or the child has exited. Writes by others do not end it: a
    /// draft being typed moves the ledger on every key and is still a draft.
    /// Output reaching the screen is what prompts each new reading, so an
    /// emptying whose redraw lags is still caught.
    pub(crate) async fn wait_for_composer_empty(&self) {
        match self
            .wait_for_composer(None, &mut Requester::none(), |observation| {
                (screen_accepts_write(observation.screen_rule)
                    && observation.readout == ComposerReadout::Empty)
                    .then_some(())
            })
            .await
        {
            // The caller's own loop sees an exited child.
            ComposerWait::Done(()) | ComposerWait::Exited => {}
            // Neither can end a wait with no ledger count and no requester.
            ComposerWait::OtherWriter | ComposerWait::RequesterGone => {}
        }
    }

    /// What ends [`WindowEntry::wait_for_composer`] as things stand, in its
    /// order of rank.
    fn wait_ended<T>(
        &self,
        exit: &watch::Receiver<bool>,
        requester: &Requester,
        own_seq: Option<u64>,
    ) -> Option<ComposerWait<T>> {
        if *exit.borrow() {
            Some(ComposerWait::Exited)
        } else if requester.has_left() {
            Some(ComposerWait::RequesterGone)
        } else if own_seq.is_some_and(|seq| self.write_seq() != seq) {
            Some(ComposerWait::OtherWriter)
        } else {
            None
        }
    }

    /// [`WindowEntry::wait_for_composer`] for what cleans up after this
    /// call's own writes, which no requester stops. `Ok(None)` is another
    /// writer.
    async fn wait_for_composer_unwatched<T>(
        &self,
        own_seq: u64,
        settle: impl FnMut(&ComposerObservation) -> Option<T>,
    ) -> PtyResult<Option<T>> {
        match self
            .wait_for_composer(Some(own_seq), &mut Requester::none(), settle)
            .await
        {
            ComposerWait::Done(settled) => Ok(Some(settled)),
            // No requester was given, so it never leaves.
            ComposerWait::OtherWriter | ComposerWait::RequesterGone => Ok(None),
            ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// `observed` may not show input written before it yet. Writes the probe
    /// behind that input and writes `body` only once the probe shows alone
    /// and is erased; a probe that shows behind anything else means someone's
    /// text is there, and only the probe is erased. Nothing of `body` is
    /// written on any declining path.
    async fn confirm_empty_then_write(
        &self,
        body: &str,
        probe: char,
        observed: &ComposerObservation,
        seq: u64,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if !screen_accepts_write(observed.screen_rule) {
            return Ok(decline_not_accepting(None));
        }
        if *lock_recover(&self.unanswered_probe) == Some(seq) {
            // An earlier probe is still on its way: a second one behind it
            // would leave one of them in the composer.
            let shown_before = |observation: &ComposerObservation| match &observation.readout {
                ComposerReadout::Body(text) if screen_accepts_write(observation.screen_rule) => {
                    text.strip_suffix(probe).map(str::to_string)
                }
                ComposerReadout::Body(_)
                | ComposerReadout::Empty
                | ComposerReadout::NotObserved
                | ComposerReadout::Tail(_) => None,
            };
            return match self
                .wait_for_composer(Some(seq), requester, shown_before)
                .await
            {
                ComposerWait::Done(before) if before.is_empty() => {
                    self.write_once_probe_erased(body, probe, seq, requester)
                        .await
                }
                ComposerWait::Done(before) => {
                    self.erase_probe_behind(seq, probe, &before).await?;
                    Ok(foreign_draft(declined(NotPlacedReason::Foreign, None)))
                }
                ComposerWait::OtherWriter => Ok(declined(NotPlacedReason::Interrupted, None)),
                ComposerWait::RequesterGone => Ok(declined(NotPlacedReason::Unobservable, None)),
                ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
            };
        }
        match self
            .read_through_probe(probe, seq, &observed.readout, requester)
            .await?
        {
            ProbeReading::Shown { before } if before.is_empty() => {
                self.write_once_probe_erased(body, probe, seq + 1, requester)
                    .await
            }
            ProbeReading::Shown { before } => {
                self.erase_probe_behind(seq + 1, probe, &before).await?;
                Ok(foreign_draft(declined(NotPlacedReason::Foreign, None)))
            }
            ProbeReading::Interrupted => Ok(declined(NotPlacedReason::Interrupted, None)),
            ProbeReading::Abandoned => Ok(declined(NotPlacedReason::Unobservable, None)),
        }
    }

    /// The ledger vouches that only the caller's own envelope was written
    /// since its baseline, yet the composer reads empty: the envelope is not
    /// drawn yet, or the receiver has already taken or dropped it. The probe
    /// written behind it tells which, in the order the receiver handles its
    /// input: shown behind something (or past what can be read), the
    /// envelope is there and the submit key alone is pressed; shown alone,
    /// it is gone and `body` is written fresh; nothing shown, the call waits.
    async fn confirm_unshown_envelope(
        &self,
        body: &str,
        probe: char,
        observed: &ComposerObservation,
        seq: u64,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if !screen_accepts_write(observed.screen_rule) {
            return Ok(decline_not_accepting(Some(seq)));
        }
        if requester.has_left() {
            return Ok(declined(NotPlacedReason::Unobservable, Some(seq)));
        }
        let after_probe = seq + 1;
        let envelope_and_probe = self.own_text(&format!("{body}{probe}"));
        if self
            .write_judged(probe.to_string().as_bytes(), seq, envelope_and_probe)
            .await?
            == GuardedWrite::Moved
        {
            // The earlier attempt's envelope stays, carried forward as it was.
            return Ok(declined(NotPlacedReason::Interrupted, Some(seq)));
        }
        let shown = |observation: &ComposerObservation| {
            if screen_accepts_write(observation.screen_rule) {
                reflection_of(&observation.readout, body, probe)
            } else {
                None
            }
        };
        match self
            .wait_for_composer(Some(after_probe), requester, shown)
            .await
        {
            ComposerWait::Done(Reflection::Behind(before)) => {
                self.note_read_back(&before, body);
                self.erase_probe_then_resend(body, &before, after_probe, requester)
                    .await
            }
            ComposerWait::Done(Reflection::Unreadable) => {
                // Handled in order, so the erase reaches the probe ahead of
                // the submit key whether or not it shows.
                match self
                    .write_judged(&self.input.erase, after_probe, self.own_text(body))
                    .await?
                {
                    GuardedWrite::Written => {
                        self.resend_key_only(body, after_probe + 1, requester).await
                    }
                    GuardedWrite::Moved => {
                        Ok(declined(NotPlacedReason::Interrupted, Some(after_probe)))
                    }
                }
            }
            ComposerWait::Done(Reflection::Dropped) => {
                self.write_once_probe_erased(body, probe, after_probe, requester)
                    .await
            }
            ComposerWait::OtherWriter => Ok(declined_unvouched(NotPlacedReason::Interrupted)),
            ComposerWait::RequesterGone => {
                // Whether or not another write got in first, the requester is
                // gone and nothing more is decided here.
                self.write_judged(&self.input.erase, after_probe, self.own_text(body))
                    .await?;
                Ok(declined_unvouched(NotPlacedReason::Unobservable))
            }
            ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// Erases a probe that shows alone (ledger at `seq`) and writes `body`
    /// once the composer reads empty.
    async fn write_once_probe_erased(
        &self,
        body: &str,
        probe: char,
        seq: u64,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        match self.erase_probe(seq).await? {
            ProbeErased::Emptied(settled_rule, after_erase) => {
                self.write_fresh_and_decide(body, probe, settled_rule, after_erase, requester)
                    .await
            }
            ProbeErased::Interrupted(_) => Ok(declined(NotPlacedReason::Interrupted, None)),
        }
    }

    /// The composer shows the caller's own envelope, but someone wrote since
    /// the caller's baseline. Writes the probe behind it and presses the
    /// submit key only once what precedes the probe is the envelope and,
    /// with the probe erased, the composer reads as the envelope again.
    async fn confirm_envelope_then_resend(
        &self,
        body: &str,
        probe: char,
        observed: &ComposerReadout,
        seq: u64,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        match self
            .read_through_probe(probe, seq, observed, requester)
            .await?
        {
            ProbeReading::Shown { before }
                if composer_text_matches_ignoring_lost_whitespace(&before, body) =>
            {
                self.note_read_back(&before, body);
                self.erase_probe_then_resend(body, &before, seq + 1, requester)
                    .await
            }
            ProbeReading::Shown { before } => {
                self.erase_probe_behind(seq + 1, probe, &before).await?;
                Ok(foreign_draft(declined_unvouched(NotPlacedReason::Foreign)))
            }
            ProbeReading::Interrupted => Ok(declined_unvouched(NotPlacedReason::Interrupted)),
            ProbeReading::Abandoned => Ok(declined_unvouched(NotPlacedReason::Unobservable)),
        }
    }

    /// Erases a probe that showed right behind the caller's own envelope,
    /// reading as `before` (ledger at `seq`), and presses the submit key
    /// alone once the composer reads as `before` again.
    async fn erase_probe_then_resend(
        &self,
        body: &str,
        before: &str,
        seq: u64,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        match self
            .erase_probe_back_to(seq, before, body, requester)
            .await?
        {
            Some(ComposerWait::Done(())) => self.resend_key_only(body, seq + 1, requester).await,
            None | Some(ComposerWait::OtherWriter) => {
                Ok(declined_unvouched(NotPlacedReason::Interrupted))
            }
            Some(ComposerWait::RequesterGone) => {
                Ok(declined_unvouched(NotPlacedReason::Unobservable))
            }
            Some(ComposerWait::Exited) => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// Erases a probe that showed behind `before` (ledger at `seq`), and reads
    /// until the erase is reached -- the composer holds `before` again, or
    /// something that no longer ends with the probe -- or another writer
    /// moves the ledger, so the next reader sees what was there before the
    /// probe. [`GuardedWrite::Moved`] when the erase itself was not written.
    async fn erase_probe_behind(
        &self,
        seq: u64,
        probe: char,
        before: &str,
    ) -> PtyResult<GuardedWrite> {
        if self
            .write_judged(&self.input.erase, seq, TakeBackRegistration::Empty)
            .await?
            == GuardedWrite::Moved
        {
            return Ok(GuardedWrite::Moved);
        }
        self.wait_for_composer_unwatched(seq + 1, |observation| {
            let erased = match &observation.readout {
                ComposerReadout::Body(text) => !text.ends_with(probe),
                ComposerReadout::Empty
                | ComposerReadout::NotObserved
                | ComposerReadout::Tail(_) => true,
            };
            (erased || composer_reads_back_as(&observation.readout, before)).then_some(())
        })
        .await?;
        Ok(GuardedWrite::Written)
    }

    /// Writes the probe behind everything written so far (ledger at `seq`)
    /// and reads until the composer has changed from `before_probe` and ends
    /// with the probe on a screen that accepts a write. Input is handled in
    /// order, so a probe that shows means everything before it has been
    /// handled. A requester that leaves first writes no probe, and one that
    /// leaves while it is on its way leaves it recorded as unanswered.
    async fn read_through_probe(
        &self,
        probe: char,
        seq: u64,
        before_probe: &ComposerReadout,
        requester: &mut Requester,
    ) -> PtyResult<ProbeReading> {
        if requester.has_left() {
            return Ok(ProbeReading::Abandoned);
        }
        let probe_text = probe.to_string();
        let after_probe = seq + 1;
        // Nothing before this probe is confirmed as this call's own yet.
        if self
            .write_judged(probe_text.as_bytes(), seq, TakeBackRegistration::Empty)
            .await?
            == GuardedWrite::Moved
        {
            return Ok(ProbeReading::Interrupted);
        }
        lock_recover(&self.own_part).left = OwnLeft::UnconfirmedProbe(before_probe.clone());
        let shown_before = |observation: &ComposerObservation| match &observation.readout {
            ComposerReadout::Body(text)
                if &observation.readout != before_probe
                    && screen_accepts_write(observation.screen_rule) =>
            {
                text.strip_suffix(probe).map(str::to_string)
            }
            ComposerReadout::Body(_)
            | ComposerReadout::Empty
            | ComposerReadout::NotObserved
            | ComposerReadout::Tail(_) => None,
        };
        match self
            .wait_for_composer(Some(after_probe), requester, shown_before)
            .await
        {
            ComposerWait::Done(before) => Ok(ProbeReading::Shown { before }),
            ComposerWait::OtherWriter => Ok(ProbeReading::Interrupted),
            ComposerWait::RequesterGone => {
                // Still on its way: the next call settles it by this count.
                *lock_recover(&self.unanswered_probe) = Some(after_probe);
                Ok(ProbeReading::Abandoned)
            }
            ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// Whether the write ledger alone vouches for
    /// re-sending `residue` -- the caller brought the same text it wants to
    /// write, and not one write has landed in this window since the count it
    /// brought with it. Every write, from every writer, moves that count, so
    /// a match means the composer holds what this caller left there and
    /// nothing else; another writer's bytes cannot pass as the caller's own
    /// by being identical. This is why what the screen shows may be
    /// disregarded when it holds: the single decision point behind both
    /// readings that cannot restore the caller's envelope from the screen (a
    /// region that reads as something else, and one that cannot be read at
    /// all).
    fn record_vouches_for_residue(&self, residue: Option<ComposerResidue<'_>>, body: &str) -> bool {
        residue.is_some_and(|residue| residue.body == body && residue.write_seq == self.write_seq())
    }

    /// Whether this window's degenerate-empty marker is still active: a write
    /// that could not be confirmed and could not be taken back leaves this
    /// set to the sequence number at that instant, and it stays active only
    /// while no further write has moved the sequence past this value.
    fn degenerate_and_unchanged(&self) -> bool {
        match *lock_recover(&self.degenerate_since) {
            Some(seq) => seq == self.write_seq(),
            None => false,
        }
    }

    /// Writes the submit key alone: the caller has already established that
    /// its own envelope is the only thing in the composer, so the body is not
    /// rewritten. The write gate itself was already checked against the
    /// reading that decided this. A requester that has left gets nothing
    /// written, and the envelope stays where it is.
    async fn resend_key_only(
        &self,
        body: &str,
        seq: u64,
        requester: &Requester,
    ) -> PtyResult<SubmitJudgment> {
        if requester.has_left() {
            return Ok(declined(NotPlacedReason::Unobservable, Some(seq)));
        }
        let plan = plan_submit_line(&self.input, body, SubmitAction::ResendOnly);
        let last_index = plan.steps.len().saturating_sub(1);
        let own_writes = plan.steps.len() as u64;
        // The submit key closes the composer's content out of this call's
        // hands, so its write leaves nothing to take back.
        let registration = |index: usize| {
            if index == last_index {
                TakeBackRegistration::Empty
            } else {
                TakeBackRegistration::Keep
            }
        };
        match self.run_judged_steps(&plan, seq, registration).await? {
            JudgedSteps::Written(outcome) => Ok(placed(outcome, seq + own_writes)),
            JudgedSteps::Moved { at } => Ok(declined(NotPlacedReason::Interrupted, Some(at))),
        }
    }

    /// Writes `body` fresh into a composer just observed empty, then the
    /// probe behind it, and decides from what the composer shows once the
    /// probe does (see [`Reflection`]): the body landed, so the probe is
    /// erased and the submit key pressed once the composer reads as it did
    /// before the probe; the body was dropped, so the probe is erased and
    /// nothing is left; or nothing shows yet, so the call waits.
    /// `screen_rule` is the rule verdict already read alongside the empty
    /// composer, so the write gate is checked before this call's very first
    /// byte rather than against a fresh reading of its own; `seq` is the
    /// ledger that empty reading was taken at.
    ///
    /// A screen that stops accepting a write once something showed (a dialog
    /// covering the composer) is waited out, never declined on. A write from
    /// another writer ends the call with the body reported as remaining: the
    /// body is registered as this call's own, so that writer's bytes go out
    /// behind its take-back, and nothing more of this plan is written after
    /// them ([`WindowEntry::submit_message`] then checks what is left).
    /// A requester that leaves once the body is written gets no submit key;
    /// the body is taken back instead (see
    /// [`WindowEntry::take_back_for_departed_requester`]).
    async fn write_fresh_and_decide(
        &self,
        body: &str,
        probe: char,
        screen_rule: Option<DetectedStatus>,
        seq: u64,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if !screen_accepts_write(screen_rule) {
            return Ok(decline_not_accepting(None));
        }
        if requester.has_left() {
            return Ok(declined(NotPlacedReason::Unobservable, None));
        }

        let plan = plan_submit_line(&self.input, body, SubmitAction::Write);
        let mut steps = plan.steps.into_iter();
        let (Some(body_step), Some(submit_step)) = (steps.next(), steps.next()) else {
            // Structurally unreachable: `SubmitAction::Write` always builds a
            // body step followed by a submit step. Fail closed rather than
            // write nothing observably and claim success.
            return Ok(declined(NotPlacedReason::Unobservable, None));
        };

        // What the ledger reads as once each of this call's own writes has
        // gone out: the baseline a follow-up attempt brings back.
        let after_body = seq + 1;
        if self
            .write_judged(&body_step.bytes, seq, self.own_text(body))
            .await?
            == GuardedWrite::Moved
        {
            return Ok(declined(NotPlacedReason::Interrupted, None));
        }
        if !body_step.delay_after.is_zero() {
            tokio::time::sleep(body_step.delay_after).await;
        }
        if self.write_seq() != after_body {
            return Ok(declined(NotPlacedReason::Interrupted, Some(after_body)));
        }
        if requester.has_left() {
            return self
                .take_back_for_departed_requester(body, probe, after_body)
                .await;
        }

        let after_probe = after_body + 1;
        if self
            .write_judged(
                probe.to_string().as_bytes(),
                after_body,
                self.own_text(&format!("{body}{probe}")),
            )
            .await?
            == GuardedWrite::Moved
        {
            return Ok(declined(NotPlacedReason::Interrupted, Some(after_body)));
        }
        match self
            .wait_for_reflection(body, probe, after_probe, requester)
            .await
        {
            ComposerWait::Done(Reflection::Behind(before)) => {
                self.erase_probe_then_submit(
                    body,
                    probe,
                    &before,
                    &submit_step,
                    after_probe,
                    requester,
                )
                .await
            }
            ComposerWait::Done(Reflection::Unreadable) => {
                self.erase_unread_probe_then_submit(
                    body,
                    probe,
                    &submit_step,
                    after_probe,
                    requester,
                )
                .await
            }
            ComposerWait::Done(Reflection::Dropped) => self.erase_dropped_probe(after_probe).await,
            ComposerWait::OtherWriter => {
                Ok(declined(NotPlacedReason::Interrupted, Some(after_probe)))
            }
            ComposerWait::RequesterGone => {
                self.take_back_for_departed_requester(&format!("{body}{probe}"), probe, after_probe)
                    .await
            }
            ComposerWait::Exited => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// [`WindowEntry::wait_for_composer`] for what a fresh `body` and the
    /// probe behind it (ledger at `seq`) leave in the composer, on a screen
    /// that accepts a write.
    async fn wait_for_reflection(
        &self,
        body: &str,
        probe: char,
        seq: u64,
        requester: &mut Requester,
    ) -> ComposerWait<Reflection> {
        let started = Instant::now();
        let reflection = self
            .wait_for_composer(Some(seq), requester, |observation| {
                if screen_accepts_write(observation.screen_rule) {
                    reflection_of(&observation.readout, body, probe)
                } else {
                    None
                }
            })
            .await;
        let (outcome, reflected) = match &reflection {
            ComposerWait::Done(Reflection::Behind(before)) => {
                self.note_read_back(before, body);
                (
                    "behind",
                    composer_text_matches_ignoring_lost_whitespace(before, body),
                )
            }
            ComposerWait::Done(Reflection::Dropped) => ("dropped", false),
            ComposerWait::Done(Reflection::Unreadable) => ("unreadable", false),
            ComposerWait::OtherWriter => ("other_writer", false),
            ComposerWait::RequesterGone => ("requester_gone", false),
            ComposerWait::Exited => ("exited", false),
        };
        // Whether what shows before the probe matches the body this call
        // wrote is not part of the decision, but is worth seeing when
        // debugging a delivery.
        tracing::debug!(
            window_id = %self.window_id(),
            body_bytes = body.len(),
            elapsed_ms = started.elapsed().as_millis(),
            outcome,
            reflected,
            "composer reflection observed"
        );
        reflection
    }

    /// Erases a probe that showed behind `before` (ledger at `seq`) and waits
    /// until the composer reads as `before` again, on a screen that accepts a
    /// write. A reading that cannot be restored is still only this call's own
    /// writes, the same as a body read back as unreadable. `own` is this
    /// call's text the composer holds once the probe is erased. `None` when
    /// another write got in before the erase, which is then not written.
    async fn erase_probe_back_to(
        &self,
        seq: u64,
        before: &str,
        own: &str,
        requester: &mut Requester,
    ) -> PtyResult<Option<ComposerWait<()>>> {
        if self
            .write_judged(&self.input.erase, seq, self.own_text(own))
            .await?
            == GuardedWrite::Moved
        {
            return Ok(None);
        }
        let reads_as_before = |observation: &ComposerObservation| {
            (screen_accepts_write(observation.screen_rule)
                && (composer_reads_back_as(&observation.readout, before)
                    || matches!(observation.readout, ComposerReadout::NotObserved)))
            .then_some(())
        };
        Ok(Some(
            self.wait_for_composer(Some(seq + 1), requester, reads_as_before)
                .await,
        ))
    }

    /// The body landed and the probe shows behind it, reading as `before`
    /// (ledger at `seq`). Erases the probe and presses the submit key once
    /// the composer reads as `before` again.
    async fn erase_probe_then_submit(
        &self,
        body: &str,
        probe: char,
        before: &str,
        submit_step: &SubmitStep,
        seq: u64,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if requester.has_left() {
            return self
                .take_back_for_departed_requester(&format!("{body}{probe}"), probe, seq)
                .await;
        }
        let after_erase = seq + 1;
        match self
            .erase_probe_back_to(seq, before, body, requester)
            .await?
        {
            Some(ComposerWait::Done(())) => self.press_submit_key(submit_step, after_erase).await,
            None => Ok(declined(NotPlacedReason::Interrupted, Some(seq))),
            Some(ComposerWait::OtherWriter) => {
                Ok(declined(NotPlacedReason::Interrupted, Some(after_erase)))
            }
            Some(ComposerWait::RequesterGone) => {
                self.take_back_for_departed_requester(&format!("{body}{probe}"), probe, after_erase)
                    .await
            }
            Some(ComposerWait::Exited) => Err(crate::pty::PtyError::ChildExited),
        }
    }

    /// The body landed past what the composer can be read back as (ledger at
    /// `seq`, the probe written): erases the probe and presses the submit key
    /// without waiting to see either. Input is handled in order, so the erase
    /// reaches the probe ahead of the submit key whether or not it shows.
    async fn erase_unread_probe_then_submit(
        &self,
        body: &str,
        probe: char,
        submit_step: &SubmitStep,
        seq: u64,
        requester: &Requester,
    ) -> PtyResult<SubmitJudgment> {
        let written = format!("{body}{probe}");
        if requester.has_left() {
            return self
                .take_back_for_departed_requester(&written, probe, seq)
                .await;
        }
        let after_erase = seq + 1;
        if self
            .write_judged(&self.input.erase, seq, self.own_text(body))
            .await?
            == GuardedWrite::Moved
        {
            return Ok(declined(NotPlacedReason::Interrupted, Some(seq)));
        }
        if requester.has_left() {
            return self
                .take_back_for_departed_requester(&written, probe, after_erase)
                .await;
        }
        self.press_submit_key(submit_step, after_erase).await
    }

    /// The receiver dropped the body and shows the probe alone (ledger at
    /// `seq`): erases the probe and declines once the composer reads empty,
    /// nothing of this call's left behind.
    async fn erase_dropped_probe(&self, seq: u64) -> PtyResult<SubmitJudgment> {
        match self.erase_probe(seq).await? {
            ProbeErased::Emptied(..) => Ok(declined(NotPlacedReason::NotReflected, None)),
            ProbeErased::Interrupted(_) => Ok(declined_unvouched(NotPlacedReason::Interrupted)),
        }
    }

    /// The requester left once this call had written `written` into the
    /// composer, leaving the ledger at `seq`. Takes it back and answers once
    /// the composer is confirmed empty, however long the receiver takes,
    /// holding the window's turn meanwhile so nothing is written on top.
    /// Without a way to take it back, the body is reported as remaining and
    /// the degenerate-empty marker stops a later attempt from writing it a
    /// second time.
    async fn take_back_for_departed_requester(
        &self,
        written: &str,
        probe: char,
        seq: u64,
    ) -> PtyResult<SubmitJudgment> {
        if self.input.take_back() == TakeBack::Unavailable {
            *lock_recover(&self.degenerate_since) = Some(self.write_seq());
            return Ok(declined_unvouched(NotPlacedReason::NotReflected));
        }
        // The take-back is a write of its own, so it waits for a screen that
        // accepts one, the same as every other write.
        let Some(before_take_back) = self
            .wait_for_composer_unwatched(seq, |observation| {
                screen_accepts_write(observation.screen_rule).then(|| observation.readout.clone())
            })
            .await?
        else {
            return Ok(declined(NotPlacedReason::Interrupted, Some(seq)));
        };
        match self
            .take_back_and_settle(written, probe, seq, &before_take_back)
            .await?
        {
            TakenBack::Emptied(..) => Ok(declined(NotPlacedReason::NotReflected, None)),
            TakenBack::Declined(judgment) => Ok(judgment),
        }
    }

    /// The composer holds a leftover this caller's own record vouches for
    /// (`residue`), but the text it wants to send now differs from it. Takes
    /// the leftover back and, once the composer is confirmed empty, writes
    /// `body` as a fresh message; any other outcome leaves the leftover
    /// reported as remaining.
    async fn take_back_own_residue_then_write(
        &self,
        body: &str,
        probe: char,
        residue: ComposerResidue<'_>,
        observed: &ComposerObservation,
        requester: &mut Requester,
    ) -> PtyResult<SubmitJudgment> {
        if self.input.take_back() == TakeBack::Unavailable {
            return Ok(foreign_draft(declined(NotPlacedReason::Foreign, None)));
        }
        if !screen_accepts_write(observed.screen_rule) {
            return Ok(decline_not_accepting(Some(residue.write_seq)));
        }
        if requester.has_left() {
            return Ok(declined(
                NotPlacedReason::Unobservable,
                Some(residue.write_seq),
            ));
        }
        match self
            .take_back_and_settle(residue.body, probe, residue.write_seq, &observed.readout)
            .await?
        {
            TakenBack::Emptied(settled_rule, seq) => {
                self.write_fresh_and_decide(body, probe, settled_rule, seq, requester)
                    .await
            }
            TakenBack::Declined(judgment) => Ok(judgment),
        }
    }

    /// Writes the take-back of `written` (whose write left the ledger at
    /// `seq_after_written`, and the composer reading as `before_take_back`),
    /// then the profile's probe, and settles what the composer holds once the
    /// probe shows (see [`WindowEntry::settle_with_probe`]). A foreign write
    /// since is a declined judgment that is never taken back.
    async fn take_back_and_settle(
        &self,
        written: &str,
        probe: char,
        seq_after_written: u64,
        before_take_back: &ComposerReadout,
    ) -> PtyResult<TakenBack> {
        // Only a runtime shutting down leaves the size unread, and nothing can
        // be written then either.
        let (cols, _) = self
            .session
            .size()
            .await
            .ok_or(crate::pty::PtyError::WriteFailed)?;
        let take_back_plan = self.input.plan_take_back(written, cols);
        let after_take_back = seq_after_written + take_back_plan.steps.len() as u64;
        // Until the probe behind it shows, the take-back may not have reached
        // `written` (a receiver dropping input), so `written` stays registered.
        match self
            .run_judged_steps(&take_back_plan, seq_after_written, |_| {
                self.own_text(written)
            })
            .await?
        {
            JudgedSteps::Written(_) => {}
            JudgedSteps::Moved { at } => {
                return Ok(TakenBack::Declined(declined(
                    NotPlacedReason::Interrupted,
                    Some(at),
                )));
            }
        }
        tracing::debug!(
            window_id = %self.window_id(),
            take_back = ?self.input.take_back(),
            "composer take-back written"
        );
        self.settle_with_probe(probe, after_take_back, before_take_back, written)
            .await
    }

    /// Writes the probe behind a take-back (ledger at `seq`) and reads until
    /// the composer ends with it on a screen that accepts a write, with the
    /// take-back reached: the reading differs from `before_take_back` and is
    /// not `written` still showing. The receiver handles input in order, so
    /// the probe showing alone means the take-back emptied the composer; the
    /// probe is then erased, and the composer must read empty afterwards.
    /// Anything before the probe is something the take-back left, reported
    /// as remaining with no baseline a retry could press against; so is
    /// another writer's write meanwhile.
    async fn settle_with_probe(
        &self,
        probe: char,
        seq: u64,
        before_take_back: &ComposerReadout,
        written: &str,
    ) -> PtyResult<TakenBack> {
        let after_probe = seq + 1;
        if self
            .write_judged(
                probe.to_string().as_bytes(),
                seq,
                self.own_text(&format!("{written}{probe}")),
            )
            .await?
            == GuardedWrite::Moved
        {
            return Ok(TakenBack::Declined(declined(
                NotPlacedReason::Interrupted,
                Some(seq),
            )));
        }
        let probe_shown_alone = |observation: &ComposerObservation| match &observation.readout {
            ComposerReadout::Body(text)
                if &observation.readout != before_take_back
                    && screen_accepts_write(observation.screen_rule)
                    && !reads_as_before_take_back(text, written, probe) =>
            {
                text.strip_suffix(probe).map(str::is_empty)
            }
            ComposerReadout::Body(_)
            | ComposerReadout::Empty
            | ComposerReadout::NotObserved
            | ComposerReadout::Tail(_) => None,
        };
        match self
            .wait_for_composer_unwatched(after_probe, probe_shown_alone)
            .await?
        {
            Some(true) => {
                let erased = self.erase_probe(after_probe).await?;
                Ok(self.taken_back_once(erased))
            }
            Some(false) => {
                // Input is handled in order, so this erases the probe behind
                // whatever the take-back left.
                match self
                    .write_judged(&self.input.erase, after_probe, self.own_text(written))
                    .await?
                {
                    GuardedWrite::Written => Ok(TakenBack::Declined(
                        self.decline_unconfirmed_take_back(NotPlacedReason::NotReflected),
                    )),
                    GuardedWrite::Moved => Ok(TakenBack::Declined(declined_unvouched(
                        NotPlacedReason::Interrupted,
                    ))),
                }
            }
            None => Ok(TakenBack::Declined(declined_unvouched(
                NotPlacedReason::Interrupted,
            ))),
        }
    }

    /// A take-back's outcome once its probe has been erased.
    fn taken_back_once(&self, erased: ProbeErased) -> TakenBack {
        match erased {
            ProbeErased::Emptied(rule, seq) => TakenBack::Emptied(rule, seq),
            ProbeErased::Interrupted(after_erase) => {
                TakenBack::Declined(declined(NotPlacedReason::Interrupted, Some(after_erase)))
            }
        }
    }

    /// Erases a probe that shows alone in the composer, with the ledger at
    /// `seq` and nothing else written since, and reads until the composer is
    /// empty on a screen that accepts a write, or another writer moves the
    /// ledger. Erasing its own probe is cleanup, so no requester stops it.
    async fn erase_probe(&self, seq: u64) -> PtyResult<ProbeErased> {
        let after_erase = seq + 1;
        if self
            .write_judged(&self.input.erase, seq, TakeBackRegistration::Empty)
            .await?
            == GuardedWrite::Moved
        {
            return Ok(ProbeErased::Interrupted(seq));
        }
        let emptied = |observation: &ComposerObservation| {
            (matches!(observation.readout, ComposerReadout::Empty)
                && screen_accepts_write(observation.screen_rule))
            .then_some(observation.screen_rule)
        };
        match self
            .wait_for_composer_unwatched(after_erase, emptied)
            .await?
        {
            Some(rule) => {
                *lock_recover(&self.unanswered_probe) = None;
                *lock_recover(&self.degenerate_since) = None;
                Ok(ProbeErased::Emptied(rule, after_erase))
            }
            None => Ok(ProbeErased::Interrupted(after_erase)),
        }
    }

    /// Settles a probe an earlier call left unanswered, when the
    /// composer now holds it alone on a screen that accepts a write and
    /// nothing was written since: whatever was written before it has been
    /// handled and nothing else remains, so the probe is erased and the window
    /// may be written to again.
    async fn settle_unanswered_probe(
        &self,
        text: &str,
        probe: char,
        screen_rule: Option<DetectedStatus>,
    ) -> PtyResult<Option<TakenBack>> {
        let Some(seq) = *lock_recover(&self.unanswered_probe) else {
            return Ok(None);
        };
        let probe_alone = text == probe.to_string();
        if !probe_alone || !screen_accepts_write(screen_rule) || self.write_seq() != seq {
            return Ok(None);
        }
        let erased = self.erase_probe(seq).await?;
        Ok(Some(self.taken_back_once(erased)))
    }

    /// Declines after a take-back whose effect could not be confirmed: the
    /// composer may still hold the body, the take-back's own bytes, or a
    /// half-erased body. The degenerate-empty marker stops a later attempt
    /// that reads the composer empty from writing the body again on top.
    fn decline_unconfirmed_take_back(&self, reason: NotPlacedReason) -> SubmitJudgment {
        *lock_recover(&self.degenerate_since) = Some(self.write_seq());
        declined_unvouched(reason)
    }

    /// Presses the plan's own submit key once the composer has been
    /// confirmed to hold this call's own write and nothing else's (the
    /// ledger at `seq`), and answers at once: what the receiver does with it
    /// is the caller's to confirm.
    async fn press_submit_key(
        &self,
        submit_step: &SubmitStep,
        seq: u64,
    ) -> PtyResult<SubmitJudgment> {
        let status_before_submit = self.current_status();
        match self
            .write_judged(&submit_step.bytes, seq, TakeBackRegistration::Empty)
            .await?
        {
            GuardedWrite::Written => Ok(placed(
                SubmitOutcome {
                    status_before_submit,
                    submitted_at: tokio::time::Instant::now(),
                },
                seq + 1,
            )),
            GuardedWrite::Moved => Ok(declined(NotPlacedReason::Interrupted, Some(seq))),
        }
    }

    /// Takes this window's delivery lock, which one handoff delivery holds
    /// from before it judges this window's startup phase until its own
    /// delivery verdict is final, re-sends included.
    ///
    /// The guard is handed back as-is: where it is dropped is the caller's
    /// decision, and that instant is the whole contract.
    ///
    /// Turns are handed out in the order `lock` was called -- Tokio's mutex
    /// is FIFO -- which is the order deliveries reached this queue, not the
    /// order their requests arrived: an approval wait sits outside it. A
    /// caller writing a single sequence takes only
    /// [`WindowEntry::submit_message`]'s lock and never this one, so it waits
    /// for a sequence already in progress but never for another delivery's
    /// confirmation and re-sends.
    pub async fn lock_delivery(&self) -> AsyncMutexGuard<'_, ()> {
        self.delivery_lock.lock().await
    }

    /// [`Self::lock_delivery`] for a delivery that must not queue without
    /// bound: `None` when `max_waiters` other deliveries already wait for the
    /// turn. The one counting point for every delivery kind (chat, intro,
    /// handoff); the delivery holding the turn is not counted.
    ///
    /// Cancel-safe: a dropped call leaves the queue and gives its place back.
    pub async fn lock_delivery_bounded(
        &self,
        max_waiters: usize,
    ) -> Option<AsyncMutexGuard<'_, ()>> {
        if let Ok(guard) = self.delivery_lock.try_lock() {
            return Some(guard);
        }
        let _place = TurnWaiterPlace::take(&self.turn_waiters, max_waiters)?;
        Some(self.delivery_lock.lock().await)
    }

    /// Verifies `candidate` against *this window's* auth token, which is what
    /// both a `StatusEvent` about this window and an `Identify` claiming it
    /// are proven with.
    ///
    /// The token this returns `true` for is the one injected into this
    /// window's own child environment, so a child that reads its
    /// `YAGURA_STATUS_TOKEN` can only speak for itself; the value never
    /// authenticates a request aimed at a different window.
    ///
    /// One case still returns `false` for every candidate: the token is
    /// unverifiable (the OS RNG could not be read when the window was opened),
    /// which fails every such request closed rather than accepting on a token
    /// nobody holds.
    ///
    /// The one documented exception to the isolation above is
    /// [`DaemonConfig::status_token_override`]: when it is set, every window
    /// adopts that same value, so any window's token verifies against any
    /// other window (a test / development facility, never production).
    #[must_use]
    pub fn verify_status_token(&self, candidate: &str) -> bool {
        self.status_token.verify(candidate)
    }

    /// Records a structured-source status event on this window's detector.
    /// Returns `false` without effect once the window has exited.
    pub fn record_status_event(&self, status: WindowStatus) -> bool {
        match lock_recover(&self.detector).as_ref() {
            Some(detector) => {
                detector.record_status_event(status);
                true
            }
            None => false,
        }
    }

    /// Tells this window's detector that its model was resized, so a model
    /// the resize had to rebuild is noticed at once. A no-op once the window
    /// has exited.
    pub fn propagate_resize(&self) {
        if let Some(detector) = lock_recover(&self.detector).as_ref() {
            detector.notify_resized();
        }
    }

    /// Arms resize-redraw attribution on this window's detector. Returns
    /// `false` without effect once the window has exited, the same shape as
    /// [`WindowEntry::record_status_event`].
    pub fn notify_resize_started(&self) -> bool {
        match lock_recover(&self.detector).as_ref() {
            Some(detector) => {
                detector.notify_resize_started();
                true
            }
            None => false,
        }
    }

    /// Whether this window currently has a detector attached (`false` once
    /// `Exited` teardown has run).
    pub fn has_detector(&self) -> bool {
        lock_recover(&self.detector).is_some()
    }

    /// Ends this window's detection driver task with a panic in its next
    /// step (see [`DetectorHandle::inject_driver_panic`]); a window with no
    /// detector is left alone. Behind the `fault-injection` feature only.
    #[cfg(feature = "fault-injection")]
    pub fn inject_detector_driver_panic(&self) {
        if let Some(detector) = lock_recover(&self.detector).as_ref() {
            detector.inject_driver_panic();
        }
    }

    /// Whether anything still holds this window's detector handle (the entry
    /// itself or the exit watcher), observed through the weak reference kept
    /// independently of `detector`; it converges to `false` once an `Exited`
    /// window's teardown has released every holder.
    ///
    /// A driver task that died on its own does not release the handle, so it
    /// is not seen here; [`publish_detector_loss`] reports that window as
    /// `unknown` instead.
    pub fn detector_task_alive(&self) -> bool {
        self.detector_weak.strong_count() > 0
    }

    /// Subscribes to confirmed status changes for this window.
    ///
    /// Each delivered value is the whole published pair: the confirmed
    /// status, when it became confirmed, and the generation assigned to that
    /// transition. A receiver therefore builds whatever it forwards from the
    /// value it was handed, and never reads this entry back -- reading back
    /// would report the window's *current* generation, which may already be
    /// newer than the change being forwarded.
    pub fn subscribe_status(&self) -> broadcast::Receiver<StatusAt> {
        self.status_changes.subscribe()
    }

    /// The current confirmed status together with the instant it became the
    /// confirmed status, read as one value so the two can never disagree.
    pub fn status_snapshot(&self) -> StatusAt {
        *self.current_status.borrow()
    }

    /// The current confirmed status, projected to [`WindowStatus`] (see
    /// [`WindowEntry::current_detected`] for the reason behind an `Unknown`).
    pub fn current_status(&self) -> WindowStatus {
        self.status_snapshot().status.status()
    }

    /// The current confirmed status together with why it is `Unknown`, when
    /// it is.
    #[must_use]
    pub fn current_detected(&self) -> DetectedStatus {
        self.status_snapshot().status
    }

    /// Subscribes to the confirmed status as a `watch` channel: the returned
    /// receiver always observes the latest value and can never miss a
    /// transition the way `subscribe_status`'s `broadcast::Receiver` can lag
    /// under load. Used by [`crate::waiter::wait_for_status`], which holds
    /// only the receiver and never a reference back to this entry.
    pub fn watch_status(&self) -> watch::Receiver<StatusAt> {
        self.current_status.subscribe()
    }

    /// What this window's *current* published pair says about its startup
    /// phase, latching if the pair already proves it. A caller that gets
    /// [`StartupEvidence::Building`] may wait for the hold to complete; a
    /// caller that gets [`StartupEvidence::Absent`] has nothing to wait for.
    pub fn startup_evidence(&self) -> StartupEvidence {
        self.ready_hold
            .observe(self.status_snapshot(), tokio::time::Instant::now())
    }

    /// Whether this window is already known to be past its startup phase.
    /// Unlike [`WindowEntry::startup_evidence`], reading it never latches.
    #[must_use]
    pub fn has_left_startup_phase(&self) -> bool {
        self.ready_hold.is_latched()
    }

    /// Records what a completed hold wait proved, so the next handoff into
    /// this window does not wait again.
    pub fn note_left_startup_phase(&self) {
        self.ready_hold.latch();
    }

    /// Registers this caller as the (new) attach holder, superseding any
    /// previous holder (which is notified via the returned generation's
    /// predecessor's kick channel).
    pub async fn register_attach(&self) -> (u64, oneshot::Receiver<()>) {
        let generation = self.attach_generation.fetch_add(1, Ordering::SeqCst) + 1;
        let (kick_tx, kick_rx) = oneshot::channel();
        {
            let mut slot = lock_recover(&self.current_attach);
            if let Some((_, old_kick_tx)) = slot.take() {
                let _ = old_kick_tx.send(());
            }
            *slot = Some((generation, kick_tx));
        }
        (generation, kick_rx)
    }

    /// Clears the attach slot if it is still held by `generation` (a stale
    /// disconnect/reattach must not clobber a newer holder).
    pub async fn clear_attach_if(&self, generation: u64) {
        let mut slot = lock_recover(&self.current_attach);
        if matches!(slot.as_ref(), Some((g, _)) if *g == generation) {
            *slot = None;
        }
    }

    /// Whether some connection currently holds this window's attach slot.
    /// Used by [`WindowRegistry`] to decide which `Exited` entries are safe
    /// to evict.
    fn is_attached(&self) -> bool {
        lock_recover(&self.current_attach).is_some()
    }

    /// Changes this window's display name and returns the value as stored,
    /// re-read under the same guard rather than echoed back from the request,
    /// so that concurrent renames converge on one last-writer-wins value.
    ///
    /// A label change never touches the child process, so this succeeds on an
    /// exited window too.
    ///
    /// # Errors
    ///
    /// [`WindowNameError`] if `name` fails [`validate_window_name`]; the
    /// stored name is left untouched. The check lives here, not in the
    /// caller, so that this and [`WindowRegistry::open`] -- the only two ways
    /// a name reaches storage -- cannot drift apart.
    pub fn rename(&self, name: &str) -> Result<String, WindowNameError> {
        validate_window_name(name)?;
        let mut stored = lock_recover(&self.name);
        name.clone_into(&mut stored);
        Ok(stored.clone())
    }

    /// Replaces the presentation layout this window carries. The value is
    /// stored verbatim; validating it (including its size) is the caller's
    /// responsibility.
    fn set_layout(&self, layout_json: String) {
        *lock_recover(&self.layout_json) = Some(layout_json);
    }

    /// The presentation layout last reported for this window, if any.
    fn layout_json(&self) -> Option<String> {
        lock_recover(&self.layout_json).clone()
    }

    /// This window's current record, before [`WindowRegistry::list`] adds the
    /// listing-only `room` decoration.
    pub(crate) fn window_info(&self) -> WindowInfo {
        // One read of the published pair, so the reported status, its reason
        // and the generation all describe the same transition.
        let at = self.status_snapshot();
        WindowInfo {
            window_id: self.window_id.clone(),
            name: lock_recover(&self.name).clone(),
            vendor: self.vendor.clone(),
            cwd: self.cwd.clone(),
            status: at.status.status(),
            exit_code: self.session.exit_status(),
            layout_json: self.layout_json(),
            room: None,
            unknown_reason: at.status.unknown_reason(),
            detection_warning: self.detection_warning.clone(),
            status_rev: Some(at.generation),
            mcp_connected: Some(self.mcp_connected()),
            status_reports_missing: Some(self.report_gap()),
        }
    }

    /// Whether this window's CLI is expected to report its turns but the
    /// screen has shown a running turn for the grace time without one. `false`
    /// once the detector is gone.
    pub fn report_gap(&self) -> bool {
        lock_recover(&self.detector)
            .as_ref()
            .is_some_and(|detector| detector.report_gap())
    }

    /// Whether this window's CLI was launched with hooks that should report
    /// its turns. `false` once the detector is gone.
    pub fn reports_expected(&self) -> bool {
        lock_recover(&self.detector)
            .as_ref()
            .is_some_and(|detector| detector.reports_expected())
    }

    /// Declares that this window's CLI reports its turns through hooks the
    /// CLI reads from its own settings rather than from its argv.
    pub(crate) fn expect_status_reports(&self) {
        if let Some(detector) = lock_recover(&self.detector).as_ref() {
            detector.expect_status_reports(true);
        }
    }

    /// Whether at least one live connection has identified as this window.
    pub(crate) fn mcp_connected(&self) -> bool {
        self.mcp_connections.load(Ordering::SeqCst) > 0
    }

    /// Counts one more connection identified as this window until the
    /// returned claim is dropped.
    pub(crate) fn claim_mcp_connection(&self) -> McpConnectionClaim {
        self.mcp_connections.fetch_add(1, Ordering::SeqCst);
        McpConnectionClaim {
            connections: Arc::clone(&self.mcp_connections),
        }
    }
}

/// One connection's identification as a window, held for as long as the
/// connection lives: dropping it is the only way the count goes down, so
/// every way a connection can end releases it.
#[derive(Debug)]
pub(crate) struct McpConnectionClaim {
    connections: Arc<AtomicUsize>,
}

impl Drop for McpConnectionClaim {
    fn drop(&mut self) {
        self.connections.fetch_sub(1, Ordering::SeqCst);
    }
}

impl std::fmt::Debug for WindowEntry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // `try_lock`, never `lock`: `std::sync::Mutex` is not reentrant, so
        // formatting an entry from code that already holds the name guard
        // would deadlock on itself. A diagnostic impl must not hang the daemon.
        let name = match self.name.try_lock() {
            Ok(name) => name.clone(),
            Err(std::sync::TryLockError::Poisoned(poisoned)) => poisoned.into_inner().clone(),
            Err(std::sync::TryLockError::WouldBlock) => "<locked>".to_string(),
        };
        f.debug_struct("WindowEntry")
            .field("window_id", &self.window_id)
            .field("name", &name)
            .field("vendor", &self.vendor)
            .field("cwd", &self.cwd)
            .field("current_status", &self.current_status())
            .field("has_detector", &self.has_detector())
            .finish_non_exhaustive()
    }
}

/// A callback run at the end of every successful `open`; see
/// [`WindowRegistry::set_window_opened_hook`].
type WindowOpenedHook = Box<dyn Fn(&Arc<WindowEntry>) + Send + Sync>;

/// Starts or wakes one window's notice delivery agent; see
/// [`WindowRegistry::set_notice_launcher`].
pub(crate) type NoticeLauncher = Box<dyn Fn(&Arc<WindowEntry>) + Send + Sync>;

/// The outcome of [`WindowRegistry::liveness`]: this is the single predicate
/// callers use to tell whether a window can still take a request. Being
/// registered and being alive are different questions -- an entry whose
/// child has already exited stays registered (it is still a restore
/// candidate and can still be looked up), but is not alive.
#[derive(Debug, Clone)]
pub enum WindowLiveness {
    /// The entry exists, is not marked closed, and its confirmed status is
    /// not [`WindowStatus::Exited`].
    Alive(Arc<WindowEntry>),
    /// The entry exists but is marked closed, or its confirmed status is
    /// [`WindowStatus::Exited`].
    Dead,
    /// No entry with this id has ever been registered here.
    NotFound,
}

/// `window_id -> WindowEntry` registry: the SSOT for opening, listing,
/// looking up and tearing down windows.
pub struct WindowRegistry {
    config: DaemonConfig,
    entries: Mutex<Vec<Arc<WindowEntry>>>,
    /// Serializes the whole of [`WindowRegistry::open`] (limit check ->
    /// spawn -> insert) against concurrent opens, so the dual resource
    /// limits are enforced atomically rather than raced past.
    open_lock: AsyncMutex<()>,
    next_id: AtomicU64,
    /// Per-instance suffix appended to every assigned `window_id`, so that
    /// the sequence restarting at 1 in a new daemon cannot make an old
    /// client's stale identifier resolve to a different window.
    boot_nonce: String,
    /// The one `StatusEvent` auth token every window must adopt, when
    /// [`DaemonConfig::status_token_override`] supplied a well-formed value.
    /// `None` -- the production setting -- means each window gets its own
    /// freshly generated token instead, which is what keeps one window's child
    /// from being able to speak for another.
    status_token_override: Option<Arc<StatusToken>>,
    /// The channel [`WindowRegistry::request_snapshot`] sends
    /// restore-candidate rows on, installed once at start-up by
    /// `store::wire_into`. `None` when persistence failed to
    /// initialize, or this registry was never wired to a store at all (a
    /// plain daemon build, or most tests).
    store_sender: Mutex<Option<std::sync::mpsc::Sender<Vec<crate::store::SnapshotWindow>>>>,
    /// Called at the end of every successful `open`, once the
    /// new entry is already registered and can be looked up by id. `None`
    /// when chat is disabled or this registry predates chat entirely (most
    /// tests): a registry never knows what a chat participant is on its own.
    window_opened_hook: Mutex<Option<WindowOpenedHook>>,
    /// Starts or wakes a window's notice delivery agent. `None` until an IPC
    /// server is built on this registry: notices queued before then wait.
    notice_launcher: Mutex<Option<NoticeLauncher>>,
    /// Caps concurrent workspace-key walks: they block and cannot be
    /// aborted, so an unbounded number would drain the shared blocking pool.
    workspace_walks: tokio::sync::Semaphore,
    /// Serializes writing MCP settings files, so two opens needing the same
    /// file never race on one temporary file name.
    mcp_config_write: AsyncMutex<()>,
    /// Windows whose id and token already exist but whose open has not yet
    /// registered them or failed. A child can identify as early as it is
    /// spawned, so an `identify` for one of these waits instead of answering
    /// `window_not_found`.
    opening: OpeningWindows,
}

impl WindowRegistry {
    /// Builds an empty registry; construction spawns nothing.
    pub fn new(config: DaemonConfig) -> Self {
        // A well-formed override is adopted as-is (the only supported way for a
        // test / development setup to hold a known token); anything else leaves
        // every window to generate its own and only warns.
        let status_token_override = match config.status_token_override.as_deref() {
            Some(value) => match StatusToken::from_override(value) {
                Some(token) => {
                    // The override turns per-window isolation off, so its use
                    // must leave a trace in the log even though the value
                    // never does.
                    tracing::warn!(
                        "status token override is active (test/dev facility); per-window token \
                         isolation is disabled, so one window's token also authenticates an \
                         identify claiming any other window"
                    );
                    Some(Arc::new(token))
                }
                None => {
                    tracing::warn!(
                        "status_token_override is not a well-formed 32-character hex value; \
                         generating a per-window token instead"
                    );
                    None
                }
            },
            None => None,
        };

        let workspace_walks = tokio::sync::Semaphore::new(config.max_windows);

        Self {
            config,
            entries: Mutex::new(Vec::new()),
            open_lock: AsyncMutex::new(()),
            next_id: AtomicU64::new(0),
            boot_nonce: boot_nonce(),
            status_token_override,
            store_sender: Mutex::new(None),
            window_opened_hook: Mutex::new(None),
            notice_launcher: Mutex::new(None),
            workspace_walks,
            mcp_config_write: AsyncMutex::new(()),
            opening: Mutex::new(HashMap::new()),
        }
    }

    /// Opens a new window: validates the request, enforces the dual
    /// resource limits (evicting an unattached `Exited` entry if the total
    /// cap requires it), spawns the PTY and its per-window detector, and
    /// registers the result.
    ///
    /// Returns the registered entry together with the assigned `window_id`
    /// and the detection definition's load warning, if any
    /// ([`OpenedWindow`]): a definition that fails to load leaves the window
    /// open with its status pinned to `Unknown`, so the reason travels back
    /// with the success rather than as an error.
    pub async fn open(&self, req: OpenWindowRequest) -> Result<OpenedWindow, WindowOpenError> {
        self.open_impl(req, &[], Vec::new(), WindowProvenance::default())
            .await
    }

    /// [`WindowRegistry::open`]'s additional-env entry point: opens a
    /// window exactly as `open` does, plus environment variables beyond the
    /// registry's own `YAGURA_STATUS_TOKEN` / `YAGURA_WINDOW_ID` injection --
    /// a launch profile's `[env]` table, with its reserved keys already
    /// dropped by `crate::profile::parse_profile` -- and the window's
    /// `provenance` (who opened it, for which plan task, and from which
    /// profile launch), plus the argv elements a profile's `[mcp]` and
    /// `[hooks]` tables contribute (`injections`, appended in the order
    /// given).
    ///
    /// `req` is unchanged from `open`'s (no new field on [`OpenWindowRequest`]):
    /// the extra environment, the injections and the provenance travel as
    /// separate arguments instead, so the existing `OpenWindow` wire message
    /// and every other caller of `open` stay untouched.
    ///
    /// # Errors
    ///
    /// The same [`WindowOpenError`]s as [`WindowRegistry::open`]. An
    /// injection that cannot be carried out (its settings file cannot be
    /// written, or the command interpreter would reinterpret it) is dropped
    /// with a warning, on its own, rather than failing the open.
    pub async fn open_with_extra_env(
        &self,
        req: OpenWindowRequest,
        extra_env: Vec<(String, String)>,
        injections: Vec<CliInjection>,
        provenance: WindowProvenance,
    ) -> Result<OpenedWindow, WindowOpenError> {
        self.open_impl(req, &extra_env, injections, provenance)
            .await
    }

    /// Shared body of [`WindowRegistry::open`] and
    /// [`WindowRegistry::open_with_extra_env`].
    async fn open_impl(
        &self,
        req: OpenWindowRequest,
        extra_profile_env: &[(String, String)],
        injections: Vec<CliInjection>,
        provenance: WindowProvenance,
    ) -> Result<OpenedWindow, WindowOpenError> {
        let OpenWindowRequest {
            command,
            cwd,
            name,
            vendor,
            detection,
        } = req;

        // Validate and normalize all input before entering the critical section.
        // `vendor` comes back out of the validator, so the stored value cannot
        // be one that never passed the gate.
        let ValidatedRequest {
            name,
            vendor,
            resolved_cwd,
            cwd_string,
        } = validate_open_request(
            command.clone(),
            cwd,
            self.config.default_window_cwd.as_deref(),
            name,
            vendor,
            detection.clone(),
        )?;

        // Early rejection, so a request that is over the limit does not pay
        // for the filesystem walk below. The guard is dropped here: the walk
        // waits on the caller's own cwd, and holding `open_lock` across it
        // would stall every other connection's open behind that one wait.
        {
            let _limit_guard = self.open_lock.lock().await;
            self.enforce_limits_before_open()?;
        }

        // Bounded by `workspace_walks`; `acquire` only errs
        // once the semaphore is closed, which never happens here.
        let walk_permit_acquire = self.workspace_walks.acquire();
        let walk_permit = walk_permit_acquire
            .await
            .map_err(|_| WindowOpenError::ResourceExhausted)?;

        // Resolved from the validated cwd, before this window is registered,
        // so every entry the registry ever holds already carries its chat
        // room key.
        let workspace = resolve_key_blocking(resolved_cwd.clone()).await;
        drop(walk_permit);

        // Written outside `open_lock`, after the early rejection: a request
        // over the limit never touches the disk, and one slow write never
        // stalls another connection's open.
        let mut prepared = Vec::with_capacity(injections.len());
        for injection in injections {
            prepared.push(self.prepare_injection(injection).await);
        }

        // Re-checked as the binding decision: only a check inside the lock
        // that is held through registration can count what is really there.
        let _open_guard = self.open_lock.lock().await;
        self.enforce_limits_before_open()?;

        // Generate window ID and status token early so they can be injected
        // into the child environment before spawn.
        let window_id = self.allocate_window_id();
        let status_token = self.acquire_status_token();
        // Declared after `_open_guard`, so it is released on every return
        // path, and only once registration (if any) has already happened.
        let _opening = self.begin_opening(&window_id);

        let (session, hooks_injected) = self
            .spawn_pty_for_window(
                &command,
                &resolved_cwd,
                &status_token,
                &window_id,
                extra_profile_env,
                &prepared,
            )
            .await?;
        let session = Arc::new(session);

        let DetectionSetup {
            handle: detector_handle,
            detected_rx,
            warning: detection_warning,
            input,
            definition_name,
            composer_access,
            abandons_calls_at_turn_end,
        } = self.initialize_detection_for_window(&window_id, &session, detection.as_deref());
        // Decided where the injection is finally carried out, so a hooks table
        // that was dropped on the way (gate, file, shim) expects nothing.
        if hooks_injected {
            detector_handle.expect_status_reports(true);
        }
        // Started before the remaining awaited steps of this open so the
        // detector keeps converging while they run.
        spawn_output_pump(&session, Arc::downgrade(&detector_handle));

        let entry = self.create_window_entry(
            window_id.clone(),
            name,
            vendor,
            cwd_string,
            workspace,
            command,
            detection,
            session.clone(),
            detector_handle.clone(),
            status_token,
            input,
            composer_access,
            abandons_calls_at_turn_end,
            definition_name,
            detection_warning.clone(),
            provenance,
        );

        self.register_window(&session, entry.clone(), detector_handle, detected_rx)?;
        // Registered above, so a hook that spawns a watcher observes an entry
        // the registry can already look up by id.
        if let Some(hook) = lock_recover(&self.window_opened_hook).as_ref() {
            hook(&entry);
        }

        // One trigger point inside `open` covers every open.
        self.request_snapshot();

        Ok(OpenedWindow {
            window_id,
            detection_warning,
            entry,
        })
    }

    /// Enforces the dual resource limits before a new entry is spawned: the
    /// living-window cap always rejects
    /// outright, while the total-entry cap tries to evict the oldest
    /// unattached `Exited` entry first and only rejects if none exists.
    fn enforce_limits_before_open(&self) -> Result<(), WindowOpenError> {
        let mut entries = lock_recover(&self.entries);
        if let Some(index) = self.slot_for_new_entry(&entries)? {
            let evicted = entries.remove(index);
            // The last reference to the entry is not let go under the lock.
            drop(entries);
            drop(evicted);
        }
        Ok(())
    }

    /// Where a new entry fits among `entries`: `Ok(None)` when it fits as it
    /// is, `Ok(Some(index))` when the entry at `index` must be evicted first.
    fn slot_for_new_entry(
        &self,
        entries: &[Arc<WindowEntry>],
    ) -> Result<Option<usize>, WindowOpenError> {
        let living = entries
            .iter()
            .filter(|entry| entry.current_status() != WindowStatus::Exited)
            .count();
        if living >= self.config.max_windows {
            return Err(WindowOpenError::ResourceExhausted);
        }

        let total_cap = self.config.max_windows.saturating_mul(2);
        if entries.len() < total_cap {
            return Ok(None);
        }
        entries
            .iter()
            .position(|entry| {
                entry.current_status() == WindowStatus::Exited && !entry.is_attached()
            })
            .map(Some)
            .ok_or(WindowOpenError::ResourceExhausted)
    }

    /// Judges a window open before anything is asked or spawned: the request
    /// is validated exactly as [`WindowRegistry::open`] validates it and the
    /// resource limits are checked, without evicting anything. Returns the
    /// working directory the window would start in, resolved the way the open
    /// resolves it.
    ///
    /// The limits are checked again when the window is actually opened, so
    /// this is an early refusal, not a reservation.
    ///
    /// # Errors
    ///
    /// The [`WindowOpenError`] the open itself would fail with.
    pub fn check_open_request(&self, req: &OpenWindowRequest) -> Result<String, WindowOpenError> {
        let OpenWindowRequest {
            command,
            cwd,
            name,
            vendor,
            detection,
        } = req.clone();
        let validated = validate_open_request(
            command,
            cwd,
            self.config.default_window_cwd.as_deref(),
            name,
            vendor,
            detection,
        )?;
        self.slot_for_new_entry(&lock_recover(&self.entries))?;
        Ok(validated.cwd_string)
    }

    /// Makes a [`CliInjection`] ready to spawn with: writes its settings
    /// file when one is needed and not already present, and returns the argv
    /// elements to append. A file that cannot be written drops the whole
    /// injection (with a warning), since its elements would name a file the
    /// CLI cannot read.
    async fn prepare_injection(&self, injection: CliInjection) -> PreparedInjection {
        let CliInjection {
            kind,
            args,
            config_file,
        } = injection;
        let Some(config_file) = config_file else {
            return PreparedInjection { kind, args };
        };
        let _write_guard = self.mcp_config_write.lock().await;
        let target = config_file.path();
        let written = tokio::task::spawn_blocking(move || write_injected_config_file(&config_file))
            .await
            .map_err(|err| std::io::Error::other(err.to_string()))
            .and_then(|outcome| outcome);
        match written {
            Ok(()) => PreparedInjection { kind, args },
            Err(err) => {
                tracing::warn!(
                    path = %target.display(),
                    error = %err,
                    "could not write the {} settings file; the window opens without that injection",
                    kind.label()
                );
                PreparedInjection {
                    kind,
                    args: Vec::new(),
                }
            }
        }
    }

    /// Spawns a PTY session with the given command in the resolved working
    /// directory, injecting `YAGURA_STATUS_TOKEN` and `YAGURA_WINDOW_ID` into
    /// the child environment if the token is verifiable, the MCP bridge path
    /// when one was resolved, plus `extra_profile_env` (a launch profile's
    /// own `[env]` table; empty for an `OpenWindow` request). `injections`
    /// are appended to `argv` in order for this spawn only.
    ///
    /// This is the registry's one spawn call site, and the only place
    /// `argv[0]` is resolved to a concrete executable through `yagura-os`
    /// (`resolve_program`). A resolution the command interpreter would
    /// reinterpret is refused (`InvalidArgument`); one that resolves to
    /// nothing (`Resolution::NotFound`) is passed through unchanged and left
    /// to fail on its own (`SpawnFailed`), preserving the existing
    /// `OpenWindow` contract for a program that simply is not there.
    async fn spawn_pty_for_window(
        &self,
        argv: &[String],
        resolved_cwd: &std::path::Path,
        status_token: &StatusToken,
        window_id: &str,
        extra_profile_env: &[(String, String)],
        injections: &[PreparedInjection],
    ) -> Result<(PtySession, bool), WindowOpenError> {
        let platform = self.config.host_os.platform();
        let predicate = crate::profile::is_executable_predicate(&self.config);
        let resolution =
            program::resolve_program(argv, platform, &self.config.program_lookup, &predicate)
                .map_err(|rejected| WindowOpenError::InvalidArgument {
                    reason: rejected.0,
                    argument: None,
                })?;
        let mut full_argv = argv.to_vec();
        let mut hooks_injected = false;
        for injection in injections {
            let args = injection_args_for(&resolution, injection);
            hooks_injected |= injection.kind == InjectionKind::Hooks && !args.is_empty();
            full_argv.extend_from_slice(args);
        }
        let mut child_env =
            child_environment(&self.config, status_token, window_id, extra_profile_env);
        // The hooks name the daemon bare, and cmd looks in the working
        // directory before PATH unless told not to. A profile's own value wins.
        if hooks_injected
            && platform == program::Platform::Windows
            && !child_env
                .iter()
                .any(|(key, _)| key.eq_ignore_ascii_case(NO_CWD_PROGRAM_SEARCH_ENV))
        {
            child_env.push((NO_CWD_PROGRAM_SEARCH_ENV.to_string(), "1".to_string()));
        }
        let extra_env: Vec<(&str, &str)> = child_env
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();
        // `into_spawn_argv` (yagura-os) owns the CmdShim expansion in full,
        // including the `call` prefix cmd.exe's `/C` preprocessing sometimes
        // needs: this OS boundary does no Windows-specific reconstruction of
        // its own. The injections are already part of `full_argv`, so that
        // decision sees them too.
        let spawn_argv = program::into_spawn_argv(&resolution, &full_argv, platform);
        let spawn_argv: Vec<&str> = spawn_argv.iter().map(String::as_str).collect();

        let session = PtySession::spawn_with_command_in_env(
            &spawn_argv,
            Some(resolved_cwd),
            &extra_env,
            &self.config,
        )
        .await
        .map_err(|err| WindowOpenError::SpawnFailed(err.to_string()))?;
        Ok((session, hooks_injected))
    }

    /// Initializes the detection engine for a new window, loading its
    /// definition and spawning the detector handle.
    /// The `Weak` the entry keeps is derived once, in
    /// [`Self::create_window_entry`]; handing a second one out from here
    /// would be the same value computed twice.
    ///
    /// The definition's `[input]` table is read into an [`InputProfile`]
    /// *before* the definition is moved into [`DetectionEngine::new`] --
    /// once moved, nothing downstream of the engine can see it again.
    fn initialize_detection_for_window(
        &self,
        window_id: &str,
        session: &PtySession,
        detection_override: Option<&str>,
    ) -> DetectionSetup {
        let resolved_name = detection_override
            .unwrap_or(self.config.detection_name.as_str())
            .to_string();
        let outcome = load_definition_for(&self.config, detection_override);
        let outcome = self.reject_when_submit_delay_does_not_outlast_confirm(outcome);
        // The one place the two-variant outcome is widened into the optional
        // pair the detection engine and `DetectionSetup` are built from.
        let (definition, warning) = match outcome {
            DetectionLoadOutcome::Loaded(definition) => (Some(definition), None),
            DetectionLoadOutcome::Failed(reason) => (None, Some(reason)),
        };
        // Only `Some` once the definition load has survived every rejection
        // above: a name this window failed to resolve a definition for is
        // never a participation key.
        let definition_name = definition.is_some().then_some(resolved_name);
        let input =
            InputProfile::from_section(definition.as_ref().and_then(|def| def.input.as_ref()));
        let composer_access = definition
            .as_ref()
            .map_or(ComposerAccess::Absent, definition_composer_access);
        let abandons_calls_at_turn_end = definition
            .as_ref()
            .is_some_and(|definition| definition.requests.abandoned_at_turn_end);
        let engine = DetectionEngine::new(definition, &self.config, session.model_for_detection());
        let (detector_handle, detected_rx) =
            tracing::info_span!("detector", window_id).in_scope(|| DetectorHandle::spawn(engine));
        DetectionSetup {
            handle: Arc::new(detector_handle),
            detected_rx,
            warning,
            input,
            definition_name,
            composer_access,
            abandons_calls_at_turn_end,
        }
    }

    /// The window-open-time half of the submit-delay-outlasts-confirm check:
    /// `validate_input_section` (`detector::definition`) can
    /// only compare `input.submit_delay_ms` against a definition's own
    /// `[debounce]` table, so a definition that omits `[debounce]` loads
    /// successfully even when the `confirm_ms` it actually runs under
    /// (`DaemonConfig::confirm_ms`) is not outlasted. This is the one place
    /// both values are visible together, and it rejects a violation the
    /// same way [`load_definition_for`] rejects any other malformed
    /// definition: no definition, permanent `Unknown`, and a caller-facing
    /// warning. Shares [`check_submit_delay_outlasts`] with
    /// `validate_input_section` so the two call sites apply one comparison.
    fn reject_when_submit_delay_does_not_outlast_confirm(
        &self,
        outcome: DetectionLoadOutcome,
    ) -> DetectionLoadOutcome {
        let DetectionLoadOutcome::Loaded(definition) = &outcome else {
            return outcome;
        };
        let Some(section) = definition.input.as_ref() else {
            return outcome;
        };
        if section.delivery_confirm_ms == Some(0) {
            return outcome;
        }
        let submit_delay_ms = section
            .submit_delay_ms
            .unwrap_or(INPUT_SUBMIT_DELAY_DEFAULT_MS);
        let (effective_confirm_ms, source) = match definition.debounce {
            Some(debounce) => (debounce.confirm_ms, ConfirmMsSource::Debounce),
            None => (self.config.confirm_ms, ConfirmMsSource::DaemonDefault),
        };
        match check_submit_delay_outlasts(submit_delay_ms, effective_confirm_ms, source) {
            Ok(()) => outcome,
            Err(err) => {
                let rendered = err.to_string();
                DetectionLoadOutcome::Failed(detection_load_failure_text(&boundary_summary(
                    &err, &rendered,
                )))
            }
        }
    }

    /// What the daemon may type into a window opened with the definition
    /// named by `detection_override`, read the same way
    /// [`Self::initialize_detection_for_window`] reads it (through
    /// [`load_definition_for`] and
    /// [`Self::reject_when_submit_delay_does_not_outlast_confirm`]) but
    /// without opening anything. `None` when the definition cannot be read:
    /// the caller's own open path is what decides then, since a window still
    /// tells a disabled layer apart from every other unreadable case.
    pub(crate) fn composer_access_before_open(
        &self,
        detection_override: Option<&str>,
    ) -> Option<ComposerAccess> {
        let outcome = load_definition_for(&self.config, detection_override);
        match self.reject_when_submit_delay_does_not_outlast_confirm(outcome) {
            DetectionLoadOutcome::Loaded(definition) => {
                Some(definition_composer_access(&definition))
            }
            DetectionLoadOutcome::Failed(_) => None,
        }
    }

    /// This registry's boot nonce: the per-instance suffix every identifier
    /// minted for this daemon boot carries.
    ///
    /// Exposed so that approval request ids are built from the same nonce as
    /// window ids rather than from a second, independently generated one.
    #[must_use]
    pub fn boot_nonce(&self) -> &str {
        &self.boot_nonce
    }

    /// Allocates a new window ID by incrementing the sequence and combining
    /// with the boot nonce. Generated early so it can be injected into the
    /// child environment; sequence gaps may occur when a spawn fails.
    fn allocate_window_id(&self) -> String {
        format!(
            "w{}-{}",
            self.next_id.fetch_add(1, Ordering::SeqCst) + 1,
            self.boot_nonce
        )
    }

    /// Acquires this window's status token: the override (shared by all windows
    /// in test/dev mode) or a freshly generated per-window token.
    fn acquire_status_token(&self) -> Arc<StatusToken> {
        match &self.status_token_override {
            Some(shared) => Arc::clone(shared),
            None => Arc::new(StatusToken::generate()),
        }
    }

    /// Constructs a new `WindowEntry` with all per-window resources bound
    /// together. The argument count reflects the field count.
    #[allow(clippy::too_many_arguments)]
    fn create_window_entry(
        &self,
        window_id: String,
        name: String,
        vendor: Option<String>,
        cwd: String,
        workspace: WorkspaceKey,
        command: Vec<String>,
        detection: Option<String>,
        session: Arc<PtySession>,
        detector_handle: Arc<DetectorHandle>,
        status_token: Arc<StatusToken>,
        input: InputProfile,
        composer_access: ComposerAccess,
        abandons_calls_at_turn_end: bool,
        detection_definition: Option<String>,
        detection_warning: Option<String>,
        provenance: WindowProvenance,
    ) -> Arc<WindowEntry> {
        let (status_changes, _) = broadcast::channel(STATUS_BROADCAST_CAPACITY);
        // The same rule the engine itself applies at
        // construction -- a resolved definition means nothing has been
        // judged yet, no definition at all pins the window to that reason
        // instead.
        let initial_status = if detection_definition.is_some() {
            DetectedStatus::Unknown(UnknownReason::NotYetJudged)
        } else {
            DetectedStatus::Unknown(UnknownReason::NoDefinition)
        };
        let opened_at = tokio::time::Instant::now();
        let (current_status_tx, _) = watch::channel(StatusAt {
            status: initial_status,
            changed_at: opened_at,
            evidence_since: opened_at,
            generation: 0,
        });
        let ready_hold = ReadyHoldLatch::new(input.ready_hold);
        let detector_weak = Arc::downgrade(&detector_handle);

        Arc::new(WindowEntry {
            window_id,
            name: Mutex::new(name),
            vendor,
            cwd,
            workspace,
            command,
            detection,
            detection_definition,
            detection_warning,
            layout_json: Mutex::new(None),
            closed: AtomicBool::new(false),
            session,
            detector: Mutex::new(Some(detector_handle)),
            detector_weak,
            status_changes,
            current_status: current_status_tx,
            ready_hold,
            current_attach: Mutex::new(None),
            attach_generation: AtomicU64::new(0),
            status_token,
            input,
            composer_access,
            abandons_calls_at_turn_end,
            input_lock: AsyncMutex::new(()),
            delivery_lock: AsyncMutex::new(()),
            turn_waiters: AtomicUsize::new(0),
            degenerate_since: Mutex::new(None),
            unanswered_probe: Mutex::new(None),
            interrupt_bytes: self.config().interrupt_bytes.clone(),
            take_back_call: Mutex::new(None),
            own_part: Mutex::new(OwnPart::default()),
            chat_intro_outcome: std::sync::OnceLock::new(),
            notices: Mutex::new(NoticeQueue::default()),
            notice_wake: Notify::new(),
            notice_attempts: AtomicU64::new(0),
            provenance,
            mcp_connections: Arc::new(AtomicUsize::new(0)),
        })
    }

    /// Registers the window entry in the registry, spawns all background
    /// tasks (output pump, exit watcher, status forwarder), and returns
    /// ownership of the entry to the caller.
    fn register_window(
        &self,
        session: &Arc<PtySession>,
        entry: Arc<WindowEntry>,
        detector_handle: Arc<DetectorHandle>,
        detected_rx: mpsc::UnboundedReceiver<ConfirmedStatus>,
    ) -> Result<(), WindowOpenError> {
        // The output pump is already running (started right after the
        // detector was created, see `open_impl`); exactly `entry` and this
        // watcher task hold a strong `Arc<DetectorHandle>` from here on.
        spawn_exit_watcher(session.clone(), detector_handle.clone());
        spawn_status_forwarder(Arc::downgrade(&entry), detected_rx);
        drop(detector_handle);

        lock_recover(&self.entries).push(entry);
        Ok(())
    }

    /// Lists every window, in registration order, decorating each
    /// non-`Exited` entry with its `room`: the id from
    /// [`workspace::room_id_for`] and a display name disambiguated against
    /// every other live window's workspace plus the built-in `main` room
    /// (which always participates in collision detection but is never
    /// itself reported). An `Exited` window carries `room: None`, having
    /// left its room. No I/O is performed while the entries lock is held.
    pub fn list(&self) -> Vec<WindowInfo> {
        let entries = lock_recover(&self.entries);
        let mut infos: Vec<WindowInfo> = entries.iter().map(|entry| entry.window_info()).collect();

        let mut rooms: Vec<(String, Option<WorkspaceKey>)> = vec![("main".to_string(), None)];
        let mut seen_room_ids = HashSet::new();
        // Indexed alongside `infos`, so the decorating pass below reads each
        // window's room id instead of deriving it a second time. `None` is an
        // `Exited` window, which has left its room.
        let mut room_id_by_index: Vec<Option<String>> = Vec::with_capacity(infos.len());
        for (info, entry) in infos.iter().zip(entries.iter()) {
            if info.status == WindowStatus::Exited {
                room_id_by_index.push(None);
                continue;
            }
            let room_id = workspace::room_id_for(entry.workspace());
            if seen_room_ids.insert(room_id.clone()) {
                rooms.push((room_id.clone(), Some(entry.workspace().clone())));
            }
            room_id_by_index.push(Some(room_id));
        }
        let names = workspace::disambiguate(&rooms);
        let name_by_room_id: HashMap<&str, &str> = rooms
            .iter()
            .zip(names.iter())
            .skip(1)
            .map(|((room_id, _), name)| (room_id.as_str(), name.as_str()))
            .collect();

        for (info, room_id) in infos.iter_mut().zip(room_id_by_index) {
            let Some(room_id) = room_id else {
                continue;
            };
            if let Some(name) = name_by_room_id.get(room_id.as_str()) {
                info.room = Some(RoomRef {
                    room_id,
                    name: (*name).to_string(),
                });
            }
        }
        infos
    }

    /// Marks `window_id` as being opened until the returned value is dropped.
    pub(crate) fn begin_opening(&self, window_id: &str) -> OpeningWindow<'_> {
        open_in_progress(&self.opening, window_id)
    }

    /// Looks up a window by ID, first waiting out an open of that ID that
    /// is still in progress. Bounded by the open itself, not by a timer.
    pub(crate) async fn get_after_open(&self, window_id: &str) -> Option<Arc<WindowEntry>> {
        lookup_once_opened(&self.opening, window_id, || self.get(window_id)).await
    }

    /// Looks up a window by ID.
    pub fn get(&self, window_id: &str) -> Option<Arc<WindowEntry>> {
        lock_recover(&self.entries)
            .iter()
            .find(|entry| entry.window_id == window_id)
            .cloned()
    }

    /// The single "is this window alive" predicate: whether
    /// `window_id` can still take a request, distinct from whether it is
    /// merely still registered (see [`WindowLiveness`]). This is the only
    /// place that answers the question; callers elsewhere must not grow a
    /// second liveness check of their own.
    pub fn liveness(&self, window_id: &str) -> WindowLiveness {
        let Some(entry) = self.get(window_id) else {
            return WindowLiveness::NotFound;
        };
        if entry.closed.load(Ordering::SeqCst) || entry.current_status() == WindowStatus::Exited {
            return WindowLiveness::Dead;
        }
        WindowLiveness::Alive(entry)
    }

    /// Installs the channel [`WindowRegistry::request_snapshot`] sends
    /// restore-candidate rows on (`store::wire_into` is its only caller). Persistence being disabled or unwired is represented by
    /// never calling this at all, so `request_snapshot` stays a no-op.
    pub fn set_store_sender(&self, tx: std::sync::mpsc::Sender<Vec<crate::store::SnapshotWindow>>) {
        *lock_recover(&self.store_sender) = Some(tx);
    }

    /// This registry's own configuration, for a caller that needs it outside
    /// this module (the chat intro hook reads `handoff_ready_timeout_secs`
    /// the same way a delivery does).
    pub(crate) fn config(&self) -> &DaemonConfig {
        &self.config
    }

    /// Installs the hook called at the end of every successful `open`
    /// (`chat::install_intro_hook` is its only caller). Chat
    /// being disabled, or never wired at all, is represented by never calling
    /// this, so `open_impl` stays a no-op past registration in that case.
    pub fn set_window_opened_hook(&self, hook: WindowOpenedHook) {
        *lock_recover(&self.window_opened_hook) = Some(hook);
    }

    /// Installs the IPC layer's notice launcher, then runs it for every
    /// window that already has notices queued: a window opened before the
    /// IPC server existed (the daemon's first) must not wait for another
    /// notice to be woken.
    pub(crate) fn set_notice_launcher(&self, launcher: NoticeLauncher) {
        // Held across the scan, so a notice queued meanwhile finds this
        // launcher installed rather than none.
        let mut installed = lock_recover(&self.notice_launcher);
        let waiting: Vec<Arc<WindowEntry>> = lock_recover(&self.entries)
            .iter()
            .filter(|entry| entry.has_queued_notices())
            .cloned()
            .collect();
        for entry in &waiting {
            launcher(entry);
        }
        *installed = Some(launcher);
    }

    /// Starts `entry`'s notice delivery agent, or wakes the running one. A
    /// no-op until a launcher is installed.
    pub(crate) fn wake_notices(&self, entry: &Arc<WindowEntry>) {
        if let Some(launcher) = lock_recover(&self.notice_launcher).as_ref() {
            launcher(entry);
        }
    }

    /// Every window's participation-relevant facts, for a caller outside
    /// this module to compute its own notion of "participant" without this
    /// module knowing what one is: the chat room is the
    /// only caller today.
    pub fn list_participation(&self) -> Vec<WindowParticipation> {
        lock_recover(&self.entries)
            .iter()
            .map(|entry| WindowParticipation {
                window_id: entry.window_id.clone(),
                name: lock_recover(&entry.name).clone(),
                detection_definition: entry.detection_definition.clone(),
                status: entry.current_status(),
                workspace: entry.workspace.clone(),
                composer_access: entry.composer_access,
            })
            .collect()
    }

    /// Builds the current restore-candidate snapshot (registration order,
    /// closed windows excluded, positions renumbered from 0) and sends it to
    /// the store's writer thread. A no-op when no sender was
    /// installed. The send is fire-and-forget: a full or disconnected
    /// channel never blocks or fails the caller, matching every other
    /// persistence failure mode in this module.
    pub fn request_snapshot(&self) {
        let sender = lock_recover(&self.store_sender).clone();
        let Some(sender) = sender else {
            return;
        };
        let rows: Vec<crate::store::SnapshotWindow> = lock_recover(&self.entries)
            .iter()
            .filter(|entry| !entry.closed.load(Ordering::SeqCst))
            .enumerate()
            .map(|(position, entry)| {
                let launch = entry.provenance.launch.as_ref();
                crate::store::SnapshotWindow {
                    position: i64::try_from(position).unwrap_or(i64::MAX),
                    command: entry.command.clone(),
                    cwd: entry.cwd.clone(),
                    name: lock_recover(&entry.name).clone(),
                    vendor: entry.vendor.clone(),
                    detection: entry.detection.clone(),
                    layout_json: entry.layout_json(),
                    profile: launch.map(|launch| launch.profile.clone()),
                    model: launch.and_then(|launch| launch.model.clone()),
                    effort: launch.and_then(|launch| launch.effort.clone()),
                }
            })
            .collect();
        let _ = sender.send(rows);
    }

    /// Records `layout_json` as `entry`'s presentation layout and asks the
    /// store to persist the resulting configuration.
    ///
    /// The snapshot request lives here rather than at the call site so that
    /// storing a layout and persisting it cannot drift apart: a layout that
    /// only ever reached memory would be lost at the very restart it exists
    /// for.
    pub fn set_window_layout(&self, entry: &WindowEntry, layout_json: String) {
        entry.set_layout(layout_json);
        self.request_snapshot();
    }

    /// Terminates every window's child process. Statuses converge to
    /// `Exited` asynchronously as each session's exit is observed; this
    /// method does not wait for that convergence.
    pub async fn terminate_all(&self) {
        let entries: Vec<Arc<WindowEntry>> = lock_recover(&self.entries).clone();
        for entry in entries {
            entry.session.terminate();
        }
    }
}

/// Each in-progress open's window id, mapped to a receiver that closes when
/// that open finishes.
type OpeningWindows = Mutex<HashMap<String, watch::Receiver<()>>>;

/// One open in progress, from [`WindowRegistry::begin_opening`] until drop.
/// Dropping it removes the id first and then closes the channel, so a waiter
/// woken by the close always finds the open settled.
pub(crate) struct OpeningWindow<'a> {
    opening: &'a OpeningWindows,
    window_id: String,
    _released: watch::Sender<()>,
}

fn open_in_progress<'a>(opening: &'a OpeningWindows, window_id: &str) -> OpeningWindow<'a> {
    let (released, pending) = watch::channel(());
    lock_recover(opening).insert(window_id.to_string(), pending);
    OpeningWindow {
        opening,
        window_id: window_id.to_string(),
        _released: released,
    }
}

impl Drop for OpeningWindow<'_> {
    fn drop(&mut self) {
        lock_recover(self.opening).remove(&self.window_id);
    }
}

/// `lookup`, retried once after any open of `window_id` still in progress
/// has finished. The first lookup comes first so a registered window never
/// waits on anything.
async fn lookup_once_opened<T>(
    opening: &OpeningWindows,
    window_id: &str,
    lookup: impl Fn() -> Option<T>,
) -> Option<T> {
    if let Some(found) = lookup() {
        return Some(found);
    }
    let pending = lock_recover(opening).get(window_id).cloned();
    if let Some(mut pending) = pending {
        // Nothing is ever sent, so this returns only once the sender is gone.
        let _closed = pending.changed().await;
    }
    lookup()
}

impl std::fmt::Debug for WindowRegistry {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("WindowRegistry")
            .field("window_count", &lock_recover(&self.entries).len())
            .finish_non_exhaustive()
    }
}

/// The environment a window's child is given on top of what it inherits:
/// its own identity (only when its token is verifiable -- a window id
/// without auth cannot authenticate status events), the MCP bridge path when
/// one was resolved, the report directory in front of `PATH` when one was
/// prepared, and the profile's own `[env]` table (whose `PATH` wins).
///
/// On a Windows host, `WSLENV` is replaced by one value naming every
/// variable above, so they also cross into a WSL child; the profile's own
/// `WSLENV` (or, without one, the daemon's) is kept as its base rather than
/// passed a second time. The bridge path is forwarded with `/p` so WSL
/// translates it at the boundary. The instance name is only forwarded, never
/// set: a child inherits it from the daemon like any other variable.
fn child_environment(
    config: &DaemonConfig,
    status_token: &StatusToken,
    window_id: &str,
    extra_profile_env: &[(String, String)],
) -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = Vec::new();
    let mut forwarded: Vec<String> = Vec::new();
    if let Some(hex) = status_token.as_hex() {
        env.push((STATUS_TOKEN_ENV.to_string(), hex.to_string()));
        env.push((WINDOW_ID_ENV.to_string(), window_id.to_string()));
        forwarded.push(WINDOW_ID_ENV.to_string());
        forwarded.push(STATUS_TOKEN_ENV.to_string());
    }
    if let Some(bridge) = config.mcp_bridge.as_deref() {
        env.push((
            MCP_BRIDGE_ENV.to_string(),
            bridge.to_string_lossy().into_owned(),
        ));
        forwarded.push(format!("{MCP_BRIDGE_ENV}/p"));
    }
    if let Some(path) = config
        .report_dir
        .as_deref()
        .and_then(|dir| path_led_by(dir, &inherited_child_path(config)))
    {
        env.push((PATH_ENV.to_string(), path));
    }

    let forwards_across_wsl = config.host_os.platform() == program::Platform::Windows;
    let mut profile_wslenv: Option<String> = None;
    for (key, value) in extra_profile_env {
        if forwards_across_wsl && key.eq_ignore_ascii_case(WSLENV) {
            profile_wslenv = Some(value.clone());
        } else {
            env.push((key.clone(), value.clone()));
        }
    }

    if forwards_across_wsl {
        if std::env::var_os(INSTANCE_ENV).is_some() {
            forwarded.push(INSTANCE_ENV.to_string());
        }
        let base = profile_wslenv
            .clone()
            .or_else(|| std::env::var_os(WSLENV).and_then(|value| value.into_string().ok()));
        let names: Vec<&str> = forwarded.iter().map(String::as_str).collect();
        let merged = yagura_os::merge_wslenv(base.as_deref(), &names);
        if profile_wslenv.is_some() || !merged.is_empty() {
            env.push((WSLENV.to_string(), merged));
        }
    }
    env
}

/// The variable naming which environment variables cross the WSL boundary.
const WSLENV: &str = "WSLENV";

const PATH_ENV: &str = "PATH";

/// Makes cmd skip the working directory when it looks a program up by name.
const NO_CWD_PROGRAM_SEARCH_ENV: &str = "NoDefaultCurrentDirectoryInExePath";

/// The `PATH` a child receives before this daemon adds to it: the base
/// environment's, else the PTY layer's default. On Windows that default is
/// built from the registry, not the daemon's own `PATH`, so it is asked for
/// rather than read from this process.
fn inherited_child_path(config: &DaemonConfig) -> Option<std::ffi::OsString> {
    config
        .child_base_env
        .get(std::ffi::OsStr::new(PATH_ENV))
        .map(std::ffi::OsStr::to_os_string)
        .or_else(|| yagura_os::pty::default_env_var(std::ffi::OsStr::new(PATH_ENV)))
}

/// `dir` followed by `inherited` kept verbatim. `None` when the result cannot
/// be expressed as a `PATH` string, leaving the child's `PATH` untouched.
fn path_led_by(dir: &Path, inherited: &Option<std::ffi::OsString>) -> Option<String> {
    let mut path = std::env::join_paths([dir]).ok()?;
    if let Some(inherited) = inherited.as_ref().filter(|value| !value.is_empty()) {
        path.push(if cfg!(windows) { ";" } else { ":" });
        path.push(inherited);
    }
    path.into_string().ok()
}

/// An injection whose settings file is on disk, ready to append to the spawn
/// argv.
struct PreparedInjection {
    kind: InjectionKind,
    args: Vec<String>,
}

/// `injection`'s args, or nothing when `resolution` wraps a
/// command-interpreter shim and one of them carries a character that
/// interpreter would reinterpret: the window still opens, only without that
/// injection (the other kind is judged on its own).
fn injection_args_for<'a>(
    resolution: &program::Resolution,
    injection: &'a PreparedInjection,
) -> &'a [String] {
    let reinterpreted = matches!(resolution, program::Resolution::CmdShim { .. })
        && injection
            .args
            .iter()
            .any(|arg| program::contains_cmd_metacharacter(arg));
    if reinterpreted {
        tracing::warn!(
            "dropped the {} injection: the command interpreter would reinterpret one of its \
             arguments",
            injection.kind.label()
        );
        return &[];
    }
    &injection.args
}

/// Blocking: writes `file` unless a file of that name is already there. The
/// name is derived from the content, so an existing file already holds it.
fn write_injected_config_file(file: &InjectedConfigFile) -> std::io::Result<()> {
    if file.path().is_file() {
        return Ok(());
    }
    let base = file.dir.parent().unwrap_or(&file.dir);
    yagura_os::replace_config_file(base, &file.dir, &file.file_name, &file.content)
}

/// Validates the entire open request: command, display name, vendor label,
/// detection name, and working directory. Applies the documented defaults, so
/// every validated field is ready for spawn and storage.
fn validate_open_request(
    command: Vec<String>,
    cwd: Option<RequestedCwd>,
    default_cwd: Option<&Path>,
    name: Option<String>,
    vendor: Option<String>,
    detection: Option<String>,
) -> Result<ValidatedRequest, WindowOpenError> {
    let Some(command0) = command.first() else {
        return Err(WindowOpenError::InvalidArgument {
            reason: "command must not be empty".to_string(),
            argument: None,
        });
    };
    // A NUL byte survives JSON decoding, so it is rejected here; an empty
    // element is a profile-authoring rule, not a spawn invariant.
    yagura_protocol::validate_argv_nul_free(&command).map_err(|err| {
        WindowOpenError::InvalidArgument {
            reason: err.to_string(),
            argument: None,
        }
    })?;

    // Every display name is checked at the boundary that stores it, so no
    // window can carry a name `rename` would refuse. The derived default
    // goes through the same gate because `command[0]` is caller-controlled.
    let name = name.unwrap_or_else(|| default_name_from_command(command0));
    validate_window_name(&name).map_err(|err| WindowOpenError::InvalidArgument {
        reason: err.to_string(),
        argument: None,
    })?;

    // The vendor is checked by the same gate as the name because the GUI
    // renders it as a badge beside the name: an unchecked vendor would give
    // back the impersonation reach the name rule takes away.
    if let Some(vendor) = vendor.as_deref() {
        validate_window_vendor(vendor).map_err(|err| WindowOpenError::InvalidArgument {
            reason: format!("vendor: {err}"),
            argument: None,
        })?;
    }

    // CWE-22: the detection name becomes a path component before it is ever
    // used, so it is validated up front with the single charset check.
    if let Some(detection) = detection.as_deref()
        && !is_valid_detection_name(detection)
    {
        return Err(WindowOpenError::InvalidArgument {
            reason: format!("detection name must match [a-z0-9_-]{{1,64}}, got {detection:?}"),
            argument: None,
        });
    }

    // A start-up default that has since disappeared is refused rather than
    // swapped for the daemon's directory, so a window never opens somewhere
    // nobody chose without a word.
    let resolved_cwd = match (cwd.as_ref(), default_cwd) {
        (Some(requested), _) => resolve_existing_dir(requested.path(), requested.origin())?,
        (None, Some(default)) => resolve_existing_dir(default, CwdOrigin::DaemonDefault)?,
        (None, None) => {
            let current = std::env::current_dir().map_err(|err| {
                WindowOpenError::invalid_cwd(
                    CwdOrigin::DaemonCurrentDir,
                    format!("failed to resolve the daemon's current directory: {err}"),
                )
            })?;
            resolve_existing_dir(&current, CwdOrigin::DaemonCurrentDir)?
        }
    };
    let cwd_string = resolved_cwd.to_string_lossy().to_string();

    Ok(ValidatedRequest {
        name,
        vendor,
        resolved_cwd,
        cwd_string,
    })
}

/// Canonicalizes `path`, strips the Windows extended-length prefix and
/// rejects anything that does not resolve to an existing directory. Every
/// failure is reported as
/// [`WindowOpenError::InvalidArgument`]: a missing or non-existent `cwd` is
/// a caller mistake, not a spawn failure.
///
/// `origin` says where `path` came from, and is handed to the one classifier
/// ([`WindowOpenError::invalid_cwd`]) that turns it into the refused
/// argument, so no caller has to classify the failure itself.
///
/// Reachable from `ipc.rs` so that a launch planned from a profile is held to
/// the same rule *before* anything is spawned, without a second definition of
/// what an acceptable working directory is.
pub(crate) fn resolve_existing_dir(
    path: &Path,
    origin: CwdOrigin,
) -> Result<PathBuf, WindowOpenError> {
    let canonical = path.canonicalize().map_err(|_| {
        WindowOpenError::invalid_cwd(
            origin,
            format!(
                "cwd does not exist or cannot be resolved: {}",
                path.display()
            ),
        )
    })?;
    if !canonical.is_dir() {
        return Err(WindowOpenError::invalid_cwd(
            origin,
            format!("cwd is not a directory: {}", path.display()),
        ));
    }
    Ok(crate::strip_extended_length_prefix(&canonical))
}

/// Derives the default window name from `command[0]`'s file stem, or the
/// whole value if it has none.
fn default_name_from_command(command0: &str) -> String {
    Path::new(command0)
        .file_stem()
        .and_then(|stem| stem.to_str())
        .map(str::to_string)
        .unwrap_or_else(|| command0.to_string())
}

/// What [`load_definition_for`] resolved: either the definition to run the
/// detection engine with, or the reason there is none.
///
/// Two variants rather than two optional fields, so "no definition and no
/// reason given" -- a window left `Unknown` with nothing to tell its client --
/// cannot be built anywhere in this crate.
// One of these is built and moved at window open only, and the definition
// reaches the detection engine by value from there; boxing it would buy an
// allocation and an unboxing at that boundary and nothing else.
#[allow(clippy::large_enum_variant)]
pub(crate) enum DetectionLoadOutcome {
    /// A definition loaded: the one outcome with no window-facing warning.
    Loaded(Definition),
    /// No definition to run, carrying the English reason text that travels
    /// back to the `open_window` response: a load failure, or no definition
    /// layer configured at all.
    Failed(String),
}

/// The one place a definition's `[composer]` and `[input].probe` turn into
/// the value the rest of the crate acts on, so every reader of a loaded
/// definition -- at window open and before it -- applies the same rule.
fn definition_composer_access(definition: &Definition) -> ComposerAccess {
    if definition.composer.is_none() {
        return ComposerAccess::Absent;
    }
    match InputProfile::from_section(definition.input.as_ref()).probe {
        Some(probe) => ComposerAccess::Judged { probe },
        None => ComposerAccess::RawOnly,
    }
}

/// Loads the detection definition named by `name_override`, or by
/// [`DaemonConfig::detection_name`] when `name_override` is `None`
/// (`open_window` requests forward their own per-window override here).
///
/// Thin wrapper over [`DetectionLayers::resolve`]: every failure is logged
/// and reported as [`DetectionLoadOutcome::Failed`], whose English reason
/// pins the engine to `Unknown` while the daemon keeps serving (the fail-safe
/// applied per window) and lets the caller tell the client why. Neither layer
/// configured is reported the same way: a client cannot otherwise tell a
/// window running no detection from one whose detection is working.
pub(crate) fn load_definition_for(
    config: &DaemonConfig,
    name_override: Option<&str>,
) -> DetectionLoadOutcome {
    let layers = DetectionLayers::from_config(config);
    let name = name_override.unwrap_or(config.detection_name.as_str());
    match layers.resolve(name) {
        Ok(resolved) => {
            log_definition_loaded(&resolved);
            DetectionLoadOutcome::Loaded(resolved.definition)
        }
        Err(err @ LayerError::Disabled) => {
            // The variant's own Display already states the fact and its
            // consequence, so the operator log and the client-facing warning
            // say it with one sentence rather than two literals.
            let reason = err.to_string();
            tracing::info!("{reason}");
            DetectionLoadOutcome::Failed(reason)
        }
        Err(LayerError::InvalidName) => {
            let reason = "invalid detection name; status stays unknown";
            tracing::error!(
                detection_name = %name,
                "{reason}"
            );
            DetectionLoadOutcome::Failed(reason.to_string())
        }
        Err(err) => {
            let rendered = err.to_string();
            tracing::error!("{}", detection_load_failure_text(&rendered));
            DetectionLoadOutcome::Failed(detection_load_failure_text(&layer_error_summary(
                &err, &rendered,
            )))
        }
    }
}

/// Logs the name, layer and (for the directory layer) the path, size and
/// modification time (Unix seconds) a successfully resolved definition came
/// from. A file size or modification time that could not be read is logged
/// as `unknown` rather than omitted.
fn log_definition_loaded(resolved: &ResolvedDefinition) {
    match &resolved.source {
        DefinitionSource::Bundled => {
            tracing::info!(
                detection_name = %resolved.definition.name,
                source = "bundled",
                "loaded the detection definition"
            );
        }
        DefinitionSource::Directory {
            path,
            bytes,
            modified,
        } => {
            let bytes = bytes.map_or_else(|| "unknown".to_string(), |bytes| bytes.to_string());
            let modified_unix_secs = modified
                .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
                .map_or_else(
                    || "unknown".to_string(),
                    |elapsed| elapsed.as_secs().to_string(),
                );
            tracing::info!(
                detection_name = %resolved.definition.name,
                source = "directory",
                path = %path.display(),
                bytes = %bytes,
                modified_unix_secs = %modified_unix_secs,
                "loaded the detection definition"
            );
        }
    }
}

/// Reduces a load failure to the text that may cross the process boundary
/// towards clients and MCP callers, sanitizing per variant: a content-free
/// stock message for variants whose `Display` embeds raw file content in
/// their first line (`InvalidPattern`), or the shared line-cut summary
/// otherwise.
///
/// Pair contract: adding a `DefinitionError` variant or changing an
/// `#[error(...)]` string requires updating both this match and the
/// InvalidPattern canary test.
fn boundary_summary(err: &DefinitionError, rendered: &str) -> String {
    match err {
        // The pattern field embeds raw file content in the FIRST line of
        // Display, so line-cutting cannot sanitize it.
        DefinitionError::InvalidPattern { .. } => {
            "invalid regex pattern in the detection definition (see the daemon log for details)"
                .to_string()
        }
        // These variants' Display does not embed raw file content in a way
        // line-cutting fails to sanitize, so the shared summary applies.
        DefinitionError::Io { .. }
        | DefinitionError::SourceTooLarge { .. }
        | DefinitionError::Toml(_)
        | DefinitionError::UnsupportedSchemaVersion { .. }
        | DefinitionError::TooManyRules { .. }
        | DefinitionError::InvalidInput { .. } => summarize_definition_load_failure(rendered),
    }
}

/// [`boundary_summary`]'s counterpart for a [`LayerError`]: a
/// [`LayerError::Directory`] delegates to `boundary_summary` for its own
/// `DefinitionError`, while every other variant's `Display` carries no raw
/// file content, so the shared line-cut summary applies directly.
fn layer_error_summary(err: &LayerError, rendered: &str) -> String {
    match err {
        LayerError::Directory { source, .. } => boundary_summary(source, rendered),
        LayerError::Disabled
        | LayerError::InvalidName
        | LayerError::Unscannable { .. }
        | LayerError::NotFound { .. } => summarize_definition_load_failure(rendered),
    }
}

/// Upper bound, in code points, on the failure summary carried by a
/// `open_window` detection warning, matching the GUI's own detail cap.
const DETECTION_WARNING_SUMMARY_MAX_CHARS: usize = 200;

/// Wraps a load-failure detail in the reason text shared by the log line and
/// the client-facing warning, whose details differ.
fn detection_load_failure_text(detail: &str) -> String {
    format!("failed to load the detection definition ({detail}); status stays unknown")
}

/// Reduces a rendered load failure to its first line, capped at
/// [`DETECTION_WARNING_SUMMARY_MAX_CHARS`] code points: the TOML parser echoes
/// the offending source line after its first line, so only this summary may
/// cross the process boundary towards clients and MCP callers.
fn summarize_definition_load_failure(rendered: &str) -> String {
    rendered
        .lines()
        .next()
        .unwrap_or_default()
        .chars()
        .take(DETECTION_WARNING_SUMMARY_MAX_CHARS)
        .collect()
}

/// Tells the detector each time output reaches the window's model,
/// independently of whether a client is attached: detection must keep
/// converging while the GUI is away. The detector reads the model itself, so
/// arrivals that pile up collapse into one and nothing is lost. Holds only a
/// [`Weak`] reference to the detector so this task can never be the thing
/// keeping `Exited` teardown from completing; it upgrades on every arrival
/// and breaks as soon as that fails.
pub(crate) fn spawn_output_pump(session: &PtySession, detector: Weak<DetectorHandle>) {
    let mut arrivals = session.output_arrivals();
    // Output the child wrote before this call is already in the model.
    if *arrivals.borrow_and_update() > 0
        && let Some(detector) = detector.upgrade()
    {
        detector.notify_output();
    }
    tokio::spawn(async move {
        while arrivals.changed().await.is_ok() {
            let Some(detector) = detector.upgrade() else {
                break;
            };
            detector.notify_output();
        }
    });
}

/// Turns the child-exit fact into the detector's highest-priority evidence.
/// One of the two intended long-lived strong holders of
/// `Arc<DetectorHandle>`: this task's own clone is dropped when it returns,
/// right after the exit fact has been handed to the engine.
///
/// The process tree and the PTY are wound down by the session's own exit
/// monitor ([`PtySession`]), so this task has no teardown of its own.
pub(crate) fn spawn_exit_watcher(session: Arc<PtySession>, detector: Arc<DetectorHandle>) {
    let mut exit_rx = session.exit_notified();
    tokio::spawn(async move {
        loop {
            // The current value is inspected before awaiting so that a
            // child which exited before this task started is still
            // observed.
            if *exit_rx.borrow() {
                break;
            }
            if exit_rx.changed().await.is_err() {
                break;
            }
        }
        if !*exit_rx.borrow() {
            return;
        }
        tracing::info!(
            exit_code = ?session.exit_status(),
            "child process exited; notifying the detector"
        );
        detector.notify_exited();
    });
}

/// Consumes one window's confirmed-status stream and republishes it as (a)
/// the current value [`WindowEntry::current_status`] reports and (b) a
/// fan-out to attach holders. Once a confirmed `Exited` arrives, this is
/// the task that takes `entry.detector`, dropping the entry's strong
/// reference to the detector.
///
/// Takes only a [`Weak<WindowEntry>`]: this task must never be the thing
/// keeping an evicted entry alive.
pub(crate) fn spawn_status_forwarder(
    entry: Weak<WindowEntry>,
    mut detected_rx: mpsc::UnboundedReceiver<ConfirmedStatus>,
) {
    tokio::spawn(async move {
        while let Some(confirmed) = detected_rx.recv().await {
            let Some(entry) = entry.upgrade() else {
                break;
            };
            publish_status(
                entry.window_id(),
                &entry.current_status,
                &entry.status_changes,
                &entry.ready_hold,
                confirmed,
            );
            if confirmed.status == DetectedStatus::Exited {
                *lock_recover(&entry.detector) = None;
            }
        }
        // The stream ended, so no further status can ever be confirmed for this
        // window -- the driver task finished or died. An entry that has since
        // been evicted needs nothing, so a failed upgrade is a no-op.
        if let Some(entry) = entry.upgrade() {
            publish_detector_loss(
                entry.window_id(),
                &entry.current_status,
                &entry.status_changes,
                &entry.ready_hold,
            );
        }
    });
}

/// The `snake_case` wire string a [`WindowStatus`] serializes to (the same
/// spelling [`UnknownReason::as_wire`] uses for its own values), read off
/// its `Serialize` impl rather than restated here, so a log line or a
/// diagnostic message can never drift from the wire form the protocol crate
/// defines. Falls back to the value's `Debug` text, never reachable for the
/// defined variants (a plain field-less enum's `Serialize` impl cannot
/// fail), rather than dropping the value from its caller.
pub(crate) fn window_status_wire(status: WindowStatus) -> String {
    serde_json::to_value(status)
        .ok()
        .and_then(|value| value.as_str().map(str::to_string))
        .unwrap_or_else(|| format!("{status:?}"))
}

/// Publishes one confirmed status on a window's status sinks: the current
/// value and the fan-out to attach holders.
///
/// The status and the instant it changed go out as one value, so an observer
/// that has seen the new status can never read the previous change's age. The
/// startup latch is offered the *outgoing* pair before that, which is the
/// only point that sees how long a status actually lasted. Every call is one
/// confirmed transition, so it logs exactly one `window status confirmed`
/// info event, carrying `unknown_reason` only when `status` is `Unknown`.
///
/// The single implementation both [`spawn_status_forwarder`]'s receive loop
/// and [`publish_detector_loss`] go through, so the two can never drift into
/// updating different subsets of the sinks.
fn publish_status(
    window_id: &str,
    current_status: &watch::Sender<StatusAt>,
    status_changes: &broadcast::Sender<StatusAt>,
    ready_hold: &ReadyHoldLatch,
    confirmed: ConfirmedStatus,
) {
    let now = tokio::time::Instant::now();
    let status = confirmed.status;
    // Never later than `changed_at`.
    let evidence_since = confirmed.evidence_since.min(now);
    ready_hold.observe(*current_status.borrow(), now);
    // Published before the fan-out so a concurrent attach reports the new
    // status rather than a stale one. `send_modify` (not `send`): no receiver
    // is kept alive and `send` no-ops when there are none, and the new
    // generation is derived from the value being replaced while that value's
    // write lock is held.
    let mut generation = 0;
    current_status.send_modify(|slot| {
        generation = slot.generation.saturating_add(1);
        *slot = StatusAt {
            status,
            changed_at: now,
            evidence_since,
            generation,
        };
    });
    // The fan-out carries the pair and its generation as one value, so a
    // receiver reports the transition it was handed rather than whatever the
    // window has reached by the time it reads.
    let _ = status_changes.send(StatusAt {
        status,
        changed_at: now,
        evidence_since,
        generation,
    });
    let wire_status = window_status_wire(status.status());
    match status.unknown_reason() {
        Some(reason) => tracing::info!(
            window_id = %window_id,
            status = %wire_status,
            unknown_reason = %reason.as_wire(),
            "window status confirmed"
        ),
        None => tracing::info!(
            window_id = %window_id,
            status = %wire_status,
            "window status confirmed"
        ),
    }
}

/// Fail-safe backstop for a window whose detection stopped reporting:
/// publishes `Unknown(DetectorStopped)` so the window is not left frozen at
/// whatever status it happened to hold when its driver task died.
///
/// Both guards live here rather than at the call site, so every caller gets
/// the same behavior: a window that already reached `Exited` keeps that
/// mechanical fact, and a window already at `Unknown(DetectorStopped)`
/// publishes nothing, which makes repeated calls idempotent -- a window at a
/// *different* `Unknown` reason is still overwritten, so the reason this
/// backstop fired is never masked by whatever reason preceded it. The current value is read back through the
/// `watch::Sender` itself, since no receiver is kept alive for it.
///
/// Takes the four sinks rather than a `WindowEntry` so it can be exercised
/// against plain channels, and does not touch `entry.detector`: clearing that
/// is the receive loop's job on a confirmed `Exited` alone. The startup latch
/// is among them because the fail-safe transition is still a status ending,
/// and the status it ends may be the one that proves this window left its
/// startup phase.
fn publish_detector_loss(
    window_id: &str,
    current_status: &watch::Sender<StatusAt>,
    status_changes: &broadcast::Sender<StatusAt>,
    ready_hold: &ReadyHoldLatch,
) {
    let current = current_status.borrow().status;
    let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
    if current == DetectedStatus::Exited || current == stopped {
        return;
    }
    tracing::warn!(
        "the detection driver stopped reporting for a live window; failing its status back to \
         unknown"
    );
    publish_status(
        window_id,
        current_status,
        status_changes,
        ready_hold,
        ConfirmedStatus {
            status: stopped,
            evidence_since: tokio::time::Instant::now(),
        },
    );
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    // =================================================================
    // `publish_detector_loss`, exercised on plain channels so no PTY,
    // detector or forwarder task is involved.
    // =================================================================

    /// The sinks `publish_detector_loss` writes to, plus a subscriber on the
    /// fan-out, so a test can assert on all of them at once.
    struct StatusSinks {
        window_id: String,
        current: watch::Sender<StatusAt>,
        changes: broadcast::Sender<StatusAt>,
        ready_hold: ReadyHoldLatch,
        fanout: broadcast::Receiver<StatusAt>,
        /// The instant the published pair carried before the call under test,
        /// set well in the past so that "the timestamp moved" is decidable
        /// regardless of the clock's resolution.
        initial_changed_at: tokio::time::Instant,
    }

    impl StatusSinks {
        fn new(current_status: DetectedStatus) -> Self {
            let initial_changed_at = tokio::time::Instant::now()
                .checked_sub(std::time::Duration::from_secs(60))
                .expect("the test clock must support a 60s offset into the past");
            let (current, _) = watch::channel(StatusAt {
                status: current_status,
                changed_at: initial_changed_at,
                evidence_since: initial_changed_at,
                generation: 0,
            });
            let (changes, fanout) = broadcast::channel(STATUS_BROADCAST_CAPACITY);
            Self {
                window_id: "w-test".to_string(),
                current,
                changes,
                ready_hold: ReadyHoldLatch::new(std::time::Duration::ZERO),
                fanout,
                initial_changed_at,
            }
        }

        fn publish_loss(&self) {
            publish_detector_loss(
                &self.window_id,
                &self.current,
                &self.changes,
                &self.ready_hold,
            );
        }

        /// The status of the next value on the fan-out, if any: the pair's
        /// other fields are the publisher's own bookkeeping, not what these
        /// cases are about.
        fn fanned_out_status(&mut self) -> Option<DetectedStatus> {
            self.fanout.try_recv().ok().map(|at| at.status)
        }

        fn timestamp_moved(&self) -> bool {
            self.current.borrow().changed_at > self.initial_changed_at
        }
    }

    #[test]
    fn publish_detector_loss_fails_a_live_window_back_to_unknown() {
        // All three sinks are written.
        let mut sinks = StatusSinks::new(DetectedStatus::Running);

        sinks.publish_loss();

        let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
        assert_eq!(sinks.current.borrow().status, stopped);
        assert_eq!(sinks.fanned_out_status(), Some(stopped));
        assert!(
            sinks.timestamp_moved(),
            "the fail-safe transition is a status change and must move the change instant"
        );
    }

    #[test]
    fn publish_detector_loss_never_overwrites_an_exited_window() {
        // `Exited` is a mechanical fact that outranks the backstop.
        let mut sinks = StatusSinks::new(DetectedStatus::Exited);

        sinks.publish_loss();

        assert_eq!(sinks.current.borrow().status, DetectedStatus::Exited);
        assert_eq!(sinks.fanned_out_status(), None, "nothing may be fanned out");
        assert!(!sinks.timestamp_moved(), "the change instant must stay put");
    }

    #[test]
    fn publish_detector_loss_is_idempotent_once_the_window_is_already_unknown() {
        // A repeated call publishes nothing, so the backstop cannot spam
        // subscribers with identical transitions.
        let mut sinks = StatusSinks::new(DetectedStatus::Running);

        sinks.publish_loss();
        let stopped = DetectedStatus::Unknown(UnknownReason::DetectorStopped);
        assert_eq!(sinks.fanned_out_status(), Some(stopped));

        sinks.publish_loss();

        assert_eq!(sinks.current.borrow().status, stopped);
        assert_eq!(
            sinks.fanned_out_status(),
            None,
            "a second call at Unknown(DetectorStopped) must fan out nothing"
        );
    }

    // =================================================================
    // publish_status must hand the *outgoing* pair to the ready-hold latch
    // before publishing the new status, so a window that already held a ready
    // status for the full hold latches even though the status it is
    // transitioning to is not itself ready.
    // =================================================================

    #[test]
    fn publish_status_passes_the_outgoing_pair_to_the_ready_hold_latch() {
        use crate::handoff::ReadyHoldLatch;
        use crate::waiter::StatusAt;

        let hold = std::time::Duration::from_millis(1000);
        let ready_hold = ReadyHoldLatch::new(hold);
        let past = tokio::time::Instant::now()
            .checked_sub(std::time::Duration::from_millis(1500))
            .expect("the test clock must support a 1500ms offset into the past");
        let (current, _rx) = watch::channel(StatusAt {
            status: DetectedStatus::WaitingInput,
            changed_at: past,
            evidence_since: past,
            generation: 0,
        });
        let (changes, _fanout) = broadcast::channel(STATUS_BROADCAST_CAPACITY);

        publish_status(
            "w-test",
            &current,
            &changes,
            &ready_hold,
            ConfirmedStatus {
                status: DetectedStatus::Running,
                evidence_since: tokio::time::Instant::now(),
            },
        );

        assert!(
            ready_hold.is_latched(),
            "publish_status must observe the outgoing pair against the ready-hold latch \
             before publishing the new status, so a window that held ready for the full \
             duration latches even though the new status is not itself ready"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn publish_status_restarts_the_age_at_every_transition_on_the_paused_clock() {
        use crate::handoff::ReadyHoldLatch;
        use crate::waiter::StatusAt;

        let opened_at = tokio::time::Instant::now();
        let (current, _rx) = watch::channel(StatusAt {
            status: DetectedStatus::Running,
            changed_at: opened_at,
            evidence_since: opened_at,
            generation: 0,
        });
        let (changes, mut fanout) = broadcast::channel(STATUS_BROADCAST_CAPACITY);
        let ready_hold = ReadyHoldLatch::new(std::time::Duration::ZERO);
        let publish = |status: DetectedStatus| {
            publish_status(
                "w-test",
                &current,
                &changes,
                &ready_hold,
                ConfirmedStatus {
                    status,
                    evidence_since: tokio::time::Instant::now(),
                },
            );
        };

        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        assert_eq!(
            current.borrow().since_ms(tokio::time::Instant::now()),
            5_000,
            "the age of the opening status grows with the clock"
        );

        publish(DetectedStatus::Idle);
        assert_eq!(current.borrow().since_ms(tokio::time::Instant::now()), 0);
        tokio::time::advance(std::time::Duration::from_millis(250)).await;
        assert_eq!(current.borrow().since_ms(tokio::time::Instant::now()), 250);

        publish(DetectedStatus::Running);
        assert_eq!(
            current.borrow().since_ms(tokio::time::Instant::now()),
            0,
            "a new transition must restart the age"
        );
        let fanned_out = fanout.try_recv().and_then(|_| fanout.try_recv());
        assert_eq!(
            fanned_out.map(|at| at.since_ms(tokio::time::Instant::now())),
            Ok(0),
            "the fan-out value must carry the same restarted age"
        );
    }

    /// Long enough for a lookup that does not wait to have finished.
    const STILL_WAITING: Duration = Duration::from_millis(50);

    #[tokio::test]
    async fn a_lookup_waits_for_an_open_in_progress_and_then_finds_the_registered_window() {
        let opening = OpeningWindows::default();
        let registered: Mutex<Option<&str>> = Mutex::new(None);
        let guard = open_in_progress(&opening, "w-pending");

        let lookup = lookup_once_opened(&opening, "w-pending", || *lock_recover(&registered));
        tokio::pin!(lookup);
        assert!(
            tokio::time::timeout(STILL_WAITING, &mut lookup)
                .await
                .is_err(),
            "a lookup must wait while the open of that id is still in progress"
        );

        *lock_recover(&registered) = Some("entry");
        drop(guard);
        assert_eq!(lookup.await, Some("entry"));
    }

    #[tokio::test]
    async fn a_lookup_for_an_open_that_fails_finds_nothing_once_it_ends() {
        let opening = OpeningWindows::default();
        let guard = open_in_progress(&opening, "w-pending");

        let lookup = lookup_once_opened(&opening, "w-pending", || None::<&str>);
        tokio::pin!(lookup);
        assert!(
            tokio::time::timeout(STILL_WAITING, &mut lookup)
                .await
                .is_err(),
            "a lookup must wait while the open of that id is still in progress"
        );

        drop(guard);
        assert_eq!(lookup.await, None);
    }

    #[tokio::test]
    async fn dropping_an_open_in_progress_always_removes_it() {
        let registry = WindowRegistry::new(DaemonConfig::default());
        let guard = registry.begin_opening("w-pending");
        assert!(lock_recover(&registry.opening).contains_key("w-pending"));

        drop(guard);
        assert!(lock_recover(&registry.opening).is_empty());
        let settled = tokio::time::timeout(STILL_WAITING, registry.get_after_open("w-pending"))
            .await
            .expect("no open is in progress, so the lookup must not wait");
        assert!(settled.is_none());
    }

    #[test]
    fn to_base36_carries_every_digit_of_the_value_least_significant_first() {
        assert_eq!(to_base36(36, 2), "01");
        assert_eq!(to_base36(36 * 36, 3), "001");
        assert_eq!(to_base36(35 + 36 * 10, 2), "za");
    }

    #[tokio::test]
    async fn only_one_notice_agent_is_claimed_until_it_is_released() {
        let registry = WindowRegistry::new(DaemonConfig::default());
        let opened = registry
            .open(OpenWindowRequest {
                command: yagura_test_support::process::echo_child_command(),
                cwd: None,
                name: None,
                vendor: None,
                detection: None,
            })
            .await
            .expect("an echo_child window must open");
        let entry = opened.entry;

        assert!(entry.claim_notice_agent());
        assert!(
            !entry.claim_notice_agent(),
            "a second claim while an agent runs must be refused, or two agents deliver one notice"
        );
        assert!(entry.release_notice_agent().is_empty());
        assert!(
            entry.claim_notice_agent(),
            "a released agent must leave the window claimable again"
        );
        registry.terminate_all().await;
    }

    #[tokio::test]
    async fn the_boot_nonce_is_the_one_window_ids_carry_and_differs_per_registry() {
        let registry = WindowRegistry::new(DaemonConfig::default());
        let other = WindowRegistry::new(DaemonConfig::default());

        let window_id = registry.allocate_window_id();
        assert_eq!(
            window_id.rsplit_once('-').map(|(_, nonce)| nonce),
            Some(registry.boot_nonce()),
            "approval request ids are minted from boot_nonce, so it must be the nonce window ids carry"
        );
        assert_ne!(
            registry.boot_nonce(),
            other.boot_nonce(),
            "an id minted by one daemon must never resolve in another"
        );
    }

    #[tokio::test]
    async fn config_is_the_configuration_the_registry_was_built_with() {
        let configured = crate::config::HANDOFF_READY_TIMEOUT_DEFAULT_SECS + 1;
        let registry = WindowRegistry::new(DaemonConfig {
            handoff_ready_timeout_secs: configured,
            ..DaemonConfig::default()
        });
        assert_eq!(
            registry.config().handoff_ready_timeout_secs,
            configured,
            "notice delivery reads the playbook's handoff_ready_timeout_secs through config()"
        );
    }

    #[test]
    fn a_windows_child_without_its_own_wslenv_gets_one_forwarding_its_identity() {
        let config = DaemonConfig {
            host_os: crate::profile::ProfileOs::Windows,
            ..DaemonConfig::default()
        };
        let token = StatusToken::from_override("0123456789abcdef0123456789abcdef")
            .expect("the override is a well-formed token");

        let env = child_environment(&config, &token, "w1-test", &[]);

        let wslenv = env
            .iter()
            .find(|(key, _)| key == WSLENV)
            .map(|(_, value)| value.as_str())
            .expect("a WSL child must be told which variables cross the boundary");
        let forwarded: Vec<&str> = wslenv
            .split(':')
            .map(|item| item.split('/').next().unwrap_or(item))
            .collect();
        for name in [WINDOW_ID_ENV, STATUS_TOKEN_ENV] {
            assert!(
                forwarded.contains(&name),
                "WSLENV ({wslenv:?}) must forward {name}"
            );
        }
    }

    #[tokio::test]
    async fn output_that_reached_the_model_before_the_pump_started_is_still_judged() {
        let config = DaemonConfig::default();
        let marker = yagura_test_support::detection::CONTROL_READY_MARKER;
        let command = yagura_test_support::process::ready_child_command(marker);
        let argv: Vec<&str> = command.iter().map(String::as_str).collect();
        let session = PtySession::spawn_with_command(&argv, &config)
            .await
            .expect("a ready_child session must spawn");

        // The pump starts only once the marker is in the model, so nothing
        // but its look at what arrived before it can report the marker.
        let mut arrivals = session.output_arrivals();
        tokio::time::timeout(Duration::from_secs(10), async {
            while !session
                .text_tail(100, 1 << 16)
                .await
                .is_some_and(|(text, _)| text.contains(marker))
            {
                arrivals
                    .changed()
                    .await
                    .expect("the output channel must stay open while the child runs");
            }
        })
        .await
        .expect("the child's marker must reach the model");

        let definition = crate::detector::definition::parse_definition(
            &yagura_test_support::detection::idle_definition_toml("pump-test", "", ""),
        )
        .expect("the idle definition must parse");
        let engine = DetectionEngine::new(Some(definition), &config, session.model_for_detection());
        let (handle, mut statuses) = DetectorHandle::spawn(engine);
        let handle = Arc::new(handle);
        spawn_output_pump(&session, Arc::downgrade(&handle));

        // The definition has no time table, so the detector reads the model
        // only when told output arrived.
        let confirmed = tokio::time::timeout(Duration::from_secs(10), statuses.recv())
            .await
            .expect("the marker that arrived before the pump must still be judged")
            .expect("the detector must still be running");
        assert_eq!(confirmed.status, DetectedStatus::Idle);
        session.terminate();
    }
}
